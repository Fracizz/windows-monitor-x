mod about;
mod editor;
mod panel;
mod panel_input;
mod sampler;
mod startup;
mod state;
mod taskbar;
mod updater;

use std::{
    cell::RefCell,
    sync::{Arc, Mutex, OnceLock, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use ::windows::{
    Win32::{
        Foundation::{
            CloseHandle, D2DERR_RECREATE_TARGET, ERROR_ALREADY_EXISTS, GetLastError, HANDLE,
            HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
        },
        Globalization::SetThreadUILanguage,
        Graphics::{
            Dwm::{
                DWMNCRP_DISABLED, DWMNCRP_USEWINDOWSTYLE, DWMWA_BORDER_COLOR, DWMWA_COLOR_DEFAULT,
                DWMWA_COLOR_NONE, DWMWA_NCRENDERING_POLICY, DwmSetWindowAttribute,
            },
            Gdi::{
                BeginPaint, CreateRoundRectRgn, DeleteObject, EndPaint, GetMonitorInfoW, HDC,
                HGDIOBJ, InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
                MonitorFromWindow, PAINTSTRUCT, SetWindowRgn, UpdateWindow,
            },
        },
        System::{
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

use crate::{
    language::Language,
    model::{Metric, Reading, Sample, readings},
};
use panel::{Appearance, Layout, Panel};
use panel_input::PanelInput;
use sampler::Sampler;
use state::{DisplayOptions, LanguagePreference, Refresh, Settings, TaskbarSide, TextSize, Theme};
use taskbar::{
    DockPlacement, Observation, Placement, Taskbar, covered_by_taskbar, is_taskbar_window,
    is_tray_overflow, primary_taskbar, taskbar_has_focus, taskbar_menu_open,
};

const CLASS: PCWSTR = w!("WindowsMonitor.Panel");
const PRODUCT_NAME: &str = env!("WINDOWS_MONITOR_PRODUCT_NAME");
const GITHUB_REPOSITORY: &str = "Fracizz/windows-monitor-x";
const SAMPLE_READY: u32 = WM_APP + 1;
const TRAY_EVENT: u32 = WM_APP + 2;
const REVEAL_WINDOW: u32 = WM_APP + 3;
const DOCK_READY: u32 = WM_APP + 4;
const UPDATE_READY: u32 = WM_APP + 5;
const DOCK_Z_ORDER_CHECK: u32 = WM_APP + 6;
const TRAY_ID: u32 = 1;
const ICON_ID: PCWSTR = PCWSTR(101usize as *const u16);
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;
const DOCK_INTERVAL: Duration = Duration::from_secs(1);
const DOCK_Z_ORDER_TIMER: usize = 1;
const DOCK_Z_ORDER_SETTLE: Duration = Duration::from_millis(120);
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
const REFRESH_OPTIONS: [(usize, Refresh); 4] = [
    (109, Refresh::One),
    (110, Refresh::Two),
    (111, Refresh::Three),
    (112, Refresh::Five),
];

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

#[derive(Default)]
struct Mailbox {
    sample: Option<Sample>,
    dock: Option<(u64, Placement)>,
}

type Feed = Arc<Mutex<Mailbox>>;

enum WorkerCommand {
    Interval(Refresh),
    Dock(Option<u64>, TaskbarSide, (i32, i32)),
    RecheckDock,
    TaskbarInteraction,
    Stop,
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
        self.settings.topmost || self.placement != Some(Placement::Floating)
    }

    fn visible_dock_rect(&self) -> Option<RECT> {
        match self.placement {
            Some(Placement::Docked(rect)) if self.shown => Some(rect),
            _ => None,
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
        let mut painted = panel.paint(hwnd, self.dpi, appearance, &self.displayed);
        if painted
            .as_ref()
            .is_err_and(|error| error.code() == D2DERR_RECREATE_TARGET)
        {
            painted = panel.paint(hwnd, self.dpi, appearance, &self.displayed);
        }
        painted
    }
}

struct SamplerWorker {
    control: mpsc::Sender<WorkerCommand>,
    thread: Option<JoinHandle<()>>,
}

impl SamplerWorker {
    fn start(
        hwnd: HWND,
        feed: Feed,
        settings: Settings,
        (control, commands): (mpsc::Sender<WorkerCommand>, mpsc::Receiver<WorkerCommand>),
    ) -> std::io::Result<Self> {
        let handle = hwnd.0 as usize;
        let thread = thread::Builder::new()
            .name("monitor-sampler".into())
            .spawn(move || {
                let hwnd = HWND(handle as *mut _);
                let mut sampler = Sampler::new();
                let mut last_sample = Instant::now();
                let mut next_sample = last_sample;
                let mut interval = settings.refresh.interval();
                let mut dock = settings.taskbar.then_some((0, next_sample));
                let mut taskbar_side = settings.taskbar_side;
                let mut dock_size = Layout::Taskbar.size(settings.display);
                let mut taskbar = None;
                let mut dock_placement = DockPlacement::default();
                let mut last_placement = None;
                loop {
                    let now = Instant::now();
                    if now >= next_sample {
                        last_sample = now;
                        next_sample = now + interval;
                        let sample = sampler.poll(interval);
                        let pending = feed.lock().unwrap().sample.replace(sample).is_some();
                        if !pending && !notify(hwnd, SAMPLE_READY) {
                            break;
                        }
                    }
                    if let Some((generation, deadline)) = dock
                        && now >= deadline
                    {
                        let bar = primary_taskbar();
                        let observed = if bar.is_some_and(taskbar_menu_open) {
                            Some(Observation::MenuOpen)
                        } else {
                            if taskbar.is_none() {
                                taskbar = Taskbar::new().ok();
                            }
                            bar.and_then(|bar| {
                                taskbar.as_mut()?.observe(
                                    bar,
                                    hwnd,
                                    taskbar_side,
                                    dock_size,
                                    dock_placement.preferred_x(),
                                )
                            })
                        };
                        let placement = match observed {
                            Some(Observation::MenuOpen) => dock_placement.hold(),
                            // The XAML menu temporarily replaces the taskbar's automation tree.
                            None if bar.is_some_and(taskbar_has_focus) => dock_placement.hold(),
                            Some(Observation::Placement(placement)) => {
                                dock_placement.resolve(Some(placement), Instant::now())
                            }
                            Some(Observation::NoRoom) | None => {
                                dock_placement.resolve(None, Instant::now())
                            }
                        };
                        if let Some(placement) = placement
                            && (last_placement != Some(placement)
                                || matches!(placement, Placement::Docked(rect)
                                    if bar.is_some_and(|bar| covered_by_taskbar(bar, hwnd, rect))))
                        {
                            last_placement = Some(placement);
                            let pending = feed
                                .lock()
                                .unwrap()
                                .dock
                                .replace((generation, placement))
                                .is_some();
                            if !pending && !notify(hwnd, DOCK_READY) {
                                break;
                            }
                        }
                        dock = Some((generation, Instant::now() + DOCK_INTERVAL));
                    }
                    let wake = dock.map_or(next_sample, |(_, deadline)| deadline.min(next_sample));
                    match commands.recv_timeout(wake.saturating_duration_since(Instant::now())) {
                        Ok(WorkerCommand::Interval(refresh)) => {
                            interval = refresh.interval();
                            next_sample = last_sample + interval;
                        }
                        Ok(WorkerCommand::Dock(generation, side, size)) => {
                            taskbar_side = side;
                            dock_size = size;
                            dock = generation.map(|id| (id, Instant::now()));
                            if dock.is_none() {
                                taskbar = None;
                                dock_placement = DockPlacement::default();
                            } else {
                                dock_placement.reset_preference();
                            }
                            last_placement = None;
                        }
                        Ok(WorkerCommand::RecheckDock) => {
                            dock = dock.map(|(id, _)| (id, Instant::now()));
                        }
                        Ok(WorkerCommand::TaskbarInteraction) => {
                            // Keep the current slot while Explorer rearranges its popup controls.
                            dock_placement.hold();
                            dock = dock.map(|(id, _)| (id, Instant::now() + DOCK_INTERVAL));
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        _ => break,
                    }
                }
            })?;
        Ok(Self {
            control,
            thread: Some(thread),
        })
    }
}

fn notify(hwnd: HWND, message: u32) -> bool {
    unsafe { PostMessageW(Some(hwnd), message, WPARAM(0), LPARAM(0)) }.is_ok()
}

impl Drop for SamplerWorker {
    fn drop(&mut self) {
        let _ = self.control.send(WorkerCommand::Stop);
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
        if app
            .dock_z_order_until
            .replace(Instant::now() + DOCK_Z_ORDER_SETTLE)
            .is_none()
        {
            app.schedule_dock_z_order_check(panel);
        }
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
    let mut message = MSG::default();
    loop {
        match unsafe { GetMessageW(&mut message, None, 0, 0) }.0 {
            -1 => return Err(Error::from_thread()),
            0 => return Ok(()),
            _ => {
                if editor::handle_dialog_message(&message) {
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
            style: CS_HREDRAW | CS_VREDRAW,
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
        let (width, height) = Layout::Floating.size(settings.display);
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
            let (width, height) = layout.size(display);
            let _ = unsafe {
                SetWindowPos(
                    hwnd,
                    None,
                    suggested.left,
                    suggested.top,
                    scale(width, dpi),
                    scale(height, dpi),
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
        WM_EXITSIZEMOVE => {
            if with_app(|app| app.layout()) == Some(Layout::Taskbar) {
                return LRESULT(0);
            }
            let mut rect = RECT::default();
            if unsafe { GetWindowRect(hwnd, &mut rect) }.is_ok() {
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
        DOCK_Z_ORDER_CHECK => check_dock_z_order(hwnd),
        WM_TIMER if wparam.0 == DOCK_Z_ORDER_TIMER => check_dock_z_order(hwnd),
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
        REVEAL_WINDOW => {
            with_app(|app| app.shown = true);
            apply_visibility(hwnd);
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
                }
            } else {
                return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            }
        }
    }
    LRESULT(0)
}

fn apply_placement(hwnd: HWND) {
    let Some((placement, position, topmost, layout, display, waiting_sample, transparent)) =
        with_app(|app| {
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
                !app.waiting_sample && app.settings.theme == Theme::Transparent,
            )
        })
    else {
        return;
    };
    set_panel_layered(hwnd, transparent);
    match (waiting_sample, placement) {
        (true, _) => {
            center_starting_panel(hwnd, window_dpi(hwnd), display.text_size);
            set_topmost(hwnd, topmost);
        }
        (false, Placement::Floating) => {
            place(hwnd, position, window_dpi(hwnd), display, false);
            set_topmost(hwnd, topmost);
        }
        (false, Placement::Docked(rect)) => {
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
        (false, Placement::Hidden) => {}
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

fn window_dpi(hwnd: HWND) -> u32 {
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi == 0 { 96 } else { dpi }
}

fn scale(dip: i32, dpi: u32) -> i32 {
    (dip * dpi as i32 + 48) / 96
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
    let height = scale(text_size.scale(68), dpi).min(bounds.bottom - bounds.top);
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
    let (width, height) = Layout::Floating.size(display);
    let (width, height) = (scale(width, dpi), scale(height, dpi));
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

struct Menu(HMENU);

impl Menu {
    fn new() -> Result<Self> {
        Ok(Self(unsafe { CreatePopupMenu()? }))
    }

    fn item(&self, id: usize, label: PCWSTR, flags: MENU_ITEM_FLAGS) -> Result<()> {
        unsafe { AppendMenuW(self.0, MF_STRING | flags, id, label) }
    }

    fn separator(&self) -> Result<()> {
        unsafe { AppendMenuW(self.0, MF_SEPARATOR, 0, PCWSTR::null()) }
    }

    fn submenu(&self, label: PCWSTR, child: Self) -> Result<()> {
        let handle = child.0;
        unsafe { AppendMenuW(self.0, MF_POPUP | MF_STRING, handle.0 as usize, label)? };
        // The parent menu now owns the submenu.
        std::mem::forget(child);
        Ok(())
    }
}

impl Drop for Menu {
    fn drop(&mut self) {
        let _ = unsafe { DestroyMenu(self.0) };
    }
}

fn open_menu(hwnd: HWND, at: POINT) {
    let Some((settings, tray_ready, shown, placement, update_busy)) = with_app(|app| {
        (
            app.settings,
            app.tray_ready,
            app.shown,
            app.placement.unwrap_or_default(),
            app.update.is_some(),
        )
    }) else {
        return;
    };
    if let Ok(menu) = make_menu(
        settings,
        tray_ready,
        shown,
        placement,
        startup::status().ok(),
        update_busy,
    ) {
        let menu_host = (settings.theme == Theme::Transparent)
            .then(|| create_menu_host().ok())
            .flatten();
        let owner = menu_host.as_ref().map_or(hwnd, |window| window.0);
        let selection = unsafe {
            let _ = SetForegroundWindow(owner);
            let result = TrackPopupMenu(
                menu.0,
                TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY,
                at.x,
                at.y,
                None,
                owner,
                None,
            );
            let _ = PostMessageW(Some(owner), WM_NULL, WPARAM(0), LPARAM(0));
            result.0 as usize
        };
        command(hwnd, selection);
    }
}

fn create_menu_host() -> Result<Window> {
    // Keep the painted panel out of the foreground while Windows tracks its menu.
    unsafe {
        let instance = HINSTANCE(GetModuleHandleW(PCWSTR::null())?.0);
        let window = Window(CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP,
            w!("Static"),
            PCWSTR::null(),
            WS_POPUP,
            GetSystemMetrics(SM_XVIRTUALSCREEN).saturating_sub(32),
            GetSystemMetrics(SM_YVIRTUALSCREEN).saturating_sub(32),
            1,
            1,
            None,
            None,
            Some(instance),
            None,
        )?);
        set_panel_frame(window.0, false);
        let _ = ShowWindow(window.0, SW_SHOWNOACTIVATE);
        Ok(window)
    }
}

fn make_menu(
    settings: Settings,
    tray_ready: bool,
    shown: bool,
    placement: Placement,
    startup: Option<startup::Status>,
    update_busy: bool,
) -> Result<Menu> {
    let language = settings.display.language;
    let menu = Menu::new()?;
    let visibility = if shown {
        language.choose(w!("隐藏监控"), w!("Hide Monitor"))
    } else {
        language.choose(w!("显示监控"), w!("Show Monitor"))
    };
    let available = if tray_ready { MF_ENABLED } else { MF_GRAYED };
    menu.item(TOGGLE, visibility, available)?;
    menu.submenu(
        language.choose(w!("设置"), w!("Settings")),
        make_settings_menu(settings, placement, startup, update_busy)?,
    )?;
    menu.separator()?;
    menu.item(
        RESET_TRAFFIC,
        language.choose(w!("清零总流量"), w!("Reset Total Traffic")),
        MF_ENABLED,
    )?;
    menu.item(
        ABOUT,
        language.choose(w!("关于…"), w!("About…")),
        MF_ENABLED,
    )?;
    menu.item(EXIT, language.choose(w!("退出"), w!("Exit")), MF_ENABLED)?;
    Ok(menu)
}

fn make_settings_menu(
    settings: Settings,
    placement: Placement,
    startup: Option<startup::Status>,
    update_busy: bool,
) -> Result<Menu> {
    let language = settings.display.language;
    let menu = Menu::new()?;
    let checked = |condition| if condition { MF_CHECKED } else { MF_UNCHECKED };
    let startup_label = if startup == Some(startup::Status::OtherPath) {
        language.choose(
            w!("开机自启（更新路径）"),
            w!("Run at Sign-in (Update Path)"),
        )
    } else {
        language.choose(w!("开机自启"), w!("Run at Sign-in"))
    };
    let docked = matches!(placement, Placement::Docked(_));
    let dock_label = if settings.taskbar && placement == Placement::Floating {
        language.choose(
            w!("嵌入任务栏（等候空位）"),
            w!("Embed in Taskbar (Waiting for Space)"),
        )
    } else {
        language.choose(w!("嵌入任务栏"), w!("Embed in Taskbar"))
    };
    let topmost = if docked { MF_GRAYED } else { MF_ENABLED };
    menu.item(
        AUTOSTART,
        startup_label,
        checked(startup == Some(startup::Status::Enabled)),
    )?;
    menu.item(DOCK, dock_label, checked(settings.taskbar))?;
    menu.item(
        TOPMOST,
        language.choose(w!("窗口置顶"), w!("Always on Top")),
        topmost | checked(settings.topmost || docked),
    )?;
    menu.separator()?;

    let refresh = Menu::new()?;
    let labels = language.choose(
        [w!("1 秒"), w!("2 秒"), w!("3 秒"), w!("5 秒")],
        [
            w!("1 second"),
            w!("2 seconds"),
            w!("3 seconds"),
            w!("5 seconds"),
        ],
    );
    for ((id, interval), label) in REFRESH_OPTIONS.into_iter().zip(labels) {
        refresh.item(id, label, checked(settings.refresh == interval))?;
    }
    menu.submenu(
        language.choose(w!("刷新频率"), w!("Refresh Interval")),
        refresh,
    )?;

    let theme = Menu::new()?;
    for (id, choice, label) in [
        (
            FOLLOW_SYSTEM,
            Theme::System,
            language.choose(w!("跟随系统"), w!("Use System Theme")),
        ),
        (
            LIGHT,
            Theme::Light,
            language.choose(w!("浅色"), w!("Light")),
        ),
        (DARK, Theme::Dark, language.choose(w!("深色"), w!("Dark"))),
        (
            TRANSPARENT_THEME,
            Theme::Transparent,
            language.choose(w!("透明"), w!("Transparent")),
        ),
    ] {
        theme.item(id, label, checked(settings.theme == choice))?;
    }
    menu.submenu(language.choose(w!("主题"), w!("Theme")), theme)?;
    menu.item(
        LABEL_BOLD,
        language.choose(w!("标签加粗"), w!("Bold Labels")),
        checked(settings.label_bold),
    )?;

    let taskbar_position = Menu::new()?;
    taskbar_position.item(
        DOCK_LEFT,
        language.choose(w!("靠左"), w!("Left")),
        checked(settings.taskbar_side == TaskbarSide::Left),
    )?;
    taskbar_position.item(
        DOCK_RIGHT,
        language.choose(w!("靠右"), w!("Right")),
        checked(settings.taskbar_side == TaskbarSide::Right),
    )?;
    menu.submenu(
        language.choose(w!("任务栏位置"), w!("Taskbar Position")),
        taskbar_position,
    )?;
    menu.item(
        EDIT_METRICS,
        language.choose(w!("编辑监控项…"), w!("Edit Metrics…")),
        MF_ENABLED,
    )?;
    let (update_label, update_flags) = if update_busy {
        (
            language.choose(w!("正在处理更新…"), w!("Working on Update…")),
            MF_GRAYED,
        )
    } else {
        (
            language.choose(w!("检查更新…"), w!("Check for Updates…")),
            MF_ENABLED,
        )
    };
    menu.item(CHECK_UPDATE, update_label, update_flags)?;
    let languages = Menu::new()?;
    for (id, preference, label) in [
        (
            LANGUAGE_SYSTEM,
            LanguagePreference::System,
            language.choose(w!("跟随系统"), w!("Use System Language")),
        ),
        (
            LANGUAGE_CHINESE,
            LanguagePreference::Chinese,
            w!("简体中文"),
        ),
        (LANGUAGE_ENGLISH, LanguagePreference::English, w!("English")),
    ] {
        languages.item(id, label, checked(settings.language == preference))?;
    }
    menu.submenu(language.choose(w!("语言"), w!("Language")), languages)?;
    Ok(menu)
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
        Metric::CpuFrequency => language.choose("CPU 当前频率", "CPU Frequency"),
        Metric::Uptime => language.choose("运行时长", "Uptime"),
        Metric::Battery => language.choose("电池电量", "Battery Level"),
    }
}

fn set_display(hwnd: HWND, display: DisplayOptions) {
    let apply_now = with_app(|app| {
        if app.settings.display == display {
            return false;
        }
        let size = Layout::Taskbar.size(display);
        let resize_dock =
            app.settings.taskbar && size != Layout::Taskbar.size(app.settings.display);
        app.settings.display = display;
        if resize_dock {
            app.request_dock();
        }
        app.save();
        // Keep the current panel in its slot until the worker has checked the new dimensions.
        app.layout() != Layout::Taskbar || size == Layout::Taskbar.size(app.display)
    });
    if apply_now == Some(true) {
        apply_placement(hwnd);
    }
}

fn set_language(hwnd: HWND, preference: LanguagePreference) {
    let language = preference.resolve();
    let display = with_app(|app| {
        let previous = app.settings.display;
        if app.settings.language == preference && previous.language == language {
            return None;
        }
        app.settings.language = preference;
        if previous.language == language {
            app.save();
            return None;
        }
        Some(DisplayOptions {
            language,
            ..previous
        })
    })
    .flatten();
    if let Some(display) = display {
        set_thread_language(language);
        set_display(hwnd, display);
        editor::set_language(language);
    }
}

fn start_update(hwnd: HWND, action: updater::Action) {
    let result = with_app(|app| {
        if app.update.is_none() {
            app.update = Some(updater::start(hwnd, action, app.settings.display.language)?);
        }
        Ok::<_, std::io::Error>(())
    });
    if let Some(Err(error)) = result {
        show_update_error(Some(hwnd), &error);
    }
}

fn finish_update(hwnd: HWND) {
    let event = with_app(|app| {
        let event = app.update.as_ref()?.try_recv().ok()?;
        app.update = None;
        Some(event)
    })
    .flatten();
    match event {
        Some(updater::Event::Checked(Ok(Some(release)))) => {
            let version = release.version.to_string();
            let text = HSTRING::from(match ui_language() {
                Language::Chinese => format!("GitHub 上有新版本 v{version}。是否下载并重启应用？"),
                Language::English => format!(
                    "Version v{version} is available on GitHub. Download it and restart the app?"
                ),
            });
            let answer = unsafe {
                MessageBoxW(
                    Some(hwnd),
                    &text,
                    app_title(),
                    MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2,
                )
            };
            if answer == IDYES {
                start_update(hwnd, updater::Action::Download(release));
            }
        }
        Some(updater::Event::Checked(Ok(None))) => {
            let text = HSTRING::from(ui_language().choose(
                concat!("当前已是最新版本 v", env!("CARGO_PKG_VERSION"), "。"),
                concat!(
                    "You are using the latest version, v",
                    env!("CARGO_PKG_VERSION"),
                    "."
                ),
            ));
            unsafe { MessageBoxW(Some(hwnd), &text, app_title(), MB_OK | MB_ICONINFORMATION) };
        }
        Some(updater::Event::Downloaded(Ok(update))) => match update.launch(ui_language()) {
            Ok(()) => {
                let _ = unsafe { DestroyWindow(hwnd) };
            }
            Err(error) => show_update_error(Some(hwnd), &error),
        },
        Some(updater::Event::Checked(Err(error)) | updater::Event::Downloaded(Err(error))) => {
            show_update_error(Some(hwnd), &error);
        }
        None => {}
    }
}

fn show_update_error(hwnd: Option<HWND>, error: &impl std::fmt::Display) {
    show_warning(
        hwnd,
        ui_language().choose("无法完成在线更新。", "The update could not be completed."),
        error,
    );
}

fn show_warning(hwnd: Option<HWND>, heading: &str, error: &impl std::fmt::Display) {
    let text = HSTRING::from(format!("{heading}\n\n{error}"));
    unsafe { MessageBoxW(hwnd, &text, app_title(), MB_OK | MB_ICONWARNING) };
}

fn command(hwnd: HWND, selection: usize) {
    match selection {
        ABOUT => {
            if let Err(error) = about::open(hwnd) {
                show_warning(
                    Some(hwnd),
                    ui_language().choose(
                        "无法打开关于窗口。",
                        "The About dialog could not be opened.",
                    ),
                    &error,
                );
            }
        }
        CHECK_UPDATE => start_update(hwnd, updater::Action::Check),
        EDIT_METRICS => {
            if let Err(error) = editor::open(hwnd) {
                show_warning(
                    Some(hwnd),
                    ui_language().choose(
                        "无法打开监控项编辑器。",
                        "The metrics editor could not be opened.",
                    ),
                    &error,
                );
            }
        }
        AUTOSTART => {
            let language = ui_language();
            if let Err(error) = startup::toggle(language) {
                show_warning(
                    Some(hwnd),
                    language.choose(
                        "无法更改开机自启设置。",
                        "The run-at-sign-in setting could not be changed.",
                    ),
                    &error,
                );
            }
        }
        LANGUAGE_SYSTEM | LANGUAGE_CHINESE | LANGUAGE_ENGLISH => {
            set_language(
                hwnd,
                match selection {
                    LANGUAGE_CHINESE => LanguagePreference::Chinese,
                    LANGUAGE_ENGLISH => LanguagePreference::English,
                    _ => LanguagePreference::System,
                },
            );
        }
        TOGGLE => {
            let changed = with_app(|app| {
                if app.tray_ready {
                    app.shown = !app.shown;
                }
                app.tray_ready
            });
            if changed == Some(true) {
                apply_visibility(hwnd);
            }
        }
        DOCK => {
            with_app(|app| {
                app.settings.taskbar = !app.settings.taskbar;
                app.request_dock();
                app.placement = (!app.settings.taskbar).then_some(Placement::Floating);
                app.save();
            });
            apply_placement(hwnd);
        }
        DOCK_LEFT | DOCK_RIGHT => {
            let side = if selection == DOCK_LEFT {
                TaskbarSide::Left
            } else {
                TaskbarSide::Right
            };
            with_app(|app| {
                if app.settings.taskbar_side != side {
                    app.settings.taskbar_side = side;
                    if app.settings.taskbar {
                        app.request_dock();
                    }
                    app.save();
                }
            });
        }
        TOPMOST => {
            if let Some(topmost) = with_app(|app| {
                app.settings.topmost = !app.settings.topmost;
                app.save();
                app.panel_topmost()
            }) {
                set_topmost(hwnd, topmost);
            }
        }
        FOLLOW_SYSTEM | LIGHT | DARK | TRANSPARENT_THEME => {
            with_app(|app| {
                app.settings.theme = match selection {
                    LIGHT => Theme::Light,
                    DARK => Theme::Dark,
                    TRANSPARENT_THEME => Theme::Transparent,
                    _ => Theme::System,
                };
                app.save();
            });
            apply_placement(hwnd);
            editor::refresh_preview();
        }
        LABEL_BOLD => {
            with_app(|app| {
                app.settings.label_bold = !app.settings.label_bold;
                app.save();
            });
            invalidate(hwnd);
            editor::refresh_preview();
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
            editor::refresh_preview();
        }
        EXIT => {
            let _ = unsafe { DestroyWindow(hwnd) };
        }
        _ => {
            if let Some((_, interval)) =
                REFRESH_OPTIONS.into_iter().find(|(id, _)| *id == selection)
            {
                with_app(|app| {
                    app.settings.refresh = interval;
                    let _ = app.worker.send(WorkerCommand::Interval(interval));
                    app.save();
                });
            }
        }
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

    #[test]
    fn settings_menu_groups_preferences_and_shows_current_choices() {
        let settings = Settings {
            refresh: Refresh::Three,
            theme: Theme::Transparent,
            ..Default::default()
        };
        let menu = make_menu(
            settings,
            true,
            true,
            Placement::Floating,
            Some(startup::Status::Enabled),
            false,
        )
        .unwrap();
        unsafe {
            assert_eq!(GetMenuItemCount(Some(menu.0)), 6);
            assert_eq!(GetMenuItemID(menu.0, 0), TOGGLE as u32);
            assert_eq!(GetMenuItemID(menu.0, 4), ABOUT as u32);
            assert_eq!(GetMenuItemID(menu.0, 5), EXIT as u32);
            let settings = GetSubMenu(menu.0, 1);
            assert!(!settings.is_invalid());
            let refresh = GetSubMenu(settings, 4);
            let theme = GetSubMenu(settings, 5);
            let position = GetSubMenu(settings, 7);
            assert_eq!(GetMenuItemID(settings, 6), LABEL_BOLD as u32);
            assert_eq!(GetMenuItemID(settings, 8), EDIT_METRICS as u32);
            assert_eq!(GetMenuItemID(settings, 9), CHECK_UPDATE as u32);
            assert!(!refresh.is_invalid());
            assert!(!theme.is_invalid());
            assert!(!position.is_invalid());
            for (submenu, command) in [
                (settings, AUTOSTART),
                (settings, LABEL_BOLD),
                (refresh, 111),
                (theme, TRANSPARENT_THEME),
                (position, DOCK_RIGHT),
            ] {
                let flags = GetMenuState(submenu, command as u32, MF_BYCOMMAND);
                assert_ne!(flags, u32::MAX);
                assert_ne!(flags & MF_CHECKED.0, 0);
            }
        }
    }

    #[test]
    fn changing_interval_reschedules_the_running_sampler() {
        let window = Window(
            unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    w!("Static"),
                    PCWSTR::null(),
                    WINDOW_STYLE::default(),
                    0,
                    0,
                    0,
                    0,
                    Some(HWND_MESSAGE),
                    None,
                    None,
                    None,
                )
            }
            .unwrap(),
        );
        let settings = Settings {
            refresh: Refresh::Three,
            ..Default::default()
        };
        let feed = Feed::default();
        let command = mpsc::channel();
        let sender = command.0.clone();
        let _worker = SamplerWorker::start(window.0, feed.clone(), settings, command).unwrap();
        let wait_for_sample = || {
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                if feed.lock().unwrap().sample.take().is_some() {
                    return true;
                }
                thread::sleep(Duration::from_millis(20));
            }
            false
        };
        assert!(wait_for_sample());
        sender.send(WorkerCommand::Interval(Refresh::Five)).unwrap();
        thread::sleep(Duration::from_millis(2200));
        assert!(feed.lock().unwrap().sample.is_none());
        sender.send(WorkerCommand::Interval(Refresh::Two)).unwrap();
        assert!(wait_for_sample());
    }
}
