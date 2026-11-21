mod panel;
mod sampler;
mod state;

use std::{
    cell::RefCell,
    sync::{Arc, Mutex, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use ::windows::{
    Win32::{
        Foundation::{
            CloseHandle, D2DERR_RECREATE_TARGET, ERROR_ALREADY_EXISTS, GetLastError, HANDLE,
            HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
        },
        Graphics::Gdi::{
            BeginPaint, CreateRoundRectRgn, DeleteObject, EndPaint, GetMonitorInfoW, HGDIOBJ,
            InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint, PAINTSTRUCT,
            SetWindowRgn, UpdateWindow,
        },
        System::{LibraryLoader::GetModuleHandleW, Threading::CreateMutexW},
        UI::{
            HiDpi::GetDpiForWindow,
            Shell::{
                NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_SETVERSION,
                NIN_SELECT, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
            },
            WindowsAndMessaging::*,
        },
    },
    core::{Error, HSTRING, PCWSTR, Result, w},
};

use crate::model::{Reading, Sample, readings};
use panel::Panel;
use sampler::Sampler;
use state::{Settings, Theme};

const CLASS: PCWSTR = w!("WindowsMonitor.Panel");
const TITLE: PCWSTR = w!("系统监控");
const SAMPLE_READY: u32 = WM_APP + 1;
const TRAY_EVENT: u32 = WM_APP + 2;
const REVEAL_WINDOW: u32 = WM_APP + 3;
const TRAY_ID: u32 = 1;
const ICON_ID: PCWSTR = PCWSTR(101usize as *const u16);
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
const SAVE_INTERVAL: Duration = Duration::from_secs(60);

const TOGGLE: usize = 101;
const TOPMOST: usize = 102;
const FOLLOW_SYSTEM: usize = 103;
const LIGHT: usize = 104;
const DARK: usize = 105;
const RESET_TRAFFIC: usize = 106;
const EXIT: usize = 107;

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

type Feed = Arc<Mutex<Option<Sample>>>;

struct App {
    panel: Panel,
    settings: Settings,
    dark: bool,
    dpi: u32,
    tray: NOTIFYICONDATAW,
    tray_ready: bool,
    taskbar_created: u32,
    feed: Feed,
    sample: Sample,
    displayed: [Reading; 6],
    starting_traffic: u64,
    traffic_at_reset: u64,
    last_save: Instant,
    paint_error: Option<Error>,
}

impl App {
    fn new(settings: Settings, icon: HICON, feed: Feed) -> Result<Self> {
        let mut tray = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            uID: TRAY_ID,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
            uCallbackMessage: TRAY_EVENT,
            hIcon: icon,
            ..Default::default()
        };
        for (dst, src) in tray.szTip.iter_mut().zip("系统监控".encode_utf16()) {
            *dst = src;
        }
        let sample = Sample::default();
        Ok(Self {
            panel: Panel::new()?,
            settings,
            dark: settings.theme.is_dark(),
            dpi: 96,
            tray,
            tray_ready: false,
            taskbar_created: unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) },
            feed,
            sample,
            displayed: readings(sample, settings.traffic),
            starting_traffic: settings.traffic,
            traffic_at_reset: 0,
            last_save: Instant::now(),
            paint_error: None,
        })
    }

    fn take_sample(&mut self) {
        if let Some(sample) = self.feed.lock().unwrap().take() {
            self.sample = sample;
        }
    }

    fn total(&self) -> u64 {
        self.starting_traffic
            .saturating_add(self.sample.traffic.saturating_sub(self.traffic_at_reset))
    }

    fn update_display(&mut self) -> bool {
        let displayed = readings(self.sample, self.total());
        if self.displayed == displayed {
            return false;
        }
        self.displayed = displayed;
        true
    }

    fn save(&mut self) {
        let traffic = self.total();
        let settings = Settings {
            traffic,
            ..self.settings
        };
        if settings.save().is_ok() {
            self.settings.traffic = traffic;
        }
        self.last_save = Instant::now();
    }

    fn add_tray(&mut self, hwnd: HWND) {
        self.tray.hWnd = hwnd;
        self.tray_ready = unsafe { Shell_NotifyIconW(NIM_ADD, &self.tray).as_bool() };
        if self.tray_ready {
            self.tray.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            let _ = unsafe { Shell_NotifyIconW(NIM_SETVERSION, &self.tray) };
        }
    }

    fn paint(&mut self, hwnd: HWND) {
        let mut painted = self.panel.paint(hwnd, self.dpi, self.dark, &self.displayed);
        if painted
            .as_ref()
            .is_err_and(|error| error.code() == D2DERR_RECREATE_TARGET)
        {
            painted = self.panel.paint(hwnd, self.dpi, self.dark, &self.displayed);
        }
        self.paint_error = painted.err();
    }
}

struct SamplerWorker {
    stop: mpsc::Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl SamplerWorker {
    fn start(hwnd: HWND, feed: Feed) -> std::io::Result<Self> {
        let handle = hwnd.0 as usize;
        let (stop, stopped) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("monitor-sampler".into())
            .spawn(move || {
                let hwnd = HWND(handle as *mut _);
                let mut sampler = Sampler::new();
                loop {
                    let next = Instant::now() + SAMPLE_INTERVAL;
                    let sample = sampler.poll();
                    let should_notify = feed.lock().unwrap().replace(sample).is_none();
                    if should_notify
                        && unsafe { PostMessageW(Some(hwnd), SAMPLE_READY, WPARAM(0), LPARAM(0)) }
                            .is_err()
                    {
                        break;
                    }
                    if !matches!(
                        stopped.recv_timeout(next.saturating_duration_since(Instant::now())),
                        Err(mpsc::RecvTimeoutError::Timeout)
                    ) {
                        break;
                    }
                }
            })?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for SamplerWorker {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Instance(HANDLE);
impl Drop for Instance {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

struct Window(HWND);
impl Drop for Window {
    fn drop(&mut self) {
        if unsafe { IsWindow(Some(self.0)) }.as_bool() {
            let _ = unsafe { DestroyWindow(self.0) };
        }
    }
}

pub fn run() -> Result<()> {
    let _instance = unsafe {
        let instance = Instance(CreateMutexW(
            None,
            false,
            w!(r"Local\WindowsMonitor.Panel.v1"),
        )?);
        if GetLastError() == ERROR_ALREADY_EXISTS {
            if let Ok(hwnd) = FindWindowW(CLASS, PCWSTR::null()) {
                let _ = PostMessageW(Some(hwnd), REVEAL_WINDOW, WPARAM(0), LPARAM(0));
            }
            return Ok(());
        }
        instance
    };
    let settings = Settings::load();
    let feed = Feed::default();
    let module = unsafe { HINSTANCE(GetModuleHandleW(PCWSTR::null())?.0) };
    let icon = unsafe { LoadIconW(Some(module), ICON_ID)? };
    let app = App::new(settings, icon, feed.clone())?;
    APP.with(|state| *state.borrow_mut() = Some(app));

    let window = Window(create_window(module, icon, settings.position)?);
    let hwnd = window.0;
    let dpi = window_dpi(hwnd);
    with_app(|app| {
        app.dpi = dpi;
        app.add_tray(hwnd);
    });
    place(hwnd, settings.position, dpi, false);
    set_topmost(hwnd, settings.topmost);
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = UpdateWindow(hwnd);
    }
    if let Some(error) = with_app(|app| app.paint_error.take()).flatten() {
        return Err(error);
    }

    let _sampler = SamplerWorker::start(hwnd, feed).map_err(Error::from)?;
    let mut message = MSG::default();
    loop {
        match unsafe { GetMessageW(&mut message, None, 0, 0) }.0 {
            -1 => return Err(Error::from_thread()),
            0 => return Ok(()),
            _ => unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            },
        }
    }
}

pub fn show_error(error: &Error) {
    let message = HSTRING::from(format!("系统监控启动失败。\n\n{error}"));
    unsafe { MessageBoxW(None, &message, TITLE, MB_OK | MB_ICONERROR) };
}

fn create_window(module: HINSTANCE, icon: HICON, requested: Option<POINT>) -> Result<HWND> {
    let position = requested.unwrap_or_default();
    unsafe {
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW | CS_DROPSHADOW,
            lpfnWndProc: Some(window_proc),
            hInstance: module,
            hIcon: icon,
            hIconSm: icon,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: CLASS,
            ..Default::default()
        };
        if RegisterClassExW(&class) == 0 {
            return Err(Error::from_thread());
        }
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            CLASS,
            TITLE,
            WS_POPUP,
            position.x,
            position.y,
            panel::WIDTH,
            panel::HEIGHT,
            None,
            None,
            Some(module),
            None,
        )
    }
}

fn with_app<T>(run: impl FnOnce(&mut App) -> T) -> Option<T> {
    APP.with(|app| app.try_borrow_mut().ok()?.as_mut().map(run))
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            unsafe { BeginPaint(hwnd, &mut paint) };
            with_app(|app| app.paint(hwnd));
            let _ = unsafe { EndPaint(hwnd, &paint) };
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_NCHITTEST => return LRESULT(HTCAPTION as isize),
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_SIZE => {
            with_app(|app| app.panel.resize(hwnd, app.dpi));
        }
        WM_DPICHANGED => {
            let dpi = (wparam.0 & 0xffff) as u32;
            let suggested = unsafe { &*(lparam.0 as *const RECT) };
            with_app(|app| {
                app.dpi = dpi;
                app.panel.resize(hwnd, dpi);
            });
            let _ = unsafe {
                SetWindowPos(
                    hwnd,
                    None,
                    suggested.left,
                    suggested.top,
                    scale(panel::WIDTH, dpi),
                    scale(panel::HEIGHT, dpi),
                    SWP_NOZORDER | SWP_NOACTIVATE,
                )
            };
            round_window(hwnd, dpi);
        }
        WM_DISPLAYCHANGE | WM_SETTINGCHANGE => {
            if let Some((changed, position)) = with_app(|app| {
                let dark = app.settings.theme.is_dark();
                let changed = dark != app.dark;
                app.dark = dark;
                (changed, app.settings.position)
            }) {
                if changed {
                    invalidate(hwnd);
                }
                if msg == WM_DISPLAYCHANGE || wparam.0 == SPI_SETWORKAREA.0 as usize {
                    place(hwnd, position, window_dpi(hwnd), false);
                }
            }
        }
        WM_EXITSIZEMOVE => {
            let mut rect = RECT::default();
            if unsafe { GetWindowRect(hwnd, &mut rect) }.is_ok() {
                let position = POINT {
                    x: rect.left,
                    y: rect.top,
                };
                let position =
                    place(hwnd, Some(position), window_dpi(hwnd), true).unwrap_or(position);
                with_app(|app| {
                    app.settings.position = Some(position);
                    app.save();
                });
            }
        }
        WM_CONTEXTMENU | WM_NCRBUTTONUP => open_menu(hwnd, message_point(lparam)),
        TRAY_EVENT => match (lparam.0 as u32) & 0xffff {
            WM_CONTEXTMENU => open_menu(hwnd, message_point(LPARAM(wparam.0 as isize))),
            NIN_SELECT | NIN_KEYSELECT => command(hwnd, TOGGLE),
            _ => {}
        },
        WM_COMMAND => command(hwnd, wparam.0 & 0xffff),
        SAMPLE_READY => {
            let changed = with_app(|app| {
                app.take_sample();
                if app.last_save.elapsed() >= SAVE_INTERVAL && app.total() != app.settings.traffic {
                    app.save();
                }
                app.update_display()
            });
            if changed == Some(true) && unsafe { IsWindowVisible(hwnd).as_bool() } {
                invalidate(hwnd);
            }
        }
        REVEAL_WINDOW => {
            let _ = unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
            invalidate(hwnd);
        }
        WM_CLOSE => {
            let _ = unsafe { DestroyWindow(hwnd) };
        }
        WM_QUERYENDSESSION => return LRESULT(1),
        WM_ENDSESSION if wparam.0 != 0 => {
            with_app(|app| {
                app.take_sample();
                app.save();
            });
        }
        WM_DESTROY => {
            with_app(|app| {
                app.take_sample();
                app.save();
                if app.tray_ready {
                    let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &app.tray) };
                }
            });
            unsafe { PostQuitMessage(0) };
        }
        other => {
            if with_app(|app| app.taskbar_created != 0 && app.taskbar_created == other)
                == Some(true)
            {
                let ready = with_app(|app| {
                    app.add_tray(hwnd);
                    app.tray_ready
                });
                if ready == Some(false) {
                    let _ = unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
                    invalidate(hwnd);
                }
            } else {
                return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            }
        }
    }
    LRESULT(0)
}

fn window_dpi(hwnd: HWND) -> u32 {
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi == 0 { 96 } else { dpi }
}

fn scale(dip: i32, dpi: u32) -> i32 {
    (dip * dpi as i32 + 48) / 96
}

fn place(hwnd: HWND, requested: Option<POINT>, dpi: u32, snap: bool) -> Option<POINT> {
    let (width, height) = (scale(panel::WIDTH, dpi), scale(panel::HEIGHT, dpi));
    let reference = requested
        .map(|position| POINT {
            x: position.x.saturating_add(width / 2),
            y: position.y.saturating_add(height / 2),
        })
        .unwrap_or_default();
    let mut monitor = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        GetMonitorInfoW(
            MonitorFromPoint(reference, MONITOR_DEFAULTTONEAREST),
            &mut monitor,
        )
    }
    .ok()
    .ok()?;
    let work = monitor.rcWork;
    let margin = scale(12, dpi);
    let limit = POINT {
        x: (work.right - width).max(work.left),
        y: (work.bottom - height).max(work.top),
    };
    let mut position = requested.unwrap_or(POINT {
        x: limit.x.saturating_sub(margin),
        y: limit.y.saturating_sub(margin),
    });
    position.x = position.x.clamp(work.left, limit.x);
    position.y = position.y.clamp(work.top, limit.y);
    if snap {
        position.x = snap_coordinate(position.x, work.left, limit.x, margin);
        position.y = snap_coordinate(position.y, work.top, limit.y, margin);
    }
    unsafe {
        SetWindowPos(
            hwnd,
            None,
            position.x,
            position.y,
            width,
            height,
            SWP_NOACTIVATE | SWP_NOZORDER,
        )
        .ok()?;
    }
    round_window(hwnd, window_dpi(hwnd));
    Some(position)
}

fn snap_coordinate(value: i32, min: i32, max: i32, margin: i32) -> i32 {
    if max - min < margin * 3 {
        return value;
    }
    if value - min < margin * 2 {
        min + margin
    } else if max - value < margin * 2 {
        max - margin
    } else {
        value
    }
}

fn round_window(hwnd: HWND, dpi: u32) {
    let mut rect = RECT::default();
    unsafe {
        if GetClientRect(hwnd, &mut rect).is_ok() {
            let diameter = scale(panel::RADIUS * 2, dpi);
            let region =
                CreateRoundRectRgn(0, 0, rect.right + 1, rect.bottom + 1, diameter, diameter);
            if !region.is_invalid() && SetWindowRgn(hwnd, Some(region), true) == 0 {
                let _ = DeleteObject(HGDIOBJ(region.0));
            }
        }
    }
}

fn set_topmost(hwnd: HWND, topmost: bool) {
    let order = if topmost {
        HWND_TOPMOST
    } else {
        HWND_NOTOPMOST
    };
    let _ = unsafe {
        SetWindowPos(
            hwnd,
            Some(order),
            0,
            0,
            0,
            0,
            SWP_NOSIZE | SWP_NOMOVE | SWP_NOACTIVATE,
        )
    };
}

fn invalidate(hwnd: HWND) {
    let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
}

fn message_point(value: LPARAM) -> POINT {
    if value.0 == -1 {
        let mut point = POINT::default();
        let _ = unsafe { GetCursorPos(&mut point) };
        return point;
    }
    POINT {
        x: i32::from(value.0 as i16),
        y: i32::from((value.0 >> 16) as i16),
    }
}

struct Menu(HMENU);
impl Drop for Menu {
    fn drop(&mut self) {
        let _ = unsafe { DestroyMenu(self.0) };
    }
}

fn open_menu(hwnd: HWND, at: POINT) {
    let Some((settings, tray_ready)) = with_app(|app| (app.settings, app.tray_ready)) else {
        return;
    };
    let visible = unsafe { IsWindowVisible(hwnd).as_bool() };
    if let Ok(menu) = make_menu(settings, tray_ready, visible) {
        let selection = unsafe {
            let _ = SetForegroundWindow(hwnd);
            let result = TrackPopupMenu(
                menu.0,
                TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY,
                at.x,
                at.y,
                None,
                hwnd,
                None,
            );
            let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
            result.0 as usize
        };
        command(hwnd, selection);
    }
}

fn make_menu(settings: Settings, tray_ready: bool, visible: bool) -> Result<Menu> {
    let menu = Menu(unsafe { CreatePopupMenu()? });
    let checked = |condition| if condition { MF_CHECKED } else { MF_UNCHECKED };
    unsafe {
        let visibility = if visible {
            w!("隐藏浮窗")
        } else {
            w!("显示浮窗")
        };
        let available = if tray_ready { MF_ENABLED } else { MF_GRAYED };
        AppendMenuW(menu.0, MF_STRING | available, TOGGLE, visibility)?;
        AppendMenuW(
            menu.0,
            MF_STRING | checked(settings.topmost),
            TOPMOST,
            w!("窗口置顶"),
        )?;
        AppendMenuW(menu.0, MF_SEPARATOR, 0, PCWSTR::null())?;
        AppendMenuW(
            menu.0,
            MF_STRING | checked(settings.theme == Theme::System),
            FOLLOW_SYSTEM,
            w!("跟随系统"),
        )?;
        AppendMenuW(
            menu.0,
            MF_STRING | checked(settings.theme == Theme::Light),
            LIGHT,
            w!("浅色"),
        )?;
        AppendMenuW(
            menu.0,
            MF_STRING | checked(settings.theme == Theme::Dark),
            DARK,
            w!("深色"),
        )?;
        AppendMenuW(menu.0, MF_SEPARATOR, 0, PCWSTR::null())?;
        AppendMenuW(menu.0, MF_STRING, RESET_TRAFFIC, w!("清零总流量"))?;
        AppendMenuW(menu.0, MF_STRING, EXIT, w!("退出"))?;
    }
    Ok(menu)
}

fn command(hwnd: HWND, selection: usize) {
    match selection {
        TOGGLE => {
            if with_app(|app| app.tray_ready) == Some(true) {
                let visible = unsafe { IsWindowVisible(hwnd).as_bool() };
                let _ =
                    unsafe { ShowWindow(hwnd, if visible { SW_HIDE } else { SW_SHOWNOACTIVATE }) };
                if !visible {
                    invalidate(hwnd);
                }
            }
        }
        TOPMOST => {
            if let Some(topmost) = with_app(|app| {
                app.settings.topmost = !app.settings.topmost;
                app.save();
                app.settings.topmost
            }) {
                set_topmost(hwnd, topmost);
            }
        }
        FOLLOW_SYSTEM | LIGHT | DARK => {
            with_app(|app| {
                app.settings.theme = match selection {
                    LIGHT => Theme::Light,
                    DARK => Theme::Dark,
                    _ => Theme::System,
                };
                app.dark = app.settings.theme.is_dark();
                app.save();
            });
            invalidate(hwnd);
        }
        RESET_TRAFFIC => {
            with_app(|app| {
                app.take_sample();
                app.starting_traffic = 0;
                app.traffic_at_reset = app.sample.traffic;
                app.update_display();
                app.save();
            });
            invalidate(hwnd);
        }
        EXIT => {
            let _ = unsafe { DestroyWindow(hwnd) };
        }
        _ => {}
    }
}
