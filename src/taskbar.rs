//! 关闭窗口 → 收到托盘（最小化 + 去掉任务栏/Alt+Tab 里的痕迹）。
//!
//! 为什么不用 `Visible(false)`：Windows 对隐藏窗口不会处理 `RedrawRequested`，
//! eframe 的 `update()` 就停了，托盘的事件队列永远没人排空，表现就是「点托盘没反应」。
//! 最小化的窗口仍会收到重绘请求，`update()` 继续跑，所以这里用「最小化 + 工具窗口样式」：
//!
//! * `SW_MINIMIZE`：窗口不在屏幕上；
//! * `WS_EX_TOOLWINDOW`：任务栏和 Alt+Tab 都不再列出它（用户看到的就是「界面完全关闭」）；
//! * `ITaskbarList::DeleteTab`：再把任务栏按钮清一次，双保险。
//!
//! 还原时反过来：`SW_RESTORE` + 去掉 `WS_EX_TOOLWINDOW` + `AddTab`。
//!
//! **最小化按钮不再收托盘**（v0.4.2 起）：点最小化就是普通最小化，任务栏按钮留在原处。
//!
//! 窗口句柄不能按标题找（`FindWindowW` 会抓到另一个同名实例），这里按**本进程 pid**
//! 枚举顶层可见窗口，找到后缓存成 `AtomicIsize`。

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Shell::{ITaskbarList, TaskbarList};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowLongPtrW, GetWindowThreadProcessId, IsWindowVisible, SetForegroundWindow,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, GWL_EXSTYLE, SWP_FRAMECHANGED, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SW_MINIMIZE, SW_RESTORE,
};

/// `WS_EX_TOOLWINDOW`：不在任务栏、不在 Alt+Tab 里出现。
const WS_EX_TOOLWINDOW: isize = 0x0000_0080;

static HWND_CACHE: AtomicIsize = AtomicIsize::new(0);

struct FindCtx {
    pid: u32,
    found: Option<HWND>,
}

unsafe extern "system" fn find_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = &mut *(lparam.0 as *mut FindCtx);
    let mut pid = 0u32;
    let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if pid == ctx.pid && IsWindowVisible(hwnd).as_bool() {
        ctx.found = Some(hwnd);
        return BOOL(0); // 找到就停
    }
    BOOL(1) // 继续枚举
}

/// 本进程的主窗口：按 pid 找第一个可见顶层窗口。
fn find_own_window() -> Option<HWND> {
    let cached = HWND_CACHE.load(Ordering::Relaxed);
    if cached != 0 {
        return Some(HWND(cached as *mut core::ffi::c_void));
    }

    let mut ctx = FindCtx {
        pid: unsafe { GetCurrentProcessId() },
        found: None,
    };
    // SAFETY: ctx 在调用期间存活；回调只写 ctx.found。
    let _ = unsafe {
        EnumWindows(
            Some(find_window),
            LPARAM(&mut ctx as *mut FindCtx as isize),
        )
    };
    let hwnd = ctx.found?;
    HWND_CACHE.store(hwnd.0 as isize, Ordering::Relaxed);
    Some(hwnd)
}

thread_local! {
    static COM_READY: Cell<bool> = const { Cell::new(false) };
    static TASKBAR: RefCell<Option<ITaskbarList>> = const { RefCell::new(None) };
}

/// 本进程主窗口句柄：给原生文件选择框当 owner 用，别让它跑到主窗口后面去。
/// 找不到也没关系，对话框退化成无主窗口。
pub fn main_window() -> Option<HWND> {
    find_own_window()
}

fn taskbar() -> Option<ITaskbarList> {
    COM_READY.with(|ready| {
        if !ready.get() {
            // SAFETY: 主线程调用；已初始化时返回 S_FALSE，忽略即可。整个进程只做一次。
            let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
            ready.set(true);
        }
    });

    TASKBAR.with(|slot| {
        if slot.borrow().is_none() {
            // SAFETY: TaskbarList 是系统 CLSID，InProc，无需外部对象。
            let created: windows::core::Result<ITaskbarList> =
                unsafe { CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER) };
            if let Ok(taskbar) = created {
                // SAFETY: 标准 ITaskbarList 初始化。
                unsafe {
                    let _ = taskbar.HrInit();
                }
                *slot.borrow_mut() = Some(taskbar);
            }
        }
        slot.borrow().clone()
    })
}

fn set_tool_window(hwnd: HWND, on: bool) {
    // SAFETY: hwnd 是本进程主窗口。
    unsafe {
        let current = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let next = if on {
            current | WS_EX_TOOLWINDOW
        } else {
            current & !WS_EX_TOOLWINDOW
        };
        if current != next {
            let _ = SetWindowLongPtrW(hwnd, GWL_EXSTYLE, next);
            // 扩展样式改完要让系统重算一次非客户区，Alt+Tab 的列表才会立刻更新。
            let _ = SetWindowPos(
                hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
        }
    }
}

/// 关闭窗口：最小化 + 工具窗口样式（任务栏、Alt+Tab 里都不再出现）。
pub fn hide_window_to_tray() {
    let Some(hwnd) = find_own_window() else {
        return;
    };
    set_tool_window(hwnd, true);
    // SAFETY: hwnd 是本进程主窗口。
    unsafe {
        let _ = ShowWindow(hwnd, SW_MINIMIZE);
    }
    if crate::trace_enabled() {
        crate::trace(&format!("taskbar: hide hwnd={hwnd:?}"));
    }
    if let Some(taskbar) = taskbar() {
        // SAFETY: hwnd 有效；DeleteTab 只影响任务栏按钮。
        let result = unsafe { taskbar.DeleteTab(hwnd) };
        if crate::trace_enabled() {
            crate::trace(&format!("taskbar: delete_tab={:?}", result.is_ok()));
        }
    }
}

/// 从托盘还原窗口：恢复正常窗口样式 + 任务栏按钮，并拿到前台。
pub fn restore_from_tray() {
    let Some(hwnd) = find_own_window() else {
        return;
    };
    // SAFETY: hwnd 是本进程主窗口。
    unsafe {
        let _ = ShowWindow(hwnd, SW_RESTORE);
    }
    set_tool_window(hwnd, false);
    if let Some(taskbar) = taskbar() {
        // SAFETY: hwnd 有效；AddTab 只影响任务栏按钮。
        let result = unsafe { taskbar.AddTab(hwnd) };
        if crate::trace_enabled() {
            crate::trace(&format!("taskbar: add_tab={:?}", result.is_ok()));
        }
    }
    // SAFETY: hwnd 是本进程主窗口。
    unsafe {
        let _ = SetForegroundWindow(hwnd);
    }
}
