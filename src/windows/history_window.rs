use std::{cell::RefCell, io, sync::mpsc, thread};

use ::windows::{
    Win32::{
        Foundation::{
            COLORREF, FILETIME, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, SYSTEMTIME,
            WPARAM,
        },
        Graphics::Gdi::{
            BeginPaint, COLOR_3DLIGHT, COLOR_BTNFACE, COLOR_HIGHLIGHT, COLOR_WINDOW,
            COLOR_WINDOWTEXT, CreatePen, DEFAULT_GUI_FONT, DeleteObject, EndPaint, FillRect,
            GetMonitorInfoW, GetStockObject, GetSysColor, GetSysColorBrush, GetTextExtentPoint32W,
            HDC, HGDIOBJ, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow, PAINTSTRUCT,
            PS_SOLID, Polyline, SelectObject, SetBkMode, SetPixelV, SetTextColor, TRANSPARENT,
            TextOutW,
        },
        System::{
            LibraryLoader::GetModuleHandleW,
            Time::{
                FileTimeToSystemTime, GetTimeZoneInformationForYear, SystemTimeToFileTime,
                SystemTimeToTzSpecificLocalTime, TIME_ZONE_INFORMATION,
                TzSpecificLocalTimeToSystemTime,
            },
        },
        UI::{
            Controls::*,
            HiDpi::AdjustWindowRectExForDpi,
            Input::KeyboardAndMouse::{EnableWindow, SetFocus},
            WindowsAndMessaging::*,
        },
    },
    core::{HSTRING, PCWSTR, Result, w},
};

use crate::{
    history::{self, COLUMNS, Range, Series},
    language::Language,
    model::Metric,
};

use super::{
    Window, WorkerCommand, control, history_directory, invalidate, metric_name, notify, position,
    register_class, scale, send, window_dpi, with_app,
};

const CLASS: PCWSTR = w!("WindowsMonitor.History");
const STYLE: WINDOW_STYLE = WINDOW_STYLE(WS_OVERLAPPEDWINDOW.0 | WS_CLIPCHILDREN.0);
const EX_STYLE: WINDOW_EX_STYLE = WS_EX_CONTROLPARENT;
const QUERY_READY: u32 = WM_APP + 1;
const RECORD: usize = 11;
const METRIC: usize = 12;
const FROM: usize = 13;
const TO: usize = 14;
const NOW: usize = 15;
const WIDTH: i32 = 800;
const HEIGHT: i32 = 490;
const HOUR: u64 = 3_600_000;
const UNIX_FILETIME_MS: u64 = 11_644_473_600_000;

thread_local! {
    static HISTORY: RefCell<Option<HistoryWindow>> = const { RefCell::new(None) };
}

type QueryResult = io::Result<Series>;

enum Status {
    Empty,
    InvalidRange,
    Pending(mpsc::Receiver<QueryResult>),
    Loaded(Series),
    Failed(String),
}

struct HistoryWindow {
    hwnd: HWND,
    from_label: HWND,
    from: HWND,
    to_label: HWND,
    to: HWND,
    now: HWND,
    query: HWND,
    metric_label: HWND,
    metric_choice: HWND,
    record: HWND,
    retention: HWND,
    summary: HWND,
    capture_status: HWND,
    capture_error: Option<String>,
    language: Language,
    metric: Metric,
    status: Status,
}

pub fn open(owner: HWND, anchor: HWND) -> Result<()> {
    if let Some(hwnd) = with_history(|view| view.hwnd) {
        unsafe {
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            }
            let _ = SetForegroundWindow(hwnd);
        }
        return Ok(());
    }
    let Some((language, recording, icon)) = with_app(|app| {
        (
            app.settings.display.language,
            app.settings.record_history,
            app.tray.hIcon,
        )
    }) else {
        return Ok(());
    };
    let instance = unsafe { HINSTANCE(GetModuleHandleW(PCWSTR::null())?.0) };
    register_class(
        CLASS,
        instance,
        Some(history_proc),
        icon,
        COLOR_BTNFACE.0 as isize + 1,
    )?;

    let dpi = window_dpi(anchor);
    let mut area = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let mut bounds = RECT {
        right: scale(WIDTH, dpi),
        bottom: scale(HEIGHT, dpi),
        ..Default::default()
    };
    unsafe {
        GetMonitorInfoW(
            MonitorFromWindow(anchor, MONITOR_DEFAULTTONEAREST),
            &mut area,
        )
        .ok()?;
        AdjustWindowRectExForDpi(&mut bounds, STYLE, false, EX_STYLE, dpi)?;
    }
    let work = area.rcWork;
    let width = (bounds.right - bounds.left).min(work.right - work.left);
    let height = (bounds.bottom - bounds.top).min(work.bottom - work.top);
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
            Some(owner),
            None,
            Some(instance),
            None,
        )?
    });
    let hwnd = window.0;
    let mut view = HistoryWindow::new(hwnd, language, recording)?;
    view.capture_error = capture_error();
    view.update_status();
    view.arrange();
    let focus = view.from;
    HISTORY.with(|slot| *slot.borrow_mut() = Some(view));
    std::mem::forget(window);
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(focus));
    }
    with_history(|view| view.start_query());
    Ok(())
}

impl HistoryWindow {
    fn new(hwnd: HWND, language: Language, recording: bool) -> Result<Self> {
        unsafe {
            InitCommonControlsEx(&INITCOMMONCONTROLSEX {
                dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
                dwICC: ICC_DATE_CLASSES | ICC_STANDARD_CLASSES,
            })
            .ok()?;
        }
        let label = || control(hwnd, WC_STATICW, PCWSTR::null(), 0, 0);
        let date = |id| control(hwnd, DATETIMEPICK_CLASSW, PCWSTR::null(), WS_TABSTOP.0, id);
        let mut view = Self {
            hwnd,
            from_label: label()?,
            from: date(FROM)?,
            to_label: label()?,
            to: date(TO)?,
            now: control(hwnd, WC_BUTTONW, PCWSTR::null(), WS_TABSTOP.0, NOW)?,
            query: control(
                hwnd,
                WC_BUTTONW,
                PCWSTR::null(),
                WS_TABSTOP.0 | BS_DEFPUSHBUTTON as u32,
                IDOK.0 as usize,
            )?,
            metric_label: label()?,
            metric_choice: control(
                hwnd,
                WC_COMBOBOXW,
                PCWSTR::null(),
                WS_TABSTOP.0 | CBS_DROPDOWNLIST as u32 | WS_VSCROLL.0,
                METRIC,
            )?,
            record: control(
                hwnd,
                WC_BUTTONW,
                PCWSTR::null(),
                WS_TABSTOP.0 | BS_AUTOCHECKBOX as u32,
                RECORD,
            )?,
            retention: label()?,
            summary: label()?,
            capture_status: label()?,
            capture_error: None,
            language,
            metric: Metric::Cpu,
            status: Status::Empty,
        };
        let font = unsafe { GetStockObject(DEFAULT_GUI_FONT) };
        for handle in [
            view.from_label,
            view.from,
            view.to_label,
            view.to,
            view.now,
            view.query,
            view.metric_label,
            view.metric_choice,
            view.record,
            view.retention,
            view.summary,
            view.capture_status,
        ] {
            send(handle, WM_SETFONT, font.0 as usize, 0);
        }
        let now = history::now_ms();
        for (picker, time) in [(view.from, now.saturating_sub(HOUR)), (view.to, now)] {
            send(
                picker,
                DTM_SETFORMATW,
                0,
                w!("yyyy-MM-dd HH:mm:ss").0 as isize,
            );
            set_picker_time(picker, time)?;
        }
        send(view.record, BM_SETCHECK, usize::from(recording), 0);
        view.translate_controls();
        Ok(view)
    }

    fn translate_controls(&mut self) {
        let language = self.language;
        unsafe {
            let _ = SetWindowTextW(self.hwnd, language.choose(w!("历史记录"), w!("History")));
            let _ = SetWindowTextW(self.from_label, language.choose(w!("开始"), w!("From")));
            let _ = SetWindowTextW(self.to_label, language.choose(w!("结束"), w!("To")));
            let _ = SetWindowTextW(self.now, language.choose(w!("现在"), w!("Now")));
            let _ = SetWindowTextW(self.query, language.choose(w!("查询"), w!("Query")));
            let _ = SetWindowTextW(self.metric_label, language.choose(w!("指标"), w!("Metric")));
            let _ = SetWindowTextW(
                self.record,
                language.choose(w!("记录历史数据"), w!("Record history")),
            );
            let _ = SetWindowTextW(
                self.retention,
                language.choose(w!("最近 7 天 · 本机时间"), w!("Last 7 days · local time")),
            );
        }
        let choice = send(self.metric_choice, CB_GETCURSEL, 0, 0);
        send(self.metric_choice, CB_RESETCONTENT, 0, 0);
        for metric in Metric::ALL {
            let name = HSTRING::from(metric_name(metric, language));
            send(self.metric_choice, CB_ADDSTRING, 0, name.as_ptr() as isize);
        }
        let choice = usize::try_from(choice).unwrap_or(self.metric.index());
        send(self.metric_choice, CB_SETCURSEL, choice, 0);
        self.update_status();
    }

    fn arrange(&self) {
        let dpi = window_dpi(self.hwnd);
        let px = |size| scale(size, dpi);
        let mut area = RECT::default();
        let _ = unsafe { GetClientRect(self.hwnd, &mut area) };
        let left = px(16);
        let gap = px(8);
        let label = px(34);
        let now = px(60);
        let query = px(76);
        let height = px(26);
        let date = ((area.right - 2 * left - 2 * label - 3 * gap - now - query) / 2).max(px(166));
        let to = left + label + date + gap;
        let buttons = to + label + date + gap;
        position(self.from_label, left, px(20), label, height);
        position(self.from, left + label, px(15), date, height);
        position(self.to_label, to, px(20), label, height);
        position(self.to, to + label, px(15), date, height);
        position(self.now, buttons, px(15), now, height);
        position(self.query, buttons + now + gap, px(15), query, height);
        position(self.metric_label, left, px(54), px(50), height);
        position(self.metric_choice, left + px(50), px(49), px(226), px(290));
        position(self.record, left + px(286), px(49), px(144), height);
        position(
            self.retention,
            left + px(438),
            px(54),
            (area.right - left * 2 - px(438)).max(0),
            height,
        );
        position(
            self.summary,
            left,
            px(88),
            (area.right - 2 * left).max(0),
            height,
        );
        position(
            self.capture_status,
            left,
            area.bottom - px(26),
            (area.right - 2 * left).max(0),
            px(22),
        );
    }

    fn selected_range(&self, now: u64) -> Option<Range> {
        let start = picker_time(self.from)?;
        let end = picker_time(self.to)?;
        if start > end {
            return None;
        }
        Range::recent(start, end.saturating_add(999), now)
    }

    fn start_query(&mut self) {
        if matches!(self.status, Status::Pending(_)) {
            return;
        }
        let selected = send(self.metric_choice, CB_GETCURSEL, 0, 0);
        if let Some(&metric) = Metric::ALL.get(selected as usize) {
            self.metric = metric;
        }
        self.status = match (self.selected_range(history::now_ms()), history_directory()) {
            (None, _) => Status::InvalidRange,
            (_, None) => Status::Failed("LOCALAPPDATA".into()),
            (Some(range), Some(directory)) => {
                let (result, receiver) = mpsc::channel();
                let metric = self.metric;
                let handle = self.hwnd.0 as usize;
                match thread::Builder::new()
                    .name("monitor-history-query".into())
                    .spawn(move || {
                        let data = history::query(&directory, range, metric);
                        if result.send(data).is_ok() {
                            notify(HWND(handle as *mut _), QUERY_READY);
                        }
                    }) {
                    Ok(_) => Status::Pending(receiver),
                    Err(error) => Status::Failed(error.to_string()),
                }
            }
        };
        self.update_status();
    }

    fn finish_query(&mut self) {
        if let Status::Pending(receiver) = &self.status {
            self.status = match receiver.try_recv() {
                Ok(Ok(data)) => Status::Loaded(data),
                Ok(Err(error)) => Status::Failed(error.to_string()),
                Err(_) => return,
            };
            self.update_status();
        }
    }

    fn update_status(&self) {
        let language = self.language;
        let summary = match &self.status {
            Status::Empty => String::new(),
            Status::InvalidRange => language
                .choose(
                    "请选择最近 7 天内的起止时间，结束时间应不早于开始时间。",
                    "Choose a range within the last 7 days. End must be at or after start.",
                )
                .into(),
            Status::Pending(_) => language.choose("正在查询…", "Querying…").into(),
            Status::Loaded(data) if data.count == 0 => language
                .choose(
                    "该时间范围内没有可用样本。",
                    "No samples in this time range.",
                )
                .into(),
            Status::Loaded(data) => {
                let unit = Unit::for_metric(self.metric, data.max);
                let min = unit.format(data.min);
                let mean = unit.format(data.average());
                let max = unit.format(data.max);
                let count = data.count;
                if language == Language::Chinese {
                    format!("{count} 个样本　最小 {min}　平均 {mean}　峰值 {max}")
                } else {
                    format!("{count} samples     Min {min}     Mean {mean}     Peak {max}")
                }
            }
            Status::Failed(error) => format!(
                "{}  {error}",
                language.choose("查询失败。", "Query failed.")
            ),
        };
        let capture = self
            .capture_error
            .as_ref()
            .map_or_else(String::new, |error| {
                format!(
                    "{}  {error}",
                    language.choose("无法写入历史记录。", "Could not record history.")
                )
            });
        let enabled = !matches!(self.status, Status::Pending(_));
        unsafe {
            let _ = SetWindowTextW(self.summary, &HSTRING::from(summary));
            let _ = SetWindowTextW(self.capture_status, &HSTRING::from(capture));
            let _ = ShowWindow(
                self.capture_status,
                if self.capture_error.is_some() {
                    SW_SHOWNA
                } else {
                    SW_HIDE
                },
            );
            for control in [self.query, self.now, self.metric_choice] {
                let _ = EnableWindow(control, enabled);
            }
        }
        invalidate(self.hwnd);
    }

    fn paint(&self, dc: HDC) {
        let mut area = RECT::default();
        let _ = unsafe { GetClientRect(self.hwnd, &mut area) };
        let dpi = window_dpi(self.hwnd);
        let px = |size| scale(size, dpi);
        let canvas = RECT {
            left: px(16),
            top: px(116),
            right: area.right - px(16),
            bottom: area.bottom - px(if self.capture_error.is_some() { 31 } else { 16 }),
        };
        unsafe {
            FillRect(dc, &area, GetSysColorBrush(COLOR_BTNFACE));
            FillRect(dc, &canvas, GetSysColorBrush(COLOR_WINDOW));
        }
        let Status::Loaded(data) = &self.status else {
            return;
        };
        let graph = RECT {
            left: canvas.left + px(84),
            top: canvas.top + px(16),
            right: canvas.right - px(18),
            bottom: canvas.bottom - px(40),
        };
        if graph.right <= graph.left || graph.bottom <= graph.top {
            return;
        }
        let unit = Unit::for_metric(self.metric, data.max);
        let ceiling = unit.ceiling(data.max);
        let width = graph.right - graph.left;
        let height = graph.bottom - graph.top;
        let point = |index: usize, value: f64| POINT {
            x: graph.left + (index as i32 * width) / (COLUMNS as i32 - 1),
            y: graph.bottom - (value / ceiling * f64::from(height)).round() as i32,
        };
        let font = unsafe {
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, COLORREF(GetSysColor(COLOR_WINDOWTEXT)));
            SelectObject(dc, GetStockObject(DEFAULT_GUI_FONT))
        };
        with_pen(
            dc,
            COLORREF(unsafe { GetSysColor(COLOR_3DLIGHT) }),
            1,
            || {
                for tick in 0..=4 {
                    let value = ceiling * f64::from(tick) / 4.0;
                    let y = point(0, value).y;
                    line(dc, POINT { x: graph.left, y }, POINT { x: graph.right, y });
                    let label = unit.format(value);
                    let (text, size) = measure(dc, &label);
                    unsafe {
                        let _ = TextOutW(dc, graph.left - px(8) - size.cx, y - size.cy / 2, &text);
                    }
                }
            },
        );
        for tick in 0..=2 {
            let time = data.range.start + (data.range.end - data.range.start) * tick / 2;
            if let Ok(time) = local_time(time) {
                let label = timestamp(time);
                let (text, size) = measure(dc, &label);
                let x = graph.left + width * tick as i32 / 2 - size.cx / 2;
                unsafe {
                    let _ = TextOutW(
                        dc,
                        x.clamp(canvas.left, (canvas.right - size.cx).max(canvas.left)),
                        graph.bottom + px(10),
                        &text,
                    );
                }
            }
        }
        let ink = COLORREF(unsafe { GetSysColor(COLOR_HIGHLIGHT) });
        with_pen(dc, ink, px(2), || {
            for (index, column) in data.columns.iter().enumerate() {
                let Some(column) = column else { continue };
                if let Some(previous) = column.join {
                    let last = data.columns[previous].unwrap().last;
                    line(dc, point(previous, last), point(index, column.first));
                }
                let low = point(index, column.min);
                let high = point(index, column.max);
                if low == high {
                    unsafe {
                        let _ = SetPixelV(dc, low.x, low.y, ink);
                    }
                } else {
                    line(dc, low, high);
                }
            }
        });
        unsafe { SelectObject(dc, font) };
    }
}

struct Unit {
    scale: f64,
    label: &'static str,
}

impl Unit {
    fn for_metric(metric: Metric, max: f64) -> Self {
        let (scale, label) = match metric {
            Metric::Cpu
            | Metric::Memory
            | Metric::Disk
            | Metric::AvailableMemoryPercent
            | Metric::Battery
            | Metric::GpuMemoryPercent => (1.0, "%"),
            Metric::CpuFrequency => (1_000.0, "GHz"),
            Metric::AvailableMemory | Metric::UsedMemory | Metric::UsedGpuMemory => (1e9, "GB"),
            Metric::Uptime if max >= 86_400.0 => (86_400.0, "d"),
            Metric::Uptime if max >= 3600.0 => (3600.0, "h"),
            Metric::Uptime if max >= 60.0 => (60.0, "min"),
            Metric::Uptime => (1.0, "s"),
            Metric::Traffic => byte_unit(max, ["B", "KB", "MB", "GB", "TB", "PB", "EB"]),
            Metric::Upload | Metric::Download | Metric::DiskRead | Metric::DiskWrite => {
                byte_unit(max, ["B/s", "KB/s", "MB/s", "GB/s", "TB/s", "PB/s", "EB/s"])
            }
        };
        Self { scale, label }
    }

    fn format(&self, value: f64) -> String {
        let value = value / self.scale;
        let precision = if value > 0.0 && value < 0.01 {
            4
        } else if value < 100.0 {
            2
        } else {
            1
        };
        let number = format!("{value:.precision$}");
        let number = number.trim_end_matches('0').trim_end_matches('.');
        format!("{number} {}", self.label)
    }

    fn ceiling(&self, max: f64) -> f64 {
        if self.label == "%" {
            return 100.0_f64.max(max);
        }
        let max = max / self.scale;
        if max <= 0.0 {
            return self.scale;
        }
        let magnitude = 10.0_f64.powf(max.log10().floor());
        let top = [1.0, 2.0, 2.5, 5.0, 10.0]
            .into_iter()
            .find(|step| max <= step * magnitude)
            .unwrap_or(10.0);
        top * magnitude * self.scale
    }
}

fn byte_unit(max: f64, labels: [&'static str; 7]) -> (f64, &'static str) {
    let mut scale = 1.0;
    let mut label = labels[0];
    for next in &labels[1..] {
        if max < scale * 1000.0 {
            break;
        }
        scale *= 1000.0;
        label = next;
    }
    (scale, label)
}

fn timestamp(time: SYSTEMTIME) -> String {
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        time.wYear, time.wMonth, time.wDay, time.wHour, time.wMinute, time.wSecond
    )
}

fn local_time(time: u64) -> Result<SYSTEMTIME> {
    let ticks = (time + UNIX_FILETIME_MS) * 10_000;
    let filetime = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    let mut utc = SYSTEMTIME::default();
    let mut local = SYSTEMTIME::default();
    unsafe {
        FileTimeToSystemTime(&filetime, &mut utc)?;
        SystemTimeToTzSpecificLocalTime(None, &utc, &mut local)?;
        let zone = timezone_for_year(local.wYear)?;
        SystemTimeToTzSpecificLocalTime(Some(&zone), &utc, &mut local)?;
    }
    Ok(local)
}

fn timezone_for_year(year: u16) -> Result<TIME_ZONE_INFORMATION> {
    let mut zone = TIME_ZONE_INFORMATION::default();
    unsafe { GetTimeZoneInformationForYear(year, None, &mut zone)? };
    Ok(zone)
}

fn set_picker_time(hwnd: HWND, time: u64) -> Result<()> {
    let local = local_time(time)?;
    send(
        hwnd,
        DTM_SETSYSTEMTIME,
        GDT_VALID.0 as usize,
        &raw const local as isize,
    );
    Ok(())
}

fn picker_time(hwnd: HWND) -> Option<u64> {
    let mut local = SYSTEMTIME::default();
    let mut utc = SYSTEMTIME::default();
    let mut filetime = FILETIME::default();
    if send(hwnd, DTM_GETSYSTEMTIME, 0, &mut local as *mut _ as isize) != GDT_VALID.0 as isize {
        return None;
    }
    local.wMilliseconds = 0;
    let zone = timezone_for_year(local.wYear).ok()?;
    unsafe {
        TzSpecificLocalTimeToSystemTime(Some(&zone), &local, &mut utc).ok()?;
        SystemTimeToFileTime(&utc, &mut filetime).ok()?;
    }
    let ticks = (u64::from(filetime.dwHighDateTime) << 32) | u64::from(filetime.dwLowDateTime);
    let epoch = (ticks / 10_000).checked_sub(UNIX_FILETIME_MS)?;
    // Reject nonexistent local times when clocks move forward.
    (timestamp(local_time(epoch).ok()?) == timestamp(local)).then_some(epoch)
}

fn measure(dc: HDC, text: &str) -> (Vec<u16>, SIZE) {
    let text: Vec<_> = text.encode_utf16().collect();
    let mut size = SIZE::default();
    unsafe {
        let _ = GetTextExtentPoint32W(dc, &text, &mut size);
    }
    (text, size)
}

fn with_pen(dc: HDC, color: COLORREF, width: i32, draw: impl FnOnce()) {
    let pen = unsafe { HGDIOBJ(CreatePen(PS_SOLID, width, color).0) };
    if !pen.is_invalid() {
        let old = unsafe { SelectObject(dc, pen) };
        draw();
        unsafe {
            SelectObject(dc, old);
            let _ = DeleteObject(pen);
        }
    }
}

fn line(dc: HDC, a: POINT, b: POINT) {
    unsafe {
        let _ = Polyline(dc, &[a, b]);
    }
}

fn with_history<T>(run: impl FnOnce(&mut HistoryWindow) -> T) -> Option<T> {
    HISTORY.with(|slot| slot.try_borrow_mut().ok()?.as_mut().map(run))
}

fn capture_error() -> Option<String> {
    with_app(|app| {
        if app.settings.record_history {
            app.feed.lock().unwrap().history_error.clone()
        } else {
            None
        }
    })
    .flatten()
}

pub fn refresh_capture_status() {
    let error = capture_error();
    with_history(|view| {
        view.capture_error = error;
        view.update_status();
    });
}

pub fn set_language(language: Language) {
    with_history(|view| {
        view.language = language;
        view.translate_controls();
    });
}

pub fn handle_dialog_message(message: &MSG) -> bool {
    with_history(|view| view.hwnd).is_some_and(|hwnd| unsafe {
        (message.hwnd == hwnd || IsChild(hwnd, message.hwnd).as_bool())
            && IsDialogMessageW(hwnd, message).as_bool()
    })
}

unsafe extern "system" fn history_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_SIZE => {
            with_history(|view| view.arrange());
            invalidate(hwnd);
        }
        WM_GETMINMAXINFO => {
            let dpi = window_dpi(hwnd);
            let info = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
            info.ptMinTrackSize = POINT {
                x: scale(622, dpi),
                y: scale(330, dpi),
            };
        }
        WM_DPICHANGED => {
            let bounds = unsafe { &*(lparam.0 as *const RECT) };
            position(
                hwnd,
                bounds.left,
                bounds.top,
                bounds.right - bounds.left,
                bounds.bottom - bounds.top,
            );
            with_history(|view| view.arrange());
            invalidate(hwnd);
        }
        WM_COMMAND => match (wparam.0 & 0xffff, (wparam.0 >> 16) as u32) {
            (id, _) if id == IDOK.0 as usize => {
                with_history(|view| view.start_query());
            }
            (METRIC, CBN_SELCHANGE) => {
                with_history(|view| view.start_query());
            }
            (NOW, BN_CLICKED) => {
                with_history(|view| {
                    if set_picker_time(view.to, history::now_ms()).is_ok() {
                        view.start_query();
                    }
                });
            }
            (RECORD, code) if code == BN_CLICKED => {
                let enabled = send(HWND(lparam.0 as *mut _), BM_GETCHECK, 0, 0) != 0;
                with_app(|app| {
                    app.settings.record_history = enabled;
                    let _ = app.worker.send(WorkerCommand::RecordHistory(enabled));
                    app.save();
                });
                refresh_capture_status();
            }
            (id, _) if id == IDCANCEL.0 as usize => {
                let _ = unsafe { DestroyWindow(hwnd) };
            }
            _ => {}
        },
        QUERY_READY => {
            with_history(|view| view.finish_query());
        }
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            let dc = unsafe { BeginPaint(hwnd, &mut paint) };
            with_history(|view| view.paint(dc));
            unsafe {
                let _ = EndPaint(hwnd, &paint);
            }
        }
        WM_DESTROY => {
            HISTORY.with(|slot| slot.borrow_mut().take());
        }
        _ => return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
    LRESULT(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_time_controls_select_a_range_to_the_second() {
        let window = Window(
            unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    WC_STATICW,
                    PCWSTR::null(),
                    WS_POPUP,
                    0,
                    0,
                    WIDTH,
                    HEIGHT,
                    None,
                    None,
                    None,
                    None,
                )
            }
            .unwrap(),
        );
        let view = HistoryWindow::new(window.0, Language::English, false).unwrap();
        let start = 1_768_478_400_000; // 2026-01-15 12:00:00 UTC
        let end = start + 21_000;
        let now = end + 60_000;
        set_picker_time(view.from, start).unwrap();
        set_picker_time(view.to, end).unwrap();
        assert_eq!(
            view.selected_range(now),
            Some(Range {
                start,
                end: end + 999
            })
        );

        set_picker_time(view.from, end + 1_000).unwrap();
        assert!(view.selected_range(now).is_none());
    }
}
