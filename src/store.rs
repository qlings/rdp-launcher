//! 单 exe 内嵌存储。
//!
//! 运行时数据写在 **exe 文件的 NTFS 数据流**里（`rdp-launcher.exe:rdp-launcher.cfg`），
//! 不在旁边生成任何文件。正在运行的 exe 本体被系统锁定、不能直接改字节，但
//! 「另一个数据流」可以正常读写，这就是选它的原因。
//!
//! 退出时再派生一个临时 helper（还是这份 exe 的副本），等主进程结束后把同一份
//! 加密数据**追加到 exe 文件尾部**。这样：
//!
//! * 正常退出后，数据真的在 exe 字节里，压缩/拷到 U 盘也不会丢；
//! * 运行时随时保存，崩溃了也有数据流兜底；
//! * 启动时数据流和尾部都有，取 revision 更大的那份。
//!
//! 格式（数据流内，以及 exe 尾部）：
//!
//! ```text
//! [AES-GCM 密文][u32 密文长度][16 字节 MAGIC]
//! ```

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::crypto;
use crate::models::{self, clean_host, new_id, Group, Host, Settings, DEFAULT_PORT};

pub const VERSION: u32 = 3;
const MAGIC: &[u8; 16] = b"RDP-LAUNCHER-CFG";
const TRAILER_LEN: u64 = 20;
const STREAM_SUFFIX: &str = ":rdp-launcher.cfg";

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Store {
    #[serde(default)]
    pub version: u32,
    /// 每次保存 +1，用来在「数据流」和「exe 尾部」之间挑更新的那份。
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub hosts: Vec<Host>,
    #[serde(default)]
    pub groups: Vec<Group>,
    #[serde(default)]
    pub settings: Settings,
    /// 最近连接过的主机 id，托盘右键菜单用；最新的在最前面。
    #[serde(default)]
    pub recent: Vec<String>,
}

impl Default for Store {
    fn default() -> Self {
        Self {
            version: VERSION,
            revision: 1,
            hosts: Vec::new(),
            groups: Vec::new(),
            settings: Settings::default(),
            recent: Vec::new(),
        }
    }
}

impl Store {
    pub fn seed() -> Self {
        let mut store = Self::default();
        store.groups = vec![Group::new("公司", 0), Group::new("家庭", 1)];
        store.hosts = vec![
            Host {
                id: "seed-demo-1".into(),
                name: "办公室主力机".into(),
                host: "192.168.1.10".into(),
                port: 33890,
                username: "administrator".into(),
                groups: vec!["公司".into()],
                drives: vec!["C".into()],
                clipboard: true,
                audio: true,
                order: 0,
                ..Default::default()
            },
            Host {
                id: "seed-demo-2".into(),
                name: "家里 NAS 跳板".into(),
                host: "192.168.1.20".into(),
                port: DEFAULT_PORT,
                username: "admin".into(),
                groups: vec!["家庭".into()],
                drives: vec![],
                clipboard: true,
                audio: true,
                order: 1,
                ..Default::default()
            },
        ];
        store.normalize();
        store
    }

    /// 补齐并清理所有跨字段约束。加载、保存前都调用一次，保证 UI 拿到的是干净数据。
    pub fn normalize(&mut self) {
        self.version = VERSION;
        if self.revision == 0 {
            self.revision = 1;
        }

        // 分组名去空白、去重
        let mut seen = HashSet::new();
        self.groups.retain(|g| {
            let name = g.name.trim();
            if name.is_empty() || !seen.insert(name.to_string()) {
                false
            } else {
                true
            }
        });
        for (i, group) in self.groups.iter_mut().enumerate() {
            group.name = group.name.trim().to_string();
            group.order = i as i64;
            group.orig = group.name.clone();
        }

        for host in &mut self.hosts {
            host.host = clean_host(&host.host);
            // 兼容旧数据：地址里带端口的，拆到端口字段
            if host.host.matches(':').count() == 1 && host.port == DEFAULT_PORT {
                if let Ok((h, p)) = models::parse_host_port(&host.host) {
                    host.host = h;
                    host.port = p;
                }
            }
            if host.port == 0 {
                host.port = DEFAULT_PORT;
            }
            host.name = host.name.trim().to_string();
            host.username = host.username.trim().to_string();

            let mut groups: Vec<String> = Vec::new();
            for group in &host.groups {
                let name = group.trim();
                if !name.is_empty() && !groups.iter().any(|g| g == name) {
                    groups.push(name.to_string());
                }
            }
            host.groups = groups;

            let mut drives: Vec<String> = Vec::new();
            for drive in &host.drives {
                if let Some(c) = drive.trim().chars().next() {
                    let c = c.to_ascii_uppercase();
                    if c.is_ascii_uppercase() && !drives.iter().any(|d| d == &c.to_string()) {
                        drives.push(c.to_string());
                    }
                }
            }
            host.drives = drives;

            let id = host.id.trim().to_string();
            host.id = if id.is_empty() { new_id() } else { id };
        }

        // 主机引用了不存在的分组：补进分组表
        let mut referenced: Vec<String> = Vec::new();
        for host in &self.hosts {
            for group in &host.groups {
                if !referenced.iter().any(|g| g == group) {
                    referenced.push(group.clone());
                }
            }
        }
        for name in referenced {
            if !self.groups.iter().any(|g| g.name == name) {
                let order = self.groups.len() as i64;
                self.groups.push(Group::new(name, order));
            }
        }

        // id 去重
        let mut ids = HashSet::new();
        for host in &mut self.hosts {
            while !ids.insert(host.id.clone()) {
                host.id = new_id();
            }
        }

        if !self.settings.default_group.is_empty()
            && !self
                .groups
                .iter()
                .any(|g| g.name == self.settings.default_group)
        {
            self.settings.default_group.clear();
        }

        // 探活间隔只认设置里给的几档，手改过的配置文件不至于变成每 3 秒扫一次
        if !models::PROBE_INTERVALS.contains(&self.settings.probe_interval_secs) {
            self.settings.probe_interval_secs = 60;
        }

        // 最近主机：只留还存在的，去重，最多 8 条
        let host_ids: HashSet<String> = self.hosts.iter().map(|h| h.id.clone()).collect();
        let mut seen_recent = HashSet::new();
        self.recent.retain(|id| {
            host_ids.contains(id) && seen_recent.insert(id.clone())
        });
        self.recent.truncate(8);
    }

    /// 连接成功后把主机挪到最近列表最前面。
    pub fn touch_recent(&mut self, id: &str) {
        self.recent.retain(|item| item != id);
        self.recent.insert(0, id.to_string());
        self.recent.truncate(8);
    }

    pub fn ensure_group(&mut self, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        if !self.groups.iter().any(|g| g.name == name) {
            let order = self.groups.len() as i64;
            self.groups.push(Group::new(name, order));
        }
    }

    /// 序号默认值：新主机排在最后。
    pub fn next_order(&self) -> i64 {
        self.hosts.iter().map(|h| h.order).max().unwrap_or(-1) + 1
    }
}

// ---------------------------------------------------------------------------
// 读写
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct LoadOutcome {
    pub store: Store,
    /// 非致命问题：例如旧 JSON 导入成功但删不掉、首次保存失败。
    pub warning: Option<String>,
}

/// 读取内嵌配置。优先取「数据流」和「exe 尾部」里 revision 更大的那份。
pub fn load(path: &Path) -> Result<LoadOutcome, String> {
    if !path.is_file() {
        return Err(format!("程序文件不存在：{}", path.display()));
    }

    let mut candidates: Vec<(&str, Store)> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    match read_stream(path) {
        Ok(Some(blob)) => match decode_store(&blob) {
            Ok(store) => candidates.push(("exe 数据流", store)),
            Err(e) => errors.push(format!("exe 数据流：{e}")),
        },
        Ok(None) => {}
        Err(e) => errors.push(e),
    }

    match read_tail(path) {
        Ok(Some(blob)) => match decode_store(&blob) {
            Ok(store) => candidates.push(("exe 尾部", store)),
            Err(e) => errors.push(format!("exe 尾部：{e}")),
        },
        Ok(None) => {}
        Err(e) => errors.push(e),
    }

    if let Some((_, mut store)) = candidates.into_iter().max_by_key(|(_, s)| s.revision) {
        store.normalize();
        // 一边坏、一边还能解：用能解的那份，但必须告诉用户「你看到的是旧备份」，
        // 不然主机/设置静默回退，用户会以为数据丢了。
        let warning = if errors.is_empty() {
            None
        } else {
            Some(format!(
                "另一份配置已损坏，已自动改用较新的备份：{}",
                errors.join("；")
            ))
        };
        return Ok(LoadOutcome { store, warning });
    }

    if !errors.is_empty() {
        // 两条路都读不出来：明确报错，不静默覆盖
        return Err(errors.join("；"));
    }

    let legacy = path.with_file_name("rdp_hosts.json");
    if legacy.is_file() {
        let mut store = import_legacy(&legacy)?;
        store.normalize();
        let mut warning = None;
        if let Err(e) = save(path, &mut store) {
            warning = Some(format!("旧配置已读取，但写入 exe 失败：{e}"));
        } else if let Err(e) = fs::remove_file(&legacy) {
            warning = Some(format!(
                "旧配置已导入 exe，但删除 {} 失败：{e}",
                legacy.display()
            ));
        }
        return Ok(LoadOutcome { store, warning });
    }

    let mut store = Store::seed();
    store.normalize();
    let warning = save(path, &mut store)
        .err()
        .map(|e| format!("首次写入内置配置失败：{e}"));
    Ok(LoadOutcome { store, warning })
}

/// 两份配置都解不开时，先把原始字节复制到 `%TEMP%\rdp-launcher-corrupt-<时间>`。
///
/// 主程序在这之后会用空配置继续跑，用户随便点一下就会 `save()`（截断重写数据流），
/// 退出时 helper 还会覆盖 exe 尾部 —— 不先留一份，坏数据就再也救不回来了。
pub fn backup_broken(path: &Path) -> Option<PathBuf> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("rdp-launcher-corrupt-{stamp}"));
    fs::create_dir_all(&dir).ok()?;

    let mut copied = false;
    if let Ok(bytes) = fs::read(path) {
        if fs::write(dir.join("rdp-launcher.exe.bak"), bytes).is_ok() {
            copied = true;
        }
    }
    let stream = stream_path(path);
    if let Ok(bytes) = fs::read(&stream) {
        if fs::write(dir.join("rdp-launcher.cfg.bak"), bytes).is_ok() {
            copied = true;
        }
    }
    copied.then_some(dir)
}

/// 把加密配置写进 exe 的数据流。每次保存 revision +1。
pub fn save(path: &Path, store: &mut Store) -> Result<(), String> {
    store.version = VERSION;
    store.revision = store.revision.saturating_add(1);
    let blob = encode_store(store)?;
    write_stream(path, &blob)
}

fn encode_store(store: &Store) -> Result<Vec<u8>, String> {
    let plain = serde_json::to_vec(store).map_err(|e| format!("序列化配置失败：{e}"))?;
    crypto::encrypt_blob(&plain)
}

fn decode_store(blob: &[u8]) -> Result<Store, String> {
    let plain = crypto::decrypt_blob(blob)?;
    let mut store: Store =
        serde_json::from_slice(&plain).map_err(|e| format!("配置格式错误：{e}"))?;
    store.normalize();
    Ok(store)
}

fn stream_path(path: &Path) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(STREAM_SUFFIX);
    PathBuf::from(value)
}

fn write_stream(path: &Path, blob: &[u8]) -> Result<(), String> {
    if blob.len() > u32::MAX as usize {
        return Err("配置太大，超出单 exe 存储上限".into());
    }
    let stream = stream_path(path);
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&stream)
        .map_err(|e| {
            format!(
                "写入 exe 内置数据失败：{e}\n路径：{}\n\
                 需要把 exe 放在 NTFS 磁盘上（FAT32/exFAT 不支持数据流）。",
                stream.display()
            )
        })?;
    file.write_all(blob)
        .map_err(|e| format!("写入配置失败：{e}"))?;
    file.write_all(&(blob.len() as u32).to_le_bytes())
        .map_err(|e| format!("写入配置长度失败：{e}"))?;
    file.write_all(MAGIC)
        .map_err(|e| format!("写入配置标记失败：{e}"))?;
    file.flush()
        .map_err(|e| format!("刷新配置到磁盘失败：{e}"))?;
    Ok(())
}

fn read_stream(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let stream = stream_path(path);
    let mut file = match File::open(&stream) {
        Ok(file) => file,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("读取 exe 内置数据失败：{e}")),
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|e| format!("读取 exe 内置数据失败：{e}"))?;
    if bytes.is_empty() {
        return Ok(None);
    }
    parse_record(&bytes)
}

fn read_tail(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let mut file = File::open(path).map_err(|e| format!("打开程序文件失败：{e}"))?;
    let len = file
        .metadata()
        .map_err(|e| format!("读取程序文件大小失败：{e}"))?
        .len();
    let Some(base) = embedded_base(&mut file, len)? else {
        return Ok(None);
    };
    let blob_len = len - base - TRAILER_LEN;
    if blob_len == 0 {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(base))
        .map_err(|e| format!("定位配置失败：{e}"))?;
    let mut blob = vec![0u8; blob_len as usize];
    file.read_exact(&mut blob)
        .map_err(|e| format!("读取配置失败：{e}"))?;
    Ok(Some(blob))
}

/// 校验 `[blob][len][MAGIC]` 并取出 blob。
fn parse_record(bytes: &[u8]) -> Result<Option<Vec<u8>>, String> {
    if bytes.len() < TRAILER_LEN as usize {
        return Ok(None);
    }
    let tail = &bytes[bytes.len() - TRAILER_LEN as usize..];
    if &tail[4..20] != MAGIC {
        return Ok(None);
    }
    let blob_len = u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]) as u64;
    if blob_len == 0 || blob_len + TRAILER_LEN != bytes.len() as u64 {
        return Err("内置配置长度异常，数据可能已损坏".into());
    }
    Ok(Some(bytes[..blob_len as usize].to_vec()))
}

/// exe 尾部有配置段就返回它的起始偏移。
fn embedded_base(file: &mut File, len: u64) -> Result<Option<u64>, String> {
    if len < TRAILER_LEN + 1 {
        return Ok(None);
    }
    file.seek(SeekFrom::End(-(TRAILER_LEN as i64)))
        .map_err(|e| format!("定位程序文件尾部失败：{e}"))?;
    let mut tail = [0u8; 20];
    file.read_exact(&mut tail)
        .map_err(|e| format!("读取程序文件尾部失败：{e}"))?;
    if &tail[4..20] != MAGIC {
        return Ok(None);
    }
    let blob_len = u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]) as u64;
    if blob_len == 0 || blob_len + TRAILER_LEN > len {
        return Err("内置配置长度异常，程序文件可能被破坏".into());
    }
    let base = len - TRAILER_LEN - blob_len;
    if base < 64 {
        return Err("内置配置位置异常，程序文件可能被破坏".into());
    }
    Ok(Some(base))
}

// ---------------------------------------------------------------------------
// 退出时把数据流写回 exe 字节
// ---------------------------------------------------------------------------

/// 派生一个临时 helper（exe 副本），等主进程退出后把数据流追加到 exe 尾部。
/// 这样正常关掉软件后，exe 本身就带着数据，压缩/拷贝都不会丢。
pub fn spawn_embed_helper(target: &Path) {
    let Some(current) = std::env::current_exe().ok() else {
        return;
    };
    let helper = std::env::temp_dir().join(format!(
        "rdp-launcher-embed-{}-{}.exe",
        std::process::id(),
        models::new_id()
    ));
    if fs::copy(&current, &helper).is_err() {
        return;
    }
    #[cfg(windows)]
    use std::os::windows::process::CommandExt;
    let mut command = std::process::Command::new(&helper);
    command
        .arg("--embed-store")
        .arg(target)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW，避免调试构建闪黑框
    let _ = command.spawn();
}

/// helper 进程入口：等目标 exe 解锁，然后把数据流里的最新配置写进 exe 尾部。
pub fn wait_and_embed(target: &Path) {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        match OpenOptions::new().read(true).write(true).open(target) {
            Ok(mut file) => {
                let _ = embed_into_open_file(&mut file, target);
                return;
            }
            Err(e) if e.raw_os_error() == Some(32) && Instant::now() < deadline => {
                // ERROR_SHARING_VIOLATION：主进程还没退干净
                std::thread::sleep(Duration::from_millis(150));
            }
            Err(_) => return,
        }
    }
}

/// 直接对（未运行的）exe 做一次「尾部写入」，测试用。
#[cfg(test)]
pub fn embed_store_in_exe(target: &Path) -> Result<(), String> {
    let Some(blob) = read_stream(target)? else {
        return Ok(());
    };
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(target)
        .map_err(|e| format!("打开程序文件失败：{e}"))?;
    embed_into_open_file_with_blob(&mut file, &blob)
}

fn embed_into_open_file(file: &mut File, target: &Path) -> Result<(), String> {
    let Some(blob) = read_stream(target)? else {
        return Ok(());
    };
    embed_into_open_file_with_blob(file, &blob)
}

fn embed_into_open_file_with_blob(file: &mut File, blob: &[u8]) -> Result<(), String> {
    if blob.len() > u32::MAX as usize {
        return Err("配置太大".into());
    }
    let len = file
        .metadata()
        .map_err(|e| format!("读取程序文件大小失败：{e}"))?
        .len();
    let base = embedded_base(file, len)?.unwrap_or(len);
    file.seek(SeekFrom::Start(base))
        .map_err(|e| format!("定位配置写入位置失败：{e}"))?;
    file.write_all(blob)
        .map_err(|e| format!("写入配置失败：{e}"))?;
    file.write_all(&(blob.len() as u32).to_le_bytes())
        .map_err(|e| format!("写入配置长度失败：{e}"))?;
    file.write_all(MAGIC)
        .map_err(|e| format!("写入配置标记失败：{e}"))?;
    file.set_len(base + TRAILER_LEN + blob.len() as u64)
        .map_err(|e| format!("截断程序文件失败：{e}"))?;
    file.flush()
        .map_err(|e| format!("刷新配置到磁盘失败：{e}"))?;
    Ok(())
}

/// 清理上一天遗留的 helper 副本（它们自己删不掉自己）。
pub fn cleanup_old_helpers() {
    let Ok(entries) = fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("rdp-launcher-embed-") || !name.ends_with(".exe") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let old = meta
            .modified()
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > Duration::from_secs(300));
        if old {
            let _ = fs::remove_file(entry.path());
        }
    }
}

// ---------------------------------------------------------------------------
// 旧版 JSON 迁移
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct LegacyHost {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    group: String,
    #[serde(default)]
    address: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
    #[serde(default = "default_true")]
    clipboard: bool,
    #[serde(default)]
    sync_c: bool,
    #[serde(default)]
    sync_d: bool,
}

fn default_true() -> bool {
    true
}

fn import_legacy(path: &Path) -> Result<Store, String> {
    let raw = fs::read_to_string(path)
        .map_err(|e| format!("读取旧配置失败：{e}\n{}", path.display()))?;
    if raw.trim().is_empty() {
        return Err("旧配置 rdp_hosts.json 是空文件".into());
    }
    let list: Vec<LegacyHost> = serde_json::from_str(&raw)
        .map_err(|e| format!("旧配置 rdp_hosts.json 格式错误：{e}"))?;

    let mut store = Store::default();
    for (index, item) in list.into_iter().enumerate() {
        let (host, port) = models::parse_host_port(&item.address).map_err(|e| {
            format!(
                "旧配置第 {} 台主机地址「{}」无效：{e}",
                index + 1,
                item.address
            )
        })?;
        let mut groups = Vec::new();
        if !item.group.trim().is_empty() {
            groups.push(item.group.trim().to_string());
        }
        let mut drives = Vec::new();
        if item.sync_c {
            drives.push("C".to_string());
        }
        if item.sync_d {
            drives.push("D".to_string());
        }
        store.hosts.push(Host {
            id: if item.id.trim().is_empty() {
                new_id()
            } else {
                item.id.trim().to_string()
            },
            name: item.name,
            host,
            port,
            username: item.username,
            password: crypto::decrypt_legacy_password(&item.password),
            groups,
            drives,
            clipboard: item.clipboard,
            audio: true,
            order: index as i64,
        });
    }

    for name in models::collect_groups(&store.hosts) {
        store.ensure_group(&name);
    }
    Ok(store)
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rdp_store_{tag}_{}", new_id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fake_exe(dir: &Path) -> PathBuf {
        let path = dir.join("rdp-launcher.exe");
        // 不用真的 PE：存储层只认文件尾部，MZ + 填充足够
        let mut bytes = b"MZ".to_vec();
        bytes.resize(4096, 0);
        fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn roundtrip_through_stream() {
        let dir = temp_dir("stream");
        let path = fake_exe(&dir);
        let mut store = Store::seed();
        store.settings.dark = true;
        store.settings.close_to_tray = true;
        store.settings.default_group = "公司".into();
        store.hosts[0].password = "秘密口令".into();
        store.hosts[0].drives = vec!["C".into(), "D".into()];
        store.touch_recent("seed-demo-1");
        store.normalize();
        let first_rev = store.revision;
        save(&path, &mut store).unwrap();
        assert!(store.revision > first_rev);

        let loaded = load(&path).unwrap().store;
        assert_eq!(loaded.hosts.len(), 2);
        assert_eq!(loaded.hosts[0].password, "秘密口令");
        assert_eq!(loaded.hosts[0].port, 33890);
        assert_eq!(loaded.hosts[0].groups, vec!["公司".to_string()]);
        assert_eq!(loaded.settings.default_group, "公司");
        assert!(loaded.settings.dark);
        assert!(loaded.settings.close_to_tray);
        assert_eq!(loaded.recent, vec!["seed-demo-1".to_string()]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn embeds_stream_into_tail_for_portable_copy() {
        let dir = temp_dir("embed");
        let path = fake_exe(&dir);
        let mut store = Store::seed();
        save(&path, &mut store).unwrap();
        let without_tail = fs::metadata(&path).unwrap().len();

        // 模拟正常退出：把数据流写进 exe 字节
        embed_store_in_exe(&path).unwrap();
        let with_tail = fs::metadata(&path).unwrap().len();
        assert!(with_tail > without_tail, "尾部应该追加了配置");

        // 删掉数据流，模拟「压缩后只剩字节」的场景，仍应读得到
        fs::remove_file(stream_path(&path)).unwrap();
        assert!(read_stream(&path).unwrap().is_none());
        let loaded = load(&path).unwrap().store;
        assert_eq!(loaded.hosts[0].id, "seed-demo-1");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn newer_revision_wins_between_stream_and_tail() {
        let dir = temp_dir("revision");
        let path = fake_exe(&dir);
        let mut store = Store::seed();
        save(&path, &mut store).unwrap();
        embed_store_in_exe(&path).unwrap();

        // 模拟运行时又保存了两次：数据流更新，尾部还是旧的
        let mut store2 = store.clone();
        save(&path, &mut store2).unwrap();
        store2.hosts[0].name = "数据流里的新名字".into();
        save(&path, &mut store2).unwrap();

        let loaded = load(&path).unwrap().store;
        assert_eq!(loaded.hosts[0].name, "数据流里的新名字");
        assert!(loaded.revision > store2.revision - 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn encrypted_blob_does_not_leak_password() {
        let dir = temp_dir("cipher");
        let path = fake_exe(&dir);
        let mut store = Store::seed();
        store.hosts[0].password = "P@ssw0rd-明文".into();
        save(&path, &mut store).unwrap();
        let stream = stream_path(&path);
        let bytes = fs::read(&stream).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("P@ssw0rd-明文"));
        assert!(!text.contains("办公室主力机"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_stream_falls_back_to_tail_with_warning() {
        let dir = temp_dir("corrupt_stream");
        let path = fake_exe(&dir);
        let mut store = Store::seed();
        save(&path, &mut store).unwrap();
        embed_store_in_exe(&path).unwrap();

        // 数据流写坏：长度字段和实际长度对不上
        let mut bad = Vec::new();
        bad.extend_from_slice(&5u32.to_le_bytes());
        bad.extend_from_slice(MAGIC);
        fs::write(stream_path(&path), bad).unwrap();

        let outcome = load(&path).unwrap();
        assert!(outcome.warning.is_some(), "用旧备份时必须提示");
        assert_eq!(outcome.store.hosts.len(), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn broken_store_is_backed_up_before_it_gets_overwritten() {
        let dir = temp_dir("backup");
        let path = fake_exe(&dir);
        let mut bad = Vec::new();
        bad.extend_from_slice(&5u32.to_le_bytes());
        bad.extend_from_slice(MAGIC);
        fs::write(stream_path(&path), bad).unwrap();

        assert!(load(&path).is_err(), "两份都坏时必须报错，不能静默覆盖");
        let backup = backup_broken(&path).expect("应该留下原始字节备份");
        assert!(backup.join("rdp-launcher.exe.bak").is_file());
        assert!(backup.join("rdp-launcher.cfg.bak").is_file());
        let _ = fs::remove_dir_all(&backup);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn first_run_seeds_and_writes() {
        let dir = temp_dir("seed");
        let path = fake_exe(&dir);
        let outcome = load(&path).unwrap();
        assert_eq!(outcome.store.hosts.len(), 2);
        assert!(outcome.warning.is_none());
        // 第二次读取应该读到同一份，而不是重新播种
        let again = load(&path).unwrap();
        assert_eq!(again.store.hosts[0].id, "seed-demo-1");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn imports_and_removes_legacy_json() {
        let dir = temp_dir("legacy");
        let path = fake_exe(&dir);
        let legacy = dir.join("rdp_hosts.json");
        fs::write(
            &legacy,
            r#"[
              {"id":"old-1","name":"老主机","group":"旧组","address":"10.1.2.3:3390",
               "username":"u","password":"","clipboard":true,"sync_c":true,"sync_d":false}
            ]"#,
        )
        .unwrap();

        let outcome = load(&path).unwrap();
        assert_eq!(outcome.store.hosts.len(), 1);
        assert_eq!(outcome.store.hosts[0].host, "10.1.2.3");
        assert_eq!(outcome.store.hosts[0].port, 3390);
        assert_eq!(outcome.store.hosts[0].groups, vec!["旧组".to_string()]);
        assert_eq!(outcome.store.hosts[0].drives, vec!["C".to_string()]);
        assert!(!legacy.exists(), "迁移成功后应删掉旧 JSON");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn normalize_repairs_hosts_and_default_group() {        let mut store = Store::default();
        store.settings.default_group = "不存在".into();
        store.hosts.push(Host {
            id: String::new(),
            host: " 192.168.1.1:3390 ".into(),
            port: DEFAULT_PORT,
            groups: vec![" 甲 ".into(), "甲".into(), "".into()],
            drives: vec!["c".into(), "C".into(), "1".into()],
            ..Default::default()
        });
        store.normalize();
        assert!(store.settings.default_group.is_empty());
        assert_eq!(store.hosts[0].host, "192.168.1.1");
        assert_eq!(store.hosts[0].port, 3390);
        assert_eq!(store.hosts[0].groups, vec!["甲".to_string()]);
        assert_eq!(store.hosts[0].drives, vec!["C".to_string()]);
        assert!(store.groups.iter().any(|g| g.name == "甲"));
        assert!(!store.hosts[0].id.is_empty());
    }

    #[test]
    fn normalize_keeps_only_allowed_probe_intervals() {
        let mut store = Store::default();
        store.settings.probe_interval_secs = 7;
        store.normalize();
        assert_eq!(store.settings.probe_interval_secs, 60, "乱写的间隔回到默认 1 分钟");

        store.settings.probe_interval_secs = 0;
        store.normalize();
        assert_eq!(store.settings.probe_interval_secs, 0, "「不自动」要保留");

        store.settings.probe_interval_secs = 300;
        store.normalize();
        assert_eq!(store.settings.probe_interval_secs, 300);
    }

    #[test]
    fn recent_hosts_are_deduped_limited_and_pruned() {
        let mut store = Store::seed();
        store.touch_recent("seed-demo-1");
        store.touch_recent("seed-demo-2");
        store.touch_recent("seed-demo-1");
        assert_eq!(store.recent, vec!["seed-demo-1", "seed-demo-2"]);

        store.touch_recent("已经删掉的主机");
        store.normalize();
        assert!(!store.recent.iter().any(|id| id == "已经删掉的主机"));
        assert_eq!(store.recent[0], "seed-demo-1");
    }
}
