mod about;
mod commands;
mod edge;
mod editor;
mod history_export;
mod manager;
mod menu;
mod panel;
mod panel_input;
mod sampler;
mod startup;
mod state;
mod taskbar;
mod ui;
mod updater;
mod worker;

use std::{
    cell::RefCell,
    io,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use ::windows::{
    Win32::{
        Foundation::{
            CloseHandle, D2DERR_RECREATE_TARGET, ERROR_ALREADY_EXISTS, ERROR_CANCELLED,
            GetLastError, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
        },
        Globalization::SetThreadUILanguage,
        Graphics::{
            Dwm::{
                DWMNCRP_DISABLED, DWMNCRP_USEWINDOWSTYLE, DWMWA_BORDER_COLOR, DWMWA_COLOR_DEFAULT,
                DWMWA_COLOR_NONE, DWMWA_NCRENDERING_POLICY, DwmSetWindowAttribute,
            },
            Gdi::{
                BeginPaint, CreateRoundRectRgn, DeleteObject, EndPaint, GetMonitorInfoW, HDC,
                HGDIOBJ, InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTONULL,
                MONITORINFO, MonitorFromPoint, MonitorFromWindow, PAINTSTRUCT, SetWindowRgn,
                UpdateWindow,
            },
        },
        System::{
            Com::{
                COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
            },
            LibraryLoader::GetModuleHandleW,
            Threading::{
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CloseThreadpoolWait, CreateMutexW,
                CreateThreadpoolWait, CreateWaitableTimerExW, PTP_CALLBACK_INSTANCE, PTP_WAIT,
                SetThreadpoolWait, SetWaitableTimer, TIMER_ALL_ACCESS,
                WaitForThreadpoolWaitCallbacks,
            },
        },
        UI::{
            Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent},
            Shell::{
                NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_SETVERSION,
                NIN_SELECT, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, SEE_MASK_FLAG_NO_UI,
                SHELLEXECUTEINFOW, Shell_NotifyIconW, ShellExecuteExW,
            },
            WindowsAndMessaging::*,
        },
    },
    core::{Error, HSTRING, PCWSTR, Result, w},
};

use crate::{
    history,
    language::Language,
    model::{Metric, Reading, Sample, readings},
};
use commands::{
    command, double_click, double_click_labels, finish_history_export, finish_update, set_display,
    shell_open, show_update_error,
};
use menu::open_menu;
use panel::{Appearance, Layout, LayoutSize, Panel};
use panel_input::PanelInput;
use sampler::Sampler;
use state::{
    DisplayOptions, DoubleClick, LanguagePreference, Refresh, Settings, TaskbarSide, TextSize,
    Theme,
};
use taskbar::{
    DockPlacement, Observation, Placement, Taskbar, covered_by_taskbar, is_taskbar_window,
    is_tray_overflow, primary_taskbar, taskbar_has_focus, taskbar_menu_open, taskbar_topmost,
};
use ui::{Window, control, position, register_class, scale, send, window_dpi};
use worker::{Feed, SamplerWorker, WorkerCommand, notify};

const CLASS: PCWSTR = w!("WindowsMonitor.Panel");
const PRODUCT_NAME: &str = env!("WINDOWS_MONITOR_PRODUCT_NAME");
const GITHUB_REPOSITORY: &str = "Fracizz/windows-monitor-x";
const SAMPLE_READY: u32 = WM_APP + 1;
const TRAY_EVENT: u32 = WM_APP + 2;
const REVEAL_WINDOW: u32 = WM_APP + 3;
const DOCK_READY: u32 = WM_APP + 4;
const UPDATE_READY: u32 = WM_APP + 5;
const DOCK_Z_ORDER_CHECK: u32 = WM_APP + 6;
const HISTORY_STATUS: u32 = WM_APP + 7;
const HISTORY_EXPORTED: u32 = WM_APP + 8;
const TRAY_ID: u32 = 1;
const ICON_ID: PCWSTR = PCWSTR(101usize as *const u16);
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;
const DOCK_INTERVAL: Duration = Duration::from_secs(1);
const DOCK_Z_ORDER_TIMER: usize = 1;
const PAINT_RETRY_TIMER: usize = 2;
/// 绘制失败（如显示设备暂时不可用）后等待多久重绘；渲染目标已在失败时丢弃，重绘会重新创建。
const PAINT_RETRY: u32 = 1_000;
const DOCK_Z_ORDER_SETTLE: Duration = Duration::from_millis(120);
/// 前台切换时任务栏未置顶：等待 Explorer 恢复任务栏置顶期间持续盯住 z 序的时长。
const DOCK_TOPMOST_SETTLE: Duration = Duration::from_millis(800);
const SAVE_INTERVAL: Duration = Duration::from_secs(60);

const TOGGLE: usize = 101;
const TOPMOST: usize = 102;
const FOLLOW_SYSTEM: usize = 103;
const LIGHT: usize = 104;
const DARK: usize = 105;
const RESET_TRAFFIC: usize = 106;
const EXIT: usize = 107;
const DOCK: usize = 108;
const AUTOSTART: usize = 113;
const DOCK_LEFT: usize = 114;
const DOCK_RIGHT: usize = 115;
const EDIT_METRICS: usize = 116;
const CHECK_UPDATE: usize = 117;
const TRANSPARENT_THEME: usize = 118;
const LABEL_BOLD: usize = 119;
const ABOUT: usize = 120;
const LANGUAGE_SYSTEM: usize = 121;
const LANGUAGE_CHINESE: usize = 122;
const LANGUAGE_ENGLISH: usize = 123;
const EXPORT_HISTORY: usize = 124;
const OPEN_MANAGER: usize = 125;
const RECORD_HISTORY: usize = 126;
const DOUBLE_CLICK_OPTIONS: [(usize, DoubleClick); 4] = [
    (127, DoubleClick::Nothing),
    (128, DoubleClick::TaskManager),
    (129, DoubleClick::ResourceMonitor),
    (130, DoubleClick::Manager),
];
const REFRESH_OPTIONS: [(usize, Refresh); 4] = [
    (109, Refresh::One),
    (110, Refresh::Two),
    (111, Refresh::Three),
    (112, Refresh::Five),
];

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

struct App {
    panel: Option<Panel>,
    panel_input: PanelInput,
    settings: Settings,
    dark: bool,
    dpi: u32,
    tray: NOTIFYICONDATAW,
    tray_ready: bool,
    taskbar_created: u32,
    feed: Feed,
    worker: mpsc::Sender<WorkerCommand>,
    shown: bool,
    dock_generation: u64,
    placement: Option<Placement>,
    dock_z_order_until: Option<Instant>,
    dock_timer: Option<DockTimer>,
    sample: Sample,
    waiting_sample: bool,
    displayed: [Reading; Metric::ALL.len()],
    display: DisplayOptions,
    starting_traffic: u64,
    traffic_at_reset: u64,
    last_save: Instant,
    save_pending: bool,
    paint_error: Option<Error>,
    update: Option<mpsc::Receiver<updater::Event>>,
    history_exporting: bool,
    /// 进入系统移动循环时的窗口矩形；单击或双击浮窗也会进入该循环，用它判断是否真的移动过。
    move_origin: Option<RECT>,
    /// 浮窗贴边收起状态。
    edge_dock: Option<edge::EdgeDock>,
    /// 右键菜单打开期间不收起贴边浮窗。
    menu_open: bool,
}

impl App {
    fn new(
        settings: Settings,
        icon: HICON,
        feed: Feed,
        worker: mpsc::Sender<WorkerCommand>,
    ) -> Self {
        let mut tray = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            uID: TRAY_ID,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
            uCallbackMessage: TRAY_EVENT,
            hIcon: icon,
            ..Default::default()
        };
        for (dst, src) in tray.szTip[..127]
            .iter_mut()
            .zip(PRODUCT_NAME.encode_utf16())
        {
            *dst = src;
        }
        let sample = Sample::default();
        Self {
            panel: None,
            panel_input: PanelInput::default(),
            settings,
            dark: settings.theme.is_dark(false),
            dpi: 96,
            tray,
            tray_ready: false,
            taskbar_created: unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) },
            feed,
            worker,
            shown: true,
            dock_generation: 0,
            placement: (!settings.taskbar).then_some(Placement::Floating),
            dock_z_order_until: None,
            dock_timer: None,
            sample,
            waiting_sample: true,
            displayed: readings(sample, settings.traffic, settings.display.language),
            display: settings.display,
            starting_traffic: settings.traffic,
            traffic_at_reset: 0,
            last_save: Instant::now(),
            save_pending: false,
            paint_error: None,
            update: None,
            history_exporting: false,
            move_origin: None,
            edge_dock: None,
            menu_open: false,
        }
    }

    fn take_sample(&mut self) {
        if let Some(sample) = self.feed.lock().unwrap().sample.take() {
            self.sample = sample;
        }
    }

    fn layout(&self) -> Layout {
        if matches!(self.placement, Some(Placement::Docked(_))) {
            Layout::Taskbar
        } else {
            Layout::Floating
        }
    }

    fn panel_topmost(&self) -> bool {
        self.settings.topmost
            || self.placement != Some(Placement::Floating)
            || self.edge_dock.is_some()
    }

    fn visible_dock_rect(&self) -> Option<RECT> {
        match self.placement {
            Some(Placement::Docked(rect)) if self.shown => Some(rect),
            _ => None,
        }
    }

    /// 在 `settle` 时间内反复检查面板是否被任务栏盖住；已在检查中则只延长截止时间。
    fn settle_dock_z_order(&mut self, hwnd: HWND, settle: Duration) {
        let until = Instant::now() + settle;
        match self.dock_z_order_until {
            Some(current) => self.dock_z_order_until = Some(current.max(until)),
            None => {
                self.dock_z_order_until = Some(until);
                self.schedule_dock_z_order_check(hwnd);
            }
        }
    }

    fn schedule_dock_z_order_check(&mut self, hwnd: HWND) {
        if self.dock_timer.as_ref().is_some_and(DockTimer::arm) {
            return;
        }
        self.dock_timer = None;
        if unsafe {
            SetCoalescableTimer(
                Some(hwnd),
                DOCK_Z_ORDER_TIMER,
                USER_TIMER_MINIMUM,
                None,
                TIMERV_NO_COALESCING,
            )
        } == 0
        {
            self.dock_z_order_until = None;
        }
    }

    fn request_dock(&mut self) {
        self.dock_generation = self.dock_generation.wrapping_add(1);
        let generation = self.settings.taskbar.then_some(self.dock_generation);
        let _ = self.worker.send(WorkerCommand::Dock(
            generation,
            self.settings.taskbar_side,
            Layout::Taskbar.size(self.settings.display),
        ));
    }

    fn refresh_theme(&mut self) -> bool {
        let dark = self
            .settings
            .theme
            .is_dark(self.layout() == Layout::Taskbar);
        let changed = dark != self.dark;
        self.dark = dark;
        changed
    }

    fn total(&self) -> u64 {
        self.starting_traffic
            .saturating_add(self.sample.traffic.saturating_sub(self.traffic_at_reset))
    }

    fn update_display(&mut self) -> bool {
        let displayed = readings(self.sample, self.total(), self.display.language);
        let changed = self
            .display
            .metrics
            .iter()
            .any(|metric| self.displayed[metric.index()] != displayed[metric.index()]);
        self.displayed = displayed;
        changed
    }

    fn save(&mut self) {
        let traffic = self.total();
        let settings = Settings {
            traffic,
            ..self.settings
        };
        self.save_pending = settings.save().is_err();
        if !self.save_pending {
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

    fn resize_panel(&mut self, hwnd: HWND) {
        if let Some(panel) = &mut self.panel {
            panel.resize(hwnd, self.dpi);
        }
    }

    fn paint(&mut self, hwnd: HWND, dc: HDC) {
        if self.waiting_sample {
            panel::paint_starting(hwnd, dc, self.dpi, self.dark, self.display);
            return;
        }

        let initializing = self.panel.is_none();
        let result = self.paint_metrics(hwnd);
        if initializing {
            self.paint_error = result.err();
        } else if result.is_err() {
            // 本次绘制区域已被 EndPaint 视为有效，读数不变时不会再重绘，因此稍后主动重试。
            unsafe { SetTimer(Some(hwnd), PAINT_RETRY_TIMER, PAINT_RETRY, None) };
        }
    }

    fn paint_metrics(&mut self, hwnd: HWND) -> Result<()> {
        let appearance = if self.settings.theme == Theme::Transparent {
            Appearance::Transparent { dark: self.dark }
        } else {
            Appearance::Solid { dark: self.dark }
        };
        if self.panel.is_none() {
            let mut panel = Panel::new(self.display.language)?;
            panel.set_layout(self.layout(), self.display);
            self.panel = Some(panel);
        }
        let panel = self.panel.as_mut().expect("panel initialized");
        panel.set_label_style(self.display.language, self.settings.label_bold)?;
        let capsule = self
            .edge_dock
            .filter(|dock| dock.hidden())
            .map(|dock| dock.vertical());
        let cpu = &self.displayed[Metric::Cpu.index()];
        let paint = |panel: &mut Panel| {
            if let Some(vertical) = capsule {
                panel.paint_capsule(hwnd, self.dpi, self.dark, vertical, cpu)
            } else {
                panel.paint(hwnd, self.dpi, appearance, &self.displayed)
            }
        };
        let mut painted = paint(panel);
        if painted
            .as_ref()
            .is_err_and(|error| error.code() == D2DERR_RECREATE_TARGET)
        {
            painted = paint(panel);
        }
        painted
    }
}

struct Instance(HANDLE);
impl Drop for Instance {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

struct DockTimer {
    timer: Instance,
    wait: PTP_WAIT,
}

impl DockTimer {
    fn new(hwnd: HWND) -> Result<Self> {
        unsafe {
            let timer = Instance(CreateWaitableTimerExW(
                None,
                PCWSTR::null(),
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS.0,
            )?);
            let wait = CreateThreadpoolWait(Some(dock_timer_fired), Some(hwnd.0), None)?;
            Ok(Self { timer, wait })
        }
    }

    fn arm(&self) -> bool {
        // Check again in 2 ms. Relative due times use negative 100 ns units.
        let due = -20_000;
        unsafe {
            SetThreadpoolWait(self.wait, Some(self.timer.0), None);
            SetWaitableTimer(self.timer.0, &due, 0, None, None, false).is_ok()
        }
    }
}

impl Drop for DockTimer {
    fn drop(&mut self) {
        unsafe {
            SetThreadpoolWait(self.wait, None, None);
            WaitForThreadpoolWaitCallbacks(self.wait, true);
            CloseThreadpoolWait(self.wait);
        }
    }
}

unsafe extern "system" fn dock_timer_fired(
    _: PTP_CALLBACK_INSTANCE,
    hwnd: *mut std::ffi::c_void,
    _: PTP_WAIT,
    _: u32,
) {
    let _ = notify(HWND(hwnd), DOCK_Z_ORDER_CHECK);
}

struct TaskbarEvents([HWINEVENTHOOK; 2]);

impl TaskbarEvents {
    fn watch() -> Self {
        Self(
            [
                (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MENUPOPUPEND),
                (EVENT_OBJECT_SHOW, EVENT_OBJECT_HIDE),
            ]
            .map(|(first, last)| unsafe {
                SetWinEventHook(
                    first,
                    last,
                    None,
                    Some(taskbar_event),
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
                )
            }),
        )
    }
}

impl Drop for TaskbarEvents {
    fn drop(&mut self) {
        for hook in self.0 {
            if !hook.is_invalid() {
                let _ = unsafe { UnhookWinEvent(hook) };
            }
        }
    }
}

unsafe extern "system" fn taskbar_event(
    _: HWINEVENTHOOK,
    event: u32,
    window: HWND,
    object: i32,
    child: i32,
    thread: u32,
    _: u32,
) {
    let visibility_event = matches!(event, EVENT_OBJECT_SHOW | EVENT_OBJECT_HIDE);
    if visibility_event && (object != OBJID_WINDOW.0 || child != CHILDID_SELF as i32) {
        return;
    }
    let Some(bar) = primary_taskbar() else {
        return;
    };
    if event == EVENT_SYSTEM_FOREGROUND {
        // 截图覆盖层、全屏应用盖住任务栏时 Explorer 会撤掉任务栏的置顶，前台切走后约几十毫秒
        // 再把任务栏提到最上层，盖住面板；轮询要最长 1 秒才发现，看起来像面板消失了一下。
        let restoring = !taskbar_topmost(bar);
        with_app(|app| {
            // 全屏应用退出或切走后立即重新探测，不必等下一次轮询才恢复面板。
            if app.placement == Some(Placement::Hidden) {
                let _ = app.worker.send(WorkerCommand::RecheckDock);
            }
            if restoring && app.visible_dock_rect().is_some() {
                let panel = app.tray.hWnd;
                app.settle_dock_z_order(panel, DOCK_TOPMOST_SETTLE);
            }
        });
    }
    let is_interaction = if visibility_event {
        is_tray_overflow(bar, window)
    } else {
        is_taskbar_window(bar, window)
            || (event != EVENT_SYSTEM_FOREGROUND
                && thread != 0
                && thread == unsafe { GetWindowThreadProcessId(bar, None) })
    };
    if !is_interaction {
        return;
    }
    let Some((panel, rect)) = with_app(|app| {
        if !app.settings.taskbar {
            return None;
        }
        let _ = app.worker.send(WorkerCommand::TaskbarInteraction);
        let rect = app.visible_dock_rect()?;
        let panel = app.tray.hWnd;
        // Explorer can raise the taskbar just after sending the foreground event.
        app.settle_dock_z_order(panel, DOCK_Z_ORDER_SETTLE);
        Some((panel, rect))
    })
    .flatten() else {
        return;
    };
    keep_dock_above_taskbar(bar, panel, rect);
}

fn check_dock_z_order(hwnd: HWND) {
    let Some(rect) = with_app(|app| {
        let rect = app.visible_dock_rect();
        let settling = rect.is_some()
            && app
                .dock_z_order_until
                .is_some_and(|until| Instant::now() < until);
        if settling {
            if app.dock_timer.is_some() {
                app.schedule_dock_z_order_check(hwnd);
            }
        } else {
            app.dock_z_order_until = None;
            if app.dock_timer.is_none() {
                let _ = unsafe { KillTimer(Some(hwnd), DOCK_Z_ORDER_TIMER) };
            }
        }
        rect
    }) else {
        return;
    };
    if let Some(rect) = rect
        && let Some(bar) = primary_taskbar()
    {
        keep_dock_above_taskbar(bar, hwnd, rect);
    }
}

fn keep_dock_above_taskbar(bar: HWND, hwnd: HWND, rect: RECT) {
    if covered_by_taskbar(bar, hwnd, rect) {
        set_topmost(hwnd, true);
    }
}

pub fn run() -> Result<()> {
    let settings = Settings::load();
    set_thread_language(settings.display.language);
    if let Some(result) = updater::handle_startup_args(settings.display.language) {
        if let Err(error) = result {
            show_update_error(None, &error);
        }
        return Ok(());
    }
    let background = std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == startup::BACKGROUND_ARG);
    let _instance = unsafe {
        let instance = Instance(CreateMutexW(
            None,
            false,
            w!(r"Local\WindowsMonitor.Panel.v1"),
        )?);
        if GetLastError() == ERROR_ALREADY_EXISTS {
            if !background && let Ok(hwnd) = FindWindowW(CLASS, PCWSTR::null()) {
                let mut process = 0;
                GetWindowThreadProcessId(hwnd, Some(&mut process));
                if process != 0 {
                    let _ = AllowSetForegroundWindow(process);
                }
                let _ = PostMessageW(Some(hwnd), REVEAL_WINDOW, WPARAM(0), LPARAM(0));
            }
            return Ok(());
        }
        instance
    };
    let feed = Feed::default();
    let module = unsafe { HINSTANCE(GetModuleHandleW(PCWSTR::null())?.0) };
    let icon = unsafe { LoadIconW(Some(module), ICON_ID)? };
    let worker = mpsc::channel();
    let app = App::new(settings, icon, feed.clone(), worker.0.clone());
    APP.with(|state| *state.borrow_mut() = Some(app));

    let window = Window(create_window(CLASS, module, icon, settings)?);
    let hwnd = window.0;
    let _ = show_starting_panel(hwnd);

    let _sampler = SamplerWorker::start(hwnd, feed, settings, worker).map_err(Error::from)?;
    with_app(|app| {
        app.add_tray(hwnd);
        app.dock_timer = DockTimer::new(hwnd).ok();
    });
    let _taskbar_events = TaskbarEvents::watch();
    let _ = startup::migrate();
    if !background {
        command(hwnd, OPEN_MANAGER);
    }
    let mut message = MSG::default();
    loop {
        match unsafe { GetMessageW(&mut message, None, 0, 0) }.0 {
            -1 => return Err(Error::from_thread()),
            0 => return Ok(()),
            _ => {
                if editor::handle_dialog_message(&message)
                    || manager::handle_dialog_message(&message)
                {
                    continue;
                }
                unsafe {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
                if let Some(error) = with_app(|app| app.paint_error.take()).flatten() {
                    return Err(error);
                }
            }
        }
    }
}

fn app_title() -> &'static HSTRING {
    static TITLE: OnceLock<HSTRING> = OnceLock::new();
    TITLE.get_or_init(|| HSTRING::from(PRODUCT_NAME))
}

fn ui_language() -> Language {
    with_app(|app| app.settings.display.language)
        .unwrap_or_else(|| Settings::load().display.language)
}

fn set_thread_language(language: Language) {
    unsafe { SetThreadUILanguage(language.choose(0x0804, 0x0409)) };
}

pub fn show_error(error: &Error) {
    let heading = ui_language().choose("系统监控启动失败。", "Windows Monitor could not start.");
    let message = HSTRING::from(format!("{heading}\n\n{error}"));
    unsafe { MessageBoxW(None, &message, app_title(), MB_OK | MB_ICONERROR) };
}

fn show_starting_panel(hwnd: HWND) -> bool {
    let dpi = window_dpi(hwnd);
    with_app(|app| app.dpi = dpi);
    apply_placement(hwnd);
    unsafe { UpdateWindow(hwnd).as_bool() }
}

fn create_window(
    class_name: PCWSTR,
    module: HINSTANCE,
    icon: HICON,
    settings: Settings,
) -> Result<HWND> {
    let position = settings.position.unwrap_or_default();
    unsafe {
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
            lpfnWndProc: Some(window_proc),
            hInstance: module,
            hIcon: icon,
            hIconSm: icon,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: class_name,
            ..Default::default()
        };
        if RegisterClassExW(&class) == 0 {
            return Err(Error::from_thread());
        }
        let (width, height) = Layout::Floating.size(settings.display).pixels(96);
        // Windows can ignore a background process's request to promote an existing window.
        let mut ex_style = WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
        if settings.taskbar || settings.topmost {
            ex_style |= WS_EX_TOPMOST;
        }
        CreateWindowExW(
            ex_style,
            class_name,
            app_title(),
            WS_POPUP,
            position.x,
            position.y,
            width,
            height,
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

fn panel_hit_test() -> u32 {
    if with_app(|app| app.waiting_sample || app.layout() == Layout::Taskbar) == Some(true) {
        HTCLIENT
    } else {
        HTCAPTION
    }
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
            let dc = unsafe { BeginPaint(hwnd, &mut paint) };
            with_app(|app| app.paint(hwnd, dc));
            let _ = unsafe { EndPaint(hwnd, &paint) };
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_NCHITTEST => return LRESULT(panel_hit_test() as isize),
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_SIZE => {
            with_app(|app| app.resize_panel(hwnd));
        }
        WM_WINDOWPOSCHANGED => {
            sync_panel_input(hwnd);
            return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
        }
        WM_DPICHANGED => {
            let dpi = (wparam.0 & 0xffff) as u32;
            let suggested = unsafe { &*(lparam.0 as *const RECT) };
            let (layout, display, waiting_sample) = with_app(|app| {
                app.dpi = dpi;
                app.resize_panel(hwnd);
                (app.layout(), app.display, app.waiting_sample)
            })
            .unwrap_or_default();
            if waiting_sample {
                center_starting_panel(hwnd, dpi, display.text_size);
                return LRESULT(0);
            }
            // 收起成胶囊时直接采用系统建议的矩形，保持胶囊尺寸。
            if with_app(|app| app.edge_dock.is_some_and(|dock| dock.hidden())) == Some(true) {
                let _ = unsafe {
                    SetWindowPos(
                        hwnd,
                        None,
                        suggested.left,
                        suggested.top,
                        suggested.right - suggested.left,
                        suggested.bottom - suggested.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    )
                };
                return LRESULT(0);
            }
            let (width, height) = layout.size(display).pixels(dpi);
            let _ = unsafe {
                SetWindowPos(
                    hwnd,
                    None,
                    suggested.left,
                    suggested.top,
                    width,
                    height,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                )
            };
            shape_panel(hwnd, dpi, layout.radius(display.text_size));
        }
        WM_DISPLAYCHANGE | WM_SETTINGCHANGE | WM_THEMECHANGED => {
            if with_app(|app| app.refresh_theme()) == Some(true) {
                invalidate(hwnd);
            }
            editor::refresh_preview();
            if msg == WM_DISPLAYCHANGE || wparam.0 == SPI_SETWORKAREA.0 as usize {
                recheck_placement(hwnd);
            }
        }
        WM_ENTERSIZEMOVE => {
            let mut rect = RECT::default();
            let origin = unsafe { GetWindowRect(hwnd, &mut rect) }
                .is_ok()
                .then_some(rect);
            with_app(|app| app.move_origin = origin);
        }
        WM_EXITSIZEMOVE => {
            let Some((layout, origin)) = with_app(|app| (app.layout(), app.move_origin.take()))
            else {
                return LRESULT(0);
            };
            if layout == Layout::Taskbar {
                return LRESULT(0);
            }
            let mut rect = RECT::default();
            // 只是单击或双击而没有拖动时，不重新吸附也不写设置文件。
            if unsafe { GetWindowRect(hwnd, &mut rect) }.is_ok() && origin != Some(rect) {
                // 拖到可收起的屏幕边缘附近（原吸附范围内）时贴边收起，否则按普通浮窗吸附。
                let threshold = scale(24, window_dpi(hwnd));
                if let Some(position) =
                    edge::dragged_edge(rect, threshold).and_then(|edge| edge::dock(hwnd, edge))
                {
                    with_app(|app| {
                        app.settings.position = Some(position);
                        app.save();
                    });
                    return LRESULT(0);
                }
                edge::undock(hwnd);
                set_topmost(hwnd, with_app(|app| app.panel_topmost()).unwrap_or(false));
                let position = POINT {
                    x: rect.left,
                    y: rect.top,
                };
                let display = with_app(|app| app.display).unwrap_or_default();
                let position = place(hwnd, Some(position), window_dpi(hwnd), display, true)
                    .unwrap_or(position);
                with_app(|app| {
                    app.settings.position = Some(position);
                    app.save();
                });
            }
        }
        WM_CONTEXTMENU | WM_NCRBUTTONUP => open_menu(hwnd, message_point(lparam)),
        // 浮窗命中 HTCAPTION（非客户区双击），任务栏模式命中 HTCLIENT（需 CS_DBLCLKS）。
        WM_NCLBUTTONDBLCLK | WM_LBUTTONDBLCLK => double_click(hwnd),
        DOCK_Z_ORDER_CHECK => check_dock_z_order(hwnd),
        WM_TIMER if wparam.0 == DOCK_Z_ORDER_TIMER => check_dock_z_order(hwnd),
        WM_TIMER if wparam.0 == edge::EDGE_TIMER => edge::tick(hwnd),
        WM_TIMER if wparam.0 == PAINT_RETRY_TIMER => {
            let _ = unsafe { KillTimer(Some(hwnd), PAINT_RETRY_TIMER) };
            invalidate(hwnd);
        }
        TRAY_EVENT => match (lparam.0 as u32) & 0xffff {
            WM_CONTEXTMENU => open_menu(hwnd, message_point(LPARAM(wparam.0 as isize))),
            NIN_SELECT | NIN_KEYSELECT => command(hwnd, TOGGLE),
            _ => {}
        },
        WM_COMMAND => command(hwnd, wparam.0 & 0xffff),
        SAMPLE_READY => {
            let Some((first_sample, changed)) = with_app(|app| {
                app.take_sample();
                let first_sample = std::mem::replace(&mut app.waiting_sample, false);
                if app.save_pending
                    || (app.last_save.elapsed() >= SAVE_INTERVAL
                        && app.total() != app.settings.traffic)
                {
                    app.save();
                }
                (first_sample, app.update_display())
            }) else {
                return LRESULT(0);
            };
            if first_sample {
                apply_placement(hwnd);
            } else if changed && unsafe { IsWindowVisible(hwnd).as_bool() } {
                invalidate(hwnd);
            }
            editor::refresh_preview();
            manager::refresh_readings();
        }
        DOCK_READY => {
            let applied = with_app(|app| {
                let latest = app.feed.lock().unwrap().dock.take();
                if let Some((generation, placement)) = latest
                    && app.settings.taskbar
                    && app.dock_generation == generation
                {
                    app.placement = Some(placement);
                    return true;
                }
                false
            });
            if applied == Some(true) {
                apply_placement(hwnd);
            }
        }
        UPDATE_READY => finish_update(hwnd),
        HISTORY_STATUS => manager::refresh_settings(),
        HISTORY_EXPORTED => finish_history_export(hwnd),
        REVEAL_WINDOW => {
            with_app(|app| app.shown = true);
            apply_visibility(hwnd);
            command(hwnd, OPEN_MANAGER);
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
            manager::close();
            with_app(|app| {
                app.dock_timer = None;
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
                    if !app.tray_ready {
                        app.shown = true;
                    }
                    app.tray_ready
                });
                if ready.is_some() {
                    recheck_placement(hwnd);
                    apply_visibility(hwnd);
                    manager::refresh_settings();
                }
            } else {
                return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            }
        }
    }
    LRESULT(0)
}

fn apply_placement(hwnd: HWND) {
    let Some((placement, position, topmost, layout, display, waiting_sample)) = with_app(|app| {
        let layout = app.layout();
        let language_changed = app.display.language != app.settings.display.language;
        app.display = app.settings.display;
        if language_changed {
            app.update_display();
        }
        if let Some(panel) = &mut app.panel {
            panel.set_layout(layout, app.display);
        }
        app.refresh_theme();
        (
            app.placement.unwrap_or_default(),
            app.settings.position,
            app.panel_topmost(),
            layout,
            app.display,
            app.waiting_sample,
        )
    }) else {
        return;
    };
    // 读数就绪后始终用分层窗口按像素 alpha 呈现：实色主题的圆角也能抗锯齿，
    // 不再依赖 SetWindowRgn 的硬裁剪（会产生边缘毛刺）。启动阶段仍由 GDI 绘制，保留窗口区域。
    set_panel_layered(hwnd, !waiting_sample);
    match (waiting_sample, placement) {
        (true, _) => {
            edge::undock(hwnd);
            center_starting_panel(hwnd, window_dpi(hwnd), display.text_size);
            set_topmost(hwnd, topmost);
        }
        (false, Placement::Floating) => {
            place(hwnd, position, window_dpi(hwnd), display, false);
            edge::restore(hwnd);
            set_topmost(
                hwnd,
                topmost || with_app(|app| app.edge_dock.is_some()) == Some(true),
            );
        }
        (false, Placement::Docked(rect)) => {
            edge::undock(hwnd);
            let _ = unsafe {
                SetWindowPos(
                    hwnd,
                    Some(HWND_TOPMOST),
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    SWP_NOACTIVATE,
                )
            };
            shape_panel(hwnd, window_dpi(hwnd), layout.radius(display.text_size));
        }
        (false, Placement::Hidden) => edge::undock(hwnd),
    }
    apply_visibility(hwnd);
}

fn set_panel_layered(hwnd: HWND, enabled: bool) {
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let layered = WS_EX_LAYERED.0 as isize;
        let updated = if enabled {
            style | layered
        } else {
            style & !layered
        };
        if updated != style {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, updated);
            set_panel_frame(hwnd, !enabled);
            if enabled {
                SetWindowRgn(hwnd, None, true);
            }
        }
    }
}

fn set_panel_frame(hwnd: HWND, enabled: bool) {
    let (rendering, border) = if enabled {
        (DWMNCRP_USEWINDOWSTYLE, DWMWA_COLOR_DEFAULT)
    } else {
        (DWMNCRP_DISABLED, DWMWA_COLOR_NONE)
    };
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_NCRENDERING_POLICY,
            (&raw const rendering).cast(),
            size_of_val(&rendering) as u32,
        );
        // Suppressing the DWM border color is supported on Windows 11.
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            (&raw const border).cast(),
            size_of_val(&border) as u32,
        );
    }
}

fn apply_visibility(hwnd: HWND) {
    let shown = with_app(|app| app.shown && app.placement != Some(Placement::Hidden)) == Some(true);
    if shown != unsafe { IsWindowVisible(hwnd).as_bool() } {
        let _ = unsafe { ShowWindow(hwnd, if shown { SW_SHOWNOACTIVATE } else { SW_HIDE }) };
    }
    sync_panel_input(hwnd);
    if shown {
        invalidate(hwnd);
    }
    manager::refresh_status();
}

fn sync_panel_input(hwnd: HWND) {
    with_app(|app| {
        let style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
        app.panel_input
            .sync(hwnd, style & WS_EX_LAYERED.0 as isize != 0);
    });
}

fn recheck_placement(hwnd: HWND) {
    let dock = with_app(|app| {
        if app.settings.taskbar {
            let _ = app.worker.send(WorkerCommand::RecheckDock);
        }
        app.settings.taskbar
    });
    if dock == Some(false) {
        apply_placement(hwnd);
    }
}

fn center_starting_panel(hwnd: HWND, dpi: u32, text_size: TextSize) {
    let mut monitor = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !unsafe {
        GetMonitorInfoW(
            MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST),
            &mut monitor,
        )
    }
    .as_bool()
    {
        return;
    }
    let bounds = monitor.rcMonitor;
    let width = scale(text_size.scale(184), dpi).min(bounds.right - bounds.left);
    let height = scale(text_size.scale(62), dpi).min(bounds.bottom - bounds.top);
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            None,
            bounds.left + (bounds.right - bounds.left - width) / 2,
            bounds.top + (bounds.bottom - bounds.top - height) / 2,
            width,
            height,
            SWP_NOACTIVATE | SWP_NOZORDER,
        );
    }
    round_window(hwnd, dpi, Layout::Floating.radius(text_size));
}

fn place(
    hwnd: HWND,
    requested: Option<POINT>,
    dpi: u32,
    display: DisplayOptions,
    snap: bool,
) -> Option<POINT> {
    let (width, height) = Layout::Floating.size(display).pixels(dpi);
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
    shape_panel(
        hwnd,
        window_dpi(hwnd),
        Layout::Floating.radius(display.text_size),
    );
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

fn shape_panel(hwnd: HWND, dpi: u32, radius: i32) {
    if unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } & WS_EX_LAYERED.0 as isize == 0 {
        round_window(hwnd, dpi, radius);
    }
}

fn round_window(hwnd: HWND, dpi: u32, radius: i32) {
    let mut rect = RECT::default();
    unsafe {
        if GetClientRect(hwnd, &mut rect).is_ok() {
            let diameter = scale(radius * 2, dpi);
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

fn metric_name(metric: Metric, language: Language) -> &'static str {
    match metric {
        Metric::Upload => language.choose("上传速度", "Upload Speed"),
        Metric::Download => language.choose("下载速度", "Download Speed"),
        Metric::Cpu => language.choose("CPU 使用率", "CPU Usage"),
        Metric::Memory => language.choose("内存使用率", "Memory Usage"),
        Metric::Disk => language.choose("磁盘活动率", "Disk Activity"),
        Metric::Traffic => language.choose("累计流量", "Total Traffic"),
        Metric::AvailableMemory => language.choose("可用内存（GB）", "Available Memory (GB)"),
        Metric::AvailableMemoryPercent => language.choose("可用内存百分比", "Available Memory (%)"),
        Metric::UsedMemory => language.choose("已用内存（GB）", "Used Memory (GB)"),
        Metric::DiskRead => language.choose("磁盘读取速度", "Disk Read Speed"),
        Metric::DiskWrite => language.choose("磁盘写入速度", "Disk Write Speed"),
        Metric::GpuMemoryPercent => language.choose("显存使用率", "VRAM Usage (%)"),
        Metric::UsedGpuMemory => language.choose("已用显存（GB）", "Used VRAM (GB)"),
        Metric::CpuFrequency => language.choose("CPU 当前频率", "CPU Frequency"),
        Metric::Uptime => language.choose("运行时长", "Uptime"),
        Metric::Battery => language.choose("电池电量", "Battery Level"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::windows::Win32::Graphics::Gdi::GetWindowRgnBox;

    #[test]
    fn shows_feedback_before_sampling_with_saved_taskbar_mode() {
        let mut monitor = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        unsafe {
            GetMonitorInfoW(
                MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTONEAREST),
                &mut monitor,
            )
            .ok()
            .unwrap();
        }
        let position = POINT {
            x: monitor.rcWork.left + 24,
            y: monitor.rcWork.top + 24,
        };
        let settings = Settings {
            taskbar: true,
            topmost: false,
            theme: Theme::Transparent,
            position: Some(position),
            ..Default::default()
        };
        let feed = Feed::default();
        let (sender, _commands) = mpsc::channel();
        let module = unsafe { HINSTANCE(GetModuleHandleW(PCWSTR::null()).unwrap().0) };
        let icon = unsafe { LoadIconW(None, IDI_APPLICATION).unwrap() };
        let started = Instant::now();
        APP.with(|state| {
            *state.borrow_mut() = Some(App::new(settings, icon, feed.clone(), sender))
        });
        let window = Window(
            create_window(w!("WindowsMonitor.StartupTest"), module, icon, settings).unwrap(),
        );
        let created_topmost =
            unsafe { GetWindowLongPtrW(window.0, GWL_EXSTYLE) } & WS_EX_TOPMOST.0 as isize;
        let initial_shadow =
            unsafe { GetClassLongPtrW(window.0, GCL_STYLE) } & CS_DROPSHADOW.0 as usize;
        let painted = show_starting_panel(window.0);
        let visible = unsafe { IsWindowVisible(window.0).as_bool() };
        let status_visible = with_app(|app| app.waiting_sample && app.panel.is_none());
        let elapsed = started.elapsed();
        let mut starting_rect = RECT::default();
        unsafe { GetWindowRect(window.0, &mut starting_rect).unwrap() };
        let mut region_bounds = RECT::default();
        let startup_region = unsafe { GetWindowRgnBox(window.0, &mut region_bounds) };

        feed.lock().unwrap().sample = Some(Sample {
            memory: Some(crate::model::PhysicalMemory {
                total: 100,
                available: 50,
            }),
            ..Default::default()
        });
        unsafe {
            window_proc(window.0, SAMPLE_READY, WPARAM(0), LPARAM(0));
            let _ = UpdateWindow(window.0);
        }
        let metrics_visible =
            with_app(|app| !app.waiting_sample && app.panel.is_some() && app.paint_error.is_none());
        let layered =
            unsafe { GetWindowLongPtrW(window.0, GWL_EXSTYLE) } & WS_EX_LAYERED.0 as isize;
        let pending_topmost =
            unsafe { GetWindowLongPtrW(window.0, GWL_EXSTYLE) } & WS_EX_TOPMOST.0 as isize;
        let mut panel_rect = RECT::default();
        unsafe { GetWindowRect(window.0, &mut panel_rect).unwrap() };
        let transparent_region = unsafe { GetWindowRgnBox(window.0, &mut region_bounds) };

        // This test owns a separate window and leaves the user's settings file alone.
        APP.with(|state| state.borrow_mut().take());
        assert!(painted && visible);
        assert_eq!(status_visible, Some(true));
        assert_eq!(metrics_visible, Some(true));
        assert_ne!(layered, 0);
        assert_ne!(startup_region.0, 0);
        assert_eq!(transparent_region.0, 0);
        assert_ne!(created_topmost, 0);
        assert_eq!(initial_shadow, 0);
        assert_ne!(pending_topmost, 0);
        let screen = monitor.rcMonitor;
        assert!((starting_rect.left + starting_rect.right - screen.left - screen.right).abs() <= 1);
        assert!((starting_rect.top + starting_rect.bottom - screen.top - screen.bottom).abs() <= 1);
        assert_eq!((panel_rect.left, panel_rect.top), (position.x, position.y));
        eprintln!(
            "Initial window painted in {} ms before sampling",
            elapsed.as_millis()
        );
    }
}
