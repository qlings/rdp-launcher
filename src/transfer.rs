//! 配置导出 / 导入。
//!
//! 导出：把整包配置（主机**含明文密码**、分组、设置项、最近连接）序列化后 AES-GCM 加密，
//! 前面加 16 字节 MAGIC + u32 长度，写成独立文件 `xxx.rdplcfg`。
//!
//! 密钥和 exe 内嵌存储是同一把（见 `crypto`），所以**另一份本软件拿到文件就能直接解开**——
//! 这就是「换台电脑导进去」要的效果。代价：加密只保证文件不是明文、被手改过能发现，
//! 不保证「别人拿到文件也解不开」。导出提示里把这句话说清楚。
//!
//! 这里只放纯逻辑（打包 / 拆包 / 合并），弹窗和原生文件选择框在 `dialogs` / `filedlg`。

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::crypto;
use crate::models::{self, new_id, Group, Host, Settings};
use crate::store::Store;

/// 导出文件的扩展名：`rdp-launcher-配置-20261126-1820.rdplcfg`
pub const FILE_EXT: &str = "rdplcfg";
/// 文件头：先认这 16 字节，别把随便什么文件当配置读。
pub const MAGIC: &[u8; 16] = b"RDP-LAUNCHER-XFR";
/// 文件格式版本：以后字段结构变了，靠它拦住「比本软件新」的文件。
pub const FORMAT: u32 = 1;
const HEADER_LEN: usize = MAGIC.len() + 4;
/// 配置再大也就几 MB，超过这个数肯定是拿错文件了。
const MAX_BLOB: usize = 16 * 1024 * 1024;

// ---------------------------------------------------------------------------
// 文档
// ---------------------------------------------------------------------------

/// 一份导出文件里的全部内容。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TransferDoc {
    #[serde(default)]
    pub format: u32,
    /// 导出它的软件版本，导入弹窗里显示用（不参与兼容判断）
    #[serde(default)]
    pub app_version: String,
    /// 导出时间（本地时间字符串）
    #[serde(default)]
    pub exported_at: String,
    #[serde(default)]
    pub hosts: Vec<Host>,
    #[serde(default)]
    pub groups: Vec<Group>,
    #[serde(default)]
    pub settings: Settings,
    /// 最近连接过的主机 id（拖进来一起走，导完托盘顺序不变）
    #[serde(default)]
    pub recent: Vec<String>,
}

impl TransferDoc {
    /// 从当前 store 打包一份。`app_version` / `exported_at` 由调用方给，方便单测。
    pub fn from_store(store: &Store, app_version: &str, exported_at: &str) -> Self {
        Self {
            format: FORMAT,
            app_version: app_version.to_string(),
            exported_at: exported_at.to_string(),
            hosts: store.hosts.clone(),
            groups: store.groups.clone(),
            settings: store.settings.clone(),
            recent: store.recent.clone(),
        }
    }

    /// `MAGIC | u32 密文长度 | 密文`。
    pub fn to_bytes(&self) -> Result<Vec<u8>, String> {
        let json = serde_json::to_vec(self).map_err(|e| format!("打包配置失败：{e}"))?;
        let blob = crypto::encrypt_blob(&json)?;
        if blob.len() > MAX_BLOB {
            return Err("配置太大，无法导出".into());
        }
        let mut out = Vec::with_capacity(HEADER_LEN + blob.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&(blob.len() as u32).to_le_bytes());
        out.extend_from_slice(&blob);
        Ok(out)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() < HEADER_LEN {
            return Err("不是本软件的配置文件（文件太小）".into());
        }
        if &bytes[..MAGIC.len()] != MAGIC {
            return Err("不是本软件的配置文件".into());
        }
        let len = u32::from_le_bytes([
            bytes[MAGIC.len()],
            bytes[MAGIC.len() + 1],
            bytes[MAGIC.len() + 2],
            bytes[MAGIC.len() + 3],
        ]) as usize;
        if len == 0 || len > MAX_BLOB || HEADER_LEN + len > bytes.len() {
            return Err("配置文件不完整，文件可能损坏或被截断".into());
        }
        let plain = crypto::decrypt_blob(&bytes[HEADER_LEN..HEADER_LEN + len])?;
        let doc: Self = serde_json::from_slice(&plain)
            .map_err(|_| "配置文件内容无法识别（可能来自别的软件）".to_string())?;
        if doc.format > FORMAT {
            return Err(format!(
                "配置来自更新的软件版本（文件格式 {}），请先升级本软件",
                doc.format
            ));
        }
        Ok(doc)
    }

    pub fn write(&self, path: &Path) -> Result<(), String> {
        let bytes = self.to_bytes()?;
        std::fs::write(path, bytes).map_err(|e| format!("写文件失败：{e}"))
    }

    pub fn read(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("读文件失败：{e}"))?;
        Self::from_bytes(&bytes)
    }

    /// 「3 台主机 · 2 个分组 · 最近连接」这种一行摘要。
    pub fn summary(&self) -> String {
        let mut text = format!("{} 台主机 · {} 个分组", self.hosts.len(), self.groups.len());
        if !self.recent.is_empty() {
            text.push_str(&format!(" · 最近连接 {} 条", self.recent.len()));
        }
        text
    }
}

// ---------------------------------------------------------------------------
// 合并
// ---------------------------------------------------------------------------

/// 用户在导入弹窗里勾了什么。
#[derive(Clone, Debug, Default)]
pub struct TransferPick {
    /// `doc.hosts` 里要导入的下标
    pub hosts: Vec<usize>,
    pub groups: bool,
    pub settings: bool,
    pub recent: bool,
}

impl TransferPick {
    pub fn is_empty(&self) -> bool {
        self.hosts.is_empty() && !self.groups && !self.settings && !self.recent
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MergeReport {
    pub hosts_added: usize,
    /// 同地址同端口已经在列表里，跳过
    pub hosts_skipped: usize,
    pub groups_added: usize,
    pub settings_applied: bool,
    pub recent_added: usize,
    /// 新加进来的主机 id，用来只探测这几台
    pub added_ids: Vec<String>,
}

impl MergeReport {
    pub fn summary(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.hosts_added > 0 {
            parts.push(format!("{} 台主机", self.hosts_added));
        }
        if self.groups_added > 0 {
            parts.push(format!("{} 个分组", self.groups_added));
        }
        if self.settings_applied {
            parts.push("设置项".to_string());
        }
        if self.recent_added > 0 {
            parts.push(format!("{} 条最近连接", self.recent_added));
        }
        if parts.is_empty() {
            "没有导入任何内容".to_string()
        } else {
            format!("已导入 {}", parts.join("、"))
        }
    }
}

/// 把勾中的内容合并进现有配置。**不覆盖**已有主机：同地址同端口跳过。
///
/// 合并完会 `normalize()` 一次（分组表、id 去重、最近连接清理都靠它）。
pub fn merge(store: &mut Store, doc: &TransferDoc, pick: &TransferPick) -> MergeReport {
    let mut report = MergeReport::default();

    if pick.groups {
        for group in &doc.groups {
            let name = group.name.trim();
            if name.is_empty() || store.groups.iter().any(|g| g.name == name) {
                continue;
            }
            let order = store.groups.len() as i64;
            store.groups.push(Group::new(name, order));
            report.groups_added += 1;
        }
    }

    let mut id_map: HashMap<String, String> = HashMap::new();
    for &index in &pick.hosts {
        let Some(source) = doc.hosts.get(index) else {
            continue;
        };
        if source.host.trim().is_empty()
            || models::address_exists(&store.hosts, &source.host, source.port)
        {
            report.hosts_skipped += 1;
            continue;
        }
        let mut host = source.clone();
        // id 撞车（从同一份配置导过两次、或本来就是同一台机器的另一份）就换新的，
        // 不改 id 的话探活结果和托盘菜单会认错主机。
        if host.id.trim().is_empty() || store.hosts.iter().any(|h| h.id == host.id) {
            let old = host.id.clone();
            host.id = new_id();
            id_map.insert(old, host.id.clone());
        }
        report.added_ids.push(host.id.clone());
        store.hosts.push(host);
        report.hosts_added += 1;
    }

    if pick.settings {
        store.settings = doc.settings.clone();
        report.settings_applied = true;
    }

    if pick.recent {
        for id in &doc.recent {
            let mapped = id_map.get(id).cloned().unwrap_or_else(|| id.clone());
            if store.hosts.iter().any(|h| h.id == mapped) && !store.recent.contains(&mapped) {
                store.recent.push(mapped);
                report.recent_added += 1;
            }
        }
    }

    store.normalize();
    report
}

// ---------------------------------------------------------------------------
// 时间 / 文件名
// ---------------------------------------------------------------------------

/// 本地时间：`(2026-11-26 18:20, 20261126-1820)`。文件名和弹窗里都用它。
pub fn local_stamp() -> (String, String) {
    use windows::Win32::System::SystemInformation::GetLocalTime;
    // SAFETY: GetLocalTime 没有任何前置条件，返回值是拷出来的 SYSTEMTIME。
    let st = unsafe { GetLocalTime() };
    (
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute
        ),
        format!(
            "{:04}{:02}{:02}-{:02}{:02}",
            st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute
        ),
    )
}

/// 导出时的默认文件名：`rdp-launcher-配置-20261126-1820.rdplcfg`。
pub fn default_file_name() -> String {
    let (_, compact) = local_stamp();
    format!("rdp-launcher-配置-{compact}.{FILE_EXT}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_store() -> Store {
        let mut store = Store::default();
        store.groups = vec![Group::new("公司", 0), Group::new("家里", 1)];
        store.hosts = vec![
            Host {
                id: "a1".into(),
                name: "办公室".into(),
                host: "10.0.0.5".into(),
                port: 3389,
                username: "admin".into(),
                password: "s3cret".into(),
                groups: vec!["公司".into()],
                drives: vec!["C".into()],
                clipboard: true,
                audio: false,
                order: 3,
            },
            Host {
                id: "b2".into(),
                name: "家里 NAS".into(),
                host: "192.168.1.9".into(),
                port: 13389,
                username: "u".into(),
                password: String::new(),
                groups: vec!["家里".into()],
                drives: vec![],
                clipboard: false,
                audio: true,
                order: 4,
            },
        ];
        store.settings.dark = true;
        store.settings.probe_interval_secs = 300;
        store.recent = vec!["b2".into(), "a1".into()];
        store.normalize();
        store
    }

    #[test]
    fn roundtrip_keeps_passwords_and_settings() {
        let store = sample_store();
        let doc = TransferDoc::from_store(&store, "9.9.9", "2026-11-26 18:20");
        let bytes = doc.to_bytes().unwrap();
        assert_eq!(&bytes[..MAGIC.len()], MAGIC);

        let back = TransferDoc::from_bytes(&bytes).unwrap();
        assert_eq!(back.hosts.len(), 2);
        assert_eq!(back.hosts[0].password, "s3cret");
        assert_eq!(back.groups.len(), 2);
        assert!(back.settings.dark);
        assert_eq!(back.settings.probe_interval_secs, 300);
        assert_eq!(back.recent, vec!["b2".to_string(), "a1".to_string()]);
        assert_eq!(back.app_version, "9.9.9");
        assert_eq!(back.exported_at, "2026-11-26 18:20");
    }

    #[test]
    fn exported_bytes_are_encrypted_not_plaintext_json() {
        let doc = TransferDoc::from_store(&sample_store(), "0.7.0", "x");
        let bytes = doc.to_bytes().unwrap();
        let haystack = String::from_utf8_lossy(&bytes);
        assert!(!haystack.contains("s3cret"));
        assert!(!haystack.contains("10.0.0.5"));
    }

    #[test]
    fn rejects_foreign_and_truncated_files() {
        assert!(TransferDoc::from_bytes(b"not a config at all").is_err());
        let bytes = TransferDoc::from_store(&sample_store(), "0.7.0", "x")
            .to_bytes()
            .unwrap();
        assert!(TransferDoc::from_bytes(&bytes[..HEADER_LEN + 8]).is_err());
    }

    #[test]
    fn merge_adds_hosts_groups_settings_and_skips_duplicates() {
        let source = sample_store();
        let doc = TransferDoc::from_store(&source, "0.7.0", "x");

        let mut target = Store::default();
        target.hosts.push(Host {
            id: "other".into(),
            host: "10.0.0.5".into(),
            port: 3389,
            ..Default::default()
        });
        target.normalize();

        let pick = TransferPick {
            hosts: vec![0, 1],
            groups: true,
            settings: true,
            recent: true,
        };
        let report = merge(&mut target, &doc, &pick);

        // 第一台同地址同端口被跳过，第二台进来了
        assert_eq!(report.hosts_added, 1);
        assert_eq!(report.hosts_skipped, 1);
        assert_eq!(report.added_ids.len(), 1);
        assert_eq!(target.hosts.len(), 2);
        assert_eq!(target.hosts[1].name, "家里 NAS");
        assert_eq!(report.groups_added, 2);
        assert!(report.settings_applied);
        assert!(target.settings.dark);
        assert_eq!(report.recent_added, 1);
        assert_eq!(target.recent, vec![target.hosts[1].id.clone()]);
    }

    #[test]
    fn merge_reassigns_colliding_host_ids() {
        let source = sample_store();
        let doc = TransferDoc::from_store(&source, "0.7.0", "x");

        // 目标里已经有个别的地址、但 id 相同的主机
        let mut target = Store::default();
        target.hosts.push(Host {
            id: "a1".into(),
            host: "172.16.0.1".into(),
            port: 3389,
            ..Default::default()
        });
        target.normalize();

        let pick = TransferPick {
            hosts: vec![0],
            recent: true,
            ..Default::default()
        };
        let report = merge(&mut target, &doc, &pick);
        assert_eq!(report.hosts_added, 1);
        assert_eq!(target.hosts.len(), 2);
        assert_ne!(target.hosts[1].id, "a1");
        assert_eq!(target.hosts[1].password, "s3cret");
        // id 换过之后，最近连接里的旧 id 也能对上新的
        assert_eq!(report.recent_added, 1);
    }

    #[test]
    fn merge_with_empty_pick_changes_nothing() {
        let source = sample_store();
        let doc = TransferDoc::from_store(&source, "0.7.0", "x");
        let mut target = Store::default();
        let before = target.hosts.len();
        let report = merge(&mut target, &doc, &TransferPick::default());
        assert_eq!(report.hosts_added, 0);
        assert_eq!(report.hosts_skipped, 0);
        assert_eq!(report.groups_added, 0);
        assert!(!report.settings_applied);
        assert_eq!(report.recent_added, 0);
        assert_eq!(target.hosts.len(), before);
        assert_eq!(report.summary(), "没有导入任何内容");
    }

    #[test]
    fn local_stamp_is_well_formed() {
        let (display, compact) = local_stamp();
        assert_eq!(display.len(), 16);
        assert_eq!(compact.len(), 13);
        assert!(display.contains(':'));
        assert!(compact.contains('-'));
        assert!(default_file_name().ends_with(".rdplcfg"));
    }

    /// 手动验证用：生成一份样例导出文件，给「导入配置」弹窗截图 / 真机试导入。
    /// `cargo test writes_sample_export_file -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn writes_sample_export_file() {
        let doc = TransferDoc::from_store(&sample_store(), "0.7.0", "2026-11-26 18:20");
        let path = std::env::temp_dir().join("rdp-launcher-sample.rdplcfg");
        doc.write(&path).unwrap();
        println!("sample export: {}", path.display());
    }
}
