use ::windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::*,
    },
    core::{PCWSTR, Result, w},
};

use super::{Window, message_point, open_menu, panel_hit_test, register_class, set_panel_frame};

const CLASS: PCWSTR = w!("WindowsMonitor.PanelInput");

#[derive(Default)]
pub struct PanelInput(Option<Window>);

impl PanelInput {
    pub fn sync(&mut self, owner: HWND, transparent: bool) {
        if !transparent || !unsafe { IsWindowVisible(owner).as_bool() } {
            if let Some(window) = &self.0
                && unsafe { IsWindowVisible(window.0).as_bool() }
            {
                let _ = unsafe { ShowWindow(window.0, SW_HIDE) };
            }
            return;
        }
        let mut rect = RECT::default();
        if unsafe { GetWindowRect(owner, &mut rect) }.is_err() {
            return;
        }
        if self.0.is_none() {
            self.0 = create(owner, rect).ok().map(Window);
        }
        let Some(window) = &self.0 else { return };
        let mut current = RECT::default();
        if unsafe {
            IsWindowVisible(window.0).as_bool() && GetWindowRect(window.0, &mut current).is_ok()
        } && current == rect
        {
            return;
        }
        let _ = unsafe {
            SetWindowPos(
                window.0,
                None,
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
                SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_NOZORDER | SWP_SHOWWINDOW,
            )
        };
    }
}

fn create(owner: HWND, rect: RECT) -> Result<HWND> {
    unsafe {
        let instance = HINSTANCE(GetModuleHandleW(PCWSTR::null())?.0);
        // 浮窗双击由非客户区消息处理；任务栏模式命中客户区，需要 CS_DBLCLKS 才会产生双击消息。
        register_class(
            CLASS,
            instance,
            CS_DBLCLKS,
            Some(input_proc),
            HICON::default(),
            0,
        )?;
        // The owned window handles input without contributing any pixels to the desktop.
        let window = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP,
            CLASS,
            PCWSTR::null(),
            WS_POPUP,
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
            Some(owner),
            None,
            Some(instance),
            None,
        )?;
        set_panel_frame(window, false);
        Ok(window)
    }
}

unsafe extern "system" fn input_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_NCHITTEST => LRESULT(panel_hit_test() as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_CONTEXTMENU | WM_NCRBUTTONUP => {
            if let Ok(owner) = unsafe { GetWindow(hwnd, GW_OWNER) } {
                open_menu(owner, message_point(lparam));
            }
            LRESULT(0)
        }
        WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK | WM_LBUTTONDBLCLK => {
            if let Ok(owner) = unsafe { GetWindow(hwnd, GW_OWNER) } {
                unsafe { SendMessageW(owner, message, Some(wparam), Some(lparam)) }
            } else {
                LRESULT(0)
            }
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}
