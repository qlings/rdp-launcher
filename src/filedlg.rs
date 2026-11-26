//! 原生文件选择框（comdlg32 的 `GetOpenFileNameW` / `GetSaveFileNameW`）。
//!
//! 不引第三方 `rfd`：comdlg32 是系统自带，项目里已经依赖 `windows` crate，
//! 加一个 feature 就能用，二进制也不会变大。
//!
//! 这两个都是**模态阻塞**调用：点「导出」时主线程会停在这一行，直到用户选完文件。
//! egui 那一帧会晚一点收尾，画面看起来是「停在导出那一刻」，这是预期行为。

use std::path::PathBuf;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, GetSaveFileNameW, OPENFILENAMEW, OFN_EXPLORER, OFN_FILEMUSTEXIST,
    OFN_HIDEREADONLY, OFN_NOCHANGEDIR, OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST,
};

/// 路径缓冲区按 UTF-16 算：Windows 路径上限 32767，这里给常见的 1024 个字符，
/// 够长又不会在栈上摆一大块。
const BUF_LEN: usize = 1024;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 过滤器：`标签 (*.ext)\0*.ext\0所有文件 (*.*)\0*.*\0\0`
fn filter(ext: &str) -> Vec<u16> {
    let mut out = Vec::new();
    for part in [
        format!("配置备份 (*.{ext})"),
        format!("*.{ext}"),
        "所有文件 (*.*)".to_string(),
        "*.*".to_string(),
    ] {
        out.extend(part.encode_utf16());
        out.push(0);
    }
    // 双 NUL 结尾：整串过滤器到最后一个空字符串为止
    out.push(0);
    out
}

fn buffer_with_name(name: &str) -> Vec<u16> {
    let mut buf = vec![0u16; BUF_LEN];
    let encoded: Vec<u16> = name.encode_utf16().take(BUF_LEN - 1).collect();
    buf[..encoded.len()].copy_from_slice(&encoded);
    buf
}

fn buffer_to_path(buf: &[u16]) -> PathBuf {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    PathBuf::from(String::from_utf16_lossy(&buf[..end]))
}

/// 保存对话框。取消 / 出错都返回 None（不弹第二条错误框，交给调用方决定说什么）。
pub fn save_file(
    owner: Option<HWND>,
    title: &str,
    ext: &str,
    default_name: &str,
) -> Option<PathBuf> {
    let mut buf = buffer_with_name(default_name);
    let filter = filter(ext);
    let title = wide(title);
    let def_ext = wide(ext);
    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner.unwrap_or_default(),
        lpstrFilter: PCWSTR(filter.as_ptr()),
        nFilterIndex: 1,
        lpstrFile: PWSTR(buf.as_mut_ptr()),
        nMaxFile: BUF_LEN as u32,
        lpstrTitle: PCWSTR(title.as_ptr()),
        Flags: OFN_EXPLORER | OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        lpstrDefExt: PCWSTR(def_ext.as_ptr()),
        lCustData: LPARAM(0),
        ..Default::default()
    };
    // SAFETY: 所有指针都指向本函数里活着的缓冲区，lStructSize 已按真实结构体填好。
    if !unsafe { GetSaveFileNameW(&mut ofn) }.as_bool() {
        return None;
    }
    Some(buffer_to_path(&buf))
}

/// 打开对话框：只让选已存在的文件。
pub fn open_file(owner: Option<HWND>, title: &str, ext: &str) -> Option<PathBuf> {
    let mut buf = vec![0u16; BUF_LEN];
    let filter = filter(ext);
    let title = wide(title);
    let def_ext = wide(ext);
    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner.unwrap_or_default(),
        lpstrFilter: PCWSTR(filter.as_ptr()),
        nFilterIndex: 1,
        lpstrFile: PWSTR(buf.as_mut_ptr()),
        nMaxFile: BUF_LEN as u32,
        lpstrTitle: PCWSTR(title.as_ptr()),
        Flags: OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_HIDEREADONLY
            | OFN_NOCHANGEDIR,
        lpstrDefExt: PCWSTR(def_ext.as_ptr()),
        ..Default::default()
    };
    // SAFETY: 同上，指针都指向本函数里活着的缓冲区。
    if !unsafe { GetOpenFileNameW(&mut ofn) }.as_bool() {
        return None;
    }
    Some(buffer_to_path(&buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_is_double_null_terminated() {
        let f = filter("rdplcfg");
        assert_eq!(*f.last().unwrap(), 0);
        assert_eq!(f[f.len() - 2], 0);
        let text = String::from_utf16_lossy(&f);
        assert!(text.contains("*.rdplcfg"));
        assert!(text.contains("*.*"));
    }

    #[test]
    fn buffer_roundtrip_stops_at_nul() {
        let buf = buffer_with_name("配置.rdplcfg");
        assert_eq!(buffer_to_path(&buf), PathBuf::from("配置.rdplcfg"));
    }
}
