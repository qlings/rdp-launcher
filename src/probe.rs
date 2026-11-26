//! 后台 TCP 端口探活。
//!
//! 设计（v0.6.0）：
//!
//! * 冷启动不等探活：窗口先画出来，结果回来再 `request_repaint()` 更新圆点。
//! * **启动测一次，之后定时刷新**（间隔来自 `settings.probe_interval_secs`，0 = 不自动），
//!   顶部「● 在线 x/y」点一下也能立刻重测。以前只在启动时测一次，机器开久了
//!   圆点就是半年前的结论。
//! * **同一地址 + 端口只连一次**：复制出来的副本和原主机共用一份结果，
//!   结果按 id 扇出（以前每台主机各扫一遍，副本越多越慢）。
//! * **结果按 id 缓存**：刷新时旧结果先留着，新结果回来再覆盖，圆点不会整排变灰。
//! * 有限线程池，最大并发 32，单次连接超时 800ms，单批 2.5 秒截止。

use std::collections::{HashMap, HashSet};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use crate::models::Host;

pub const MAX_CONCURRENCY: usize = 32;
pub const CONNECT_TIMEOUT: Duration = Duration::from_millis(800);
/// 一整批探活的最长时间。DNS 卡住时靠它兜底，避免 UI 一直「检测中」+ 空转重绘。
pub const BATCH_DEADLINE: Duration = Duration::from_millis(2500);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeStatus {
    /// 还没测出结果
    Checking,
    Online,
    Offline,
    /// 设置里关掉了探活
    Disabled,
}

impl ProbeStatus {
    pub fn label(self) -> &'static str {
        match self {
            ProbeStatus::Checking => "检测中",
            ProbeStatus::Online => "在线",
            ProbeStatus::Offline => "离线",
            ProbeStatus::Disabled => "未探测",
        }
    }
}

/// 一条待探测的任务：只带 id / 地址 / 端口，不复制整台 Host（里面有明文密码）。
#[derive(Clone, Debug)]
pub struct Target {
    pub id: String,
    pub host: String,
    pub port: u16,
}

impl Target {
    pub fn from_host(host: &Host) -> Option<Self> {
        if host.host.trim().is_empty() {
            return None;
        }
        Some(Self {
            id: host.id.clone(),
            host: host.host.clone(),
            port: host.port,
        })
    }
}

/// 从主机列表里挑出可探测目标。
pub fn targets_of(hosts: &[Host]) -> Vec<Target> {
    hosts.iter().filter_map(Target::from_host).collect()
}

/// 只探测指定 id 的主机（新增 / 改地址 / 导入之后用，不用全量重扫）。
pub fn targets_for(hosts: &[Host], ids: &[String]) -> Vec<Target> {
    let wanted: HashSet<&str> = ids.iter().map(String::as_str).collect();
    hosts
        .iter()
        .filter(|h| wanted.contains(h.id.as_str()))
        .filter_map(Target::from_host)
        .collect()
}

/// 去重键：地址大小写不敏感，端口参与。
fn dedupe_key(host: &str, port: u16) -> String {
    format!("{}\u{1}{port}", host.trim().to_ascii_lowercase())
}

fn probe_one(task: &Target) -> bool {
    // 域名可能解析出多个地址，任意一个连上就算在线。
    let Ok(addrs) = (task.host.as_str(), task.port).to_socket_addrs() else {
        return false;
    };
    for addr in addrs {
        if TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).is_ok() {
            return true;
        }
    }
    false
}

/// 对一批主机并发探活。
///
/// * 每测完一台就往 `tx` 发一条 `(主机 id, 是否在线)`。
/// * 所有 worker 共享一个队列，谁空了谁退出，不会给空闲主机留线程。
/// * 所有 worker 退出后 `tx` 的全部克隆被释放，接收端的 `recv`/`try_recv`
///   会拿到 `Disconnected` —— 调用方据此判断「全部测完」。
pub fn spawn_probe_batch(tasks: Vec<Target>, tx: Sender<(String, bool)>) {
    if tasks.is_empty() {
        // 没有可测的，直接关掉发送端，让接收端立刻看到 Disconnected
        drop(tx);
        return;
    }

    let total = tasks.len();
    let queue = std::sync::Arc::new(std::sync::Mutex::new(tasks));
    let worker_count = MAX_CONCURRENCY.min(total).max(1);

    for _ in 0..worker_count {
        let queue = std::sync::Arc::clone(&queue);
        let tx = tx.clone();
        std::thread::spawn(move || loop {
            let task = {
                match queue.lock() {
                    Ok(mut q) => q.pop(),
                    Err(_) => None,
                }
            };
            let Some(task) = task else { break };
            let online = probe_one(&task);
            if tx.send((task.id, online)).is_err() {
                break;
            }
        });
    }

    // 原始句柄必须丢掉，否则接收端永远等不到断开信号。
    drop(tx);
}

/// 给 UI 用的状态表：id -> 状态。没有条目的默认当作 Checking。
pub type StatusMap = HashMap<String, ProbeStatus>;

pub fn status_of(map: &StatusMap, id: &str) -> ProbeStatus {
    map.get(id).copied().unwrap_or(ProbeStatus::Checking)
}

/// 一次进行中的探测批次。
struct Run {
    rx: Receiver<(String, bool)>,
    deadline: Instant,
    /// 去重键 -> 这一份结果要扇出给哪些主机 id
    fanout: HashMap<String, Vec<String>>,
}

/// 探活控制器：主程序只跟它打交道，状态、进行中的批次、上次完成时间都在这里。
///
/// 允许同时有多批（例如定时刷新跑到一半时用户改了某台主机的地址），
/// 各批结果互不覆盖，谁先回来谁先落。
#[derive(Default)]
pub struct ProbeRunner {
    statuses: StatusMap,
    runs: Vec<Run>,
    last_finished: Option<Instant>,
}

impl ProbeRunner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn status(&self, id: &str) -> ProbeStatus {
        status_of(&self.statuses, id)
    }

    pub fn is_running(&self) -> bool {
        !self.runs.is_empty()
    }

    /// 最近一次探测全部结束的时间；从没测过是 None。
    pub fn last_finished(&self) -> Option<Instant> {
        self.last_finished
    }

    pub fn online_count(&self) -> usize {
        self.statuses
            .values()
            .filter(|s| **s == ProbeStatus::Online)
            .count()
    }

    /// 清空所有结果（关掉探活、整批作废时用）。
    pub fn clear(&mut self) {
        self.statuses.clear();
        self.runs.clear();
        self.last_finished = None;
    }

    /// 丢掉某台主机的状态（删除主机时用）。
    pub fn forget(&mut self, id: &str) {
        self.statuses.remove(id);
    }

    /// 让某些主机回到「检测中」（地址变了之后，旧结论已经没意义）。
    pub fn invalidate(&mut self, ids: &[String]) {
        for id in ids {
            self.statuses.remove(id);
        }
    }

    /// 复制主机时沿用原主机的状态：地址端口一样，没必要重新扫。
    pub fn copy_status(&mut self, from: &str, to: &str) {
        if let Some(status) = self.statuses.get(from).copied() {
            self.statuses.insert(to.to_string(), status);
        }
    }

    /// 连接成功后立刻标在线，不用等下一轮探测。
    pub fn mark_online(&mut self, id: &str) {
        self.statuses.insert(id.to_string(), ProbeStatus::Online);
    }

    /// 丢掉已经不存在的主机 id，避免状态表越攒越大。
    pub fn prune(&mut self, keep: &HashSet<String>) {
        self.statuses.retain(|id, _| keep.contains(id));
    }

    /// 发一批探测。同一地址 + 端口只连一次，结果扇出给所有同地址的主机。
    pub fn start(&mut self, targets: Vec<Target>) {
        let mut fanout: HashMap<String, Vec<String>> = HashMap::new();
        let mut unique: Vec<Target> = Vec::new();
        for target in targets {
            let key = dedupe_key(&target.host, target.port);
            match fanout.get_mut(&key) {
                Some(ids) => ids.push(target.id),
                None => {
                    fanout.insert(key.clone(), vec![target.id.clone()]);
                    unique.push(Target {
                        id: key,
                        host: target.host,
                        port: target.port,
                    });
                }
            }
        }
        let (tx, rx) = channel();
        spawn_probe_batch(unique, tx);
        self.runs.push(Run {
            rx,
            deadline: Instant::now() + BATCH_DEADLINE,
            fanout,
        });
    }

    /// 收结果。返回 true 表示有变化（调用方要 request_repaint）。
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        let mut index = 0;
        while index < self.runs.len() {
            let mut finished = false;
            loop {
                match self.runs[index].rx.try_recv() {
                    Ok((key, online)) => {
                        let status = if online {
                            ProbeStatus::Online
                        } else {
                            ProbeStatus::Offline
                        };
                        if let Some(ids) = self.runs[index].fanout.get(&key) {
                            for id in ids {
                                self.statuses.insert(id.clone(), status);
                            }
                        }
                        changed = true;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        finished = true;
                        break;
                    }
                }
            }
            // DNS 卡住时 worker 可能很久不返回：到点就收摊，没出结果的按离线算，
            // 不然会一直「检测中」并且每 120ms 空转重绘。
            if !finished && Instant::now() >= self.runs[index].deadline {
                for ids in self.runs[index].fanout.values() {
                    for id in ids {
                        self.statuses.entry(id.clone()).or_insert(ProbeStatus::Offline);
                    }
                }
                finished = true;
                changed = true;
            }
            if finished {
                self.runs.remove(index);
                self.last_finished = Some(Instant::now());
            } else {
                index += 1;
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn target(id: &str, address: &str) -> Target {
        let (h, p) = crate::models::parse_host_port(address).unwrap();
        Target {
            id: id.into(),
            host: h,
            port: p,
        }
    }

    /// 等一批探测结束（最多 5 秒）。
    fn wait_for(runner: &mut ProbeRunner) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while runner.is_running() && Instant::now() < deadline {
            runner.poll();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!runner.is_running(), "探测应该在 5 秒内结束");
    }

    #[test]
    fn probes_local_closed_port_as_offline() {
        // 一个几乎不可能有服务在听的端口
        let task = target("t", "127.0.0.1:1");
        assert!(!probe_one(&task));
    }

    #[test]
    fn probes_local_listener_as_online() {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = Target {
            id: "t".into(),
            host: "127.0.0.1".into(),
            port,
        };
        assert!(probe_one(&task));
        drop(listener);
    }

    #[test]
    fn batch_reports_every_host_once() {
        let (tx, rx) = mpsc::channel();
        let targets = vec![target("a", "127.0.0.1:1"), target("b", "127.0.0.1:2")];
        spawn_probe_batch(targets, tx);
        let mut seen = Vec::new();
        // 全部 worker 退出后 channel 关闭，recv 返回 Err，循环自然结束
        while let Ok((id, _ok)) = rx.recv_timeout(Duration::from_secs(5)) {
            seen.push(id);
        }
        seen.sort();
        assert_eq!(seen, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn invalid_address_is_skipped_not_panicking() {
        let (tx, rx) = mpsc::channel();
        let hosts = vec![
            Host {
                id: "bad".into(),
                host: "127.0.0.1".into(),
                port: 1,
                ..Default::default()
            },
            Host::default(),
        ];
        let targets = targets_of(&hosts);
        assert_eq!(targets.len(), 1, "空地址的主机不该进探测队列");
        spawn_probe_batch(targets, tx);
        let first = rx.recv_timeout(Duration::from_secs(5));
        assert!(first.is_ok());
        // 空地址那台被跳过，不会再有一条结果
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
    }

    #[test]
    fn runner_fans_one_connection_out_to_all_copies() {
        // 两台主机同地址同端口（复制出来的副本）：只连一次，两边都拿到结果
        let mut runner = ProbeRunner::new();
        let targets = vec![
            target("a", "127.0.0.1:1"),
            target("a2", "127.0.0.1:1"),
            target("b", "127.0.0.1:2"),
        ];
        runner.start(targets);
        wait_for(&mut runner);
        assert_eq!(runner.status("a"), ProbeStatus::Offline);
        assert_eq!(runner.status("a2"), ProbeStatus::Offline);
        assert_eq!(runner.status("b"), ProbeStatus::Offline);
        assert_eq!(runner.status("missing"), ProbeStatus::Checking);
        assert!(runner.last_finished().is_some());
    }

    #[test]
    fn runner_marks_online_listener_and_ignores_stale_ids() {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut runner = ProbeRunner::new();
        runner.start(vec![Target {
            id: "live".into(),
            host: "127.0.0.1".into(),
            port,
        }]);
        wait_for(&mut runner);
        assert_eq!(runner.status("live"), ProbeStatus::Online);
        assert_eq!(runner.online_count(), 1);

        runner.invalidate(&["live".to_string()]);
        assert_eq!(runner.status("live"), ProbeStatus::Checking);
        runner.mark_online("live");
        assert_eq!(runner.status("live"), ProbeStatus::Online);
        runner.forget("live");
        assert_eq!(runner.status("live"), ProbeStatus::Checking);
        drop(listener);
    }

    #[test]
    fn concurrent_batches_do_not_block_each_other() {
        let mut runner = ProbeRunner::new();
        runner.start(vec![target("first", "127.0.0.1:1")]);
        runner.start(vec![target("second", "127.0.0.1:2")]);
        wait_for(&mut runner);
        assert_eq!(runner.status("first"), ProbeStatus::Offline);
        assert_eq!(runner.status("second"), ProbeStatus::Offline);
    }

    #[test]
    fn targets_for_only_picks_requested_ids() {
        let hosts = vec![
            Host {
                id: "a".into(),
                host: "10.0.0.1".into(),
                ..Default::default()
            },
            Host {
                id: "b".into(),
                host: "10.0.0.2".into(),
                ..Default::default()
            },
        ];
        let picked = targets_for(&hosts, &["b".to_string()]);
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].id, "b");
    }
}
