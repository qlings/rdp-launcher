//! 后台 TCP 端口探活。
//!
//! 冷启动不等探活：UI 先画出来，检测结果回来再 `request_repaint()` 更新圆点。
//! 有限线程池，最大并发 32，超时 800ms。设置里关掉探活时主程序根本不会调这里。

use std::collections::HashMap;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
    let queue = Arc::new(Mutex::new(tasks));
    let worker_count = MAX_CONCURRENCY.min(total).max(1);

    for _ in 0..worker_count {
        let queue = Arc::clone(&queue);
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
}
