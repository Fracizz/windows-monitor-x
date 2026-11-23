//! 各窗口共用的 Win32 辅助函数：控件创建、DPI 换算、对话框字体与窗口居中。

use ::windows::{
    Win32::{
        Foundation::{ERROR_CLASS_ALREADY_EXISTS, HINSTANCE, HWND, LPARAM, RECT, WPARAM},
        Graphics::Gdi::{
            CreateFontIndirectW, DEFAULT_GUI_FONT, DeleteObject, GetDC, GetMonitorInfoW,
            GetStockObject, GetTextMetricsW, HBRUSH, HFONT, HGDIOBJ, MONITOR_DEFAULTTONEAREST,
            MONITORINFO, MonitorFromWindow, ReleaseDC, SelectObject, TEXTMETRICW,
        },
        UI::{
            Controls::{
                ICC_LISTVIEW_CLASSES, ICC_STANDARD_CLASSES, INITCOMMONCONTROLSEX,
                InitCommonControlsEx,
            },
            HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow, SystemParametersInfoForDpi},
            WindowsAndMessaging::*,
        },
    },
    core::{Error, PCWSTR, Result},
};

pub struct Window(pub HWND);

impl Drop for Window {
    fn drop(&mut self) {
        if unsafe { IsWindow(Some(self.0)) }.as_bool() {
            let _ = unsafe { DestroyWindow(self.0) };
        }
    }
}

pub fn register_class(
    class: PCWSTR,
    instance: HINSTANCE,
    style: WNDCLASS_STYLES,
    proc: WNDPROC,
    icon: HICON,
    background: isize,
) -> Result<()> {
    unsafe {
        let definition = WNDCLASSW {
            style,
            lpfnWndProc: proc,
            hInstance: instance,
            hIcon: icon,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hbrBackground: HBRUSH(background as *mut _),
            lpszClassName: class,
            ..Default::default()
        };
        if RegisterClassW(&definition) == 0 {
            let error = Error::from_thread();
            if error.code() != ERROR_CLASS_ALREADY_EXISTS.to_hresult() {
                return Err(error);
            }
        }
    }
    Ok(())
}

pub fn control(parent: HWND, class: PCWSTR, label: PCWSTR, style: u32, id: usize) -> Result<HWND> {
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            label,
            WS_CHILD | WS_VISIBLE | WINDOW_STYLE(style),
            0,
            0,
            0,
            0,
            Some(parent),
            Some(HMENU(id as *mut _)),
            None,
            None,
        )
    }
}

pub fn position(hwnd: HWND, x: i32, y: i32, width: i32, height: i32) {
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            None,
            x,
            y,
            width,
            height,
            SWP_NOACTIVATE | SWP_NOZORDER,
        );
    }
}

pub fn send(hwnd: HWND, message: u32, wparam: usize, lparam: isize) -> isize {
    unsafe { SendMessageW(hwnd, message, Some(WPARAM(wparam)), Some(LPARAM(lparam))).0 }
}

pub fn window_dpi(hwnd: HWND) -> u32 {
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi == 0 { 96 } else { dpi }
}

pub fn scale(dip: i32, dpi: u32) -> i32 {
    (dip * dpi as i32 + 48) / 96
}

/// 列表视图与标准控件所需的公共控件初始化；重复调用无副作用。
pub fn init_common_controls() -> Result<()> {
    unsafe {
        InitCommonControlsEx(&INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_LISTVIEW_CLASSES | ICC_STANDARD_CLASSES,
        })
        .ok()
    }
}

/// 客户区为 `width`×`height` DIP 时的窗口外框尺寸（像素）。
pub fn window_size(
    width: i32,
    height: i32,
    dpi: u32,
    style: WINDOW_STYLE,
    ex_style: WINDOW_EX_STYLE,
) -> Result<(i32, i32)> {
    let mut rect = RECT {
        right: scale(width, dpi),
        bottom: scale(height, dpi),
        ..Default::default()
    };
    unsafe { AdjustWindowRectExForDpi(&mut rect, style, false, ex_style, dpi)? };
    Ok((rect.right - rect.left, rect.bottom - rect.top))
}

/// `anchor` 所在显示器的工作区。
pub fn work_area(anchor: HWND) -> Option<RECT> {
    let mut monitor = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        GetMonitorInfoW(
            MonitorFromWindow(anchor, MONITOR_DEFAULTTONEAREST),
            &mut monitor,
        )
    }
    .as_bool()
    .then_some(monitor.rcWork)
}

/// 窗口的位置与外框尺寸（像素）。
#[derive(Clone, Copy)]
pub struct Bounds {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// 在 `anchor` 所在显示器的工作区内居中放置客户区为 `width`×`height` DIP 的窗口。
pub fn centered_bounds(
    anchor: HWND,
    width: i32,
    height: i32,
    style: WINDOW_STYLE,
    ex_style: WINDOW_EX_STYLE,
) -> Result<Bounds> {
    let work = work_area(anchor).ok_or_else(Error::from_thread)?;
    let (width, height) = window_size(width, height, window_dpi(anchor), style, ex_style)?;
    let width = width.min(work.right - work.left);
    let height = height.min(work.bottom - work.top);
    Ok(Bounds {
        x: work.left + (work.right - work.left - width) / 2,
        y: work.top + (work.bottom - work.top - height) / 2,
        width,
        height,
    })
}

/// 处理 `WM_DPICHANGED`：移动到系统建议的位置与大小。
pub fn follow_dpi_change(hwnd: HWND, lparam: LPARAM) {
    let suggested = unsafe { &*(lparam.0 as *const RECT) };
    position(
        hwnd,
        suggested.left,
        suggested.top,
        suggested.right - suggested.left,
        suggested.bottom - suggested.top,
    );
}

/// 按显示器 DPI 创建的系统消息字体（随“使文本更大”设置变化），用于对话框控件。
pub struct Fonts {
    pub dpi: u32,
    body: HFONT,
    /// 正文字体的行高（像素）。
    pub line_height: i32,
}

impl Fonts {
    pub fn new(dpi: u32) -> Self {
        let mut metrics = NONCLIENTMETRICSW {
            cbSize: size_of::<NONCLIENTMETRICSW>() as u32,
            ..Default::default()
        };
        let mut fonts = Self {
            dpi,
            body: HFONT::default(),
            line_height: scale(16, dpi),
        };
        unsafe {
            if SystemParametersInfoForDpi(
                SPI_GETNONCLIENTMETRICS.0,
                metrics.cbSize,
                Some((&raw mut metrics).cast()),
                0,
                dpi,
            )
            .is_ok()
            {
                fonts.body = CreateFontIndirectW(&metrics.lfMessageFont);
            }
        }
        if let Some(height) = line_height(fonts.body) {
            fonts.line_height = height;
        }
        fonts
    }

    /// 把正文字体应用到控件；字体创建失败时退回系统默认字体。
    pub fn apply_body(&self, controls: &[HWND]) {
        let font = if self.body.is_invalid() {
            unsafe { GetStockObject(DEFAULT_GUI_FONT).0 }
        } else {
            self.body.0
        };
        for &control in controls {
            send(control, WM_SETFONT, font as usize, 1);
        }
    }
}

fn line_height(font: HFONT) -> Option<i32> {
    if font.is_invalid() {
        return None;
    }
    unsafe {
        let dc = GetDC(None);
        if dc.is_invalid() {
            return None;
        }
        let previous = SelectObject(dc, HGDIOBJ(font.0));
        let mut metrics = TEXTMETRICW::default();
        let measured = GetTextMetricsW(dc, &mut metrics).as_bool();
        SelectObject(dc, previous);
        ReleaseDC(None, dc);
        measured.then_some(metrics.tmHeight + metrics.tmExternalLeading)
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        if !self.body.is_invalid() {
            let _ = unsafe { DeleteObject(HGDIOBJ(self.body.0)) };
        }
    }
}
