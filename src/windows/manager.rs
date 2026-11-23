use std::cell::RefCell;

use ::windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Gdi::{
            COLOR_WINDOW, ExcludeClipRect, FillRect, GetSysColorBrush, HDC, MapWindowPoints,
            RestoreDC, SaveDC, SetBkMode, TRANSPARENT,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Controls::*,
            Input::KeyboardAndMouse::{EnableWindow, SetFocus},
            WindowsAndMessaging::*,
        },
    },
    core::{HSTRING, PCWSTR, PWSTR, Result, w},
};

use super::{
    ABOUT, AUTOSTART, CHECK_UPDATE, DARK, DOCK, DOCK_LEFT, DOCK_RIGHT, DOUBLE_CLICK_OPTIONS,
    EDIT_METRICS, EXIT, EXPORT_HISTORY, FOLLOW_SYSTEM, LABEL_BOLD, LANGUAGE_CHINESE,
    LANGUAGE_ENGLISH, LANGUAGE_SYSTEM, LIGHT, PRODUCT_NAME, RECORD_HISTORY, REFRESH_OPTIONS,
    RESET_TRAFFIC, TOGGLE, TOPMOST, TRANSPARENT_THEME, Window, command, control,
    double_click_labels, metric_name, position, register_class, scale, send,
    startup::{self, Status},
    state::{LanguagePreference, TaskbarSide, Theme},
    ui::{Fonts, centered_bounds, follow_dpi_change, init_common_controls, window_size, work_area},
    window_dpi, with_app,
};
use crate::{
    language::Language,
    model::{Metric, Reading, readings},
};

const CLASS: PCWSTR = w!("WindowsMonitor.Manager");
// 不使用 WS_CLIPCHILDREN：分组框只绘制边框，内部依赖父窗口擦除背景。
// 为避免改变大小时擦掉其他控件造成闪烁，WM_ERASEBKGND 中手动排除分组框以外的子控件。
const STYLE: WINDOW_STYLE = WS_OVERLAPPEDWINDOW;
const EX_STYLE: WINDOW_EX_STYLE = WS_EX_APPWINDOW;
const SELECT_POSITION: usize = 201;
const SELECT_THEME: usize = 202;
const SELECT_REFRESH: usize = 203;
const SELECT_LANGUAGE: usize = 204;
const LIVE_LIST: usize = 205;
const SELECT_DOUBLE_CLICK: usize = 206;
// 静态控件单行文本垂直居中（SystemServices::SS_CENTERIMAGE），避免为一个常量引入整个特性。
const SS_CENTERIMAGE: u32 = 0x200;
const WIDTH: i32 = 820;
const HEIGHT: i32 = 496;
const MIN_WIDTH: i32 = 620;
const MIN_HEIGHT: i32 = 437;
// 设置区纵向布局（DIP）：分组框从窗口顶部开始，组内各行按统一行距自上而下排布。
const GROUPS_TOP: i32 = 12;
const GROUP_PADDING_TOP: i32 = 19;
const GROUP_PADDING_BOTTOM: i32 = 10;
const GROUP_SPACING: i32 = 7;
const CHECK_HEIGHT: i32 = 24;
const BUTTON_HEIGHT: i32 = 28;
const ROW_GAP: (i32, i32) = (2, 6);
const FOOTER_HEIGHT: i32 = 45;
const FOOTER_SPACING: i32 = 8;
const POSITION_COMMANDS: [usize; 2] = [DOCK_LEFT, DOCK_RIGHT];
const THEME_COMMANDS: [usize; 4] = [FOLLOW_SYSTEM, LIGHT, DARK, TRANSPARENT_THEME];
const LANGUAGE_COMMANDS: [usize; 3] = [LANGUAGE_SYSTEM, LANGUAGE_CHINESE, LANGUAGE_ENGLISH];

thread_local! {
    static MANAGER: RefCell<Option<Manager>> = const { RefCell::new(None) };
}

struct Manager {
    hwnd: HWND,
    panel: HWND,
    version: HWND,
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
    double_click: (HWND, HWND),
    edit: HWND,
    export_history: HWND,
    record_history: HWND,
    autostart: HWND,
    reset: HWND,
    update: HWND,
    about: HWND,
    exit: HWND,
    language: Option<Language>,
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
    init_common_controls()?;
    register_class(
        CLASS,
        instance,
        WNDCLASS_STYLES::default(),
        Some(window_proc),
        icon,
        COLOR_WINDOW.0 as isize + 1,
    )?;
    let bounds = centered_bounds(panel, WIDTH, HEIGHT, STYLE, EX_STYLE)?;
    let window = Window(unsafe {
        CreateWindowExW(
            EX_STYLE,
            CLASS,
            PCWSTR::null(),
            STYLE,
            bounds.x,
            bounds.y,
            bounds.width,
            bounds.height,
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
    MANAGER.with(|slot| {
        let mut slot = slot.try_borrow_mut().ok()?;
        if slot
            .as_ref()
            .is_some_and(|manager| !unsafe { IsWindow(Some(manager.hwnd)) }.as_bool())
        {
            *slot = None;
        }
        slot.as_mut().map(run)
    })
}

impl Manager {
    fn new(hwnd: HWND, panel: HWND) -> Result<Self> {
        let static_text = || control(hwnd, WC_STATICW, PCWSTR::null(), 0, 0);
        let group = || control(hwnd, WC_BUTTONW, PCWSTR::null(), BS_GROUPBOX as u32, 0);
        let check = |id| button(hwnd, BS_AUTOCHECKBOX, id);
        let push = |id| button(hwnd, BS_PUSHBUTTON, id);
        let combo = |id| -> Result<_> {
            Ok((
                // 标签与下拉框同高并垂直居中，保证两者文字基线对齐。
                control(hwnd, WC_STATICW, PCWSTR::null(), SS_CENTERIMAGE, 0)?,
                control(
                    hwnd,
                    WC_COMBOBOXW,
                    PCWSTR::null(),
                    WS_TABSTOP.0 | CBS_DROPDOWNLIST as u32,
                    id,
                )?,
            ))
        };
        let version = static_text()?;
        let panel_group = group()?;
        let show_panel = check(TOGGLE)?;
        let topmost = check(TOPMOST)?;
        let dock = check(DOCK)?;
        let bold = check(LABEL_BOLD)?;
        let taskbar_position = combo(SELECT_POSITION)?;
        let theme = combo(SELECT_THEME)?;
        let edit = push(EDIT_METRICS)?;
        let export_history = push(EXPORT_HISTORY)?;
        let app_group = group()?;
        let refresh = combo(SELECT_REFRESH)?;
        let language_select = combo(SELECT_LANGUAGE)?;
        let double_click = combo(SELECT_DOUBLE_CLICK)?;
        let autostart = check(AUTOSTART)?;
        let record_history = check(RECORD_HISTORY)?;
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
            version,
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
            double_click,
            edit,
            export_history,
            record_history,
            autostart,
            reset: push(RESET_TRAFFIC)?,
            update: push(CHECK_UPDATE)?,
            about: push(ABOUT)?,
            exit: push(EXIT)?,
            language: None,
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
        let version = HSTRING::from(format!(
            "{} v{}",
            language.choose("版本", "Version"),
            env!("CARGO_PKG_VERSION")
        ));
        set_text(self.version, PCWSTR(version.as_ptr()));
        for (control, chinese, english) in [
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
            (
                self.double_click.0,
                w!("双击面板"),
                w!("Double-Click Panel"),
            ),
            (self.edit, w!("编辑监控项…"), w!("Edit Metrics…")),
            (
                self.record_history,
                w!("记录监控数据"),
                w!("Record History"),
            ),
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
        set_options(self.double_click.1, double_click_labels(language));
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
            (self.record_history, settings.record_history),
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
            self.export_history,
            if app.history_exporting {
                language.choose(w!("正在导出…"), w!("Exporting…"))
            } else {
                language.choose(w!("导出记录…"), w!("Export…"))
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
            let _ = EnableWindow(self.export_history, !app.history_exporting);
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
        let double_click = DOUBLE_CLICK_OPTIONS
            .iter()
            .position(|&(_, action)| action == settings.double_click);
        send(
            self.double_click.1,
            CB_SETCURSEL,
            double_click.unwrap_or(0),
            0,
        );
        self.update_status(app);
        self.update_readings(app);
    }

    fn update_status(&mut self, app: &super::App) {
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
        let dip = |px: i32| px * 96 / dpi as i32;
        let width = dip(area.right);
        let height = dip(area.bottom);
        let left = 20;
        let right = width - 20;
        let panel_width = ((right - left) / 2).clamp(310, 410);
        let live_x = left + panel_width + 20;
        let inside = left + 14;
        let column = inside + (panel_width - 28) / 2;
        let half = panel_width / 2 - 14;
        let footer = height - FOOTER_HEIGHT;
        let set = |hwnd, x, y, w, h| {
            position(
                hwnd,
                scale(x, dpi),
                scale(y, dpi),
                scale(w, dpi),
                scale(h, dpi),
            );
        };

        // 下拉框收起时的高度由字体与 DPI 决定，行高取它与复选框高度的较大者。
        let mut combo = RECT::default();
        let combo_px = if unsafe { GetWindowRect(self.theme.1, &mut combo) }.is_ok() {
            combo.bottom - combo.top
        } else {
            scale(CHECK_HEIGHT, dpi)
        };
        let combo_height = ((combo_px * 96 + dpi as i32 - 1) / dpi as i32).max(CHECK_HEIGHT);
        let panel_rows = [
            CHECK_HEIGHT,
            CHECK_HEIGHT,
            combo_height,
            combo_height,
            BUTTON_HEIGHT,
        ];
        let app_rows = [combo_height, combo_height, combo_height, CHECK_HEIGHT];
        let padding = GROUP_PADDING_TOP + GROUP_PADDING_BOTTOM;
        let fixed = panel_rows.iter().chain(&app_rows).sum::<i32>() + 2 * padding + GROUP_SPACING;
        let gaps = (panel_rows.len() + app_rows.len() - 2) as i32;
        let available = footer - FOOTER_SPACING - GROUPS_TOP;
        let gap = ((available - fixed) / gaps).clamp(ROW_GAP.0, ROW_GAP.1);
        let mut rows = Rows::new(GROUPS_TOP + GROUP_PADDING_TOP, gap);

        let place_combo = |(label, field): (HWND, HWND), y| {
            set(field, column, y, half, 150);
            // 标签与下拉框同高并垂直居中，两者文字基线对齐。
            position(
                label,
                scale(inside, dpi),
                scale(y, dpi),
                scale(column - inside, dpi),
                combo_px,
            );
        };

        let y = rows.next(CHECK_HEIGHT);
        set(self.show_panel, inside, y, column - inside, CHECK_HEIGHT);
        set(self.topmost, column, y, half, CHECK_HEIGHT);
        let y = rows.next(CHECK_HEIGHT);
        set(self.dock, inside, y, column - inside, CHECK_HEIGHT);
        set(self.bold, column, y, half, CHECK_HEIGHT);
        place_combo(self.taskbar_position, rows.next(combo_height));
        place_combo(self.theme, rows.next(combo_height));
        let y = rows.next(BUTTON_HEIGHT);
        set(self.edit, inside, y, 136, BUTTON_HEIGHT);
        set(self.export_history, inside + 146, y, 116, BUTTON_HEIGHT);
        let panel_bottom = rows.end() + GROUP_PADDING_BOTTOM;
        set(
            self.panel_group,
            left,
            GROUPS_TOP,
            panel_width,
            panel_bottom - GROUPS_TOP,
        );

        let app_top = panel_bottom + GROUP_SPACING;
        let mut rows = Rows::new(app_top + GROUP_PADDING_TOP, gap);
        place_combo(self.refresh, rows.next(combo_height));
        place_combo(self.language_select, rows.next(combo_height));
        place_combo(self.double_click, rows.next(combo_height));
        let y = rows.next(CHECK_HEIGHT);
        set(self.autostart, inside, y, 172, CHECK_HEIGHT);
        set(
            self.record_history,
            inside + 174,
            y,
            panel_width - 202,
            CHECK_HEIGHT,
        );
        let app_bottom = rows.end() + GROUP_PADDING_BOTTOM;
        set(self.app_group, left, app_top, panel_width, app_bottom - app_top);
        // 版本号放在常规设置下方、底部按钮之上的空白处。
        let version_y = (footer - FOOTER_SPACING - 22).max(app_bottom + 4);
        set(self.version, left + 2, version_y, panel_width, 22);

        let live_width = (right - live_x).max(1);
        set(self.live_heading, live_x, 15, live_width, 24);
        set(
            self.live_list,
            live_x,
            40,
            live_width,
            (footer - 52).max(1),
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
        fonts.apply_body(&[
            self.version,
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
            self.double_click.0,
            self.double_click.1,
            self.edit,
            self.export_history,
            self.record_history,
            self.autostart,
            self.reset,
            self.update,
            self.about,
            self.exit,
        ]);
        self.fonts = Some(fonts);
    }
}

/// 自上而下排布等间距的行。
struct Rows {
    y: i32,
    gap: i32,
    end: i32,
}

impl Rows {
    fn new(top: i32, gap: i32) -> Self {
        Self {
            y: top,
            gap,
            end: top,
        }
    }

    /// 返回下一行的顶部坐标。
    fn next(&mut self, height: i32) -> i32 {
        let top = self.y;
        self.end = top + height;
        self.y = self.end + self.gap;
        top
    }

    /// 最后一行的底部坐标。
    fn end(&self) -> i32 {
        self.end
    }
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

/// 用窗口底色擦除背景，但跳过分组框以外的子控件，效果等同 WS_CLIPCHILDREN 而保留分组框内部的擦除。
fn erase_background(hwnd: HWND, dc: HDC) {
    unsafe {
        let saved = SaveDC(dc);
        let mut child = GetWindow(hwnd, GW_CHILD);
        while let Ok(current) = child {
            let mut rect = RECT::default();
            if IsWindowVisible(current).as_bool()
                && !is_group_box(current)
                && GetWindowRect(current, &mut rect).is_ok()
            {
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
                MapWindowPoints(None, Some(hwnd), &mut corners);
                ExcludeClipRect(dc, corners[0].x, corners[0].y, corners[1].x, corners[1].y);
            }
            child = GetWindow(current, GW_HWNDNEXT);
        }
        let mut client = RECT::default();
        if GetClientRect(hwnd, &mut client).is_ok() {
            FillRect(dc, &client, GetSysColorBrush(COLOR_WINDOW));
        }
        let _ = RestoreDC(dc, saved);
    }
}

fn is_group_box(hwnd: HWND) -> bool {
    let mut class = [0u16; 16];
    let len = unsafe { GetClassNameW(hwnd, &mut class) } as usize;
    let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32;
    String::from_utf16_lossy(&class[..len]).eq_ignore_ascii_case("Button")
        && style & BS_TYPEMASK as u32 == BS_GROUPBOX as u32
}

fn selected(combo: HWND, id: usize) -> Option<usize> {
    let index = usize::try_from(send(combo, CB_GETCURSEL, 0, 0)).ok()?;
    match id {
        SELECT_POSITION => POSITION_COMMANDS.get(index).copied(),
        SELECT_THEME => THEME_COMMANDS.get(index).copied(),
        SELECT_LANGUAGE => LANGUAGE_COMMANDS.get(index).copied(),
        SELECT_REFRESH => REFRESH_OPTIONS.get(index).map(|&(command, _)| command),
        SELECT_DOUBLE_CLICK => DOUBLE_CLICK_OPTIONS.get(index).map(|&(command, _)| command),
        _ => None,
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        // 静态文本、复选框和分组框统一使用窗口底色，避免出现灰色色块。
        WM_CTLCOLORSTATIC => {
            unsafe { SetBkMode(HDC(wp.0 as *mut _), TRANSPARENT) };
            return LRESULT(unsafe { GetSysColorBrush(COLOR_WINDOW) }.0 as isize);
        }
        WM_COMMAND => {
            let id = wp.0 & 0xffff;
            let notification = (wp.0 >> 16) as u32;
            let source = HWND(lp.0 as *mut _);
            let selection = match (id, notification) {
                (id, CBN_SELCHANGE) => selected(source, id),
                (
                    TOGGLE | TOPMOST | DOCK | LABEL_BOLD | EDIT_METRICS | EXPORT_HISTORY
                    | RECORD_HISTORY | AUTOSTART | RESET_TRAFFIC | CHECK_UPDATE | ABOUT | EXIT,
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
        WM_ERASEBKGND => {
            erase_background(hwnd, HDC(wp.0 as *mut _));
            return LRESULT(1);
        }
        WM_DPICHANGED => {
            follow_dpi_change(hwnd, lp);
            with_manager(Manager::arrange);
        }
        WM_GETMINMAXINFO => {
            if let Ok((width, height)) =
                window_size(MIN_WIDTH, MIN_HEIGHT, window_dpi(hwnd), STYLE, EX_STYLE)
                && let Some(work) = work_area(hwnd)
            {
                let limits = unsafe { &mut *(lp.0 as *mut MINMAXINFO) };
                limits.ptMinTrackSize.x = width.min(work.right - work.left);
                limits.ptMinTrackSize.y = height.min(work.bottom - work.top);
            }
        }
        WM_NCDESTROY => {
            // 正在借用时无法立即清理；with_manager 下次调用时会发现窗口已销毁并释放状态。
            MANAGER.with(|slot| {
                if let Ok(mut slot) = slot.try_borrow_mut()
                    && slot.as_ref().is_some_and(|manager| manager.hwnd == hwnd)
                {
                    slot.take();
                }
            });
            return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
        }
        _ => return unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
    LRESULT(0)
}
