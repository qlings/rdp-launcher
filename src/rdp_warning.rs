//! 关掉 Windows 2026 更新引入的「未知的远程连接」安全警告。
//!
//! 新版 mstsc 打开**未签名**、带剪贴板/磁盘等重定向的 `.rdp` 时会弹一个
//! 「警告：未知的远程连接 / 发布者：未知发布者」对话框，并且默认把重定向全部取消。
//! 官方开关是机器策略：
//!
//! ```text
//! HKLM\Software\Policies\Microsoft\Windows NT\Terminal Services\Client
//!     RedirectionWarningDialogVersion = 1
//! ```
//!
//! 写 HKLM 需要管理员。程序优先尝试直接写（如果本身是管理员就静默生效）；
//! 普通用户则通过 `ShellExecuteW(..., "runas", ...)` 拉起一次带 UAC 的
//! `reg.exe`，用户点「是」后永久生效。
//!
//! 同时写 HKCU 的 `RdpLaunchConsentAccepted=1`，压掉第一次打开 .rdp 时的
//! 「我了解并允许」提示（这个不需要管理员）。

use windows::core::PCWSTR;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegGetValueW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    HKEY_LOCAL_MACHINE, KEY_SET_VALUE, REG_DWORD, REG_OPTION_NON_VOLATILE, RRF_RT_REG_DWORD,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

const POLICY_KEY: &str = r"Software\Policies\Microsoft\Windows NT\Terminal Services\Client";
const POLICY_VALUE: &str = "RedirectionWarningDialogVersion";
const CONSENT_KEY: &str = r"Software\Microsoft\Terminal Server Client";
const CONSENT_VALUE: &str = "RdpLaunchConsentAccepted";
const TSC_KEY: &str = r"Software\Microsoft\Terminal Server Client";
const AUTH_OVERRIDE: &str = "AuthenticationLevelOverride";
const LOCAL_DEVICES_KEY: &str = r"Software\Microsoft\Terminal Server Client\LocalDevices";
/// 勾「不再询问我是否连接到此计算机」时 mstsc 写入的 bitflags，0x6F 是常用的全允许值。
const LOCAL_DEVICES_ALLOW_ALL: u32 = 0x6F;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn read_dword(hkey: HKEY, subkey: &str, value: &str) -> Option<u32> {
    let subkey = wide(subkey);
    let value = wide(value);
    let mut data = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: 字符串以 NUL 结尾、缓冲区大小匹配 u32。
    let status = unsafe {
        RegGetValueW(
            hkey,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(value.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut data as *mut u32 as *mut core::ffi::c_void),
            Some(&mut size),
        )
    };
    (status == ERROR_SUCCESS).then_some(data)
}

fn write_dword(hkey: HKEY, subkey: &str, value: &str, data: u32) -> Result<(), u32> {
    let subkey = wide(subkey);
    let value = wide(value);
    let mut key = HKEY::default();
    // SAFETY: 路径/值名以 NUL 结尾，key 在关闭前存活。
    let status = unsafe {
        RegCreateKeyExW(
            hkey,
            PCWSTR(subkey.as_ptr()),
            0,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(status.0);
    }

    let bytes = data.to_le_bytes();
    // SAFETY: bytes 是 4 字节 REG_DWORD，调用期间存活。
    let status = unsafe {
        RegSetValueExW(
            key,
            PCWSTR(value.as_ptr()),
            0,
            REG_DWORD,
            Some(&bytes),
        )
    };
    // SAFETY: key 由 RegCreateKeyExW 创建。
    unsafe {
        let _ = RegCloseKey(key);
    }
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(status.0)
    }
}

/// 机器策略是否已经关掉警告（只有 HKLM 才是权威值）。
pub fn is_suppressed() -> bool {
    read_dword(HKEY_LOCAL_MACHINE, POLICY_KEY, POLICY_VALUE) == Some(1)
}

/// 不需要管理员的用户级设置：压掉首次打开 .rdp 的同意提示。
pub fn ensure_user_consent() {
    // 注意：这里只写 HKCU 的「首次启动同意」，不碰 HKCU 的策略值。
    // RedirectionWarningDialogVersion 是机器策略，只写 HKLM，避免和 IT 下发的策略打架。
    let _ = write_dword(HKEY_CURRENT_USER, CONSENT_KEY, CONSENT_VALUE, 1);
}

/// 压掉旧版 mstsc 的「无法识别此远程连接的发布者。是否仍要连接？」。
///
/// 两个 HKCU 值（都不需要管理员）：
/// * `AuthenticationLevelOverride=0`：全局不再做 RDP 服务器证书校验；
/// * `LocalDevices\<主机>=0x6F`：等价于每个主机都勾了「不再询问我是否连接到此计算机」。
///
/// 这会绕过服务器证书提示，只在信任的内网使用。程序本身已经对每个 `.rdp` 写
/// `authentication level:i:0`，这里补的是客户端每次弹窗之前那层检查。
pub fn ensure_no_publisher_warning(hosts: &[(String, u16)]) {
    let _ = write_dword(HKEY_CURRENT_USER, TSC_KEY, AUTH_OVERRIDE, 0);
    for (host, port) in hosts {
        allow_host(host, *port);
    }
}

/// 给单个主机写「不再询问」标志。连接前调用，覆盖新加/改地址的主机。
pub fn allow_host(host: &str, port: u16) {
    let host = host.trim();
    if host.is_empty() || host.contains('\\') {
        return;
    }
    // 对话框里的「远程计算机」通常只显示主机名，但不同版本也可能带端口；两个 key 都写。
    let _ = write_dword(
        HKEY_CURRENT_USER,
        LOCAL_DEVICES_KEY,
        host,
        LOCAL_DEVICES_ALLOW_ALL,
    );
    if port != 3389 {
        let with_port = format!("{host}:{port}");
        let _ = write_dword(
            HKEY_CURRENT_USER,
            LOCAL_DEVICES_KEY,
            &with_port,
            LOCAL_DEVICES_ALLOW_ALL,
        );
    }
}

/// 尝试直接写机器策略。进程是管理员时成功，否则返回错误码（常见 5 = 拒绝访问）。
pub fn try_set_machine() -> Result<(), u32> {
    write_dword(HKEY_LOCAL_MACHINE, POLICY_KEY, POLICY_VALUE, 1)
}

/// 拉起一次带 UAC 的 `reg.exe` 写机器策略。返回是否成功启动（不是是否授权）。
pub fn launch_elevated_fix() -> bool {
    ensure_user_consent();
    let params = format!(
        r#"add "HKLM\{POLICY_KEY}" /v {POLICY_VALUE} /t REG_DWORD /d 1 /f"#
    );
    let operation = wide("runas");
    let file = wide("reg.exe");
    let parameters = wide(&params);

    // SAFETY: 三个字符串都以 NUL 结尾并在调用期间存活；hwnd 为空。
    let result = unsafe {
        ShellExecuteW(
            windows::Win32::Foundation::HWND(std::ptr::null_mut()),
            PCWSTR(operation.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR(parameters.as_ptr()),
            PCWSTR::null(),
            SW_HIDE,
        )
    };
    result.0 as isize > 32
}

/// 设置界面显示用。
#[allow(dead_code)]
pub fn status_text() -> &'static str {
    if is_suppressed() {
        "已关闭（连接时不再弹安全警告）"
    } else {
        "未关闭（新版 Windows 会弹「未知的远程连接」）"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_constants_are_not_empty() {
        assert!(POLICY_KEY.contains("Terminal Services"));
        assert_eq!(POLICY_VALUE, "RedirectionWarningDialogVersion");
    }

    #[test]
    fn reads_current_machine_policy_without_panicking() {
        // 只读，不修改系统；单纯确认 FFI 调用不崩。
        let _ = is_suppressed();
        let _ = status_text();
    }
}
