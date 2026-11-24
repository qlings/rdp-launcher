#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! 极速轻量级 RDP 悬浮启动器。
//!
//! 冷启动路径上刻意做的事很少：读 exe 内嵌配置 → 画第一帧 → 才开始异步探活。
//! 字体从系统读，不打包进二进制；渲染后端用 glow，RDP 会话里靠 Mesa 软件渲染兜底。

mod crypto;
mod dialogs;
mod drives;
mod history;
mod models;
mod probe;
mod rdp;
mod rdp_warning;
mod store;
mod taskbar;
mod ui;
mod update;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use eframe::egui;
use egui::{FontData, FontDefinitions, FontFamily};
use tray_icon::menu::{Menu, MenuItem, PredefinedMenuItem};
use tray_icon::TrayIcon;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HWND};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};

use dialogs::{HostDialogOutcome, HostForm, SettingsDraft, SettingsOutcome};
use drives::DriveInfo;
use models::Host;
use probe::{ProbeStatus, StatusMap};
use store::Store as StoreData;
use ui::{Palette, RowAction};

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

pub(crate) fn trace_enabled() -> bool {
    std::env::var_os("RDP_LAUNCHER_TRACE").is_some()
}

/// 排障开关：`RDP_LAUNCHER_TRACE=1` 时把启动阶段写进 `%TEMP%\rdp_launcher_startup.log`。
pub(crate) fn trace(msg: &str) {
    if !trace_enabled() {
        return;
    }
    use std::io::Write;
    let path = std::env::temp_dir().join("rdp_launcher_startup.log");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{:?} {msg}", std::time::SystemTime::now());
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 启动早期或退出时没有窗口可挂错误条，只能走系统消息框。
fn show_message(title: &str, text: &str) {
    let title = wide(title);
    let text = wide(text);
    // SAFETY: 两个字符串以 NUL 结尾且在调用期间存活。
    unsafe {
        MessageBoxW(
            HWND(std::ptr::null_mut()),
            PCWSTR(text.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
}

/// 单实例：命名 Mutex 已经存在就说明有一个在跑。
///
/// 返回 false 时调用方应该提示并退出——两个实例同时写 exe 数据流，后保存的会覆盖先保存的。
fn claim_single_instance() -> bool {
    static KEEP_ALIVE: OnceLock<isize> = OnceLock::new();
    let name = wide("Local\\rdp-launcher-single-instance");
    // SAFETY: name 以 NUL 结尾且在调用期间存活；句柄故意泄漏到进程结束。
    match unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) } {
        Ok(handle) => {
            let _ = KEEP_ALIVE.set(handle.0 as isize);
            // SAFETY: 紧接着 CreateMutexW 调用，读的是它留下的 last error。
            (unsafe { GetLastError() }).0 != ERROR_ALREADY_EXISTS.0
        }
        // 拿不到 Mutex 不要拦着用户用软件
        Err(_) => true,
    }
}

/// 配置就在 exe 自己身上。这里只拿路径，不拼任何旁边文件。
fn exe_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .unwrap_or_else(|| PathBuf::from("rdp-launcher.exe"))
}

fn load_app_store(path: &Path) -> (StoreData, Option<String>) {
    match store::load(path) {
        Ok(outcome) => (outcome.store, outcome.warning),
        Err(e) => {
            // 两份配置都解不开：先把原始字节备份下来，再允许后面用空配置继续。
            // 不备份的话，用户第一次操作就会在数据流上 truncate 重写，退出时尾部也一起覆盖。
            let backup = store::backup_broken(path);
            let mut fallback = StoreData::seed();
            fallback.normalize();
            let backup_note = match backup {
                Some(dir) => format!("（原始数据已备份到 {}）", dir.display()),
                None => String::new(),
            };
            (
                fallback,
                Some(format!("读取内置配置失败：{e}{backup_note}")),
            )
        }
    }
}

fn embed_store_target() -> Option<PathBuf> {
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--embed-store" {
            return args.next().map(PathBuf::from);
        }
    }
    None
}

/// 如果 exe 旁边放了 Mesa 的 opengl32.dll，就强制走 llvmpipe。
/// 这台开发机（Intel Haswell + RDP 会话）不设这两个变量会直接崩在 GL 初始化里。
fn configure_software_render() {
    let Ok(current) = std::env::current_exe() else {
        return;
    };
    let Some(dir) = current.parent() else {
        return;
    };
    if dir.join("libgallium_wgl.dll").is_file() || dir.join("opengl32.dll").is_file() {
        std::env::set_var("GALLIUM_DRIVER", "llvmpipe");
        std::env::set_var("LIBGL_ALWAYS_SOFTWARE", "1");
    }
}

// ---------------------------------------------------------------------------
// 字体
// ---------------------------------------------------------------------------

/// 从系统读微软雅黑，没有就退到黑体。读不到就保持 egui 默认字体（中文会方框，
/// 但至少不会崩），并在启动时提示一次。
fn install_cjk_font(ctx: &egui::Context) -> Option<String> {
    let candidates = [
        (r"C:\Windows\Fonts\msyh.ttc", "微软雅黑"),
        (r"C:\Windows\Fonts\msyh.ttf", "微软雅黑"),
        (r"C:\Windows\Fonts\simhei.ttf", "黑体"),
        (r"C:\Windows\Fonts\simsun.ttc", "宋体"),
    ];

    for (path, label) in candidates {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let mut fonts = FontDefinitions::default();
        fonts.font_data.insert(
            "cjk".to_owned(),
            std::sync::Arc::new(FontData::from_owned(bytes)),
        );

        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, "cjk".to_owned());
        fonts
            .families
            .entry(FontFamily::Monospace)
            .or_default()
            .push("cjk".to_owned());

        ctx.set_fonts(fonts);
        return Some(label.to_string());
    }
    None
}

// ---------------------------------------------------------------------------
// 托盘菜单
// ---------------------------------------------------------------------------

fn build_tray_menu(store: &StoreData) -> Menu {
    let menu = Menu::new();

    // 右键直接列主机，不再套「最近主机」子菜单，少一次点击。
    let order = models::visible_indices(&store.hosts, "", None, store.settings.sort);
    if order.is_empty() {
        let empty = MenuItem::with_id("hosts-empty", "（还没有主机）", false, None);
        let _ = menu.append(&empty);
    } else {
        for index in order {
            let host = &store.hosts[index];
            let label = format!("{}  —  {}", host.display_name(), host.display_address())
                .replace('&', "&&");
            let item = MenuItem::with_id(format!("host:{}", host.id), label, true, None);
            let _ = menu.append(&item);
        }
    }

    let _ = menu.append(&PredefinedMenuItem::separator());
    let open = MenuItem::with_id("open", "打开主界面", true, None);
    let _ = menu.append(&open);
    let quit = MenuItem::with_id("quit", "退出", true, None);
    let _ = menu.append(&quit);
    menu
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

/// 托盘线程 -> 主线程的事件。
enum TrayMsg {
    Open,
    Menu(String),
}

struct LauncherApp {
    store: StoreData,
    exe_path: PathBuf,
    drives: Vec<DriveInfo>,

    search: String,
    group_filter: Option<String>,
    /// 在**过滤后**列表里的下标
    cursor: usize,
    /// 本帧的过滤 + 排序结果（每帧最多算两次，键盘/行循环共用）
    visible: Vec<usize>,

    statuses: StatusMap,
    probe_rx: Option<Receiver<(String, bool)>>,
    /// 整批探活的截止时间：DNS 卡住时也要能停下来。
    probe_deadline: Option<Instant>,

    /// 后台线程探好的「盘符是否存在」，连接时直接用，不在 UI 线程碰 SMB。
    drive_presence: HashMap<char, bool>,
    /// 后台刷新返回：最新的盘符列表 + 仍然存在的盘符
    drive_rx: Option<Receiver<(Vec<DriveInfo>, Vec<char>)>>,

    form: Option<HostForm>,
    settings_draft: Option<SettingsDraft>,
    about_open: bool,
    toasts: Vec<ui::Toast>,

    /// 写入防抖：脏了就记一笔，1.2 秒内没再改才真正落盘
    dirty: bool,
    last_flush: Instant,
    /// 上一帧用的主题，Style 只在切换时重建
    last_dark: Option<bool>,

    /// 启动后第一帧自动聚焦搜索框
    want_focus_search: bool,
    /// 键盘上下移动后把选中项滚进视野，画完一帧清掉
    scroll_to_cursor: bool,
    probe_started: bool,
    /// 调试用：RDP_LAUNCHER_AUTOCONNECT=1 时启动后自动连第一台
    autoconnect: bool,
    /// 自己数帧：egui 0.31 的 Context 没有 frame_nr()
    frame_count: u64,
    /// 读取 / 写入 exe 内嵌配置失败时的常驻红条。
    store_error: Option<String>,

    /// 托盘图标只在「关闭后保持后台」真正用到时创建
    tray: Option<TrayIcon>,
    tray_events: Arc<Mutex<Vec<TrayMsg>>>,
    hidden_to_tray: bool,
    /// 收进托盘的时刻：给「窗口被外部还原」一个稳定判定的缓冲期
    hidden_since: Option<Instant>,
    /// 用户从托盘菜单明确点「退出」时，不要再被 close_to_tray 拦截
    quit_requested: bool,

    /// Windows 新版 RDP 安全警告的顶部提示
    show_rdp_warning_hint: bool,
    warning_fix_pending_since: Option<Instant>,

    /// 「关于」里的 GitHub 最新版检查（后台线程，UI 只轮询）
    update_state: update::UpdateState,
    update_rx: Option<Receiver<Result<String, String>>>,
}

impl LauncherApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let app_start = Instant::now();
        let font_note = install_cjk_font(&cc.egui_ctx);
        trace(&format!(
            "new: font {:?}",
            app_start.elapsed()
        ));

        let exe_path = exe_path();
        let (mut store, store_error) = load_app_store(&exe_path);
        store.normalize();
        trace(&format!("new: store {:?}", app_start.elapsed()));

        // 调试开关：命令行/脚本启动时直接切主题、视图，方便截图验证
        if let Ok(view) = std::env::var("RDP_LAUNCHER_VIEW") {
            if view.eq_ignore_ascii_case("grid") {
                store.settings.view = models::ViewMode::Grid;
            } else if view.eq_ignore_ascii_case("list") {
                store.settings.view = models::ViewMode::List;
            }
        }
        if let Ok(theme) = std::env::var("RDP_LAUNCHER_THEME") {
            store.settings.dark = theme.eq_ignore_ascii_case("dark");
        }
        if let Ok(flag) = std::env::var("RDP_LAUNCHER_CLOSE_TO_TRAY") {
            store.settings.close_to_tray = flag != "0";
        }

        // Windows 2026 的「未知的远程连接」警告：能静默关就静默关；
        // 普通用户需要管理员，交给顶部提示/设置里的按钮触发 UAC。
        //
        // 两条单次写入留在主线程（要立刻决定顶部提示）；每台主机一遍的
        // `LocalDevices` 是 O(n) 次注册表写，挪到后台，不挡首帧——
        // 连接前 `connect_host` 还会再写一次，晚一点没关系。
        rdp_warning::ensure_user_consent();
        let _ = rdp_warning::try_set_machine();
        rdp_warning::ensure_auth_override();
        let host_names: Vec<(String, u16)> = store
            .hosts
            .iter()
            .map(|h| (h.host.trim().to_string(), h.port))
            .filter(|(h, _)| !h.is_empty())
            .collect();
        if !host_names.is_empty() {
            std::thread::spawn(move || rdp_warning::allow_hosts(&host_names));
        }
        let show_rdp_warning_hint = !rdp_warning::is_suppressed()
            && store.settings.rdp_warning_hint
            && std::env::var_os("RDP_LAUNCHER_HIDE_WARNING_HINT").is_none();
        trace(&format!("new: rdp_warning {:?}", app_start.elapsed()));

        let default_group = store.settings.default_group.clone();
        let group_filter = if !default_group.is_empty()
            && store.groups.iter().any(|g| g.name == default_group)
        {
            Some(default_group)
        } else {
            None
        };

        // 托盘事件用 tray-icon 推荐的 set_event_handler：回调在托盘消息线程里执行，
        // 直接塞进共享队列并 request_repaint，避免窗口隐藏时事件循环不醒。
        // 另外隐藏期间 update 每 250ms 自唤醒一次做兜底。
        let tray_events = Arc::new(Mutex::new(Vec::new()));
        {
            let queue = tray_events.clone();
            let ctx = cc.egui_ctx.clone();
            tray_icon::menu::MenuEvent::set_event_handler(Some(
                move |event: tray_icon::menu::MenuEvent| {
                    if let Ok(mut queue) = queue.lock() {
                        queue.push(TrayMsg::Menu(event.id.0));
                    }
                    ctx.request_repaint();
                },
            ));
        }
        {
            let queue = tray_events.clone();
            let ctx = cc.egui_ctx.clone();
            tray_icon::TrayIconEvent::set_event_handler(Some(
                move |event: tray_icon::TrayIconEvent| {
                    let open = matches!(
                        event,
                        tray_icon::TrayIconEvent::Click {
                            button: tray_icon::MouseButton::Left,
                            button_state: tray_icon::MouseButtonState::Up,
                            ..
                        } | tray_icon::TrayIconEvent::DoubleClick { .. }
                    );
                    if open {
                        if let Ok(mut queue) = queue.lock() {
                            queue.push(TrayMsg::Open);
                        }
                        ctx.request_repaint();
                    }
                },
            ));
        }

        let drives = drives::local_drives();
        trace(&format!("new: drives {:?}", app_start.elapsed()));
        let mut app = Self {
            store,
            exe_path,
            drives,
            search: String::new(),
            group_filter,
            cursor: 0,
            visible: Vec::new(),
            statuses: StatusMap::new(),
            probe_rx: None,
            probe_deadline: None,
            drive_presence: HashMap::new(),
            drive_rx: None,
            form: None,
            settings_draft: None,
            about_open: false,
            toasts: Vec::new(),
            dirty: false,
            last_flush: Instant::now(),
            last_dark: None,
            want_focus_search: true,
            scroll_to_cursor: false,
            probe_started: false,
            autoconnect: std::env::var_os("RDP_LAUNCHER_AUTOCONNECT").is_some(),
            frame_count: 0,
            store_error,
            tray: None,
            tray_events,
            hidden_to_tray: false,
            hidden_since: None,
            quit_requested: false,
            show_rdp_warning_hint,
            warning_fix_pending_since: None,
            update_state: update::UpdateState::Idle,
            update_rx: None,
        };

        if let Some(err) = &app.store_error {
            app.push_toast(err.clone(), true);
        } else if font_note.is_none() {
            app.push_toast("没找到系统中文字体，界面可能显示方块", true);
        }

        // 调试入口：RDP_LAUNCHER_OPEN_DIALOG=add|edit|delete|settings|toast
        match std::env::var("RDP_LAUNCHER_OPEN_DIALOG").as_deref() {
            Ok("add") => {
                app.form = Some(HostForm::new_add(
                    &app.drives,
                    app.store.next_order(),
                    app.group_filter.as_deref(),
                ))
            }
            Ok("edit") | Ok("delete") => {
                if let Some(host) = app.store.hosts.first() {
                    let mut form = HostForm::from_host(0, host);
                    if std::env::var("RDP_LAUNCHER_OPEN_DIALOG").as_deref() == Ok("delete") {
                        form.delete_armed = true;
                    }
                    app.form = Some(form);
                }
            }
            Ok("settings") => app.settings_draft = Some(SettingsDraft::new(&app.store)),
            Ok("about") => app.about_open = true,
            Ok("toast") => {
                app.push_toast("已启动远程桌面：办公室主力机", false);
            }
            _ => {}
        }
        trace(&format!("new: ready {:?}", app_start.elapsed()));
        app
    }

    /// 提示只留最近 4 条，过期的顺手丢掉。
    fn push_toast(&mut self, message: impl Into<String>, is_error: bool) {
        self.toasts.retain(|t| !t.expired());
        self.toasts.push(ui::Toast::new(message, is_error));
        if self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
    }

    fn compute_visible(&self) -> Vec<usize> {
        models::visible_indices(
            &self.store.hosts,
            &self.search,
            self.group_filter.as_deref(),
            self.store.settings.sort,
        )
    }

    /// 搜索 / 分组 / 排序 / 主机集合变了就重算一次，并夹住光标。
    fn refresh_visible(&mut self) {
        self.visible = self.compute_visible();
        if !self.visible.is_empty() && self.cursor >= self.visible.len() {
            self.cursor = self.visible.len() - 1;
        }
    }

    fn current_host_index(&self) -> Option<usize> {
        self.visible.get(self.cursor).copied()
    }

    /// 主窗口已经画出来了，才开始探活。
    fn start_probe(&mut self, ctx: &egui::Context) {
        self.statuses.clear();
        self.probe_rx = None;
        self.probe_deadline = None;
        if !self.store.settings.probe || self.store.hosts.is_empty() {
            ctx.request_repaint();
            return;
        }
        let (tx, rx) = channel();
        // 只传 id / 地址 / 端口，不把带明文密码的整个 Host 复制进线程池
        probe::spawn_probe_batch(probe::targets_of(&self.store.hosts), tx);
        self.probe_rx = Some(rx);
        self.probe_deadline = Some(Instant::now() + probe::BATCH_DEADLINE);
        ctx.request_repaint();
    }

    fn drain_probe(&mut self, ctx: &egui::Context) {
        let mut changed = false;
        let mut finished = false;
        if let Some(rx) = &self.probe_rx {
            loop {
                match rx.try_recv() {
                    Ok((id, online)) => {
                        self.statuses.insert(
                            id,
                            if online {
                                ProbeStatus::Online
                            } else {
                                ProbeStatus::Offline
                            },
                        );
                        changed = true;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        finished = true;
                        break;
                    }
                }
            }
        }
        // DNS 卡住时 worker 可能很久不返回：到点就收摊，没出结果的按离线算，
        // 不然会一直「检测中」并且每 120ms 空转重绘。
        let timed_out = !finished
            && self
                .probe_deadline
                .is_some_and(|deadline| Instant::now() >= deadline);
        if timed_out {
            for host in &self.store.hosts {
                self.statuses
                    .entry(host.id.clone())
                    .or_insert(ProbeStatus::Offline);
            }
            finished = true;
            changed = true;
        }
        if finished {
            self.probe_rx = None;
            self.probe_deadline = None;
        }
        if changed {
            ctx.request_repaint();
        }
    }

    /// 后台刷新盘符列表 + 存在性（网络盘可能卡在 SMB 上，绝不能放 UI 线程）。
    ///
    /// 启动时先用 `local_drives()` 的快速结果把窗口画出来（不查网络盘卷标），
    /// 这里再补一次完整结果：卷标、插拔出来的新盘都会一起更新。
    fn refresh_drive_presence(&mut self, ctx: &egui::Context) {
        if self.drive_rx.is_some() {
            return;
        }
        let mut letters: Vec<char> = self.drives.iter().map(|d| d.letter).collect();
        for host in &self.store.hosts {
            for drive in &host.drives {
                if let Some(c) = drive.chars().next() {
                    let c = c.to_ascii_uppercase();
                    if c.is_ascii_uppercase() && !letters.contains(&c) {
                        letters.push(c);
                    }
                }
            }
        }
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            // 完整枚举（含网络盘卷标）和存在性探测都可能卡 SMB，一起丢后台
            let drives = drives::local_drives_full();
            let mut all: Vec<char> = letters;
            for drive in &drives {
                if !all.contains(&drive.letter) {
                    all.push(drive.letter);
                }
            }
            let existing = drives::existing_letters(&all);
            let _ = tx.send((drives, existing));
        });
        self.drive_rx = Some(rx);
        ctx.request_repaint();
    }

    fn drain_drive_presence(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.drive_rx else { return };
        let Ok((drives, existing)) = rx.try_recv() else {
            return;
        };
        // 盘符列表也一起换新：插拔 U 盘 / 换网络盘不用重启程序
        if !drives.is_empty() {
            self.drives = drives;
        }
        let existing: std::collections::HashSet<char> = existing.into_iter().collect();
        for drive in &self.drives {
            self.drive_presence
                .insert(drive.letter, existing.contains(&drive.letter));
        }
        for host in &self.store.hosts {
            for raw in &host.drives {
                if let Some(c) = raw.chars().next() {
                    let c = c.to_ascii_uppercase();
                    self.drive_presence.insert(c, existing.contains(&c));
                }
            }
        }
        self.drive_rx = None;
        ctx.request_repaint();
    }

    /// 标记有改动，1.2 秒后（或退出/收托盘时）真正落盘。
    fn persist(&mut self) {
        self.dirty = true;
    }

    /// 立刻落盘。返回是否成功；失败会在界面上留常驻红条。
    fn flush_store(&mut self, ctx: Option<&egui::Context>) -> bool {
        if !self.dirty {
            return true;
        }
        self.dirty = false;
        self.last_flush = Instant::now();
        match store::save(&self.exe_path, &mut self.store) {
            Ok(()) => {
                if self
                    .store_error
                    .as_deref()
                    .is_some_and(|e| e.starts_with("配置保存失败"))
                {
                    self.store_error = None;
                }
                true
            }
            Err(e) => {
                self.dirty = true;
                let message = format!("配置保存失败：{e}");
                self.store_error = Some(message.clone());
                // 写不进去必须让用户看见，不能静默丢数据：隐藏着就把主界面叫回来
                if self.hidden_to_tray {
                    if let Some(ctx) = ctx {
                        self.show_main(ctx);
                    }
                }
                self.push_toast(message, true);
                false
            }
        }
    }

    fn connect_selected(&mut self, ctx: &egui::Context) {
        let Some(index) = self.current_host_index() else {
            self.push_toast("列表是空的，先新增一台主机", true);
            return;
        };
        self.connect_host(index, ctx);
    }

    /// 连接指定主机。连完启动器保持原样（不自动最小化），
    /// 从托盘菜单连的时候主窗口本来就藏着，也不会被叫出来。
    fn connect_host(&mut self, index: usize, ctx: &egui::Context) {
        let Some(host) = self.store.hosts.get(index).cloned() else {
            return;
        };
        // 旧版 mstsc 的「无法识别发布者」弹窗：连接前确保这个主机被标记为不再询问
        rdp_warning::allow_host(&host.host, host.port);
        match rdp::connect(&host, &self.drive_presence) {
            Ok(outcome) => {
                self.store.touch_recent(&host.id);
                self.persist();
                // 托盘菜单只列主机和排序，连接本身不改变内容：不必每次重建整个原生菜单。

                let mut msg = format!("已启动远程桌面：{}", host.display_name());
                if !outcome.missing_drives.is_empty() {
                    let letters: Vec<String> =
                        outcome.missing_drives.iter().map(|c| format!("{c}:")).collect();
                    msg = format!(
                        "{}（{} 不存在，已跳过磁盘映射）",
                        msg,
                        letters.join("/")
                    );
                }
                if !self.hidden_to_tray {
                    self.push_toast(msg, false);
                }
            }
            Err(e) => {
                self.push_toast(e, true);
                // 托盘里点连接失败时，把主界面叫出来让用户看到原因
                if self.hidden_to_tray {
                    self.show_main(ctx);
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // 托盘 / 后台
    // -----------------------------------------------------------------------

    fn ensure_tray(&mut self) -> bool {
        if self.tray.is_some() {
            return true;
        }
        let icon_data = match eframe::icon_data::from_png_bytes(include_bytes!(
            "../assets/app-256.png"
        )) {
            Ok(data) => data,
            Err(e) => {
                self.push_toast(format!("读取托盘图标失败：{e}"), true);
                return false;
            }
        };
        let icon = match tray_icon::Icon::from_rgba(icon_data.rgba, icon_data.width, icon_data.height)
        {
            Ok(icon) => icon,
            Err(e) => {
                self.push_toast(format!("托盘图标格式错误：{e}"), true);
                return false;
            }
        };

        let menu = build_tray_menu(&self.store);
        match tray_icon::TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_menu_on_right_click(true)
            .with_tooltip("RDP 启动器")
            .with_icon(icon)
            .build()
        {
            Ok(tray) => {
                let hwnd = tray.window_handle();
                self.tray = Some(tray);
                if trace_enabled() {
                    trace(&format!("tray: created hwnd={hwnd:?}"));
                }
                true
            }
            Err(e) => {
                self.push_toast(format!("创建托盘图标失败：{e}"), true);
                false
            }
        }
    }

    fn hide_to_tray(&mut self, ctx: &egui::Context) -> bool {
        if !self.ensure_tray() {
            return false;
        }
        // 收托盘前强制落盘：用户接下来可能直接杀进程或者拔 U 盘。
        if !self.flush_store(Some(ctx)) {
            // 写不进去就别藏（藏了就看不见红条）；关窗路径会被调用方当成「未处理」放行，
            // 至少用户已经看到过这条消息框，不是静默丢数据。
            if let Some(err) = &self.store_error {
                show_message("RDP 启动器 · 保存失败", err);
            }
            return false;
        }
        self.hidden_to_tray = true;
        self.hidden_since = Some(Instant::now());
        trace("tray: hide");
        // 不能 Visible(false)：隐藏窗口不会处理重绘，update 会停，托盘事件没人处理。
        // 最小化 + WS_EX_TOOLWINDOW（任务栏 / Alt+Tab 都不出现），用户看到的就是界面完全关掉。
        taskbar::hide_window_to_tray();
        true
    }

    fn show_main(&mut self, ctx: &egui::Context) {
        self.hidden_to_tray = false;
        self.hidden_since = None;
        trace("tray: show main");
        taskbar::restore_from_tray();
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        ctx.request_repaint();
    }

    fn refresh_tray_menu(&self) {
        if let Some(tray) = &self.tray {
            tray.set_menu(Some(Box::new(build_tray_menu(&self.store))));
        }
    }

    fn quit_app(&mut self, ctx: &egui::Context) {
        self.quit_requested = true;
        self.hidden_to_tray = false;
        self.tray = None;
        // 最后一份数据必须落地；失败时窗口马上就要关，用消息框告诉用户。
        if !self.flush_store(Some(ctx)) {
            if let Some(err) = &self.store_error {
                show_message("RDP 启动器 · 保存失败", err);
            }
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn drain_tray(&mut self, ctx: &egui::Context) {
        let events: Vec<TrayMsg> = match self.tray_events.lock() {
            Ok(mut queue) => std::mem::take(&mut *queue),
            Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
        };
        let mut quit = false;
        for msg in events {
            match msg {
                TrayMsg::Open => self.show_main(ctx),
                TrayMsg::Menu(id) => {
                    if id == "open" {
                        self.show_main(ctx);
                    } else if id == "quit" {
                        trace("tray: quit");
                        quit = true;
                    } else if let Some(host_id) = id.strip_prefix("host:") {
                        if let Some(index) =
                            self.store.hosts.iter().position(|h| h.id == host_id)
                        {
                            self.connect_host(index, ctx);
                        }
                    }
                }
            }
        }
        if quit {
            self.quit_app(ctx);
        }
    }

    // -----------------------------------------------------------------------
    // Windows RDP 安全警告
    // -----------------------------------------------------------------------

    fn request_rdp_warning_fix(&mut self, ctx: &egui::Context) {
        rdp_warning::ensure_user_consent();
        match rdp_warning::try_set_machine() {
            Ok(()) => {
                self.show_rdp_warning_hint = false;
                self.store.settings.rdp_warning_hint = true;
                self.warning_fix_pending_since = None;
                self.persist();
                self.push_toast("已关闭 Windows RDP 安全警告", false);
            }
            Err(_) => {
                if rdp_warning::launch_elevated_fix() {
                    self.warning_fix_pending_since = Some(Instant::now());
                    self.push_toast("请在弹出的管理员授权里点「是」", false);
                } else {
                    self.push_toast("无法启动管理员授权；可以右键以管理员身份运行一次", true);
                }
            }
        }
        ctx.request_repaint();
    }

    fn poll_warning_fix(&mut self, ctx: &egui::Context) {
        let Some(since) = self.warning_fix_pending_since else {
            return;
        };
        if rdp_warning::is_suppressed() {
            self.warning_fix_pending_since = None;
            self.show_rdp_warning_hint = false;
            self.store.settings.rdp_warning_hint = true;
            self.persist();
            self.push_toast("已关闭 Windows RDP 安全警告", false);
        } else if since.elapsed() > Duration::from_secs(120) {
            self.warning_fix_pending_since = None;
            self.push_toast("没有检测到管理员授权结果；可以在设置里重试", true);
        } else {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
    }

    fn delete_host(&mut self, index: usize, also_credential: bool) {
        if index >= self.store.hosts.len() {
            return;
        }
        let host = self.store.hosts.remove(index);
        let cleaned = if also_credential {
            rdp::cleanup_credential_for(&host, &self.store.hosts)
        } else {
            false
        };
        self.statuses.remove(&host.id);
        self.refresh_visible();
        let len = self.visible.len();
        if len == 0 {
            self.cursor = 0;
        } else if self.cursor >= len {
            self.cursor = len - 1;
        }
        self.persist();
        self.refresh_tray_menu();
        let suffix = if also_credential && !cleaned {
            "（凭据与其他主机共用，未删除）"
        } else {
            ""
        };
        self.push_toast(format!("已删除「{}」{suffix}", host.display_name()), false);
    }

    /// 复制一台主机：原样复制，名字加「副本」（重名自动加编号），插在原主机后面。
    /// 返回复制出来的新名字。
    fn duplicate_host(&mut self, index: usize) -> Option<String> {
        let source_id = self.store.hosts.get(index)?.id.clone();
        let at = models::duplicate_into(&mut self.store.hosts, index)?;
        let copy = self.store.hosts.get(at)?;
        let id = copy.id.clone();
        let name = copy.name.clone();
        self.store.normalize();
        // 地址、端口没变，探活结果直接沿用，不用再探一次
        if let Some(status) = self.statuses.get(&source_id).copied() {
            self.statuses.insert(id.clone(), status);
        }
        self.persist();
        self.refresh_tray_menu();
        self.refresh_visible();
        if let Some(pos) = self.visible.iter().position(|&i| self.store.hosts[i].id == id) {
            self.cursor = pos;
        }
        Some(name)
    }

    // -----------------------------------------------------------------------
    // 导入本机远程桌面历史
    // -----------------------------------------------------------------------

    /// 导入并把结果提示出来。设置弹窗 / 空状态里都调这个。
    fn import_history_with_toast(&mut self, ctx: &egui::Context) {
        let (added, skipped) = self.import_history();
        let message = if added > 0 {
            if skipped > 0 {
                format!("已导入 {added} 台主机（跳过 {skipped} 台已存在或无效）")
            } else {
                format!("已导入 {added} 台主机")
            }
        } else if skipped > 0 {
            "没有新主机可导入：历史里的机器都已经在列表里了".to_string()
        } else {
            "没有找到本机的远程桌面历史（在 mstsc 里连过的机器才会记录）".to_string()
        };
        self.push_toast(message, false);
        ctx.request_repaint();
    }

    /// 读 HKCU 的历史记录 → 拆地址 → 按默认值（剪贴板开、声音关、全部盘符）落成主机。
    /// 返回（实际导入数量，跳过数量：已存在 / 地址非法）。
    fn import_history(&mut self) -> (usize, usize) {
        let drives: Vec<String> = self
            .drives
            .iter()
            .map(|d| d.letter.to_ascii_uppercase().to_string())
            .collect();
        let entries: Vec<(String, u16, String)> = history::local_history()
            .into_iter()
            .map(|entry| (entry.host, entry.port, entry.username))
            .collect();
        let (added, skipped) = models::import_hosts(&mut self.store.hosts, &entries, &drives);
        if added > 0 {
            self.store.normalize();
            self.persist();
            self.refresh_tray_menu();
            self.refresh_visible();
            // 新主机也要有状态圆点：清空结果，下一帧整批重探
            self.statuses.clear();
            self.probe_started = false;
        }
        (added, skipped)
    }

    // -----------------------------------------------------------------------
    // 「关于」里的最新版检查
    // -----------------------------------------------------------------------

    /// `force = false` 时只在还没查过时发一次请求；`force = true` 是「重新检查」。
    fn start_update_check(&mut self, force: bool) {
        if self.update_rx.is_some() {
            return;
        }
        if !force && !matches!(self.update_state, update::UpdateState::Idle) {
            return;
        }
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(update::check_latest());
        });
        self.update_rx = Some(rx);
        self.update_state = update::UpdateState::Checking;
    }

    fn drain_update(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.update_rx.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(result) => {
                self.update_state = match result {
                    Ok(tag) if update::is_newer(env!("CARGO_PKG_VERSION"), &tag) => {
                        update::UpdateState::Newer(tag)
                    }
                    Ok(tag) => update::UpdateState::Latest(tag),
                    Err(reason) => update::UpdateState::Failed(reason),
                };
                ctx.request_repaint();
            }
            Err(TryRecvError::Disconnected) => {
                self.update_state =
                    update::UpdateState::Failed("检查线程异常退出".to_string());
                ctx.request_repaint();
            }
            Err(TryRecvError::Empty) => self.update_rx = Some(rx),
        }
    }

    fn save_host(&mut self, host: Host, mode: dialogs::DialogMode, ctx: &egui::Context) {
        for group in &host.groups {
            self.store.ensure_group(group);
        }
        let id = host.id.clone();
        match mode {
            dialogs::DialogMode::Edit(index) => {
                if index < self.store.hosts.len() {
                    self.store.hosts[index] = host;
                }
            }
            dialogs::DialogMode::Add => self.store.hosts.push(host),
        }
        self.store.normalize();
        self.persist();
        self.refresh_tray_menu();
        // 地址 / 端口可能改了，状态全部作废重测
        self.statuses.clear();
        self.probe_started = false;
        self.refresh_visible();

        if let Some(pos) = self
            .visible
            .iter()
            .position(|&i| self.store.hosts[i].id == id)
        {
            self.cursor = pos;
        }
        self.push_toast("已保存", false);
        // 新勾的盘符也要有存在性记录，连接时才不会乐观地把不存在的盘写进去
        self.refresh_drive_presence(ctx);
        ctx.request_repaint();
    }

    fn apply_settings(&mut self, draft: &SettingsDraft, ctx: &egui::Context) -> Result<(), String> {
        let probe_was_on = self.store.settings.probe;
        // apply 会把「旧分组名 -> 新分组名」的映射交回来，当前过滤要跟着改，
        // 否则改个名列表就整批换掉，像数据丢了。
        let rename = draft.apply(&mut self.store)?;

        if !self.store.settings.show_search {
            self.search.clear();
        }

        if let Some(current) = self.group_filter.clone() {
            if let Some(new_name) = rename.get(&current) {
                self.group_filter = Some(new_name.clone());
            }
            let still_valid = self
                .group_filter
                .as_ref()
                .is_some_and(|g| self.store.groups.iter().any(|x| &x.name == g));
            if !still_valid {
                let default = self.store.settings.default_group.clone();
                self.group_filter = if !default.is_empty() { Some(default) } else { None };
            }
        }

        match (probe_was_on, self.store.settings.probe) {
            (true, false) => {
                self.statuses.clear();
                self.probe_rx = None;
                self.probe_deadline = None;
                self.probe_started = true;
            }
            (false, true) => {
                self.statuses.clear();
                self.probe_started = false;
            }
            _ => {}
        }

        if !self.store.settings.close_to_tray {
            self.tray = None;
            self.hidden_to_tray = false;
        }
        self.show_rdp_warning_hint = !rdp_warning::is_suppressed()
            && self.store.settings.rdp_warning_hint
            && self.warning_fix_pending_since.is_none();
        self.refresh_tray_menu();
        self.refresh_visible();

        self.persist();
        if draft.fix_rdp_warning {
            self.request_rdp_warning_fix(ctx);
        }
        self.push_toast("设置已保存", false);
        ctx.request_repaint();
        Ok(())
    }

    // -----------------------------------------------------------------------
    // 键盘：必须抢在 TextEdit 之前消费，否则方向键被吞、Esc 只失焦不清空
    // -----------------------------------------------------------------------

    fn handle_keys(&mut self, ctx: &egui::Context) {
        use egui::{Key, Modifiers};

        // 弹窗打开时：Esc 关弹窗，其它键留给弹窗里的输入框
        if self.form.is_some() || self.settings_draft.is_some() || self.about_open {
            if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
                self.form = None;
                self.settings_draft = None;
                self.about_open = false;
            }
            return;
        }

        let filtered_len = self.visible.len();

        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowDown)) {
            if filtered_len > 0 {
                self.cursor = (self.cursor + 1).min(filtered_len - 1);
                self.scroll_to_cursor = true;
            }
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowUp)) {
            self.cursor = self.cursor.saturating_sub(1);
            self.scroll_to_cursor = true;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
            if filtered_len > 0 {
                self.connect_selected(ctx);
            }
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::N)) {
            self.refresh_drive_presence(ctx);
            self.form = Some(HostForm::new_add(
                &self.drives,
                self.store.next_order(),
                self.group_filter.as_deref(),
            ));
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::F))
            && self.store.settings.show_search
        {
            self.want_focus_search = true;
        }
        // Esc 只用来清搜索 / 关弹窗，不再顺手最小化窗口
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape))
            && !self.search.is_empty()
        {
            self.search.clear();
            self.cursor = 0;
            self.want_focus_search = true;
            self.refresh_visible();
        }
    }
}

impl eframe::App for LauncherApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.frame_count == 0 {
            trace("update: first frame");
        }

        self.drain_tray(ctx);
        self.poll_warning_fix(ctx);
        self.drain_drive_presence(ctx);

        // 「关闭后保持后台」：只有真正藏进托盘了才取消关闭。
        // 反过来的话，托盘建不起来（shell 没起、被安全软件拦）会变成「关也关不掉、藏也藏不了」。
        if ctx.input(|i| i.viewport().close_requested())
            && self.store.settings.close_to_tray
            && !self.quit_requested
        {
            if self.hide_to_tray(ctx) {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                return;
            }
            // 藏不了就放行，让 eframe 正常关闭
        }

        // 最小化按钮 = 普通最小化，不收托盘（v0.4.2 改的）。

        // 如果窗口被外部还原了（别的工具 / 任务管理器 / 系统），状态要同步回来，
        // 否则我们还以为在托盘里，会跳过绘制，窗口上留下停住的一帧。
        if self.hidden_to_tray
            && self
                .hidden_since
                .is_some_and(|since| since.elapsed() > Duration::from_millis(700))
            && ctx.input(|i| i.viewport().minimized == Some(false))
        {
            trace("tray: restored externally");
            self.show_main(ctx);
        }

        // 第一帧只画界面，第二帧才开始后台探活：先把窗口亮出来，再谈连不连得上。
        self.frame_count = self.frame_count.saturating_add(1);
        if !self.probe_started && self.frame_count >= 2 {
            self.probe_started = true;
            self.start_probe(ctx);
        }
        if self.frame_count == 2 {
            self.refresh_drive_presence(ctx);
        }

        self.refresh_visible();

        if self.autoconnect && self.frame_count >= 2 {
            self.autoconnect = false;
            if !self.store.hosts.is_empty() {
                self.connect_selected(ctx);
            }
        }

        self.handle_keys(ctx);
        self.drain_probe(ctx);
        self.drain_update(ctx);
        if self.frame_count <= 1 {
            trace("update: keys/probe done");
        }

        // 写入防抖：攒够 1.2 秒没有新改动才真正落盘（隐藏到托盘时也要能落盘）
        if self.dirty && self.last_flush.elapsed() >= Duration::from_millis(1200) {
            self.flush_store(Some(ctx));
        }

        // 隐藏到托盘时不用画：探活在后台继续跑，圆点下次还原时自然是最新的。
        if self.hidden_to_tray && self.toasts.is_empty() {
            let tick = if self.probe_rx.is_some() {
                Duration::from_millis(120)
            } else {
                Duration::from_millis(250)
            };
            ctx.request_repaint_after(tick);
            return;
        }

        let pal: Palette = ui::palette(self.store.settings.dark);
        // Style 只在主题真的变了时重建，没必要每帧 clone 一遍
        if self.last_dark != Some(self.store.settings.dark) {
            ui::apply_style(ctx, &pal);
            self.last_dark = Some(self.store.settings.dark);
        }
        if self.frame_count <= 1 {
            trace("update: style done");
        }

        egui::TopBottomPanel::bottom("footer")
            .frame(
                egui::Frame::new()
                    .fill(pal.bg)
                    .inner_margin(egui::Margin::symmetric(18, 8)),
            )
            .show(ctx, |ui| {
                let about_clicked = ui::draw_footer(
                    ui,
                    &pal,
                    self.store.hosts.len(),
                    self.visible.len(),
                    self.store.settings.sort,
                    self.store.settings.view,
                    env!("CARGO_PKG_VERSION"),
                );
                if about_clicked {
                    self.about_open = true;
                }
            });
        if self.frame_count <= 1 {
            trace("update: footer done");
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(pal.bg).inner_margin(egui::Margin::same(14)))
            .show(ctx, |ui| {
                if self.frame_count <= 1 {
                    trace("central: entered");
                }
                if let Some(err) = &self.store_error {
                    egui::Frame::new()
                        .fill(pal.danger_soft)
                        .corner_radius(egui::CornerRadius::same(8))
                        .stroke(egui::Stroke::new(1.0_f32, pal.danger))
                        .inner_margin(egui::Margin::same(10))
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.label(
                                egui::RichText::new(format!("⚠ 配置存储异常：{err}"))
                                    .size(12.0)
                                    .color(pal.danger),
                            );
                        });
                    ui.add_space(8.0);
                }

                if self.show_rdp_warning_hint {
                    let mut fix = false;
                    let mut ignore = false;
                    egui::Frame::new()
                        .fill(pal.accent_soft)
                        .corner_radius(egui::CornerRadius::same(8))
                        .stroke(egui::Stroke::new(1.0_f32, pal.accent))
                        .inner_margin(egui::Margin::same(10))
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.label(
                                egui::RichText::new(
                                    "Windows 新版会在连接时弹「未知的远程连接」安全警告。",
                                )
                                .size(11.5)
                                .color(pal.text),
                            );
                            ui.horizontal(|ui| {
                                if let Some(since) = self.warning_fix_pending_since {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "等待管理员授权…（{} 秒）",
                                            since.elapsed().as_secs()
                                        ))
                                        .size(11.0)
                                        .color(pal.accent),
                                    );
                                } else {
                                    if ui
                                        .add(
                                            egui::Button::new(
                                                egui::RichText::new("关闭警告（管理员一次）")
                                                    .size(11.5)
                                                    .color(egui::Color32::WHITE),
                                            )
                                            .fill(pal.accent)
                                            .corner_radius(egui::CornerRadius::same(7))
                                            .min_size(egui::Vec2::new(0.0, 29.0)),
                                        )
                                        .clicked()
                                    {
                                        fix = true;
                                    }
                                    if ui
                                        .add(
                                            egui::Button::new(
                                                egui::RichText::new("忽略")
                                                    .size(11.0)
                                                    .color(pal.text_dim),
                                            )
                                            .fill(egui::Color32::TRANSPARENT)
                                            .stroke(egui::Stroke::NONE)
                                            .min_size(egui::Vec2::new(0.0, 24.0)),
                                        )
                                        .clicked()
                                    {
                                        ignore = true;
                                    }
                                }
                            });
                        });
                    if fix {
                        self.request_rdp_warning_fix(ctx);
                    }
                    if ignore {
                        self.show_rdp_warning_hint = false;
                        self.store.settings.rdp_warning_hint = false;
                        self.persist();
                    }
                    ui.add_space(8.0);
                }

                let top = ui::draw_top_bar(
                    ui,
                    &pal,
                    &mut self.search,
                    self.store.settings.show_search,
                    &mut self.want_focus_search,
                    self.store.settings.dark,
                );
                if top.open_add {
                    self.refresh_drive_presence(ctx);
                    self.form = Some(HostForm::new_add(
                        &self.drives,
                        self.store.next_order(),
                        self.group_filter.as_deref(),
                    ));
                }
                if top.open_settings {
                    self.settings_draft = Some(SettingsDraft::new(&self.store));
                }
                if top.toggle_theme {
                    self.store.settings.dark = !self.store.settings.dark;
                    self.persist();
                }
                if self.frame_count <= 1 {
                    trace("central: topbar done");
                }

                ui.add_space(10.0);

                let group_changed =
                    ui::draw_group_bar(ui, &pal, &self.store.groups, &mut self.group_filter);
                if group_changed {
                    self.cursor = 0;
                }
                if self.frame_count <= 1 {
                    trace("central: groupbar done");
                }

                ui.add_space(10.0);

                // 搜索 / 分组在本帧刚变过才重算：没变就用帧初那份。
                // 排序 + 过滤是 O(n log n)，别每帧白排两遍。
                if top.search_changed || group_changed {
                    self.refresh_visible();
                }
                let visible_now = self.visible.clone();

                if visible_now.is_empty() {
                    let import = ui::draw_empty_state(
                        ui,
                        &pal,
                        !self.search.trim().is_empty() || self.group_filter.is_some(),
                        !self.store.hosts.is_empty(),
                    );
                    if import {
                        self.import_history_with_toast(ctx);
                    }
                    return;
                }

                let view = self.store.settings.view;
                let sort = self.store.settings.sort;

                if view == models::ViewMode::List {
                    // 虚拟化：只画视口里的那几行，几百台主机也不会每帧全排一遍文本。
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show_rows(ui, ui::LIST_ROW_HEIGHT, visible_now.len(), |ui, range| {
                            for pos in range {
                                let host_index = visible_now[pos];
                                let host = &self.store.hosts[host_index];
                                let status = self.status_for(host);
                                let selected = pos == self.cursor;
                                let action = ui::draw_host_row(
                                    ui,
                                    &pal,
                                    host,
                                    pos,
                                    selected,
                                    status,
                                    sort,
                                    selected && self.scroll_to_cursor,
                                );
                                self.handle_row_action(action, &visible_now, ctx);
                            }
                        });
                } else {
                    let gap = 12.0;
                    // ScrollArea 里 available_width() 会算到可视区外，
                    // 用 clip_rect 和当前光标位置算实际可用宽度（同列表行）。
                    let avail = (ui.clip_rect().right() - ui.cursor().min.x - 8.0).max(200.0);
                    let columns = (((avail + gap) / (232.0 + gap)).floor() as usize).max(1);
                    let card_width =
                        ((avail - gap * (columns.saturating_sub(1)) as f32) / columns as f32)
                            .max(180.0);
                    let rows = visible_now.len().div_ceil(columns);
                    ui.spacing_mut().item_spacing = egui::Vec2::new(gap, gap);
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show_rows(ui, ui::CARD_HEIGHT, rows, |ui, range| {
                            for row in range {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing = egui::Vec2::new(gap, gap);
                                    for col in 0..columns {
                                        let pos = row * columns + col;
                                        let Some(&host_index) = visible_now.get(pos) else {
                                            break;
                                        };
                                        let host = &self.store.hosts[host_index];
                                        let status = self.status_for(host);
                                        let selected = pos == self.cursor;
                                        let action = ui::draw_host_card(
                                            ui,
                                            &pal,
                                            host,
                                            pos,
                                            card_width,
                                            selected,
                                            status,
                                            sort,
                                            selected && self.scroll_to_cursor,
                                        );
                                        self.handle_row_action(action, &visible_now, ctx);
                                    }
                                });
                            }
                        });
                }
                if self.frame_count <= 1 {
                    trace("central: scroll done");
                }
            });
        if self.frame_count <= 1 {
            trace("update: central done");
        }

        // 弹窗：先 take 出来再用，避免 &mut self.form 和 &mut self.store 打架
        if self.frame_count <= 1 {
            trace(&format!(
                "update: dialogs start form={} settings={} toasts={}",
                self.form.is_some(),
                self.settings_draft.is_some(),
                self.toasts.len()
            ));
        }
        if self.form.is_some() {
            let mut form = self.form.take().expect("checked is_some");
            let outcome = dialogs::draw_host_dialog(
                ctx,
                &pal,
                &mut form,
                &self.store.groups,
                &self.drives,
                &self.store.hosts,
            );
            match outcome {
                HostDialogOutcome::None => self.form = Some(form),
                HostDialogOutcome::Cancel => {}
                HostDialogOutcome::Save(host) => {
                    let mode = form.mode;
                    self.save_host(host, mode, ctx);
                }
                HostDialogOutcome::Copy { index } => {
                    match self.duplicate_host(index) {
                        Some(name) => self.push_toast(format!("已复制为「{name}」"), false),
                        None => self.push_toast("要复制的主机已经不存在了", true),
                    }
                    ctx.request_repaint();
                }
                HostDialogOutcome::Delete {
                    index,
                    host_id,
                    name,
                    also_credential,
                } => {
                    // index 是 store.hosts 里的位置；host_id 用来二次确认，防止下标已经过期
                    if self.store.hosts.get(index).is_some_and(|h| h.id == host_id) {
                        self.delete_host(index, also_credential);
                    } else if let Some(pos) = self.store.hosts.iter().position(|h| h.id == host_id) {
                        self.delete_host(pos, also_credential);
                    } else {
                        self.push_toast(format!("「{name}」已经不存在了"), true);
                    }
                    ctx.request_repaint();
                }
            }
        }

        if self.settings_draft.is_some() {
            let mut draft = self.settings_draft.take().expect("checked is_some");
            match dialogs::draw_settings_dialog(ctx, &pal, &mut draft) {
                SettingsOutcome::None => self.settings_draft = Some(draft),
                SettingsOutcome::Cancel => {}
                SettingsOutcome::Save => match self.apply_settings(&draft, ctx) {
                    Ok(()) => {}
                    Err(e) => {
                        draft.error = Some(e);
                        self.settings_draft = Some(draft);
                    }
                },
                SettingsOutcome::ImportHistory => {
                    // 设置弹窗留着，导入结果用右下角 Toast 提示
                    self.settings_draft = Some(draft);
                    self.import_history_with_toast(ctx);
                }
            }
        }

        if self.about_open {
            self.start_update_check(false);
            match dialogs::draw_about_dialog(
                ctx,
                &pal,
                env!("CARGO_PKG_VERSION"),
                &self.update_state,
            ) {
                dialogs::AboutOutcome::None => {}
                dialogs::AboutOutcome::Close => self.about_open = false,
                dialogs::AboutOutcome::Recheck => self.start_update_check(true),
            }
        }

        // Toast
        if self.frame_count <= 1 {
            trace("update: toast start");
        }
        self.toasts.retain(|toast| !toast.expired());
        if !self.toasts.is_empty() {
            ui::draw_toasts(ctx, &pal, &self.toasts);
            ctx.request_repaint_after(std::time::Duration::from_millis(80));
        }
        if self.frame_count <= 1 {
            trace("update: toast done");
        }

        // 探活还没收完就保持刷新节奏
        if self.probe_rx.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
        }

        // 最新版检查在后台线程里跑：每 200ms 看一次回包
        if self.update_rx.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }

        // 隐藏到托盘时定时醒来排空托盘事件队列，防止某些环境下 repaint 没唤醒
        if self.hidden_to_tray {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }

        if self.scroll_to_cursor {
            self.scroll_to_cursor = false;
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // 关窗口前再写一次，防止设置弹窗还开着时直接 Alt+F4
        self.tray = None;
        if !self.flush_store(None) {
            if let Some(err) = &self.store_error {
                show_message("RDP 启动器 · 保存失败", err);
            }
        }
        // 再派生一个 helper，等本进程退出后把数据流写进 exe 字节
        store::spawn_embed_helper(&self.exe_path);
    }
}

impl LauncherApp {
    fn status_for(&self, host: &Host) -> ProbeStatus {
        if self.store.settings.probe {
            probe::status_of(&self.statuses, &host.id)
        } else {
            ProbeStatus::Disabled
        }
    }

    fn handle_row_action(
        &mut self,
        action: RowAction,
        visible: &[usize],
        ctx: &egui::Context,
    ) {
        match action {
            RowAction::None => {}
            RowAction::Select(pos) => self.cursor = pos,
            RowAction::Connect(pos) => {
                self.cursor = pos;
                self.connect_selected(ctx);
            }
            RowAction::Edit(pos) => {
                self.cursor = pos;
                // 打开编辑框前重新枚举一次驱动器：插拔 U 盘后也能看到新盘符
                self.refresh_drive_presence(ctx);
                if let Some(&real) = visible.get(pos) {
                    if let Some(host) = self.store.hosts.get(real) {
                        self.form = Some(HostForm::from_host(real, host));
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 入口
// ---------------------------------------------------------------------------

fn main() -> eframe::Result<()> {
    // helper 模式：被主进程在退出时派生出来，只做「等解锁 + 写 exe 尾部」，不开窗口
    if let Some(target) = embed_store_target() {
        store::wait_and_embed(&target);
        if let Ok(current) = std::env::current_exe() {
            let _ = std::fs::remove_file(current);
        }
        return Ok(());
    }

    store::cleanup_old_helpers();
    configure_software_render();
    trace("main: start");

    // 两个实例会各写一份数据流，后保存的覆盖先保存的：直接拦住第二个。
    if !claim_single_instance() {
        show_message(
            "RDP 启动器",
            "已经有一个 RDP 启动器在运行。\n\n同时开两个会用两份配置互相覆盖，请先在托盘里退出其中一个。",
        );
        return Ok(());
    }

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([780.0, 640.0])
        .with_min_inner_size([560.0, 430.0])
        .with_title("RDP 启动器")
        .with_resizable(true);

    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../assets/app-256.png")) {
        viewport = viewport.with_icon(std::sync::Arc::new(icon));
    }
    trace("main: viewport ready");

    // glow + glutin/WGL。有显卡驱动的正常环境直接吃到硬件 OpenGL；
    // 只有 1.1 的环境（纯 RDP 会话）会起不来，把 Mesa 的 opengl32.dll
    // 放在 exe 旁边就能强制走 llvmpipe 软件渲染。
    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        centered: true,
        vsync: true,
        ..Default::default()
    };

    // windows_subsystem="windows" 下 run_native 失败会静默退出，用户只看到「双击没反应」。
    // 这里把失败原因弹出来（最常见的就是没放 Mesa dll 的纯 RDP 会话）。
    match eframe::run_native(
        "RDP 启动器",
        options,
        Box::new(|cc| Ok(Box::new(LauncherApp::new(cc)))),
    ) {
        Ok(()) => Ok(()),
        Err(e) => {
            trace(&format!("main: run_native failed: {e}"));
            show_message(
                "RDP 启动器 · 启动失败",
                &format!(
                    "界面初始化失败：{e}\n\n\
                     如果是在远程桌面会话里（Windows 只提供 OpenGL 1.1），\
                     请把 opengl32.dll 和 libgallium_wgl.dll 放到 exe 旁边再试。"
                ),
            );
            Ok(())
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::DEFAULT_PORT;

    #[test]
    fn exe_path_has_a_file_name() {
        let p = exe_path();
        assert!(p.file_name().is_some());
    }

    #[test]
    fn seed_store_has_two_demo_hosts() {
        let store = StoreData::seed();
        assert_eq!(store.hosts.len(), 2);
        assert_eq!(store.hosts[0].port, 33890);
        assert!(store.groups.iter().any(|g| g.name == "公司"));
    }

    #[test]
    fn default_port_constant_is_3389() {
        assert_eq!(DEFAULT_PORT, 3389);
    }

    #[test]
    fn tray_menu_lists_hosts_directly_then_open_and_quit() {
        let store = StoreData::seed();
        let menu = build_tray_menu(&store);
        let items = menu.items();
        // 2 台主机 + 分隔线 + 打开 + 退出
        assert_eq!(items.len(), 5);
        assert_eq!(items[0].id().0, "host:seed-demo-1");
        assert_eq!(items[1].id().0, "host:seed-demo-2");
        assert_eq!(items[3].id().0, "open");
        assert_eq!(items[4].id().0, "quit");
    }
}
