//! 任务栏嵌入模式下承载面板画面的子窗口。
//!
//! Windows 11 打开开始菜单、搜索等系统界面时，Explorer 会把任务栏移到更高的 z 序带，
//! 普通置顶窗口无论怎样调整 z 序都只能被盖住。把面板画面放进任务栏（`Shell_TrayWnd`）的
//! 分层子窗口里，它就随任务栏一起移动，始终显示在任务栏上（TrafficMonitor 等也是这样嵌入的）。
//! 主面板窗口在嵌入期间保持隐藏，只作为位置、消息和菜单的宿主。

use ::windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Gdi::{BeginPaint, EndPaint, MapWindowPoints, PAINTSTRUCT},
        System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
        UI::WindowsAndMessaging::*,
    },
    core::{PCWSTR, Result, w},
};

use super::{Window, double_click, message_point, open_menu, register_class, with_app};

pub const CLASS_NAME: &str = "WindowsMonitor.TaskbarHost";
const CLASS: PCWSTR = w!("WindowsMonitor.TaskbarHost");

#[derive(Default)]
pub struct TaskbarHost {
    window: Option<Window>,
    /// 当前是否由子窗口显示面板（贴边槽位已嵌入任务栏）。
    active: bool,
}

impl TaskbarHost {
    /// 把子窗口放进任务栏并移到 `rect`（屏幕坐标）；成功后面板改由子窗口显示。
    pub fn attach(&mut self, owner: HWND, bar: HWND, rect: RECT) -> bool {
        if !self
            .window
            .as_ref()
            .is_some_and(|window| owned_child(window.0, Some(bar)))
        {
            // Explorer 重启后旧任务栏连同子窗口一起被销毁，需要在新任务栏下重建。
            self.window = create(owner, bar).ok().map(Window);
        }
        let Some(window) = &self.window else {
            self.active = false;
            return false;
        };
        let mut corners = [
            POINT {
                x: rect.left,
                y: rect.top,
            },
            POINT {
                x: rect.right,
                y: rect.bottom,
            },
        ];
        unsafe { MapWindowPoints(None, Some(bar), &mut corners) };
        self.active = unsafe {
            SetWindowPos(
                window.0,
                Some(HWND_TOP),
                corners[0].x,
                corners[0].y,
                corners[1].x - corners[0].x,
                corners[1].y - corners[0].y,
                SWP_NOACTIVATE,
            )
        }
        .is_ok();
        self.active
    }

    pub fn detach(&mut self) {
        self.active = false;
        self.set_visible(false);
    }

    /// 正在显示面板的子窗口。
    pub fn active(&self) -> Option<HWND> {
        self.window
            .as_ref()
            .filter(|window| self.active && owned_child(window.0, None))
            .map(|window| window.0)
    }

    pub fn set_visible(&self, visible: bool) {
        let Some(window) = &self.window else { return };
        let visible = visible && self.active;
        if visible != unsafe { IsWindowVisible(window.0).as_bool() } {
            let _ = unsafe { ShowWindow(window.0, if visible { SW_SHOWNA } else { SW_HIDE }) };
        }
        if visible {
            raise(window.0);
        }
    }

    /// Explorer 调整自己的子窗口时可能把 XAML 内容盖到面板上，确保面板在最上层。
    pub fn raise(&self) {
        if let Some(window) = self.active() {
            raise(window);
        }
    }
}

fn raise(window: HWND) {
    if unsafe { GetWindow(window, GW_HWNDPREV) }.is_ok() {
        let _ = unsafe {
            SetWindowPos(
                window,
                Some(HWND_TOP),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            )
        };
    }
}

/// 句柄仍是本线程创建的子窗口（任务栏销毁后句柄可能失效或被复用）。
fn owned_child(window: HWND, parent: Option<HWND>) -> bool {
    unsafe {
        IsWindow(Some(window)).as_bool()
            && GetWindowThreadProcessId(window, None) == GetCurrentThreadId()
            && parent.is_none_or(|parent| GetAncestor(window, GA_PARENT) == parent)
    }
}

fn create(owner: HWND, bar: HWND) -> Result<HWND> {
    unsafe {
        let instance = HINSTANCE(GetModuleHandleW(PCWSTR::null())?.0);
        register_class(
            CLASS,
            instance,
            CS_DBLCLKS,
            Some(host_proc),
            HICON::default(),
            0,
        )?;
        // 分层子窗口（Windows 8+）拥有独立的合成表面，会叠在任务栏的 XAML 内容之上；
        // 普通子窗口画在父窗口的重定向表面上，会被 XAML 内容盖住。
        let window = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_NOACTIVATE,
            CLASS,
            PCWSTR::null(),
            WS_CHILD | WS_CLIPSIBLINGS,
            0,
            0,
            0,
            0,
            Some(bar),
            None,
            Some(instance),
            None,
        )?;
        SetWindowLongPtrW(window, GWLP_USERDATA, owner.0 as isize);
        Ok(window)
    }
}

fn owner(hwnd: HWND) -> HWND {
    HWND(unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut _)
}

unsafe extern "system" fn host_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            let dc = unsafe { BeginPaint(hwnd, &mut paint) };
            with_app(|app| app.paint(hwnd, dc));
            let _ = unsafe { EndPaint(hwnd, &paint) };
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_SIZE => {
            with_app(|app| app.resize_panel(hwnd));
            LRESULT(0)
        }
        WM_NCHITTEST => LRESULT(HTCLIENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        // 不交给 DefWindowProc：子窗口未处理的右键菜单消息会转给任务栏，弹出任务栏自己的菜单。
        WM_CONTEXTMENU => {
            open_menu(owner(hwnd), message_point(lparam));
            LRESULT(0)
        }
        WM_LBUTTONDBLCLK => {
            double_click(owner(hwnd));
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}
