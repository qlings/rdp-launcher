//! Windows 底层：CredWriteW / CredDeleteW、UTF-16 LE BOM `.rdp` 生成、
//! 盘符存在性检查、mstsc.exe 拉起。
//!
//! 这里所有失败都通过 `Result<_, String>` 冒泡成可以直接显示给用户的中文短句，
//! 因为启动器最怕的是一按「连接」就静默没反应。

use std::path::{Path, PathBuf};

use windows::core::PWSTR;
use windows::Win32::Security::Credentials::{
    CredDeleteW, CredWriteW, CREDENTIALW, CRED_FLAGS, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
};

use crate::models::Host;

fn to_wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

/// CredWriteW 期望的 UTF-16 LE 字节序列（不带 NUL）。
fn to_utf16le_bytes(value: &str) -> Vec<u8> {
    value
        .encode_utf16()
        .flat_map(|unit| unit.to_le_bytes())
        .collect()
}

// ---------------------------------------------------------------------------
// 凭据管理器
// ---------------------------------------------------------------------------

/// Windows 的 RDP 凭据 TargetName 固定是 `TERMSRV/<主机>`，**端口不参与**。
///
/// 这带来一个绕不开的系统限制：同一个 IP 上的两个不同端口（两台不同的机器）
/// 没法在凭据管理器里同时保存两套密码。本程序的对策是每次连接前都重写一次，
/// 保证「这次连接」用的一定是正确密码；并发会话已经通过 NLA 认证过，不受影响，
/// 只有断开后原地重连时才可能拿到最近一次写入的密码。
pub fn cred_target(host: &str) -> String {
    format!("TERMSRV/{}", host.trim())
}

/// 写 `TERMSRV/<主机>` 凭据。
///
/// **空密码也照样写**：有些机器只有账号、没有密码，mstsc 必须先拿到一条
/// 「用户 + 空密码」的凭据才会直接过 NLA，否则会弹密码框（等于连不上）。
/// 以前这里遇到空密码直接 return，然后连接时把旧凭据删掉，空密码主机就永远被卡在密码框。
pub fn write_credential(host: &str, username: &str, password: &str) -> Result<(), String> {
    let target = cred_target(host);
    let mut target_w = to_wide(&target);
    let mut user_w = to_wide(username);
    let mut blob = to_utf16le_bytes(password);

    let mut cred = CREDENTIALW::default();
    cred.Flags = CRED_FLAGS(0);
    cred.Type = CRED_TYPE_GENERIC;
    cred.TargetName = PWSTR(target_w.as_mut_ptr());
    cred.Comment = PWSTR(std::ptr::null_mut());
    cred.CredentialBlobSize = blob.len() as u32;
    cred.CredentialBlob = blob.as_mut_ptr();
    cred.Persist = CRED_PERSIST_LOCAL_MACHINE;
    cred.AttributeCount = 0;
    cred.Attributes = std::ptr::null_mut();
    cred.TargetAlias = PWSTR(std::ptr::null_mut());
    cred.UserName = PWSTR(user_w.as_mut_ptr());

    // SAFETY: 所有指针都指向上面这些在本次调用期间存活的局部缓冲。
    let result = unsafe { CredWriteW(&cred, 0) };
    result.map_err(|e| format!("写入 Windows 凭据失败：{e}（目标 {target}）"))
}

pub fn delete_credential(host: &str) -> Result<(), String> {
    if host.trim().is_empty() {
        return Ok(());
    }
    let target = cred_target(host);
    let mut target_w = to_wide(&target);
    // SAFETY: target_w 在调用期间存活，且以 NUL 结尾。
    let result = unsafe {
        CredDeleteW(
            windows::core::PCWSTR(target_w.as_mut_ptr()),
            CRED_TYPE_GENERIC,
            0,
        )
    };
    match result {
        Ok(()) => Ok(()),
        Err(e) => {
            // ERROR_NOT_FOUND (1168) 代表本来就没有，不该报错
            if e.code().0 == 0x8007_0490u32 as i32 {
                Ok(())
            } else {
                Err(format!("删除 Windows 凭据失败：{e}（目标 {target}）"))
            }
        }
    }
}

/// 删除主机时清理凭据。如果还有别的主机共用同一个 `TERMSRV/<主机>`（同 IP 不同端口），
/// 就不能删，否则会把另一台主机的密码一起删掉。
pub fn cleanup_credential_for(host: &Host, all_hosts: &[Host]) -> bool {
    if host.host.trim().is_empty() {
        return false;
    }
    let target = cred_target(&host.host);
    // Windows 凭据管理器的 TargetName 大小写不敏感，这里也必须按不敏感比较，
    // 否则 OFFICE-PC 和 office-pc 会被当成两台不同的主机，误删共用的密码。
    let shared = all_hosts
        .iter()
        .any(|other| other.id != host.id && cred_target(&other.host).eq_ignore_ascii_case(&target));
    if shared {
        return false;
    }
    delete_credential(&host.host).is_ok()
}

// ---------------------------------------------------------------------------
// .rdp 文件
// ---------------------------------------------------------------------------

fn system32_dir() -> PathBuf {
    if let Some(root) = std::env::var_os("SystemRoot") {
        PathBuf::from(root).join("System32")
    } else {
        PathBuf::from(r"C:\Windows\System32")
    }
}

pub fn mstsc_path() -> PathBuf {
    system32_dir().join("mstsc.exe")
}

/// 临时 `.rdp` 固定在 `%TEMP%` 下，名字只由主机 id 决定，每次覆盖写。
/// 刻意不删除：mstsc.exe 是异步读取的，spawn 完立刻删会让它读到空文件。
pub fn rdp_file_path(host: &Host) -> PathBuf {
    let sanitized: String = host
        .id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let safe = if sanitized.is_empty() {
        "default"
    } else {
        sanitized.as_str()
    };
    std::env::temp_dir().join(format!("rdp_launcher_{safe}.rdp"))
}

/// 只有本机真实存在的盘符才拼进 `drivestoredirect`，格式按 mstsc 要求以 `\;` 结尾。
///
/// 会在当前线程同步访问磁盘，**只给单元测试和排障用**；正式连接走
/// `build_drive_redirect_cached`（后台线程预先探好）。
#[allow(dead_code)]
pub fn build_drive_redirect(drives: &[String]) -> (String, Vec<char>) {
    let mut parts = Vec::new();
    let mut missing = Vec::new();
    let mut seen = Vec::new();

    for raw in drives {
        let Some(letter) = raw.trim().chars().next() else {
            continue;
        };
        let letter = letter.to_ascii_uppercase();
        if !letter.is_ascii_uppercase() || seen.contains(&letter) {
            continue;
        }
        seen.push(letter);

        let probe_root = format!("{letter}:\\");
        let probe = Path::new(&probe_root);
        if probe.exists() {
            parts.push(format!("{letter}:\\;"));
        } else {
            missing.push(letter);
        }
    }
    (parts.concat(), missing)
}

/// 用后台线程预先探好的盘符存在性拼 `drivestoredirect`，不再在 UI 线程上碰 SMB。
///
/// `present` 里没有记录的盘符按「存在」乐观处理（多半是刚启动、后台还没探完），
/// 这样既不会漏映射真盘，也不会因为一张掉线的网络盘把界面卡住。
pub fn build_drive_redirect_cached(
    drives: &[String],
    present: &std::collections::HashMap<char, bool>,
) -> (String, Vec<char>) {
    let mut parts = Vec::new();
    let mut missing = Vec::new();
    let mut seen = Vec::new();

    for raw in drives {
        let Some(letter) = raw.trim().chars().next() else {
            continue;
        };
        let letter = letter.to_ascii_uppercase();
        if !letter.is_ascii_uppercase() || seen.contains(&letter) {
            continue;
        }
        seen.push(letter);

        if present.get(&letter).copied().unwrap_or(true) {
            parts.push(format!("{letter}:\\;"));
        } else {
            missing.push(letter);
        }
    }
    (parts.concat(), missing)
}

/// 生成 `.rdp` 正文。所有来源是用户的字段都已经剔过控制字符。
///
/// 会同步探测盘符，正式连接请用 `connect` 里的 cached 版本；这个保留给测试/排障。
#[allow(dead_code)]
pub fn build_rdp_content(host: &Host) -> String {
    let (drives, _missing) = build_drive_redirect(&host.drives);
    build_rdp_content_with(host, &drives)
}

fn build_rdp_content_with(host: &Host, drives: &str) -> String {
    let clipboard = if host.clipboard { 1 } else { 0 };
    // audiomode: 0 = 声音带到本机，2 = 不播放（1 = 留在远端）
    let audio = if host.audio { 0 } else { 2 };
    let username = host.username.replace(['\r', '\n'], "");
    let address = host.authority();

    let mut lines = vec![
        format!("full address:s:{address}"),
        format!("username:s:{username}"),
        "authentication level:i:0".to_string(),
        "enablecredsspsupport:i:1".to_string(),
        format!("redirectclipboard:i:{clipboard}"),
        format!("audiomode:i:{audio}"),
        format!("drivestoredirect:s:{drives}"),
        "smart sizing:i:1".to_string(),
    ];
    // 行尾用 CRLF，mstsc 在两种换行下都能读，但 CRLF 更接近它自己写出来的格式。
    lines.push(String::new());
    lines.join("\r\n")
}

/// 以 UTF-16 LE + BOM 写文件。中文用户名 / 中文临时目录路径下 mstsc 才不会乱码。
pub fn write_rdp_file(path: &Path, content: &str) -> Result<(), String> {
    let mut bytes = Vec::with_capacity(2 + content.len() * 2);
    bytes.extend_from_slice(&[0xFF, 0xFE]);
    for unit in content.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    std::fs::write(path, bytes).map_err(|e| format!("写入临时 RDP 文件失败：{e}"))
}

// ---------------------------------------------------------------------------
// 连接
// ---------------------------------------------------------------------------

pub struct LaunchOutcome {
    /// 生成并交给 mstsc 读取的临时文件。刻意不删，见 `rdp_file_path` 的说明。
    #[allow(dead_code)]
    pub rdp_path: PathBuf,
    /// 勾了磁盘同步但本地没有的盘符，用来给非阻塞提示。
    pub missing_drives: Vec<char>,
}

/// 完整的一次连接：写凭据 → 生成 .rdp → 拉起 mstsc。
///
/// `present` 是后台线程预先探好的「盘符是否存在」，这里不再同步访问磁盘。
pub fn connect(
    host: &Host,
    present: &std::collections::HashMap<char, bool>,
) -> Result<LaunchOutcome, String> {
    if host.host.trim().is_empty() {
        return Err("主机地址为空，先在编辑里补上".into());
    }

    let mstsc = mstsc_path();
    if !mstsc.is_file() {
        return Err(format!("找不到远程桌面客户端：{}", mstsc.display()));
    }

    // 空密码也要写凭据：只有账号没密码的主机，靠这条「用户 + 空密码」的凭据直接过 NLA。
    // 以前空密码会删凭据，mstsc 就会停在密码框，等于连不上。
    write_credential(&host.host, host.username.trim(), &host.password)?;

    let (drives, missing) = build_drive_redirect_cached(&host.drives, present);
    let content = build_rdp_content_with(host, &drives);
    let rdp_path = rdp_file_path(host);
    write_rdp_file(&rdp_path, &content)?;

    // 三个 stdio 全部断开：不加的话子进程会继承父进程的 stdout/stderr 句柄，
    // 从命令行或测试里启动时，父进程的输出管道会被 mstsc 一直占着。
    std::process::Command::new(&mstsc)
        .arg(&rdp_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("启动 mstsc.exe 失败：{e}"))?;

    Ok(LaunchOutcome {
        rdp_path,
        missing_drives: missing,
    })
}

/// 读回凭据，用来验证「写进去的到底是什么」。测试和排障用。
#[cfg(test)]
fn read_credential(host: &str) -> Option<(String, String)> {
    use windows::Win32::Foundation::ERROR_NOT_FOUND;
    use windows::Win32::Security::Credentials::{CredFree, CredReadW};
    use windows::core::PCWSTR;

    let target = cred_target(host);
    let mut target_w = to_wide(&target);
    let mut out = std::ptr::null_mut();
    // SAFETY: target_w 以 NUL 结尾且在调用期间存活；out 由 CredReadW 填充，用 CredFree 释放。
    let ok = unsafe { CredReadW(PCWSTR(target_w.as_mut_ptr()), CRED_TYPE_GENERIC, 0, &mut out) };
    if ok.is_err() || out.is_null() {
        let _ = ERROR_NOT_FOUND; // 只是让 import 有用处
        return None;
    }
    let cred = unsafe { &*(out as *const CREDENTIALW) };
    let user = if cred.UserName.is_null() {
        String::new()
    } else {
        unsafe { cred.UserName.to_string().unwrap_or_default() }
    };
    let blob = if cred.CredentialBlob.is_null() || cred.CredentialBlobSize == 0 {
        String::new()
    } else {
        let bytes = unsafe {
            std::slice::from_raw_parts(cred.CredentialBlob, cred.CredentialBlobSize as usize)
        };
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    };
    unsafe { CredFree(out as *const _) };
    Some((user, blob))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::parse_host_port;

    fn host(name: &str, address: &str) -> Host {
        let (h, p) = parse_host_port(address).unwrap();
        Host {
            id: format!("id-{name}"),
            name: name.into(),
            host: h,
            port: p,
            username: "admin".into(),
            password: "secret".into(),
            drives: vec!["C".into()],
            ..Default::default()
        }
    }

    #[test]
    fn rdp_keeps_full_address_and_cred_strips_port() {
        let h = host("测试", "192.168.1.10:33890");
        let content = build_rdp_content(&h);
        assert!(content.contains("full address:s:192.168.1.10:33890"));
        assert!(content.contains("username:s:admin"));
        assert!(content.contains("authentication level:i:0"));
        assert!(content.contains("enablecredsspsupport:i:1"));
        assert!(content.contains("smart sizing:i:1"));
        assert_eq!(cred_target(&h.host), "TERMSRV/192.168.1.10");
    }

    #[test]
    fn same_ip_two_ports_generate_two_targets_but_share_credential_key() {
        let a = host("A", "10.0.0.5:3389");
        let b = host("B", "10.0.0.5:3390");
        assert!(build_rdp_content(&a).contains("full address:s:10.0.0.5:3389"));
        assert!(build_rdp_content(&b).contains("full address:s:10.0.0.5:3390"));
        // Windows 的 TERMSRV target 不带端口，这是系统限制，不是本程序的 bug
        assert_eq!(cred_target(&a.host), cred_target(&b.host));
    }

    #[test]
    fn domain_with_port_is_supported() {
        let h = host("域名", "rdp.example.com:3390");
        let content = build_rdp_content(&h);
        assert!(content.contains("full address:s:rdp.example.com:3390"));
        assert_eq!(h.port, 3390);
    }

    #[test]
    fn audio_switch_is_written() {
        let mut h = host("声音", "10.0.0.1");
        h.audio = true;
        assert!(build_rdp_content(&h).contains("audiomode:i:0"));
        h.audio = false;
        assert!(build_rdp_content(&h).contains("audiomode:i:2"));
    }

    #[test]
    fn drive_redirect_skips_missing_drive() {
        let drives = vec!["C".to_string(), "Z".to_string(), "c".to_string()];
        let (value, missing) = build_drive_redirect(&drives);
        assert!(value.contains("C:\\;"));
        assert_eq!(value.matches("C:\\;").count(), 1, "重复盘符应去重");
        assert!(!missing.is_empty() || Path::new("Z:\\").exists());
        assert!(value.is_empty() || value.ends_with("\\;"));
    }

    #[test]
    fn rdp_content_bom_is_utf16le() {
        let h = host("中文", "rdp.example.com");
        let content = build_rdp_content(&h);
        let path = std::env::temp_dir().join("rdp_launcher_test_bom.rdp");
        write_rdp_file(&path, &content).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[0..2], &[0xFF, 0xFE]);
        assert_eq!(bytes[2], b'f');
        assert_eq!(bytes[3], 0);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn mstsc_exists_on_this_machine() {
        assert!(mstsc_path().is_file(), "找不到 mstsc.exe");
    }

    #[test]
    fn rdp_file_name_is_sanitized() {
        let mut h = host("x", "1.2.3.4");
        h.id = "../../evil path".into();
        let p = rdp_file_path(&h);
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        assert!(!name.contains(".."));
        assert!(!name.contains('/'));
        assert!(name.starts_with("rdp_launcher_"));
        assert!(name.ends_with(".rdp"));
    }

    #[test]
    fn writes_and_reads_back_empty_password_credential() {
        // 只有账号、没有密码的主机：凭据必须真的写进去（blob 长度 0）。
        // 用不会和真实主机撞车的假域名，测完删掉。
        let host = "rdp-launcher-selftest.invalid";
        let _ = delete_credential(host);
        write_credential(host, "probe-user", "").expect("空密码凭据应该写得进去");
        let (user, password) = read_credential(host).expect("应该能读回空密码凭据");
        assert_eq!(user, "probe-user");
        assert_eq!(password, "");
        let _ = delete_credential(host);
    }

    #[test]
    fn cleanup_keeps_shared_credential() {
        let a = host("A", "10.0.0.5:3389");
        let b = host("B", "10.0.0.5:3390");
        // 共用 TERMSRV/10.0.0.5，删 A 时不应删凭据
        assert!(!cleanup_credential_for(&a, &[a.clone(), b.clone()]));
        // 没有别的主机共用，才允许删
        assert!(cleanup_credential_for(&a, std::slice::from_ref(&a)));
    }

    #[test]
    fn cleanup_treats_host_names_case_insensitively() {
        let a = host("A", "OFFICE-PC:3389");
        let b = host("B", "office-pc:3390");
        // Windows 凭据 TargetName 不区分大小写，删 A 不能带走 B 的密码
        assert!(!cleanup_credential_for(&a, &[a.clone(), b.clone()]));
    }

    /// 真实跑一遍连接链路：写凭据 → 生成 .rdp → 拉起 mstsc。
    /// 会真的往凭据管理器写一条、真的弹一个 mstsc 窗口，所以默认 ignore。
    /// 跑法：`cargo test -- --ignored real_connect_flow --nocapture`
    #[test]
    #[ignore]
    fn real_connect_flow() {
        let mut h = host("集成测试", "127.0.0.1:3389");
        h.username = "probe-user".into();
        h.password = "probe-pw-中文".into();
        h.drives = vec!["C".into()];

        let outcome = connect(&h, &std::collections::HashMap::new()).expect("connect 应该成功");
        eprintln!("rdp file: {}", outcome.rdp_path.display());
        eprintln!("missing drives: {:?}", outcome.missing_drives);

        assert!(outcome.rdp_path.is_file(), ".rdp 文件没生成");
        let bytes = std::fs::read(&outcome.rdp_path).unwrap();
        assert_eq!(&bytes[0..2], &[0xFF, 0xFE], "必须是 UTF-16 LE BOM");

        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let text = String::from_utf16_lossy(&units);
        eprintln!("--- rdp content ---\n{text}-------------------");
        assert!(text.contains("full address:s:127.0.0.1:3389"));
        assert!(text.contains("username:s:probe-user"));
        assert!(text.contains("audiomode:i:0"));

        std::thread::sleep(std::time::Duration::from_millis(800));

        let cred = read_credential(&h.host).expect("凭据没写进去");
        eprintln!("credential: user={} blob={}", cred.0, cred.1);
        assert_eq!(cred.0, "probe-user");
        assert_eq!(cred.1, "probe-pw-中文");

        cleanup_credential_for(&h, std::slice::from_ref(&h));
        let _ = std::fs::remove_file(&outcome.rdp_path);
    }
}
