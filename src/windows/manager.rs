use std::cell::RefCell;

use ::windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::{
            COLOR_BTNFACE, CreateFontIndirectW, DEFAULT_GUI_FONT, DeleteObject, FW_BOLD,
            GetMonitorInfoW, GetStockObject, HFONT, HGDIOBJ, MONITOR_DEFAULTTONEAREST, MONITORINFO,
            MonitorFromWindow,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Controls::*,
            HiDpi::{AdjustWindowRectExForDpi, SystemParametersInfoForDpi},
            Input::KeyboardAndMouse::{EnableWindow, SetFocus},
            WindowsAndMessaging::*,
        },
    },
    core::{HSTRING, PCWSTR, PWSTR, Result, w},
};

use super::{
    ABOUT, AUTOSTART, CHECK_UPDATE, DARK, DOCK, DOCK_LEFT, DOCK_RIGHT, EDIT_METRICS, EXIT,
    FOLLOW_SYSTEM, LABEL_BOLD, LANGUAGE_CHINESE, LANGUAGE_ENGLISH, LANGUAGE_SYSTEM, LIGHT,
    OPEN_HISTORY, PRODUCT_NAME, REFRESH_OPTIONS, RESET_TRAFFIC, TOGGLE, TOPMOST, TRANSPARENT_THEME,
    Window, command, control, metric_name, position, register_class, scale, send,
    startup::{self, Status},
    state::{LanguagePreference, TaskbarSide, Theme},
    taskbar::Placement,
    window_dpi, with_app,
};
use crate::{
    language::Language,
    model::{Metric, Reading, readings},
};

const CLASS: PCWSTR = w!("WindowsMonitor.Manager");
const STYLE: WINDOW_STYLE = WINDOW_STYLE(WS_OVERLAPPEDWINDOW.0 | WS_CLIPCHILDREN.0);
const EX_STYLE: WINDOW_EX_STYLE = WS_EX_APPWINDOW;
const SELECT_POSITION: usize = 201;
const SELECT_THEME: usize = 202;
const SELECT_REFRESH: usize = 203;
const SELECT_LANGUAGE: usize = 204;
const LIVE_LIST: usize = 205;
const MIN_WIDTH: i32 = 620;
const MIN_HEIGHT: i32 = 407;
const POSITION_COMMANDS: [usize; 2] = [DOCK_LEFT, DOCK_RIGHT];
const THEME_COMMANDS: [usize; 4] = [FOLLOW_SYSTEM, LIGHT, DARK, TRANSPARENT_THEME];
const LANGUAGE_COMMANDS: [usize; 3] = [LANGUAGE_SYSTEM, LANGUAGE_CHINESE, LANGUAGE_ENGLISH];

thread_local! {
    static MANAGER: RefCell<Option<Manager>> = const { RefCell::new(None) };
}

struct Manager {
    hwnd: HWND,
    panel: HWND,
    title: HWND,
    status: HWND,
    panel_group: HWND,
    app_group: HWND,
    live_heading: HWND,
    live_list: HWND,
    show_panel: HWND,
    topmost: HWND,
    dock: HWND,
    bold: HWND,
    taskbar_position: (HWND, HWND),
    theme: (HWND, HWND),
    refresh: (HWND, HWND),
    language_select: (HWND, HWND),
    edit: HWND,
    history: HWND,
    autostart: HWND,
    reset: HWND,
    update: HWND,
    about: HWND,
    exit: HWND,
    language: Option<Language>,
    status_text: PCWSTR,
    values: [String; Metric::ALL.len()],
    fonts: Option<Fonts>,
}

pub fn open(panel: HWND) -> Result<()> {
    if let Some(hwnd) = handle() {
        unsafe {
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            }
            let _ = SetForegroundWindow(hwnd);
        }
        refresh_settings();
        return Ok(());
    }
    let Some(icon) = with_app(|app| app.tray.hIcon) else {
        return Ok(());
    };
    let instance = unsafe { HINSTANCE(GetModuleHandleW(PCWSTR::null())?.0) };
    unsafe {
        InitCommonControlsEx(&INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_LISTVIEW_CLASSES | ICC_STANDARD_CLASSES,
        })
        .ok()?;
    }
    register_class(
        CLASS,
        instance,
        Some(window_proc),
        icon,
        COLOR_BTNFACE.0 as isize + 1,
    )?;

    let dpi = window_dpi(panel);
    let mut monitor = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        GetMonitorInfoW(
            MonitorFromWindow(panel, MONITOR_DEFAULTTONEAREST),
            &mut monitor,
        )
        .ok()?;
    }
    let work = monitor.rcWork;
    let bounds = window_bounds(820, 466, dpi)?;
    let width = bounds.right.min(work.right - work.left);
    let height = bounds.bottom.min(work.bottom - work.top);
    let window = Window(unsafe {
        CreateWindowExW(
            EX_STYLE,
            CLASS,
            PCWSTR::null(),
            STYLE,
            work.left + (work.right - work.left - width) / 2,
            work.top + (work.bottom - work.top - height) / 2,
            width,
            height,
            None,
            None,
            Some(instance),
            None,
        )?
    });
    let hwnd = window.0;
    let manager = Manager::new(hwnd, panel)?;
    let focus = manager.show_panel;
    MANAGER.with(|slot| *slot.borrow_mut() = Some(manager));
    std::mem::forget(window);
    with_manager(Manager::arrange);
    refresh_settings();
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(focus));
    }
    Ok(())
}

fn handle() -> Option<HWND> {
    with_manager(|manager| manager.hwnd)
}

pub fn dialog_owner(panel: HWND) -> HWND {
    handle()
        .filter(|&hwnd| unsafe { !IsIconic(hwnd).as_bool() && IsWindowVisible(hwnd).as_bool() })
        .unwrap_or(panel)
}

pub fn close() {
    if let Some(hwnd) = handle() {
        let _ = unsafe { DestroyWindow(hwnd) };
    }
}

pub fn handle_dialog_message(message: &MSG) -> bool {
    handle().is_some_and(|hwnd| unsafe {
        (message.hwnd == hwnd || IsChild(hwnd, message.hwnd).as_bool())
            && IsDialogMessageW(hwnd, message).as_bool()
    })
}

pub fn refresh_settings() {
    with_manager(|manager| {
        with_app(|app| manager.update_settings(app, startup::status().ok()));
    });
}

pub fn refresh_status() {
    with_manager(|manager| {
        with_app(|app| manager.update_status(app));
    });
}

pub fn refresh_readings() {
    with_manager(|manager| {
        with_app(|app| manager.update_readings(app));
    });
}

fn with_manager<T>(run: impl FnOnce(&mut Manager) -> T) -> Option<T> {
    MANAGER.with(|slot| slot.try_borrow_mut().ok()?.as_mut().map(run))
}

impl Manager {
    fn new(hwnd: HWND, panel: HWND) -> Result<Self> {
        let static_text = || control(hwnd, WC_STATICW, PCWSTR::null(), 0, 0);
        let group = || control(hwnd, WC_BUTTONW, PCWSTR::null(), BS_GROUPBOX as u32, 0);
        let check = |id| button(hwnd, BS_AUTOCHECKBOX, id);
        let push = |id| button(hwnd, BS_PUSHBUTTON, id);
        let combo = |id| -> Result<_> {
            Ok((
                static_text()?,
                control(
                    hwnd,
                    WC_COMBOBOXW,
                    PCWSTR::null(),
                    WS_TABSTOP.0 | CBS_DROPDOWNLIST as u32,
                    id,
                )?,
            ))
        };
        let title = static_text()?;
        let status = static_text()?;
        let panel_group = group()?;
        let show_panel = check(TOGGLE)?;
        let topmost = check(TOPMOST)?;
        let dock = check(DOCK)?;
        let bold = check(LABEL_BOLD)?;
        let taskbar_position = combo(SELECT_POSITION)?;
        let theme = combo(SELECT_THEME)?;
        let edit = push(EDIT_METRICS)?;
        let history = push(OPEN_HISTORY)?;
        let app_group = group()?;
        let refresh = combo(SELECT_REFRESH)?;
        let language_select = combo(SELECT_LANGUAGE)?;
        let autostart = check(AUTOSTART)?;
        let live_heading = static_text()?;
        let live_list = control(
            hwnd,
            WC_LISTVIEWW,
            PCWSTR::null(),
            WS_TABSTOP.0 | WS_BORDER.0 | LVS_REPORT | LVS_SINGLESEL | LVS_NOSORTHEADER,
            LIVE_LIST,
        )?;
        let list_style = (LVS_EX_DOUBLEBUFFER | LVS_EX_FULLROWSELECT) as usize;
        send(
            live_list,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            list_style,
            list_style as isize,
        );
        for index in 0..2 {
            let column = LVCOLUMNW {
                mask: LVCF_WIDTH,
                ..Default::default()
            };
            send(
                live_list,
                LVM_INSERTCOLUMNW,
                index,
                &raw const column as isize,
            );
        }
        for (index, _) in Metric::ALL.iter().enumerate() {
            let item = LVITEMW {
                iItem: index as i32,
                ..Default::default()
            };
            send(live_list, LVM_INSERTITEMW, index, &raw const item as isize);
        }
        Ok(Self {
            hwnd,
            panel,
            title,
            status,
            panel_group,
            app_group,
            live_heading,
            live_list,
            show_panel,
            topmost,
            dock,
            bold,
            taskbar_position,
            theme,
            refresh,
            language_select,
            edit,
            history,
            autostart,
            reset: push(RESET_TRAFFIC)?,
            update: push(CHECK_UPDATE)?,
            about: push(ABOUT)?,
            exit: push(EXIT)?,
            language: None,
            status_text: PCWSTR::null(),
            values: Default::default(),
            fonts: None,
        })
    }

    fn localize(&mut self, language: Language) {
        self.language = Some(language);
        let caption = HSTRING::from(format!(
            "{} — {PRODUCT_NAME}",
            language.choose("管理窗口", "Manager")
        ));
        set_text(self.hwnd, PCWSTR(caption.as_ptr()));
        for (control, chinese, english) in [
            (self.title, w!("系统监控"), w!("Windows Monitor")),
            (self.panel_group, w!("监控面板"), w!("Monitor Panel")),
            (self.app_group, w!("常规设置"), w!("General Settings")),
            (self.live_heading, w!("实时监控"), w!("Live Readings")),
            (
                self.show_panel,
                w!("显示监控面板"),
                w!("Show Monitor Panel"),
            ),
            (self.topmost, w!("窗口置顶"), w!("Always on Top")),
            (self.dock, w!("嵌入任务栏"), w!("Embed in Taskbar")),
            (self.bold, w!("标签加粗"), w!("Bold Labels")),
            (
                self.taskbar_position.0,
                w!("任务栏位置"),
                w!("Taskbar Position"),
            ),
            (self.theme.0, w!("面板主题"), w!("Panel Theme")),
            (self.refresh.0, w!("刷新频率"), w!("Refresh Interval")),
            (self.language_select.0, w!("语言"), w!("Language")),
            (self.edit, w!("编辑监控项…"), w!("Edit Metrics…")),
            (self.history, w!("历史记录…"), w!("History…")),
            (self.reset, w!("清零总流量"), w!("Reset Total Traffic")),
            (self.about, w!("关于…"), w!("About…")),
            (self.exit, w!("退出程序"), w!("Exit App")),
        ] {
            set_text(control, language.choose(chinese, english));
        }
        set_options(
            self.taskbar_position.1,
            language.choose([w!("靠左"), w!("靠右")], [w!("Left"), w!("Right")]),
        );
        set_options(
            self.theme.1,
            language.choose(
                [w!("跟随系统"), w!("浅色"), w!("深色"), w!("透明")],
                [w!("System"), w!("Light"), w!("Dark"), w!("Transparent")],
            ),
        );
        set_options(
            self.refresh.1,
            language.choose(
                [w!("1 秒"), w!("2 秒"), w!("3 秒"), w!("5 秒")],
                [
                    w!("1 second"),
                    w!("2 seconds"),
                    w!("3 seconds"),
                    w!("5 seconds"),
                ],
            ),
        );
        set_options(
            self.language_select.1,
            [
                language.choose(w!("跟随系统"), w!("System")),
                w!("简体中文"),
                w!("English"),
            ],
        );
        let columns = language.choose([w!("指标"), w!("读数")], [w!("Metric"), w!("Reading")]);
        for (index, text) in columns.into_iter().enumerate() {
            let column = LVCOLUMNW {
                mask: LVCF_TEXT,
                pszText: PWSTR(text.0.cast_mut()),
                ..Default::default()
            };
            send(
                self.live_list,
                LVM_SETCOLUMNW,
                index,
                &raw const column as isize,
            );
        }
        for metric in Metric::ALL {
            set_cell(
                self.live_list,
                metric.index(),
                0,
                metric_name(metric, language),
            );
        }
    }

    fn update_settings(&mut self, app: &super::App, startup: Option<Status>) {
        let settings = app.settings;
        let language = settings.display.language;
        if self.language != Some(language) {
            self.localize(language);
        }
        for (control, checked) in [
            (self.show_panel, app.shown),
            (self.dock, settings.taskbar),
            (self.bold, settings.label_bold),
            (self.autostart, startup == Some(Status::Enabled)),
        ] {
            send(control, BM_SETCHECK, usize::from(checked), 0);
        }
        set_text(
            self.autostart,
            if startup == Some(Status::OtherPath) {
                language.choose(
                    w!("开机自启（更新路径）"),
                    w!("Run at Sign-in (Update Path)"),
                )
            } else {
                language.choose(w!("开机自启"), w!("Run at Sign-in"))
            },
        );
        set_text(
            self.update,
            if app.update.is_some() {
                language.choose(w!("正在检查或下载…"), w!("Updating…"))
            } else {
                language.choose(w!("检查更新…"), w!("Check for Updates…"))
            },
        );
        unsafe {
            let _ = EnableWindow(self.show_panel, app.tray_ready);
            let _ = EnableWindow(self.update, app.update.is_none());
        }
        send(
            self.taskbar_position.1,
            CB_SETCURSEL,
            usize::from(settings.taskbar_side == TaskbarSide::Right),
            0,
        );
        send(
            self.theme.1,
            CB_SETCURSEL,
            match settings.theme {
                Theme::System => 0,
                Theme::Light => 1,
                Theme::Dark => 2,
                Theme::Transparent => 3,
            },
            0,
        );
        let refresh = REFRESH_OPTIONS
            .iter()
            .position(|&(_, interval)| interval == settings.refresh);
        send(self.refresh.1, CB_SETCURSEL, refresh.unwrap_or(0), 0);
        send(
            self.language_select.1,
            CB_SETCURSEL,
            match settings.language {
                LanguagePreference::System => 0,
                LanguagePreference::Chinese => 1,
                LanguagePreference::English => 2,
            },
            0,
        );
        self.update_status(app);
        self.update_readings(app);
    }

    fn update_status(&mut self, app: &super::App) {
        let language = app.settings.display.language;
        let status = if !app.shown {
            language.choose(w!("监控面板已隐藏"), w!("Monitor panel hidden"))
        } else if app.waiting_sample {
            language.choose(w!("正在读取监控数据…"), w!("Collecting the first sample…"))
        } else {
            match app.placement {
                Some(Placement::Docked(_)) => language.choose(
                    w!("监控面板已嵌入任务栏"),
                    w!("Monitor panel embedded in the taskbar"),
                ),
                Some(Placement::Hidden) => language.choose(
                    w!("任务栏被覆盖，监控面板暂时隐藏"),
                    w!("Monitor panel hidden while the taskbar is covered"),
                ),
                _ if app.settings.taskbar => language.choose(
                    w!("当前为浮窗，正在等待任务栏空位"),
                    w!("Floating panel · Waiting for taskbar space"),
                ),
                _ => language.choose(w!("监控面板以浮窗显示"), w!("Monitor panel floating")),
            }
        };
        if status != self.status_text {
            set_text(self.status, status);
            self.status_text = status;
        }
        let docked = app.layout() == super::Layout::Taskbar;
        send(
            self.topmost,
            BM_SETCHECK,
            usize::from(app.settings.topmost || docked),
            0,
        );
        let _ = unsafe { EnableWindow(self.topmost, !docked) };
    }

    fn update_readings(&mut self, app: &super::App) {
        let language = app.settings.display.language;
        let localized;
        let values = if language == app.display.language {
            &app.displayed
        } else {
            localized = readings(app.sample, app.total(), language);
            &localized
        };
        for (row, (current, Reading { number, unit })) in
            self.values.iter_mut().zip(values).enumerate()
        {
            let text = if unit.is_empty() || *unit == "%" {
                format!("{number}{unit}")
            } else {
                format!("{number} {unit}")
            };
            if *current != text {
                set_cell(self.live_list, row, 1, &text);
                *current = text;
            }
        }
    }

    fn arrange(&mut self) {
        let dpi = window_dpi(self.hwnd);
        let mut area = RECT::default();
        if unsafe { GetClientRect(self.hwnd, &mut area) }.is_err() {
            return;
        }
        if self.fonts.as_ref().is_none_or(|fonts| fonts.dpi != dpi) {
            self.set_fonts(dpi);
        }
        let dip = |px| px * 96 / dpi as i32;
        let width = dip(area.right);
        let height = dip(area.bottom);
        let left = 20;
        let right = width - 20;
        let panel_width = ((right - left) / 2).clamp(310, 410);
        let live_x = left + panel_width + 20;
        let inside = left + 14;
        let column = inside + (panel_width - 28) / 2;
        let compact = (435 - height).clamp(0, 28);
        let footer = height - 45;
        let set = |hwnd, x, y, w, h| {
            position(
                hwnd,
                scale(x, dpi),
                scale(y, dpi),
                scale(w, dpi),
                scale(h, dpi),
            );
        };

        set(self.title, left, 14, right - left, 34);
        set(self.status, left, 51, right - left, 22);
        set(self.panel_group, left, 77, panel_width, 184 - compact / 2);
        set(
            self.app_group,
            left,
            268 - compact / 2,
            panel_width,
            113 - compact / 2,
        );
        set(self.show_panel, inside, 96, column - inside, 24);
        set(self.topmost, column, 96, panel_width / 2 - 14, 24);
        set(self.dock, inside, 125, column - inside, 24);
        set(self.bold, column, 125, panel_width / 2 - 14, 24);
        for ((label, field), y) in [
            (self.taskbar_position, 155),
            (self.theme, 185 - compact / 4),
            (self.refresh, 287 - compact / 2),
            (self.language_select, 317 - compact * 3 / 4),
        ] {
            set(label, inside, y + 4, column - inside, 22);
            set(field, column, y, panel_width / 2 - 14, 150);
        }
        set(self.edit, inside, 219 - compact / 2, 136, 28);
        set(self.history, inside + 146, 219 - compact / 2, 116, 28);
        set(self.autostart, inside, 347 - compact, panel_width - 28, 24);

        let live_width = (right - live_x).max(1);
        set(self.live_heading, live_x, 80, live_width, 24);
        set(
            self.live_list,
            live_x,
            105,
            live_width,
            (footer - 117).max(1),
        );
        let readings_width = (live_width / 3).max(94);
        // Account for the border and a potential vertical scrollbar.
        let metric_width = (live_width - readings_width - 22).max(148);
        send(
            self.live_list,
            LVM_SETCOLUMNWIDTH,
            0,
            scale(metric_width, dpi) as isize,
        );
        send(
            self.live_list,
            LVM_SETCOLUMNWIDTH,
            1,
            scale(readings_width, dpi) as isize,
        );

        set(self.reset, left, footer, 152, 30);
        set(self.update, left + 162, footer, 152, 30);
        set(self.about, left + 324, footer, 84, 30);
        set(self.exit, (right - 104).max(left + 418), footer, 104, 30);
    }

    fn set_fonts(&mut self, dpi: u32) {
        let fonts = Fonts::new(dpi);
        let fallback = unsafe { GetStockObject(DEFAULT_GUI_FONT).0 as isize };
        let use_font = |control, font: HFONT| {
            let handle = if font.is_invalid() {
                fallback
            } else {
                font.0 as isize
            };
            send(control, WM_SETFONT, handle as usize, 1);
        };
        for control in [
            self.status,
            self.panel_group,
            self.app_group,
            self.live_heading,
            self.live_list,
            self.show_panel,
            self.topmost,
            self.dock,
            self.bold,
            self.taskbar_position.0,
            self.taskbar_position.1,
            self.theme.0,
            self.theme.1,
            self.refresh.0,
            self.refresh.1,
            self.language_select.0,
            self.language_select.1,
            self.edit,
            self.history,
            self.autostart,
            self.reset,
            self.update,
            self.about,
            self.exit,
        ] {
            use_font(control, fonts.body);
        }
        use_font(self.title, fonts.title);
        self.fonts = Some(fonts);
    }
}

struct Fonts {
    dpi: u32,
    body: HFONT,
    title: HFONT,
}

impl Fonts {
    fn new(dpi: u32) -> Self {
        let mut metrics = NONCLIENTMETRICSW {
            cbSize: size_of::<NONCLIENTMETRICSW>() as u32,
            ..Default::default()
        };
        let mut fonts = Self {
            dpi,
            body: HFONT::default(),
            title: HFONT::default(),
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
                metrics.lfMessageFont.lfHeight = -scale(26, dpi);
                metrics.lfMessageFont.lfWeight = FW_BOLD.0 as i32;
                fonts.title = CreateFontIndirectW(&metrics.lfMessageFont);
            }
        }
        fonts
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        for font in [self.body, self.title] {
            if !font.is_invalid() {
                let _ = unsafe { DeleteObject(HGDIOBJ(font.0)) };
            }
        }
    }
}

fn window_bounds(width: i32, height: i32, dpi: u32) -> Result<RECT> {
    let mut rect = RECT {
        right: scale(width, dpi),
        bottom: scale(height, dpi),
        ..Default::default()
    };
    unsafe { AdjustWindowRectExForDpi(&mut rect, STYLE, false, EX_STYLE, dpi)? };
    rect.right -= rect.left;
    rect.bottom -= rect.top;
    Ok(rect)
}

fn button(hwnd: HWND, style: i32, id: usize) -> Result<HWND> {
    control(
        hwnd,
        WC_BUTTONW,
        PCWSTR::null(),
        WS_TABSTOP.0 | style as u32,
        id,
    )
}

fn set_text(hwnd: HWND, text: PCWSTR) {
    let _ = unsafe { SetWindowTextW(hwnd, text) };
}

fn set_options<const N: usize>(combo: HWND, options: [PCWSTR; N]) {
    send(combo, CB_RESETCONTENT, 0, 0);
    for option in options {
        send(combo, CB_ADDSTRING, 0, option.0 as isize);
    }
}

fn set_cell(list: HWND, row: usize, column: i32, text: &str) {
    let text = HSTRING::from(text);
    let item = LVITEMW {
        iSubItem: column,
        pszText: PWSTR(text.as_ptr().cast_mut()),
        ..Default::default()
    };
    send(list, LVM_SETITEMTEXTW, row, &raw const item as isize);
}

fn selected(combo: HWND, id: usize) -> Option<usize> {
    let index = usize::try_from(send(combo, CB_GETCURSEL, 0, 0)).ok()?;
    match id {
        SELECT_POSITION => POSITION_COMMANDS.get(index).copied(),
        SELECT_THEME => THEME_COMMANDS.get(index).copied(),
        SELECT_LANGUAGE => LANGUAGE_COMMANDS.get(index).copied(),
        SELECT_REFRESH => REFRESH_OPTIONS.get(index).map(|&(command, _)| command),
        _ => None,
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let id = wp.0 & 0xffff;
            let notification = (wp.0 >> 16) as u32;
            let source = HWND(lp.0 as *mut _);
            let selection = match (id, notification) {
                (id, CBN_SELCHANGE) => selected(source, id),
                (
                    TOGGLE | TOPMOST | DOCK | LABEL_BOLD | EDIT_METRICS | OPEN_HISTORY | AUTOSTART
                    | RESET_TRAFFIC | CHECK_UPDATE | ABOUT | EXIT,
                    BN_CLICKED,
                ) if !source.is_invalid() => Some(id),
                _ => None,
            };
            if let Some(selection) = selection
                && let Some(panel) = with_manager(|manager| manager.panel)
            {
                command(panel, selection);
            }
        }
        WM_SIZE if wp.0 != SIZE_MINIMIZED as usize => {
            with_manager(Manager::arrange);
        }
        WM_DPICHANGED => {
            let suggested = unsafe { &*(lp.0 as *const RECT) };
            position(
                hwnd,
                suggested.left,
                suggested.top,
                suggested.right - suggested.left,
                suggested.bottom - suggested.top,
            );
            with_manager(Manager::arrange);
        }
        WM_GETMINMAXINFO => {
            if let Ok(bounds) = window_bounds(MIN_WIDTH, MIN_HEIGHT, window_dpi(hwnd)) {
                let limits = unsafe { &mut *(lp.0 as *mut MINMAXINFO) };
                let mut monitor = MONITORINFO {
                    cbSize: size_of::<MONITORINFO>() as u32,
                    ..Default::default()
                };
                if unsafe {
                    GetMonitorInfoW(
                        MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST),
                        &mut monitor,
                    )
                    .as_bool()
                } {
                    limits.ptMinTrackSize.x =
                        bounds.right.min(monitor.rcWork.right - monitor.rcWork.left);
                    limits.ptMinTrackSize.y = bounds
                        .bottom
                        .min(monitor.rcWork.bottom - monitor.rcWork.top);
                }
            }
        }
        WM_NCDESTROY => {
            MANAGER.with(|slot| {
                let mut slot = slot.borrow_mut();
                if slot.as_ref().is_some_and(|manager| manager.hwnd == hwnd) {
                    slot.take();
                }
            });
            return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
        }
        _ => return unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
    LRESULT(0)
}
