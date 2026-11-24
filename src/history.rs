//! 读本机 Windows 远程桌面（mstsc）的历史记录，供「导入主机」用。
//!
//! 只有两个来源，都在 HKCU，**只读不写**：
//!
//! * `Software\Microsoft\Terminal Server Client\Default\MRU0..MRU9`
//!   —— mstsc「计算机」下拉框里的最近连接，值是 `主机` 或 `主机:端口`。
//! * `Software\Microsoft\Terminal Server Client\Servers\<主机>`
//!   —— 每台连过的服务器，子键下的 `UsernameHint` 是上次用的账号。
//!
//! 两个来源会重叠（同一台机器既有 MRU 又有 Servers 子键），这里按
//! 「主机 + 端口」合并，用户名从 Servers 补进来。

use windows::core::PCWSTR;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
};

use crate::models;

const DEFAULT_KEY: &str = r"Software\Microsoft\Terminal Server Client\Default";
const SERVERS_KEY: &str = r"Software\Microsoft\Terminal Server Client\Servers";
/// mstsc 只记最近 10 条，多读几个键名也无害。
const MRU_SLOTS: usize = 12;

/// 一条可导入的历史：已经拆好主机 / 端口，用户名可能为空。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryEntry {
    pub host: String,
    pub port: u16,
    pub username: String,
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn open_key(path: &str) -> Option<HKEY> {
    let path = wide(path);
    let mut key = HKEY::default();
    // SAFETY: 路径以 NUL 结尾且在调用期间存活；key 由系统写入。
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(path.as_ptr()),
            0,
            KEY_READ,
            &mut key,
        )
    };
    (status == ERROR_SUCCESS).then_some(key)
}

/// 读一个 REG_SZ 值（值名传 `None` 就是默认值）。读不到返回 None。
fn read_sz(key: HKEY, name: &str) -> Option<String> {
    let name = wide(name);
    let mut size = 0u32;
    // SAFETY: 先只问长度，数据指针为 None。
    let status = unsafe {
        RegQueryValueExW(
            key,
            PCWSTR(name.as_ptr()),
            None,
            None,
            None,
            Some(&mut size),
        )
    };
    if status != ERROR_SUCCESS || size == 0 {
        return None;
    }

    let mut buffer = vec![0u8; size as usize];
    // SAFETY: buffer 至少 size 字节，size 在以传入长度为准回写。
    let status = unsafe {
        RegQueryValueExW(
            key,
            PCWSTR(name.as_ptr()),
            None,
            None,
            Some(buffer.as_mut_ptr()),
            Some(&mut size),
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    buffer.truncate((size as usize).min(buffer.len()));

    let mut units: Vec<u16> = buffer
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    while units.last() == Some(&0) {
        units.pop();
    }
    let text = String::from_utf16_lossy(&units).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// MRU0..MRUn 里的原始地址。
fn mru_values() -> Vec<String> {
    let Some(key) = open_key(DEFAULT_KEY) else {
        return Vec::new();
    };
    let mut values = Vec::new();
    for slot in 0..MRU_SLOTS {
        if let Some(value) = read_sz(key, &format!("MRU{slot}")) {
            values.push(value);
        }
    }
    // SAFETY: key 来自 open_key，只关闭一次。
    unsafe {
        let _ = RegCloseKey(key);
    }
    values
}

/// `Servers` 下的子键：`(原始地址, UsernameHint)`。
fn server_values() -> Vec<(String, String)> {
    let Some(key) = open_key(SERVERS_KEY) else {
        return Vec::new();
    };
    let mut servers = Vec::new();
    let mut index = 0u32;
    loop {
        let mut name = [0u16; 256];
        let mut name_len = name.len() as u32;
        // SAFETY: name 是长度 name_len 的可写缓冲区。
        let status = unsafe {
            RegEnumKeyExW(
                key,
                index,
                windows::core::PWSTR(name.as_mut_ptr()),
                &mut name_len,
                None,
                windows::core::PWSTR::null(),
                None,
                None,
            )
        };
        if status != ERROR_SUCCESS {
            break;
        }
        index += 1;

        let host = String::from_utf16_lossy(&name[..name_len as usize]);
        if host.trim().is_empty() {
            continue;
        }

        let host_w = wide(&host);
        let mut sub = HKEY::default();
        // SAFETY: host_w 以 NUL 结尾，sub 由系统写入。
        let opened = unsafe {
            RegOpenKeyExW(key, PCWSTR(host_w.as_ptr()), 0, KEY_READ, &mut sub)
        };
        let username = if opened == ERROR_SUCCESS {
            let username = read_sz(sub, "UsernameHint").unwrap_or_default();
            // SAFETY: sub 来自 RegOpenKeyExW。
            unsafe {
                let _ = RegCloseKey(sub);
            }
            username
        } else {
            String::new()
        };
        servers.push((host, username));
    }
    // SAFETY: key 来自 open_key，只关闭一次。
    unsafe {
        let _ = RegCloseKey(key);
    }
    servers
}

/// 把 `(原始地址, 用户名)` 合并成去重后的历史列表（纯函数，方便单测）。
///
/// 顺序保留第一次出现的位置；同一个「主机 + 端口」只留一条，
/// 后出现的非空用户名会把空用户名补上。
fn merge(raw_entries: Vec<(String, String)>) -> Vec<HistoryEntry> {
    let mut merged: Vec<HistoryEntry> = Vec::new();
    for (raw, username) in raw_entries {
        let Ok((host, port)) = models::parse_host_port(&raw) else {
            continue;
        };
        let host = models::clean_host(&host);
        if models::validate_host(&host).is_err() {
            continue;
        }
        let username = username.trim().to_string();
        if let Some(existing) = merged
            .iter_mut()
            .find(|entry| entry.host.eq_ignore_ascii_case(&host) && entry.port == port)
        {
            if existing.username.is_empty() {
                existing.username = username;
            }
            continue;
        }
        merged.push(HistoryEntry {
            host,
            port,
            username,
        });
    }
    merged
}

/// 本机的远程桌面历史。读不到注册表就是空列表，不报错。
pub fn local_history() -> Vec<HistoryEntry> {
    let servers = server_values();
    let mut raw: Vec<(String, String)> = Vec::new();

    // MRU 是「最近连过的顺序」，优先；用户名去 Servers 里按主机+端口找。
    for value in mru_values() {
        let username = models::parse_host_port(&value)
            .ok()
            .and_then(|(host, port)| {
                servers
                    .iter()
                    .find(|(server, _)| {
                        models::parse_host_port(server)
                            .map(|(server_host, server_port)| {
                                server_host.eq_ignore_ascii_case(&host) && server_port == port
                            })
                            .unwrap_or(false)
                    })
                    .map(|(_, username)| username.clone())
            })
            .unwrap_or_default();
        raw.push((value, username));
    }
    raw.extend(servers);

    merge(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_duplicates_and_fills_username() {
        let entries = vec![
            ("192.168.1.10:3390".to_string(), String::new()),
            ("192.168.1.10:3390".to_string(), "admin".to_string()),
            ("office-pc".to_string(), "ql".to_string()),
            // 空地址 / 非法地址都丢掉
            (String::new(), String::new()),
            ("bad host/name".to_string(), String::new()),
        ];
        let merged = merge(entries);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].host, "192.168.1.10");
        assert_eq!(merged[0].port, 3390);
        assert_eq!(merged[0].username, "admin");
        assert_eq!(merged[1].host, "office-pc");
        assert_eq!(merged[1].port, models::DEFAULT_PORT);
        assert_eq!(merged[1].username, "ql");
    }

    #[test]
    fn host_port_variants_are_deduplicated_by_address() {
        let entries = vec![
            ("10.0.0.5:3389".to_string(), String::new()),
            ("10.0.0.5".to_string(), String::new()),
            ("[fe80::1]:3390".to_string(), String::new()),
        ];
        let merged = merge(entries);
        assert_eq!(merged.len(), 2, "10.0.0.5 和 10.0.0.5:3389 是同一台");
        assert_eq!(merged[1].host, "fe80::1");
        assert_eq!(merged[1].port, 3390);
    }

    #[test]
    fn reading_local_history_does_not_panic() {
        // 只读注册表，不改系统；空历史也只是返回空列表。
        let history = local_history();
        for entry in history {
            assert!(!entry.host.trim().is_empty());
        }
    }
}
