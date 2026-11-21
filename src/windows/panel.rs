use ::windows::{
    Win32::{
        Foundation::{COLORREF, HWND, RECT},
        Graphics::{
            Direct2D::{
                Common::{D2D_RECT_F, D2D_SIZE_U, D2D1_COLOR_F},
                D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
                D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_PRESENT_OPTIONS_IMMEDIATELY,
                D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_SOFTWARE, D2D1_ROUNDED_RECT,
                D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE, D2D1CreateFactory, ID2D1Factory,
                ID2D1HwndRenderTarget, ID2D1SolidColorBrush,
            },
            DirectWrite::{
                DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_FEATURE,
                DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES, DWRITE_FONT_STRETCH_NORMAL,
                DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_LINE_SPACING_METHOD_UNIFORM,
                DWRITE_TEXT_RANGE, DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory,
                IDWriteFactory, IDWriteTextFormat, IDWriteTextLayout, IDWriteTypography,
            },
            Gdi::{
                CreateFontIndirectW, CreateSolidBrush, DEFAULT_CHARSET, DT_CENTER, DT_NOPREFIX,
                DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawTextW, FillRect, HDC, HGDIOBJ,
                LOGFONTW, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
            },
        },
        UI::WindowsAndMessaging::GetClientRect,
    },
    core::{Result, w},
};
use windows_numerics::Vector2;

use crate::model::Reading;

const ROW_HEIGHT: f32 = 24.0;
const ROW_BASELINE: f32 = 17.0;
const UNIT_GAP: f32 = 4.0;
const PERCENT_GAP: f32 = 2.5;
const ARROW_WING: f32 = 3.7;
const ARROW_GAP: f32 = 6.5;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Layout {
    #[default]
    Floating,
    Taskbar,
}

impl Layout {
    pub const fn size(self) -> (i32, i32) {
        match self {
            Self::Floating => (420, 68),
            Self::Taskbar => (360, 36),
        }
    }

    pub const fn radius(self) -> i32 {
        match self {
            Self::Floating => 12,
            Self::Taskbar => 6,
        }
    }

    const fn slots(self) -> [Slot; 6] {
        match self {
            Self::Floating => [
                Slot::new(0.0, 25.0, 99.0, 9.0),
                Slot::new(0.0, 25.0, 99.0, 35.0),
                Slot::new(128.0, 166.0, 224.0, 9.0),
                Slot::new(128.0, 166.0, 224.0, 35.0),
                Slot::new(259.0, 312.0, 401.0, 9.0),
                Slot::new(259.0, 312.0, 401.0, 35.0),
            ],
            Self::Taskbar => [
                Slot::new(0.0, 19.0, 85.0, -3.0),
                Slot::new(0.0, 19.0, 85.0, 14.0),
                Slot::new(109.0, 144.0, 185.0, -3.0),
                Slot::new(109.0, 144.0, 185.0, 14.0),
                Slot::new(211.0, 263.0, 347.0, -3.0),
                Slot::new(211.0, 263.0, 347.0, 14.0),
            ],
        }
    }

    const fn dividers(self) -> [(Vector2, Vector2); 2] {
        match self {
            Self::Floating => [
                (point(112.0, 12.0), point(112.0, 56.0)),
                (point(242.0, 12.0), point(242.0, 56.0)),
            ],
            Self::Taskbar => [
                (point(96.0, 5.0), point(96.0, 31.0)),
                (point(198.0, 5.0), point(198.0, 31.0)),
            ],
        }
    }
}

#[derive(Clone, Copy)]
struct Slot {
    label_x: f32,
    number_x: f32,
    number_end: f32,
    row_y: f32,
}

impl Slot {
    const fn new(label_x: f32, number_x: f32, number_end: f32, row_y: f32) -> Self {
        Self {
            label_x,
            number_x,
            number_end,
            row_y,
        }
    }
}

struct Colors {
    surface: u32,
    number: u32,
    label: u32,
    border: u32,
    divider: u32,
    upload: u32,
    download: u32,
}

const LIGHT: Colors = Colors {
    surface: 0xfbf9f3,
    number: 0x282a23,
    label: 0x626354,
    border: 0xddd8c9,
    divider: 0xe8e2d3,
    upload: 0x48734c,
    download: 0x96652b,
};
const DARK: Colors = Colors {
    surface: 0x242520,
    number: 0xf5f1e6,
    label: 0xc6c5b4,
    border: 0x4b4c42,
    divider: 0x44463d,
    upload: 0xa6d3a0,
    download: 0xe7bd7b,
};

struct Target {
    window: ID2D1HwndRenderTarget,
    brush: ID2D1SolidColorBrush,
}

struct TextReading {
    displayed: Reading,
    number: IDWriteTextLayout,
    unit: IDWriteTextLayout,
    number_x: f32,
    unit_x: f32,
}

impl TextReading {
    fn arrow_x(&self) -> f32 {
        self.number_x - ARROW_WING - ARROW_GAP
    }
}

pub struct Panel {
    factory: ID2D1Factory,
    fonts: IDWriteFactory,
    number_format: IDWriteTextFormat,
    compact_format: IDWriteTextFormat,
    percent_format: IDWriteTextFormat,
    unit_format: IDWriteTextFormat,
    tabular: IDWriteTypography,
    mode: Layout,
    labels: [IDWriteTextLayout; 4],
    values: [Option<TextReading>; 6],
    target: Option<Target>,
}

impl Panel {
    pub fn new() -> Result<Self> {
        let factory = unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };
        let fonts = unsafe { DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED)? };
        let label_format = text_format(&fonts, 12.0, DWRITE_FONT_WEIGHT_NORMAL)?;
        let number_format = text_format(&fonts, 16.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?;
        let compact_format = text_format(&fonts, 15.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?;
        let percent_format = text_format(&fonts, 14.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?;
        let unit_format = text_format(&fonts, 11.5, DWRITE_FONT_WEIGHT_NORMAL)?;
        let tabular = unsafe {
            let typography = fonts.CreateTypography()?;
            typography.AddFontFeature(DWRITE_FONT_FEATURE {
                nameTag: DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES,
                parameter: 1,
            })?;
            typography
        };
        let labels = [
            layout(&fonts, &label_format, "CPU", 38.0)?,
            layout(&fonts, &label_format, "内存", 42.0)?,
            layout(&fonts, &label_format, "磁盘", 47.0)?,
            layout(&fonts, &label_format, "累计流量", 80.0)?,
        ];
        Ok(Self {
            factory,
            fonts,
            number_format,
            compact_format,
            percent_format,
            unit_format,
            tabular,
            mode: Layout::Floating,
            labels,
            values: [const { None }; 6],
            target: None,
        })
    }

    pub fn set_layout(&mut self, mode: Layout) {
        if self.mode != mode {
            self.mode = mode;
            self.values = [const { None }; 6];
        }
    }

    pub fn resize(&mut self, hwnd: HWND, dpi: u32) {
        if let Some(target) = &self.target {
            let result = client_size(hwnd).and_then(|size| unsafe {
                target.window.SetDpi(dpi as f32, dpi as f32);
                target.window.Resize(&size)
            });
            if result.is_err() {
                self.target = None;
            }
        }
    }

    pub fn paint(
        &mut self,
        hwnd: HWND,
        dpi: u32,
        dark: bool,
        readings: &[Reading; 6],
    ) -> Result<()> {
        self.update_text(readings)?;
        if self.target.is_none() {
            self.target = Some(self.create_target(hwnd, dpi)?);
        }
        let target = self.target.as_ref().expect("render target initialized");
        let colors = if dark { DARK } else { LIGHT };
        let slots = self.mode.slots();
        let result = unsafe {
            let window = &target.window;
            let brush = &target.brush;
            window.BeginDraw();
            window.Clear(Some(&color(colors.surface)));
            if self.mode == Layout::Floating {
                let (width, height) = self.mode.size();
                let radius = self.mode.radius() as f32 - 0.5;
                brush.SetColor(&color(colors.border));
                window.DrawRoundedRectangle(
                    &D2D1_ROUNDED_RECT {
                        rect: D2D_RECT_F {
                            left: 0.75,
                            top: 0.75,
                            right: width as f32 - 0.75,
                            bottom: height as f32 - 0.75,
                        },
                        radiusX: radius,
                        radiusY: radius,
                    },
                    brush,
                    1.0,
                    None,
                );
            }
            brush.SetColor(&color(colors.divider));
            for (start, end) in self.mode.dividers() {
                window.DrawLine(start, end, brush, 1.0, None);
            }
            brush.SetColor(&color(colors.label));
            for (slot, label) in slots[2..].iter().zip(&self.labels) {
                window.DrawTextLayout(
                    point(slot.label_x, slot.row_y),
                    label,
                    brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                );
            }
            for (index, (slot, text)) in slots.iter().zip(self.values.iter().flatten()).enumerate()
            {
                if index < 2 {
                    let upload = index == 0;
                    brush.SetColor(&color(if upload {
                        colors.upload
                    } else {
                        colors.download
                    }));
                    draw_arrow(target, text.arrow_x(), slot.row_y, upload);
                }
                brush.SetColor(&color(colors.number));
                window.DrawTextLayout(
                    point(text.number_x, slot.row_y),
                    &text.number,
                    brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                );
                brush.SetColor(&color(colors.label));
                window.DrawTextLayout(
                    point(text.unit_x, slot.row_y),
                    &text.unit,
                    brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                );
            }
            window.EndDraw(None, None)
        };
        if result.is_err() {
            self.target = None;
        }
        result
    }

    fn update_text(&mut self, readings: &[Reading; 6]) -> Result<()> {
        for (index, ((cached, reading), slot)) in self
            .values
            .iter_mut()
            .zip(readings)
            .zip(self.mode.slots())
            .enumerate()
        {
            if cached
                .as_ref()
                .is_some_and(|value| value.displayed == *reading)
            {
                continue;
            }
            let number_format = match index {
                2..=4 => &self.percent_format,
                _ if self.mode == Layout::Taskbar => &self.compact_format,
                _ => &self.number_format,
            };
            let number = layout(
                &self.fonts,
                number_format,
                &reading.number,
                slot.number_end - slot.number_x,
            )?;
            unsafe {
                number.SetTypography(
                    &self.tabular,
                    DWRITE_TEXT_RANGE {
                        startPosition: 0,
                        length: reading.number.encode_utf16().count() as u32,
                    },
                )?;
            }
            let unit = layout(&self.fonts, &self.unit_format, reading.unit, 43.0)?;
            let number_width = text_width(&number)?;
            let gap = match reading.unit {
                "" => 0.0,
                "%" => PERCENT_GAP,
                _ => UNIT_GAP,
            };
            let unit_x = slot.number_end - text_width(&unit)?;
            let number_x = unit_x - gap - number_width;
            *cached = Some(TextReading {
                displayed: reading.clone(),
                number,
                unit,
                number_x,
                unit_x,
            });
        }
        Ok(())
    }

    fn create_target(&self, hwnd: HWND, dpi: u32) -> Result<Target> {
        let size = client_size(hwnd)?;
        unsafe {
            let window = self.factory.CreateHwndRenderTarget(
                &D2D1_RENDER_TARGET_PROPERTIES {
                    r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                    dpiX: dpi as f32,
                    dpiY: dpi as f32,
                    ..Default::default()
                },
                &D2D1_HWND_RENDER_TARGET_PROPERTIES {
                    hwnd,
                    pixelSize: size,
                    presentOptions: D2D1_PRESENT_OPTIONS_IMMEDIATELY,
                },
            )?;
            window.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE);
            let brush = window.CreateSolidColorBrush(&color(LIGHT.number), None)?;
            Ok(Target { window, brush })
        }
    }
}

// The first frame uses GDI before DirectWrite and the system counters are ready.
pub fn paint_starting(hwnd: HWND, dc: HDC, dpi: u32, dark: bool) {
    let mut rect = RECT::default();
    if unsafe { GetClientRect(hwnd, &mut rect) }.is_err() {
        return;
    }
    let colors = if dark { DARK } else { LIGHT };
    let mut font = LOGFONTW {
        lfHeight: -super::scale(14, dpi),
        lfWeight: 400,
        lfCharSet: DEFAULT_CHARSET,
        ..Default::default()
    };
    for (dst, src) in font.lfFaceName.iter_mut().zip("Segoe UI".encode_utf16()) {
        *dst = src;
    }
    let mut text: Vec<_> = "正在读取系统指标…".encode_utf16().collect();
    unsafe {
        let background = CreateSolidBrush(gdi_color(colors.surface));
        if !background.is_invalid() {
            FillRect(dc, &rect, background);
            let _ = DeleteObject(HGDIOBJ(background.0));
        }
        let text_font = CreateFontIndirectW(&font);
        let previous_font = SelectObject(dc, HGDIOBJ(text_font.0));
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, gdi_color(colors.number));
        DrawTextW(
            dc,
            &mut text,
            &mut rect,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
        if !previous_font.is_invalid() {
            SelectObject(dc, previous_font);
        }
        if !text_font.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(text_font.0));
        }
    }
}

fn gdi_color(rgb: u32) -> COLORREF {
    COLORREF(((rgb & 0xff) << 16) | (rgb & 0xff00) | (rgb >> 16))
}

fn client_size(hwnd: HWND) -> Result<D2D_SIZE_U> {
    let mut rect = RECT::default();
    unsafe { GetClientRect(hwnd, &mut rect)? };
    Ok(D2D_SIZE_U {
        width: rect.right as u32,
        height: rect.bottom as u32,
    })
}

fn text_format(
    fonts: &IDWriteFactory,
    size: f32,
    weight: DWRITE_FONT_WEIGHT,
) -> Result<IDWriteTextFormat> {
    unsafe {
        let format = fonts.CreateTextFormat(
            w!("Segoe UI"),
            None,
            weight,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            w!("zh-CN"),
        )?;
        format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        format.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, ROW_HEIGHT, ROW_BASELINE)?;
        Ok(format)
    }
}

fn layout(
    fonts: &IDWriteFactory,
    format: &IDWriteTextFormat,
    text: &str,
    width: f32,
) -> Result<IDWriteTextLayout> {
    let utf16: Vec<_> = text.encode_utf16().collect();
    unsafe { fonts.CreateTextLayout(&utf16, format, width, ROW_HEIGHT) }
}

fn text_width(text: &IDWriteTextLayout) -> Result<f32> {
    let mut metrics = Default::default();
    unsafe { text.GetMetrics(&mut metrics)? };
    Ok(metrics.width)
}

fn draw_arrow(target: &Target, x: f32, row_y: f32, up: bool) {
    let mid = row_y + 13.0;
    let tip = mid + if up { -4.5 } else { 4.5 };
    let tail = 2.0 * mid - tip;
    let wing = tip + if up { ARROW_WING } else { -ARROW_WING };
    let center = point(x, tip);
    unsafe {
        target
            .window
            .DrawLine(point(x, tail), center, &target.brush, 1.45, None);
        target.window.DrawLine(
            point(x - ARROW_WING, wing),
            center,
            &target.brush,
            1.45,
            None,
        );
        target.window.DrawLine(
            point(x + ARROW_WING, wing),
            center,
            &target.brush,
            1.45,
            None,
        );
    }
}

const fn point(x: f32, y: f32) -> Vector2 {
    Vector2 { X: x, Y: y }
}

fn color(rgb: u32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((rgb >> 16) & 255) as f32 / 255.0,
        g: ((rgb >> 8) & 255) as f32 / 255.0,
        b: (rgb & 255) as f32 / 255.0,
        a: 1.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Sample, readings};

    #[test]
    fn text_fits_in_both_layouts() {
        let mut panel = Panel::new().unwrap();
        let sample = Sample {
            upload: Some(1.0),
            download: Some(9_990_000_000.0),
            cpu: Some(100.0),
            memory: Some(100.0),
            disk: Some(100.0),
            ..Default::default()
        };
        for mode in [Layout::Floating, Layout::Taskbar] {
            panel.set_layout(mode);
            panel
                .update_text(&readings(sample, 999_990_000_000))
                .unwrap();
            let slots = mode.slots();
            for (index, (slot, text)) in slots
                .into_iter()
                .zip(panel.values.iter().flatten())
                .enumerate()
            {
                let mut number = Default::default();
                let mut unit = Default::default();
                unsafe {
                    text.number.GetMetrics(&mut number).unwrap();
                    text.unit.GetMetrics(&mut unit).unwrap();
                }
                let right = match index {
                    0 | 1 => mode.dividers()[0].0.X,
                    2 | 3 => mode.dividers()[1].0.X,
                    _ => mode.size().0 as f32,
                };
                assert!(
                    text.number_x >= slot.number_x,
                    "{mode:?} reading start {index}"
                );
                let unit_end = text.unit_x + unit.width;
                assert!(
                    (unit_end - slot.number_end).abs() < 0.01,
                    "{mode:?} right alignment"
                );
                assert!(unit_end + 4.0 < right, "{mode:?} column margin {index}");
                if index < 2 {
                    let arrow_gap = text.number_x + number.left - (text.arrow_x() + ARROW_WING);
                    assert!((arrow_gap - ARROW_GAP).abs() < 0.01, "{mode:?} arrow gap");
                    assert!(text.arrow_x() - ARROW_WING >= 5.0, "{mode:?} arrow margin");
                }
                let expected_gap = if matches!(index, 2..=4) {
                    PERCENT_GAP
                } else {
                    UNIT_GAP
                };
                let unit_gap = text.unit_x - (text.number_x + number.left + number.width);
                assert!((unit_gap - expected_gap).abs() < 0.01, "{mode:?} unit gap");
                assert_eq!(number.lineCount, 1);
                assert_eq!(unit.lineCount, 1);
            }
            for (slot, label) in slots[2..].iter().zip(&panel.labels) {
                let mut metrics = Default::default();
                unsafe { label.GetMetrics(&mut metrics).unwrap() };
                assert!(
                    slot.label_x + metrics.width <= slot.number_x + 1.0,
                    "{mode:?} label"
                );
            }
        }
    }

    #[test]
    fn draws_both_layouts_at_175_percent_scaling() {
        use ::windows::{
            Win32::UI::WindowsAndMessaging::{
                CreateWindowExW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SetWindowPos,
                WINDOW_EX_STYLE, WS_POPUP,
            },
            core::PCWSTR,
        };

        let dpi = 168;
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("Static"),
                PCWSTR::null(),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                None,
                None,
            )
        }
        .unwrap();
        let _window = super::super::Window(hwnd);
        let mut panel = Panel::new().unwrap();
        let sample = Sample {
            upload: Some(409_000.0),
            download: Some(26_000.0),
            cpu: Some(9.0),
            memory: Some(56.0),
            disk: Some(1.0),
            ..Default::default()
        };
        let readings = readings(sample, 145_060_000);
        for mode in [Layout::Floating, Layout::Taskbar] {
            let (width, height) = mode.size();
            let scale = super::super::scale;
            unsafe {
                SetWindowPos(
                    hwnd,
                    None,
                    0,
                    0,
                    scale(width, dpi),
                    scale(height, dpi),
                    SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOZORDER,
                )
            }
            .unwrap();
            panel.set_layout(mode);
            panel.resize(hwnd, dpi);
            for dark in [false, true] {
                panel.paint(hwnd, dpi, dark, &readings).unwrap();
            }
        }
    }
}
