use std::cell::{Cell, RefCell};

use ::windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Gdi::{
            BeginPaint, COLOR_BTNFACE, COLOR_GRAYTEXT, COLOR_HIGHLIGHT, EndPaint, FillRect,
            GetSysColorBrush, MapWindowPoints, PAINTSTRUCT,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Controls::*,
            Input::KeyboardAndMouse::{
                GetCapture, ReleaseCapture, SetCapture, SetFocus, VK_ESCAPE,
            },
            WindowsAndMessaging::*,
        },
    },
    core::{PCWSTR, PWSTR, Result, w},
};

use super::panel::{Appearance, Layout, Panel};
use super::state::{Density, DisplayOptions, TextSize, Theme};
use super::{
    Window, control, invalidate, metric_name, position, register_class, scale, send, set_display,
    ui::{Fonts, centered_bounds, follow_dpi_change, init_common_controls},
    window_dpi, with_app,
};
use crate::{
    language::Language,
    model::{Metric, readings},
};

const CLASS: PCWSTR = w!("WindowsMonitor.MetricsEditor");
const PREVIEW_CLASS: PCWSTR = w!("WindowsMonitor.MetricsPreview");
const EDITOR_STYLE: WINDOW_STYLE = WINDOW_STYLE(WS_CAPTION.0 | WS_SYSMENU.0 | WS_CLIPCHILDREN.0);
const EX_STYLE: WINDOW_EX_STYLE = WINDOW_EX_STYLE(WS_EX_CONTROLPARENT.0 | WS_EX_DLGMODALFRAME.0);
const LIST: usize = 10;
const UP: usize = 11;
const DOWN: usize = 12;
const PREVIEW_MODE: usize = 13;
const TEXT_SIZE: usize = 14;
const DENSITY: usize = 15;
const GRIP_COLUMN: usize = 1;
const PREVIEW_RETRY_TIMER: usize = 1;
const GRIP_WIDTH: i32 = 24;
const WIDTH: i32 = 818;
const HEIGHT: i32 = 368;
const DENSITY_OPTIONS: [Density; 3] = [Density::Standard, Density::Compact, Density::Tight];

thread_local! {
    static EDITOR: RefCell<Option<Editor>> = const { RefCell::new(None) };
}

struct Editor {
    hwnd: HWND,
    owner: HWND,
    hint: HWND,
    list: HWND,
    up: HWND,
    down: HWND,
    mode: HWND,
    size_label: HWND,
    size: HWND,
    density_label: HWND,
    density: HWND,
    preview: HWND,
    apply: HWND,
    cancel: HWND,
    panel: Panel,
    display: DisplayOptions,
    layout: Layout,
    preview_dpi: u32,
    /// 预览区顶部（像素），由右侧下拉框的实际高度决定。
    preview_top: i32,
    drag: Option<Drag>,
    fonts: Option<Fonts>,
    /// 程序写入列表项（插入、移动、切换语言）期间为 true。此时列表视图同步发出的
    /// LVN_ITEMCHANGED 只是回显，不能反过来改写勾选状态。
    syncing: Cell<bool>,
}

struct Drag {
    from: usize,
    // Index of the gap between rows in the original list.
    slot: Option<usize>,
}

impl Drag {
    fn destination(&self) -> Option<usize> {
        self.slot.map(|slot| slot - usize::from(slot > self.from))
    }
}

pub fn open(owner: HWND, anchor: HWND) -> Result<()> {
    if let Some(hwnd) = with_editor(|editor| editor.hwnd) {
        unsafe {
            let _ = SetForegroundWindow(hwnd);
        }
        return Ok(());
    }
    let Some((display, layout, icon)) = with_app(|app| {
        let layout = if app.settings.taskbar {
            Layout::Taskbar
        } else {
            Layout::Floating
        };
        (app.settings.display, layout, app.tray.hIcon)
    }) else {
        return Ok(());
    };
    let instance = unsafe { HINSTANCE(GetModuleHandleW(PCWSTR::null())?.0) };
    init_common_controls()?;
    register_class(
        CLASS,
        instance,
        WNDCLASS_STYLES::default(),
        Some(editor_proc),
        icon,
        HBRUSH_BUTTON_FACE,
    )?;
    register_class(
        PREVIEW_CLASS,
        instance,
        WNDCLASS_STYLES::default(),
        Some(preview_proc),
        HICON::default(),
        0,
    )?;
    let bounds = centered_bounds(anchor, WIDTH, HEIGHT, EDITOR_STYLE, EX_STYLE)?;
    let window = Window(unsafe {
        CreateWindowExW(
            EX_STYLE,
            CLASS,
            PCWSTR::null(),
            EDITOR_STYLE,
            bounds.x,
            bounds.y,
            bounds.width,
            bounds.height,
            Some(owner),
            None,
            Some(instance),
            None,
        )?
    });
    let editor = Editor::new(window.0, owner, display, layout)?;
    let hwnd = window.0;
    let list = editor.list;
    EDITOR.with(|slot| *slot.borrow_mut() = Some(editor));
    std::mem::forget(window);
    with_editor(|editor| editor.arrange());
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(list));
    }
    Ok(())
}

const HBRUSH_BUTTON_FACE: isize = COLOR_BTNFACE.0 as isize + 1;

impl Editor {
    fn new(hwnd: HWND, owner: HWND, display: DisplayOptions, layout: Layout) -> Result<Self> {
        let hint = control(hwnd, WC_STATICW, PCWSTR::null(), 0, 0)?;
        let list = control(
            hwnd,
            WC_LISTVIEWW,
            PCWSTR::null(),
            WS_TABSTOP.0
                | WS_BORDER.0
                | LVS_REPORT
                | LVS_NOCOLUMNHEADER
                | LVS_SINGLESEL
                | LVS_SHOWSELALWAYS,
            LIST,
        )?;
        let up = control(
            hwnd,
            WC_BUTTONW,
            PCWSTR::null(),
            WS_TABSTOP.0 | BS_PUSHBUTTON as u32,
            UP,
        )?;
        let down = control(
            hwnd,
            WC_BUTTONW,
            PCWSTR::null(),
            WS_TABSTOP.0 | BS_PUSHBUTTON as u32,
            DOWN,
        )?;
        let mode = control(
            hwnd,
            WC_COMBOBOXW,
            PCWSTR::null(),
            WS_TABSTOP.0 | CBS_DROPDOWNLIST as u32,
            PREVIEW_MODE,
        )?;
        let size_label = control(hwnd, WC_STATICW, PCWSTR::null(), 0, 0)?;
        let size = control(
            hwnd,
            WC_COMBOBOXW,
            PCWSTR::null(),
            WS_TABSTOP.0 | WS_VSCROLL.0 | CBS_DROPDOWNLIST as u32,
            TEXT_SIZE,
        )?;
        let density_label = control(hwnd, WC_STATICW, PCWSTR::null(), 0, 0)?;
        let density = control(
            hwnd,
            WC_COMBOBOXW,
            PCWSTR::null(),
            WS_TABSTOP.0 | CBS_DROPDOWNLIST as u32,
            DENSITY,
        )?;
        let preview = control(hwnd, PREVIEW_CLASS, PCWSTR::null(), 0, 0)?;
        let apply = control(
            hwnd,
            WC_BUTTONW,
            PCWSTR::null(),
            WS_TABSTOP.0 | BS_DEFPUSHBUTTON as u32,
            IDOK.0 as usize,
        )?;
        let cancel = control(
            hwnd,
            WC_BUTTONW,
            PCWSTR::null(),
            WS_TABSTOP.0 | BS_PUSHBUTTON as u32,
            IDCANCEL.0 as usize,
        )?;
        let styles = LVS_EX_CHECKBOXES | LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER;
        send(
            list,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            styles as usize,
            styles as isize,
        );
        let column = LVCOLUMNW {
            mask: LVCF_WIDTH,
            ..Default::default()
        };
        send(list, LVM_INSERTCOLUMNW, 0, &column as *const _ as isize);
        send(
            list,
            LVM_INSERTCOLUMNW,
            GRIP_COLUMN,
            &column as *const _ as isize,
        );
        // The first logical column owns the native checkbox and metric name.
        let columns = [GRIP_COLUMN as i32, 0];
        send(
            list,
            LVM_SETCOLUMNORDERARRAY,
            columns.len(),
            columns.as_ptr() as isize,
        );
        for pixels in TextSize::MIN..=TextSize::MAX {
            let text = format!("{pixels} px\0").encode_utf16().collect::<Vec<_>>();
            send(size, CB_ADDSTRING, 0, text.as_ptr() as isize);
        }
        send(
            size,
            CB_SETCURSEL,
            (display.text_size.pixels() - TextSize::MIN) as usize,
            0,
        );
        let editor = Self {
            hwnd,
            owner,
            hint,
            list,
            up,
            down,
            mode,
            size_label,
            size,
            density_label,
            density,
            preview,
            apply,
            cancel,
            panel: Panel::new(display.language)?,
            display,
            layout,
            preview_dpi: window_dpi(hwnd),
            preview_top: scale(119, window_dpi(hwnd)),
            drag: None,
            fonts: None,
            syncing: Cell::new(false),
        };
        for (index, metric) in display.metrics.ordered().into_iter().enumerate() {
            editor.write_item(index, metric, LVM_INSERTITEMW);
        }
        editor.translate_controls();
        editor.select(0);
        Ok(editor)
    }

    fn translate_controls(&self) {
        let language = self.display.language;
        for (hwnd, text) in [
            (
                self.hwnd,
                language.choose(w!("编辑监控项"), w!("Edit Metrics")),
            ),
            (
                self.hint,
                language.choose(
                    w!("勾选要显示的项目\r\n按住行首把手，上下拖动排序"),
                    w!("Choose metrics to display\r\nDrag the handles to reorder."),
                ),
            ),
            (self.up, language.choose(w!("上移"), w!("Move Up"))),
            (self.down, language.choose(w!("下移"), w!("Move Down"))),
            (
                self.size_label,
                language.choose(w!("显示大小"), w!("Display Size")),
            ),
            (
                self.density_label,
                language.choose(w!("紧凑度"), w!("Density")),
            ),
            (self.apply, language.choose(w!("应用"), w!("Apply"))),
            (self.cancel, language.choose(w!("取消"), w!("Cancel"))),
        ] {
            let _ = unsafe { SetWindowTextW(hwnd, text) };
        }
        send(self.mode, CB_RESETCONTENT, 0, 0);
        for label in [
            language.choose(w!("浮窗预览"), w!("Floating Preview")),
            language.choose(w!("任务栏预览"), w!("Taskbar Preview")),
        ] {
            send(self.mode, CB_ADDSTRING, 0, label.as_ptr() as isize);
        }
        send(
            self.mode,
            CB_SETCURSEL,
            usize::from(self.layout == Layout::Taskbar),
            0,
        );

        send(self.density, CB_RESETCONTENT, 0, 0);
        let labels = language.choose(
            [w!("标准"), w!("紧凑"), w!("更紧凑")],
            [w!("Standard"), w!("Compact"), w!("Tight")],
        );
        for (index, (choice, label)) in DENSITY_OPTIONS.into_iter().zip(labels).enumerate() {
            send(self.density, CB_ADDSTRING, 0, label.as_ptr() as isize);
            if choice == self.display.density {
                send(self.density, CB_SETCURSEL, index, 0);
            }
        }
    }

    fn arrange(&mut self) {
        let dpi = window_dpi(self.hwnd);
        let mut bounds = RECT::default();
        if unsafe { GetClientRect(self.hwnd, &mut bounds) }.is_err() {
            return;
        }
        if self.fonts.as_ref().is_none_or(|fonts| fonts.dpi != dpi) {
            let fonts = Fonts::new(dpi);
            fonts.apply_body(&[
                self.hint,
                self.list,
                self.up,
                self.down,
                self.mode,
                self.size_label,
                self.size,
                self.density_label,
                self.density,
                self.apply,
                self.cancel,
            ]);
            self.fonts = Some(fonts);
        }
        let line_height = self
            .fonts
            .as_ref()
            .map_or(scale(16, dpi), |fonts| fonts.line_height);
        let margin = scale(16, dpi);
        let gap = scale(8, dpi);
        let button = scale(78, dpi);
        let button_height = scale(27, dpi).max(line_height + scale(10, dpi));
        let list_width = scale(214, dpi).min(bounds.right / 2);
        // 两行提示文字的高度随系统字体（含“使文本更大”）变化。
        let hint_height = scale(36, dpi).max(2 * line_height + scale(4, dpi));
        let list_top = margin + hint_height + scale(4, dpi);
        let buttons_top = bounds.bottom - margin - button_height;
        let preview_left = 2 * margin + list_width;
        for (hwnd, x, y, width, height) in [
            (self.hint, margin, margin, list_width + margin, hint_height),
            (
                self.list,
                margin,
                list_top,
                list_width,
                buttons_top - list_top - gap,
            ),
            (self.up, margin, buttons_top, button, button_height),
            (
                self.down,
                margin + button + gap,
                buttons_top,
                button,
                button_height,
            ),
            (
                self.apply,
                bounds.right - margin - 2 * button - gap,
                buttons_top,
                button,
                button_height,
            ),
            (
                self.cancel,
                bounds.right - margin - button,
                buttons_top,
                button,
                button_height,
            ),
        ] {
            position(hwnd, x, y, width.max(1), height.max(1));
        }

        // 右侧下拉框自上而下排布；收起时的高度由字体决定，标签与之垂直居中。
        let field_left = preview_left + scale(68, dpi);
        let mut y = margin;
        position(self.mode, preview_left, y, scale(140, dpi), scale(130, dpi));
        y += combo_height(self.mode, dpi) + scale(10, dpi);
        for (label, field, dropdown, spacing) in [
            (self.size_label, self.size, 240, 9),
            (self.density_label, self.density, 124, 15),
        ] {
            position(field, field_left, y, scale(86, dpi), scale(dropdown, dpi));
            let height = combo_height(field, dpi);
            position(
                label,
                preview_left,
                y + (height - line_height) / 2,
                scale(64, dpi),
                line_height,
            );
            y += height + scale(spacing, dpi);
        }
        self.preview_top = y;

        let mut list_bounds = RECT::default();
        if unsafe { GetClientRect(self.list, &mut list_bounds) }.is_ok() {
            let grip = scale(GRIP_WIDTH, dpi);
            send(self.list, LVM_SETCOLUMNWIDTH, GRIP_COLUMN, grip as isize);
            send(
                self.list,
                LVM_SETCOLUMNWIDTH,
                0,
                (list_bounds.right - grip).max(0) as isize,
            );
        }
        self.arrange_preview();
    }

    fn arrange_preview(&mut self) {
        let dpi = window_dpi(self.hwnd);
        let mut bounds = RECT::default();
        if unsafe { GetClientRect(self.hwnd, &mut bounds) }.is_err() {
            return;
        }
        let margin = scale(16, dpi);
        let left = scale(214, dpi).min(bounds.right / 2) + 2 * margin;
        let available = (bounds.right - left - margin).max(1);
        let display = self.preview_display();
        let size = self.layout.size(display);
        self.preview_dpi = size.fitting_dpi(dpi, available);
        let (width, height) = size.pixels(self.preview_dpi);
        let (width, height) = (width.max(1), height.max(1));
        position(
            self.preview,
            left + (available - width) / 2,
            self.preview_top,
            width,
            height,
        );
        self.panel.set_layout(self.layout, display);
        self.panel.resize(self.preview, self.preview_dpi);
        super::round_window(
            self.preview,
            self.preview_dpi,
            self.layout.radius(self.display.text_size),
        );
        invalidate(self.preview);
    }

    fn preview_display(&self) -> DisplayOptions {
        let mut display = self.display;
        if let Some(drag) = &self.drag
            && let Some(to) = drag.destination()
        {
            display.metrics.move_to(drag.from, to);
        }
        display
    }

    fn write_item(&self, index: usize, metric: Metric, message: u32) {
        let syncing = self.syncing.replace(true);
        let mut label: Vec<_> = metric_name(metric, self.display.language)
            .encode_utf16()
            .chain([0])
            .collect();
        let item = LVITEMW {
            mask: LVIF_TEXT,
            iItem: index as i32,
            pszText: PWSTR(label.as_mut_ptr()),
            ..Default::default()
        };
        send(self.list, message, 0, &item as *const _ as isize);
        let check = LVITEMW {
            state: LIST_VIEW_ITEM_STATE_FLAGS(if self.display.metrics.contains(metric) {
                0x2000
            } else {
                0x1000
            }),
            stateMask: LVIS_STATEIMAGEMASK,
            ..Default::default()
        };
        send(
            self.list,
            LVM_SETITEMSTATE,
            index,
            &check as *const _ as isize,
        );
        self.syncing.set(syncing);
    }

    fn select(&self, index: usize) {
        let state = LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0 | LVIS_FOCUSED.0);
        let item = LVITEMW {
            state,
            stateMask: state,
            ..Default::default()
        };
        send(
            self.list,
            LVM_SETITEMSTATE,
            index,
            &item as *const _ as isize,
        );
        send(self.list, LVM_ENSUREVISIBLE, index, 0);
    }

    fn move_row(&mut self, from: usize, to: usize) {
        if from == to {
            return;
        }
        self.display.metrics.move_to(from, to);
        for (index, metric) in self
            .display
            .metrics
            .ordered()
            .into_iter()
            .enumerate()
            .take(from.max(to) + 1)
            .skip(from.min(to))
        {
            self.write_item(index, metric, LVM_SETITEMW);
        }
        self.select(to);
        self.arrange_preview();
    }

    fn move_selection(&mut self, offset: isize) {
        let index = send(
            self.list,
            LVM_GETNEXTITEM,
            usize::MAX,
            LVNI_SELECTED as isize,
        );
        if index >= 0 {
            let end = Metric::ALL.len() as isize - 1;
            self.move_row(index as usize, (index + offset).clamp(0, end) as usize);
            unsafe {
                let _ = SetFocus(Some(self.list));
            }
        }
    }

    fn drag_over(&mut self, point: POINT) {
        let slot = self.insertion_slot(point, true);
        if let Some(drag) = &mut self.drag
            && drag.slot != slot
        {
            drag.slot = slot;
            self.arrange_preview();
            invalidate(self.list);
        }
    }

    fn insertion_slot(&self, point: POINT, scroll: bool) -> Option<usize> {
        let from = self.drag.as_ref()?.from;
        let mut point = [point];
        let mut bounds = RECT::default();
        unsafe {
            MapWindowPoints(Some(self.hwnd), Some(self.list), &mut point);
            GetClientRect(self.list, &mut bounds).ok()?;
        }
        let [point] = point;
        if point.x < 0 || point.x >= bounds.right || point.y < 0 || point.y >= bounds.bottom {
            return None;
        }
        if scroll && let Some(row) = row_bounds(self.list, 0) {
            let height = row.bottom - row.top;
            if point.y < height / 2 {
                send(self.list, LVM_SCROLL, 0, -height as isize);
            } else if point.y >= bounds.bottom - height / 2 {
                send(self.list, LVM_SCROLL, 0, height as isize);
            }
        }
        let slot = (0..Metric::ALL.len())
            .find(|&index| {
                row_bounds(self.list, index).is_some_and(|row| point.y < (row.top + row.bottom) / 2)
            })
            .unwrap_or(Metric::ALL.len());
        (slot != from && slot != from + 1).then_some(slot)
    }

    fn finish_drag(&mut self, point: Option<POINT>) -> bool {
        let slot = point.and_then(|point| self.insertion_slot(point, false));
        let Some(mut drag) = self.drag.take() else {
            return false;
        };
        let had_preview = drag.slot.is_some();
        drag.slot = slot;
        if let Some(to) = drag.destination() {
            self.move_row(drag.from, to);
        } else if had_preview {
            self.arrange_preview();
        }
        invalidate(self.list);
        true
    }

    fn draw_list(&self, draw: &NMCUSTOMDRAW) -> u32 {
        match draw.dwDrawStage {
            CDDS_PREPAINT => return CDRF_NOTIFYPOSTPAINT,
            CDDS_POSTPAINT => {}
            _ => return CDRF_DODEFAULT,
        }
        let dpi = window_dpi(self.list);
        let width = scale(GRIP_WIDTH, dpi);
        let dot = scale(2, dpi);
        let step = scale(4, dpi);
        let left = (width - step - dot) / 2;
        let grip_brush = unsafe { GetSysColorBrush(COLOR_GRAYTEXT) };
        for index in 0..Metric::ALL.len() {
            if let Some(row) = row_bounds(self.list, index) {
                let top = (row.top + row.bottom - 2 * step - dot) / 2;
                for y in [top, top + step, top + 2 * step] {
                    for x in [left, left + step] {
                        let rect = RECT {
                            left: x,
                            top: y,
                            right: x + dot,
                            bottom: y + dot,
                        };
                        unsafe { FillRect(draw.hdc, &rect, grip_brush) };
                    }
                }
            }
        }
        if let Some(slot) = self.drag.as_ref().and_then(|drag| drag.slot) {
            let mut bounds = RECT::default();
            if unsafe { GetClientRect(self.list, &mut bounds) }.is_ok()
                && let Some(row) = row_bounds(self.list, slot.min(Metric::ALL.len() - 1))
            {
                let thickness = scale(2, dpi);
                let edge = if slot == Metric::ALL.len() {
                    row.bottom
                } else {
                    row.top
                };
                let top = (edge - thickness / 2).clamp(0, (bounds.bottom - thickness).max(0));
                let line = RECT {
                    left: scale(4, dpi),
                    top,
                    right: bounds.right - scale(4, dpi),
                    bottom: top + thickness,
                };
                unsafe { FillRect(draw.hdc, &line, GetSysColorBrush(COLOR_HIGHLIGHT)) };
            }
        }
        CDRF_DODEFAULT
    }

    fn show_drag_cursor(&self, target: HWND) -> bool {
        if self.drag.is_none() {
            if target != self.list {
                return false;
            }
            let mut point = [POINT::default()];
            if unsafe { GetCursorPos(&mut point[0]) }.is_err() {
                return false;
            }
            unsafe { MapWindowPoints(None, Some(self.list), &mut point) };
            if point[0].x < 0 || point[0].x >= scale(GRIP_WIDTH, window_dpi(self.list)) {
                return false;
            }
            let mut hit = LVHITTESTINFO {
                pt: point[0],
                ..Default::default()
            };
            if send(
                self.list,
                LVM_SUBITEMHITTEST,
                0,
                &mut hit as *mut _ as isize,
            ) < 0
            {
                return false;
            }
        }
        if let Ok(cursor) = unsafe { LoadCursorW(None, IDC_SIZENS) } {
            unsafe { SetCursor(Some(cursor)) };
            return true;
        }
        false
    }

    fn list_notification(&mut self, info: &NMLISTVIEW) {
        if info.hdr.hwndFrom != self.list {
            return;
        }
        let Some(metric) = self
            .display
            .metrics
            .ordered()
            .get(info.iItem as usize)
            .copied()
        else {
            return;
        };
        match info.hdr.code {
            LVN_BEGINDRAG => {
                unsafe {
                    SetCapture(self.hwnd);
                }
                if unsafe { GetCapture() } == self.hwnd {
                    let from = info.iItem as usize;
                    self.select(from);
                    self.drag = Some(Drag { from, slot: None });
                    self.show_drag_cursor(self.list);
                }
            }
            LVN_ITEMCHANGED if info.uChanged.0 & LVIF_STATE.0 != 0 && !self.syncing.get() => {
                let state = info.uNewState & LVIS_STATEIMAGEMASK.0;
                if state != info.uOldState & LVIS_STATEIMAGEMASK.0 && state != 0 {
                    self.display.metrics.set_visible(metric, state == 0x2000);
                    self.arrange_preview();
                }
            }
            _ => {}
        }
    }
}

fn row_bounds(list: HWND, index: usize) -> Option<RECT> {
    let mut row = RECT {
        left: LVIR_BOUNDS as i32,
        ..Default::default()
    };
    (send(list, LVM_GETITEMRECT, index, &mut row as *mut _ as isize) != 0).then_some(row)
}

/// 访问编辑器状态。重入（例如列表视图在 `write_item` 中同步发通知）时借用失败，返回 `None`。
fn with_editor<T>(run: impl FnOnce(&mut Editor) -> T) -> Option<T> {
    EDITOR.with(|slot| {
        let mut slot = slot.try_borrow_mut().ok()?;
        // 窗口在借用期间被销毁时 WM_NCDESTROY 无法清理，这里补做。
        if slot
            .as_ref()
            .is_some_and(|editor| !unsafe { IsWindow(Some(editor.hwnd)) }.as_bool())
        {
            *slot = None;
        }
        slot.as_mut().map(run)
    })
}

fn combo_height(combo: HWND, dpi: u32) -> i32 {
    let mut rect = RECT::default();
    if unsafe { GetWindowRect(combo, &mut rect) }.is_ok() {
        rect.bottom - rect.top
    } else {
        scale(23, dpi)
    }
}

pub fn handle_dialog_message(message: &MSG) -> bool {
    let hwnd = with_editor(|editor| editor.hwnd);
    hwnd.is_some_and(|hwnd| unsafe {
        if message.hwnd != hwnd && !IsChild(hwnd, message.hwnd).as_bool() {
            return false;
        }
        if message.message == WM_KEYDOWN
            && message.wParam.0 == VK_ESCAPE.0 as usize
            && with_editor(|editor| editor.finish_drag(None)) == Some(true)
        {
            let _ = ReleaseCapture();
            return true;
        }
        IsDialogMessageW(hwnd, message).as_bool()
    })
}

pub fn refresh_preview() {
    if let Some(preview) = with_editor(|editor| editor.preview) {
        invalidate(preview);
    }
}

pub fn set_language(language: Language) {
    with_editor(|editor| {
        editor.display.language = language;
        editor.translate_controls();
        for (index, metric) in editor.display.metrics.ordered().into_iter().enumerate() {
            editor.write_item(index, metric, LVM_SETITEMW);
        }
        editor.arrange_preview();
    });
}

unsafe extern "system" fn editor_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        DM_GETDEFID => return LRESULT(((DC_HASDEFID << 16) | IDOK.0 as u32) as isize),
        WM_COMMAND => match wparam.0 & 0xffff {
            id if id == IDOK.0 as usize => {
                if let Some((owner, display)) = with_editor(|editor| (editor.owner, editor.display))
                {
                    set_display(owner, display);
                }
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
            }
            id if id == IDCANCEL.0 as usize => unsafe {
                let _ = DestroyWindow(hwnd);
            },
            UP | DOWN => {
                let offset = if wparam.0 & 0xffff == UP { -1 } else { 1 };
                with_editor(|editor| editor.move_selection(offset));
            }
            PREVIEW_MODE if wparam.0 >> 16 == CBN_SELCHANGE as usize => {
                with_editor(|editor| {
                    editor.layout = if send(editor.mode, CB_GETCURSEL, 0, 0) == 1 {
                        Layout::Taskbar
                    } else {
                        Layout::Floating
                    };
                    editor.arrange_preview();
                });
            }
            TEXT_SIZE if wparam.0 >> 16 == CBN_SELCHANGE as usize => {
                with_editor(|editor| {
                    let index = send(editor.size, CB_GETCURSEL, 0, 0);
                    if index >= 0
                        && let Some(size) = TextSize::new(index as i32 + TextSize::MIN)
                    {
                        editor.display.text_size = size;
                        editor.arrange_preview();
                    }
                });
            }
            DENSITY if wparam.0 >> 16 == CBN_SELCHANGE as usize => {
                with_editor(|editor| {
                    let index = send(editor.density, CB_GETCURSEL, 0, 0);
                    if let Some(&density) = DENSITY_OPTIONS.get(index as usize) {
                        editor.display.density = density;
                        editor.arrange_preview();
                    }
                });
            }
            _ => {}
        },
        WM_NOTIFY => {
            if wparam.0 == LIST && lparam.0 != 0 {
                let header = unsafe { &*(lparam.0 as *const NMHDR) };
                if matches!(header.code, LVN_BEGINDRAG | LVN_ITEMCHANGED) {
                    let info = unsafe { &*(lparam.0 as *const NMLISTVIEW) };
                    with_editor(|editor| editor.list_notification(info));
                } else if header.code == NM_CUSTOMDRAW {
                    let draw = unsafe { &*(lparam.0 as *const NMCUSTOMDRAW) };
                    return LRESULT(
                        with_editor(|editor| editor.draw_list(draw)).unwrap_or(0) as isize
                    );
                }
            }
        }
        WM_SETCURSOR if lparam.0 as u16 == HTCLIENT as u16 => {
            let target = HWND(wparam.0 as *mut _);
            if with_editor(|editor| editor.show_drag_cursor(target)) == Some(true) {
                return LRESULT(1);
            }
            return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
        }
        WM_MOUSEMOVE => {
            with_editor(|editor| editor.drag_over(super::message_point(lparam)));
        }
        WM_LBUTTONUP => {
            if with_editor(|editor| editor.finish_drag(Some(super::message_point(lparam))))
                == Some(true)
            {
                unsafe {
                    let _ = ReleaseCapture();
                }
            }
        }
        WM_CAPTURECHANGED => {
            with_editor(|editor| editor.finish_drag(None));
        }
        WM_SIZE => {
            with_editor(|editor| editor.arrange());
        }
        WM_DPICHANGED => {
            follow_dpi_change(hwnd, lparam);
            with_editor(|editor| editor.arrange());
        }
        WM_NCDESTROY => {
            // 正在借用时无法立即清理；with_editor 下次调用时会发现窗口已销毁并释放状态。
            EDITOR.with(|slot| {
                if let Ok(mut state) = slot.try_borrow_mut()
                    && state.as_ref().is_some_and(|editor| editor.hwnd == hwnd)
                {
                    state.take();
                }
            });
            return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
        }
        _ => return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
    LRESULT(0)
}

unsafe extern "system" fn preview_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_ERASEBKGND => return LRESULT(1),
        WM_TIMER if wparam.0 == PREVIEW_RETRY_TIMER => {
            let _ = unsafe { KillTimer(Some(hwnd), PREVIEW_RETRY_TIMER) };
            invalidate(hwnd);
        }
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            unsafe {
                BeginPaint(hwnd, &mut paint);
            }
            with_app(|app| {
                with_editor(|editor| {
                    if editor
                        .panel
                        .set_label_style(editor.display.language, app.settings.label_bold)
                        .is_err()
                    {
                        return;
                    }
                    let dark = app.settings.theme.is_dark(editor.layout == Layout::Taskbar);
                    let appearance = if app.settings.theme == Theme::Transparent {
                        Appearance::TransparentPreview { dark }
                    } else {
                        Appearance::Solid { dark }
                    };
                    let localized = (editor.display.language != app.display.language)
                        .then(|| readings(app.sample, app.total(), editor.display.language));
                    let values = localized.as_ref().unwrap_or(&app.displayed);
                    if editor
                        .panel
                        .paint(hwnd, editor.preview_dpi, appearance, values)
                        .is_err()
                    {
                        // 渲染目标已丢弃，稍后重绘时重新创建。
                        unsafe { SetTimer(Some(hwnd), PREVIEW_RETRY_TIMER, 1_000, None) };
                    }
                });
            });
            unsafe {
                let _ = EndPaint(hwnd, &paint);
            }
        }
        _ => return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
    LRESULT(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checking_and_dragging_change_the_preview_before_apply() {
        let owner = Window(
            unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    WC_STATICW,
                    PCWSTR::null(),
                    WS_POPUP,
                    0,
                    0,
                    100,
                    100,
                    None,
                    None,
                    None,
                    None,
                )
            }
            .unwrap(),
        );
        let settings = super::super::Settings::default();
        let (sender, _receiver) = std::sync::mpsc::channel();
        super::super::APP.with(|app| {
            *app.borrow_mut() = Some(super::super::App::new(
                settings,
                HICON::default(),
                super::super::Feed::default(),
                sender,
            ));
        });
        open(owner.0, owner.0).unwrap();
        let (hwnd, list, preview, size) =
            with_editor(|editor| (editor.hwnd, editor.list, editor.preview, editor.size)).unwrap();
        let initial_width = client_width(preview);
        let initial_dpi = with_editor(|editor| editor.preview_dpi).unwrap();
        assert!(initial_width > 0);
        let memory = Metric::AvailableMemory.index();
        assert_eq!(
            send(list, LVM_GETITEMSTATE, 0, LVIS_STATEIMAGEMASK.0 as isize),
            0x2000
        );
        assert_eq!(
            send(
                list,
                LVM_GETITEMSTATE,
                memory,
                LVIS_STATEIMAGEMASK.0 as isize
            ),
            0x1000
        );

        let item = LVITEMW {
            state: LIST_VIEW_ITEM_STATE_FLAGS(0x2000),
            stateMask: LVIS_STATEIMAGEMASK,
            ..Default::default()
        };
        send(list, LVM_SETITEMSTATE, memory, &item as *const _ as isize);
        assert_eq!(
            with_editor(|editor| editor.display.metrics.contains(Metric::AvailableMemory)),
            Some(true)
        );
        assert!(
            client_width(preview) > initial_width
                || with_editor(|editor| editor.preview_dpi).unwrap() < initial_dpi
        );

        let grip = scale(GRIP_WIDTH, window_dpi(list)) / 2;
        let memory_row = row_bounds(list, memory).unwrap();
        let mut hit = LVHITTESTINFO {
            pt: POINT {
                x: grip,
                y: (memory_row.top + memory_row.bottom) / 2,
            },
            ..Default::default()
        };
        assert_eq!(
            send(list, LVM_SUBITEMHITTEST, 0, &mut hit as *mut _ as isize),
            memory as isize
        );
        assert_eq!(hit.iSubItem as usize, GRIP_COLUMN);

        let begin_drag = || {
            const LEFT_BUTTON_DOWN: usize = 1;
            let moved = POINT {
                y: hit.pt.y - scale(24, window_dpi(list)),
                ..hit.pt
            };
            // Feed the list view's native drag detection without moving the desktop cursor.
            unsafe {
                PostMessageW(
                    Some(list),
                    WM_MOUSEMOVE,
                    WPARAM(LEFT_BUTTON_DOWN),
                    mouse_param(moved),
                )
                .unwrap();
                PostMessageW(Some(list), WM_LBUTTONUP, WPARAM(0), mouse_param(moved)).unwrap();
                send(
                    list,
                    WM_LBUTTONDOWN,
                    LEFT_BUTTON_DOWN,
                    mouse_param(hit.pt).0,
                );
                let mut message = MSG::default();
                let _ = PeekMessageW(
                    &mut message,
                    Some(list),
                    WM_LBUTTONUP,
                    WM_LBUTTONUP,
                    PM_REMOVE,
                );
            }
            assert_eq!(
                with_editor(|editor| editor.drag.as_ref().map(|drag| drag.from)),
                Some(Some(memory))
            );
        };
        let row = row_bounds(list, 0).unwrap();
        let mut points = [POINT {
            x: grip,
            y: row.top + (row.bottom - row.top) / 4,
        }];
        unsafe {
            MapWindowPoints(Some(list), Some(hwnd), &mut points);
        }
        let mouse = mouse_param(points[0]).0;
        begin_drag();
        send(hwnd, WM_MOUSEMOVE, 0, mouse);
        assert_eq!(
            with_editor(|editor| editor.preview_display().metrics.iter().next()),
            Some(Some(Metric::AvailableMemory))
        );
        assert_eq!(
            with_editor(|editor| editor.display.metrics.iter().next()),
            Some(Some(Metric::Upload))
        );
        assert!(handle_dialog_message(&MSG {
            hwnd: list,
            message: WM_KEYDOWN,
            wParam: WPARAM(VK_ESCAPE.0 as usize),
            ..Default::default()
        }));
        assert_eq!(
            with_editor(|editor| editor.preview_display().metrics.iter().next()),
            Some(Some(Metric::Upload))
        );

        begin_drag();
        send(hwnd, WM_MOUSEMOVE, 0, mouse);
        let outside = POINT { x: 0, ..points[0] };
        send(hwnd, WM_LBUTTONUP, 0, mouse_param(outside).0);
        assert_eq!(
            with_editor(|editor| editor.preview_display().metrics.iter().next()),
            Some(Some(Metric::Upload))
        );

        begin_drag();
        send(hwnd, WM_MOUSEMOVE, 0, mouse);
        send(hwnd, WM_LBUTTONUP, 0, mouse);
        assert_eq!(
            with_editor(|editor| editor.display.metrics.iter().next()),
            Some(Some(Metric::AvailableMemory))
        );
        assert_eq!(
            send(list, LVM_GETITEMSTATE, 0, LVIS_STATEIMAGEMASK.0 as isize),
            0x2000
        );
        assert_eq!(with_app(|app| app.settings.display), Some(settings.display));

        let mode = with_editor(|editor| editor.mode).unwrap();
        send(mode, CB_SETCURSEL, 1, 0);
        send(
            hwnd,
            WM_COMMAND,
            PREVIEW_MODE | ((CBN_SELCHANGE as usize) << 16),
            mode.0 as isize,
        );
        assert_eq!(with_editor(|editor| editor.layout), Some(Layout::Taskbar));
        let preview_height = client_bounds(preview).bottom;
        send(size, CB_SETCURSEL, 0, 0);
        send(
            hwnd,
            WM_COMMAND,
            TEXT_SIZE | ((CBN_SELCHANGE as usize) << 16),
            size.0 as isize,
        );
        assert_eq!(
            with_editor(|editor| editor.display.text_size.pixels()),
            Some(TextSize::MIN)
        );
        assert!(client_bounds(preview).bottom < preview_height);
        let (density, preview_width) =
            with_editor(|editor| (editor.density, client_width(preview))).unwrap();
        assert_eq!(send(density, CB_GETCURSEL, 0, 0), 0);
        send(density, CB_SETCURSEL, 2, 0);
        send(
            hwnd,
            WM_COMMAND,
            DENSITY | ((CBN_SELCHANGE as usize) << 16),
            density.0 as isize,
        );
        assert_eq!(
            with_editor(|editor| editor.display.density),
            Some(Density::Tight)
        );
        assert!(client_width(preview) < preview_width);
        assert_eq!(with_app(|app| app.settings.display), Some(settings.display));
        send(hwnd, WM_COMMAND, IDCANCEL.0 as usize, 0);
        assert_eq!(with_editor(|editor| editor.hwnd), None);
        assert_eq!(with_app(|app| app.settings.display), Some(settings.display));
        super::super::APP.with(|app| app.borrow_mut().take());
    }

    fn client_width(hwnd: HWND) -> i32 {
        client_bounds(hwnd).right
    }

    fn mouse_param(point: POINT) -> LPARAM {
        LPARAM((point.x as u16 as isize) | ((point.y as u16 as isize) << 16))
    }

    fn client_bounds(hwnd: HWND) -> RECT {
        let mut bounds = RECT::default();
        unsafe { GetClientRect(hwnd, &mut bounds) }.unwrap();
        bounds
    }
}
