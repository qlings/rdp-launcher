//! 枚举本机所有可重定向的驱动器（不只 C/D）。
//!
//! 用 `GetLogicalDrives` 拿位图，再用 `GetDriveTypeW` 过滤掉光驱和没有介质的设备。
//! 卷标读不到不算失败：U 盘刚拔、网络盘掉线都还能显示盘符。

use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::{
    GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
};
use windows::Win32::System::WindowsProgramming::{
    DRIVE_FIXED, DRIVE_REMOTE, DRIVE_REMOVABLE,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriveKind {
    Fixed,
    Removable,
    Network,
    Ram,
    Optical,
}

impl DriveKind {
    pub fn label(self) -> &'static str {
        match self {
            DriveKind::Fixed => "本地磁盘",
            DriveKind::Removable => "可移动",
            DriveKind::Network => "网络盘",
            DriveKind::Ram => "内存盘",
            DriveKind::Optical => "光驱",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriveInfo {
    /// 大写盘符，例如 'C'
    pub letter: char,
    pub label: String,
    pub kind: DriveKind,
}

impl DriveInfo {
    pub fn caption(&self) -> String {
        if self.label.trim().is_empty() {
            format!("{}:", self.letter)
        } else {
            format!("{}:  {}", self.letter, self.label)
        }
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn volume_label(root_wide: &[u16]) -> String {
    let mut name = [0u16; 261];
    let mut fs = [0u16; 261];
    // SAFETY: 两个缓冲区都在调用期间存活，长度符合 API 要求（MAX_PATH + 1）。
    let ok = unsafe {
        GetVolumeInformationW(
            PCWSTR(root_wide.as_ptr()),
            Some(&mut name),
            None,
            None,
            None,
            Some(&mut fs),
        )
    };
    if ok.is_err() {
        return String::new();
    }
    let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    String::from_utf16_lossy(&name[..end]).trim().to_string()
}

/// 返回按盘符排序的驱动器列表。
pub fn local_drives() -> Vec<DriveInfo> {
    let mask = unsafe { GetLogicalDrives() };
    if mask == 0 {
        return fallback_probe();
    }

    let mut out = Vec::new();
    for i in 0..26u8 {
        if mask & (1u32 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i) as char;
        let root = format!("{letter}:\\");
        let root_wide = wide(&root);
        // SAFETY: root_wide 以 NUL 结尾且在调用期间存活。
        let drive_type = unsafe { GetDriveTypeW(PCWSTR(root_wide.as_ptr())) };

        let kind = if drive_type == DRIVE_FIXED {
            DriveKind::Fixed
        } else if drive_type == DRIVE_REMOVABLE {
            DriveKind::Removable
        } else if drive_type == DRIVE_REMOTE {
            DriveKind::Network
        } else if drive_type == 6 {
            // DRIVE_RAMDISK
            DriveKind::Ram
        } else if drive_type == 5 {
            // DRIVE_CDROM：只有放了介质才列出来
            DriveKind::Optical
        } else {
            // 无根目录、未知设备
            continue;
        };

        let label = volume_label(&root_wide);
        if kind == DriveKind::Optical && label.is_empty() {
            continue;
        }

        out.push(DriveInfo {
            letter,
            label,
            kind,
        });
    }
    out
}

/// 检查这批盘符现在还在不在。网络盘掉线时这里可能卡到 SMB 超时，
/// 所以只允许在后台线程调用（主程序用 `existing_letters` + 缓存）。
pub fn existing_letters(letters: &[char]) -> Vec<char> {
    letters
        .iter()
        .copied()
        .filter(|&letter| std::path::Path::new(&format!("{letter}:\\")).exists())
        .collect()
}

/// `GetLogicalDrives` 失败时的兜底：直接探测盘符根目录。
fn fallback_probe() -> Vec<DriveInfo> {
    let mut out = Vec::new();
    for i in 0..26u8 {
        let letter = (b'A' + i) as char;
        let root = format!("{letter}:\\");
        if std::path::Path::new(&root).exists() {
            out.push(DriveInfo {
                letter,
                label: String::new(),
                kind: DriveKind::Fixed,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_at_least_one_drive() {
        let drives = local_drives();
        assert!(!drives.is_empty(), "本机至少应该有一个磁盘");
        assert!(drives.iter().any(|d| d.letter == 'C'));
    }

    #[test]
    fn letters_are_uppercase_ascii() {
        for drive in local_drives() {
            assert!(drive.letter.is_ascii_uppercase());
            assert!(drive.caption().starts_with(drive.letter));
        }
    }
}
