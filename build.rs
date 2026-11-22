//! 构建期把图标、版本信息、DPI 清单编进 exe。
//!
//! 不引入 winres 之类的额外依赖：直接用 Windows SDK 自带的 `rc.exe` 编一个 .res，
//! 再把它作为链接参数交给 link.exe。找不到 rc.exe 时只警告、不阻断构建。

use std::path::PathBuf;
use std::process::Command;

fn exe_dir_of(env: &str) -> Option<PathBuf> {
    std::env::var_os(env).map(PathBuf::from)
}

fn find_in_path() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join("rc.exe");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn find_in_windows_kits() -> Option<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(pf86) = exe_dir_of("ProgramFiles(x86)") {
        roots.push(pf86.join("Windows Kits").join("10").join("bin"));
    }
    if let Some(pf) = exe_dir_of("ProgramFiles") {
        roots.push(pf.join("Windows Kits").join("10").join("bin"));
    }
    roots.push(PathBuf::from(r"C:\Program Files (x86)\Windows Kits\10\bin"));

    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        let mut versions: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.join("x64").join("rc.exe").is_file())
            .collect();
        // 版本号按目录名倒序，取最新的 SDK
        versions.sort();
        if let Some(latest) = versions.pop() {
            return Some(latest.join("x64").join("rc.exe"));
        }
    }
    None
}

fn find_rc() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("RDP_RC_EXE") {
        let p = PathBuf::from(explicit);
        if p.is_file() {
            return Some(p);
        }
    }
    find_in_path().or_else(find_in_windows_kits)
}

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let icon = manifest_dir.join("assets").join("app.ico");
    let manifest = manifest_dir.join("assets").join("app.manifest");

    // 版本号只认 Cargo.toml，发布时不用再手改 build.rs 里的四处。
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
    let mut parts = version
        .split(['.', '-', '+'])
        .map(|p| p.parse::<u32>().unwrap_or(0));
    let (major, minor, patch, build) = (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    );
    let comma = format!("{major},{minor},{patch},{build}");

    println!("cargo:rerun-if-changed={}", icon.display());
    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rerun-if-env-changed=CARGO_PKG_VERSION");

    if !icon.is_file() {
        println!("cargo:warning=缺少图标文件 {}", icon.display());
        return;
    }

    let Some(rc) = find_rc() else {
        println!(
            "cargo:warning=找不到 rc.exe，exe 将没有图标。装 Windows SDK 或把 SDK 的 bin\\x64 加进 PATH。"
        );
        return;
    };

    // rc.exe 对反斜杠转义敏感，路径统一换成正斜杠
    let icon = icon.to_string_lossy().replace('\\', "/");
    let manifest = manifest.to_string_lossy().replace('\\', "/");

    let rc_text = format!(
        r#"#pragma code_page(65001)
1 ICON "{icon}"
1 24 "{manifest}"
1 VERSIONINFO
FILEVERSION {comma}
PRODUCTVERSION {comma}
FILEFLAGSMASK 0x3fL
FILEFLAGS 0x0L
FILEOS 0x40004L
FILETYPE 0x1L
FILESUBTYPE 0x0L
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "080404b0"
        BEGIN
            VALUE "FileDescription", "RDP 悬浮启动器\0"
            VALUE "FileVersion", "{version}.0\0"
            VALUE "InternalName", "rdp-launcher\0"
            VALUE "OriginalFilename", "rdp-launcher.exe\0"
            VALUE "ProductName", "RDP 启动器\0"
            VALUE "ProductVersion", "{version}.0\0"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x0804, 0x04b0
    END
END
"#
    );

    let rc_path = out_dir.join("rdp-launcher.rc");
    let res_path = out_dir.join("rdp-launcher.res");
    if let Err(e) = std::fs::write(&rc_path, rc_text) {
        println!("cargo:warning=写 rc 文件失败：{e}");
        return;
    }

    let output = Command::new(&rc)
        .current_dir(&out_dir)
        .arg("/nologo")
        .arg("/fo")
        .arg(&res_path)
        .arg(&rc_path)
        .output();

    match output {
        Ok(out) if out.status.success() => {
            println!("cargo:rustc-link-arg={}", res_path.display());
        }
        Ok(out) => {
            println!(
                "cargo:warning=rc.exe 失败（{}）：{}{}",
                rc.display(),
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Err(e) => {
            println!("cargo:warning=执行 rc.exe 失败：{e}");
        }
    }
}

