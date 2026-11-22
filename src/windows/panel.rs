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
                DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_RANGE, DWRITE_WORD_WRAPPING_NO_WRAP,
                DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, IDWriteTextLayout,
                IDWriteTypography,
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
use windows_numerics::{Matrix3x2, Vector2};

use super::state::{Density, DisplayOptions, TextSize};
use crate::model::{Metric, Reading};

const ROW_HEIGHT: f32 = 24.0;
const ROW_BASELINE: f32 = 17.0;
const UNIT_GAP: f32 = 4.0;
const PERCENT_GAP: f32 = 2.5;
const ARROW_WING: f32 = 3.7;
const ARROW_GAP: f32 = 6.5;
const MIN_ARROW_GAP: f32 = 3.0;
const MIN_ARROW_INSET: f32 = 5.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Layout {
    #[default]
    Floating,
    Taskbar,
}

impl Layout {
    pub fn size(self, display: DisplayOptions) -> (i32, i32) {
        let (width, height) = self.base_size(display);
        (
            display.text_size.scale(width),
            display.text_size.scale(height),
        )
    }

    fn base_size(self, display: DisplayOptions) -> (i32, i32) {
        let width = self
            .columns(display)
            .last()
            .map(|column| column.left + column.width);
        match self {
            Self::Floating => (width.unwrap_or(132), 68),
            Self::Taskbar => (width.unwrap_or(112), 36),
        }
    }

    pub fn radius(self, text_size: TextSize) -> i32 {
        text_size.scale(self.base_radius())
    }

    fn base_radius(self) -> i32 {
        match self {
            Self::Floating => 12,
            Self::Taskbar => 6,
        }
    }

    fn column_padding(self, density: Density) -> i32 {
        let (floating, taskbar) = match density {
            Density::Standard => (18, 13),
            Density::Compact => (12, 8),
            Density::Tight => (7, 4),
        };
        match self {
            Self::Floating => floating,
            Self::Taskbar => taskbar,
        }
    }

    fn content_width(self, metric: Metric) -> i32 {
        let (floating, taskbar) = match metric {
            Metric::Upload | Metric::Download => (77, 71),
            Metric::Cpu | Metric::Memory | Metric::Disk | Metric::Battery => (94, 76),
            Metric::Traffic => (120, 114),
            Metric::AvailableMemory | Metric::UsedMemory => (135, 131),
            Metric::AvailableMemoryPercent => (112, 106),
            Metric::CpuFrequency => (126, 120),
            Metric::Uptime => (142, 136),
        };
        match self {
            Self::Floating => floating,
            Self::Taskbar => taskbar,
        }
    }

    fn columns(self, display: DisplayOptions) -> impl Iterator<Item = Column> {
        let mut selected = display.metrics.iter();
        let mut left = 0;
        let padding = self.column_padding(display.density);
        std::iter::from_fn(move || {
            let first = selected.next()?;
            let second = selected.next();
            let content_width = self
                .content_width(first)
                .max(second.map_or(0, |metric| self.content_width(metric)));
            // At the outer edge, the network reading already leaves room for its arrow.
            let inset = if left == 0 && is_network_rate(first) && second.is_none_or(is_network_rate)
            {
                0
            } else {
                padding
            };
            let width = inset + content_width + padding;
            let column = Column {
                left,
                width,
                first,
                second,
                inset,
                content_width,
            };
            left += width;
            Some(column)
        })
    }

    fn slots(self, display: DisplayOptions) -> impl Iterator<Item = (Metric, Slot)> {
        self.columns(display).flat_map(move |column| {
            let (top, bottom) = match self {
                Self::Floating => (9.0, 35.0),
                Self::Taskbar => (-3.0, 14.0),
            };
            let first_row = if column.second.is_some() {
                top
            } else {
                (top + bottom) / 2.0
            };
            [
                Some((column.first, column.slot(first_row))),
                column.second.map(|metric| (metric, column.slot(bottom))),
            ]
            .into_iter()
            .flatten()
        })
    }

    fn dividers(self, display: DisplayOptions) -> impl Iterator<Item = (Vector2, Vector2)> {
        let (top, bottom) = match self {
            Self::Floating => (12.0, 56.0),
            Self::Taskbar => (5.0, 31.0),
        };
        self.columns(display).skip(1).map(move |column| {
            let x = column.left as f32;
            (point(x, top), point(x, bottom))
        })
    }
}

struct Column {
    left: i32,
    width: i32,
    first: Metric,
    second: Option<Metric>,
    inset: i32,
    content_width: i32,
}

impl Column {
    fn slot(&self, row_y: f32) -> Slot {
        let label_x = (self.left + self.inset) as f32;
        let content_width = self.content_width as f32;
        Slot {
            label_x,
            content_width,
            number_end: label_x + content_width,
            row_y,
        }
    }
}

struct Slot {
    label_x: f32,
    content_width: f32,
    number_end: f32,
    row_y: f32,
}

fn is_network_rate(metric: Metric) -> bool {
    matches!(metric, Metric::Upload | Metric::Download)
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
    fn arrow_x(&self, label_x: f32) -> f32 {
        let available_gap = self.number_x - label_x - MIN_ARROW_INSET - 2.0 * ARROW_WING;
        self.number_x - ARROW_WING - available_gap.clamp(MIN_ARROW_GAP, ARROW_GAP)
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
    display: DisplayOptions,
    labels: [Option<IDWriteTextLayout>; Metric::ALL.len()],
    empty_label: IDWriteTextLayout,
    values: [Option<TextReading>; Metric::ALL.len()],
    target: Option<Target>,
}

impl Panel {
    pub fn new() -> Result<Self> {
        let factory = unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };
        let fonts = unsafe { DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED)? };
        let label_format = text_format(&fonts, 12.0, DWRITE_FONT_WEIGHT_NORMAL)?;
        let number_format = text_format(
            &fonts,
            TextSize::DEFAULT as f32,
            DWRITE_FONT_WEIGHT_SEMI_BOLD,
        )?;
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
        let mut labels = [const { None }; Metric::ALL.len()];
        for metric in Metric::ALL
            .into_iter()
            .filter(|&metric| !is_network_rate(metric))
        {
            let label = match metric {
                Metric::Cpu => "CPU",
                Metric::Memory => "内存",
                Metric::AvailableMemory | Metric::AvailableMemoryPercent => "可用内存",
                Metric::UsedMemory => "已用内存",
                Metric::Disk => "磁盘",
                Metric::CpuFrequency => "CPU 频率",
                Metric::Battery => "电量",
                _ => super::metric_name(metric),
            };
            labels[metric.index()] = Some(layout(&fonts, &label_format, label, 80.0)?);
        }
        let empty_label = layout(&fonts, &label_format, "未选择监控项", 112.0)?;
        unsafe { empty_label.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)? };
        Ok(Self {
            factory,
            fonts,
            number_format,
            compact_format,
            percent_format,
            unit_format,
            tabular,
            mode: Layout::Floating,
            display: DisplayOptions::default(),
            labels,
            empty_label,
            values: [const { None }; Metric::ALL.len()],
            target: None,
        })
    }

    pub fn set_layout(&mut self, mode: Layout, display: DisplayOptions) {
        if self.mode != mode
            || self.display.metrics != display.metrics
            || self.display.density != display.density
        {
            self.values = [const { None }; Metric::ALL.len()];
        }
        self.mode = mode;
        self.display = display;
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
        readings: &[Reading; Metric::ALL.len()],
    ) -> Result<()> {
        self.update_text(readings)?;
        if self.target.is_none() {
            self.target = Some(self.create_target(hwnd, dpi)?);
        }
        let target = self.target.as_ref().expect("render target initialized");
        let colors = if dark { DARK } else { LIGHT };
        let (width, height) = self.mode.base_size(self.display);
        let factor = self.display.text_size.factor();
        let result = unsafe {
            let window = &target.window;
            let brush = &target.brush;
            window.BeginDraw();
            window.Clear(Some(&color(colors.surface)));
            window.SetTransform(&Matrix3x2::scale(factor, factor));
            if self.mode == Layout::Floating {
                let radius = self.mode.base_radius() as f32 - 0.5;
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
            for (start, end) in self.mode.dividers(self.display) {
                window.DrawLine(start, end, brush, 1.0, None);
            }
            if self.display.metrics.iter().next().is_none() {
                brush.SetColor(&color(colors.label));
                let x = (width as f32 - 112.0) / 2.0;
                let y = (height as f32 - ROW_HEIGHT) / 2.0;
                window.DrawTextLayout(
                    point(x, y),
                    &self.empty_label,
                    brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                );
            }
            for (metric, slot) in self.mode.slots(self.display) {
                let index = metric.index();
                let text = self.values[index]
                    .as_ref()
                    .expect("visible reading initialized");
                if let Some(label) = &self.labels[index] {
                    brush.SetColor(&color(colors.label));
                    window.DrawTextLayout(
                        point(slot.label_x, slot.row_y),
                        label,
                        brush,
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                    );
                } else {
                    let upload = metric == Metric::Upload;
                    brush.SetColor(&color(if upload {
                        colors.upload
                    } else {
                        colors.download
                    }));
                    draw_arrow(target, text.arrow_x(slot.label_x), slot.row_y, upload);
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

    fn update_text(&mut self, readings: &[Reading; Metric::ALL.len()]) -> Result<()> {
        for (metric, slot) in self.mode.slots(self.display) {
            let cached = &mut self.values[metric.index()];
            let reading = &readings[metric.index()];
            if cached
                .as_ref()
                .is_some_and(|value| value.displayed == *reading)
            {
                continue;
            }
            let number_format = match metric {
                Metric::Cpu
                | Metric::Memory
                | Metric::Disk
                | Metric::Battery
                | Metric::Uptime
                | Metric::AvailableMemoryPercent => &self.percent_format,
                _ if self.mode == Layout::Taskbar => &self.compact_format,
                _ => &self.number_format,
            };
            let number = layout(
                &self.fonts,
                number_format,
                &reading.number,
                slot.content_width,
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
pub fn paint_starting(hwnd: HWND, dc: HDC, dpi: u32, dark: bool, text_size: TextSize) {
    let mut rect = RECT::default();
    if unsafe { GetClientRect(hwnd, &mut rect) }.is_err() {
        return;
    }
    let colors = if dark { DARK } else { LIGHT };
    let mut font = LOGFONTW {
        lfHeight: -super::scale(text_size.scale(14), dpi),
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
    use crate::model::{Metrics, PhysicalMemory, Sample, readings};

    #[test]
    fn text_fits_in_both_layouts() {
        let mut panel = Panel::new().unwrap();
        let sample = Sample {
            upload: Some(1.0),
            download: Some(999_000_000.0),
            cpu: Some(100.0),
            memory: Some(PhysicalMemory {
                total: 2_000_000_000_000,
                available: 999_400_000_000,
            }),
            disk: Some(100.0),
            cpu_mhz: Some(5670.0),
            uptime: Some(std::time::Duration::from_secs(999 * 86_400 + 23 * 3_600)),
            battery: Some(100.0),
            ..Default::default()
        };
        let selections = [
            (Metrics::default(), [381, 326]),
            (
                Metrics::parse(
                    "uptime,upload,available_memory,cpu_frequency,battery,download,traffic",
                ),
                [635, 561],
            ),
            (
                Metrics::parse("traffic,disk,upload,download,cpu_frequency,available_memory"),
                [440, 394],
            ),
            (
                Metrics::parse("available_memory_percent,used_memory,available_memory"),
                [342, 314],
            ),
            (Metrics::parse(""), [132, 112]),
        ];
        for (metrics, expected_widths) in selections {
            for (mode, expected_width) in [Layout::Floating, Layout::Taskbar]
                .into_iter()
                .zip(expected_widths)
            {
                let display = DisplayOptions {
                    metrics,
                    ..Default::default()
                };
                panel.set_layout(mode, display);
                panel
                    .update_text(&readings(sample, 999_990_000_000))
                    .unwrap();
                assert_eq!(mode.size(display).0, expected_width);
                assert!(
                    mode.slots(display)
                        .map(|(metric, _)| metric)
                        .eq(metrics.iter())
                );
                for (metric, slot) in mode.slots(display) {
                    let text = panel.values[metric.index()].as_ref().unwrap();
                    let gap = if text.displayed.unit == "%" {
                        PERCENT_GAP
                    } else {
                        UNIT_GAP
                    };
                    let number_width = text_width(&text.number).unwrap();
                    let unit_width = text_width(&text.unit).unwrap();
                    assert!((text.unit_x + unit_width - slot.number_end).abs() < 0.01);
                    if text.displayed.unit.is_empty() {
                        assert!((text.number_x + number_width - text.unit_x).abs() < 0.01);
                    } else {
                        assert!((text.number_x + number_width + gap - text.unit_x).abs() < 0.01);
                    }
                    if let Some(label) = &panel.labels[metric.index()] {
                        assert!(
                            slot.label_x + text_width(label).unwrap() + 3.0 <= text.number_x,
                            "{mode:?} overlapping {metric:?}"
                        );
                    } else {
                        let arrow_x = text.arrow_x(slot.label_x);
                        let inset = arrow_x - ARROW_WING - slot.label_x;
                        let gap = text.number_x - arrow_x - ARROW_WING;
                        assert!(
                            inset + 0.01 >= MIN_ARROW_INSET,
                            "{mode:?} {metric:?} arrow inset: {inset}"
                        );
                        assert!(
                            gap + 0.01 >= MIN_ARROW_GAP,
                            "{mode:?} {metric:?} arrow gap: {gap}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn density_narrows_columns_and_repositions_unchanged_readings() {
        let mut panel = Panel::new().unwrap();
        let values = readings(Sample::default(), 16_000_000);
        for (mode, widths) in [
            (Layout::Floating, [381, 351, 326]),
            (Layout::Taskbar, [326, 301, 281]),
        ] {
            let height = mode.size(DisplayOptions::default()).1;
            for (density, width) in [Density::Standard, Density::Compact, Density::Tight]
                .into_iter()
                .zip(widths)
            {
                let display = DisplayOptions {
                    density,
                    ..Default::default()
                };
                assert_eq!(mode.size(display), (width, height));
                panel.set_layout(mode, display);
                panel.update_text(&values).unwrap();
                for (metric, slot) in mode.slots(display) {
                    let reading = panel.values[metric.index()].as_ref().unwrap();
                    let right = reading.unit_x + text_width(&reading.unit).unwrap();
                    assert!((right - slot.number_end).abs() < 0.01);
                }
            }
        }
    }

    #[test]
    fn taskbar_traffic_gap_is_close_to_the_middle_column() {
        let mode = Layout::Taskbar;
        let display = DisplayOptions::default();
        let mut panel = Panel::new().unwrap();
        panel.set_layout(mode, display);
        let sample = Sample {
            cpu: Some(9.0),
            memory: Some(PhysicalMemory {
                total: 100,
                available: 45,
            }),
            disk: Some(3.0),
            ..Default::default()
        };
        panel.update_text(&readings(sample, 16_000_000)).unwrap();

        let label_gap = |metric: Metric| {
            let (_, slot) = mode
                .slots(display)
                .find(|(item, _)| *item == metric)
                .unwrap();
            let label = panel.labels[metric.index()].as_ref().unwrap();
            let value = panel.values[metric.index()].as_ref().unwrap();
            value.number_x - slot.label_x - text_width(label).unwrap()
        };
        let traffic_gap = label_gap(Metric::Traffic);
        let memory_gap = label_gap(Metric::Memory);
        let cpu_gap = label_gap(Metric::Cpu);
        assert!(
            (memory_gap..=cpu_gap).contains(&traffic_gap),
            "traffic gap {traffic_gap} is outside the middle-column range {memory_gap} to {cpu_gap}"
        );
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
            memory: Some(PhysicalMemory {
                total: 100,
                available: 44,
            }),
            disk: Some(1.0),
            ..Default::default()
        };
        let readings = readings(sample, 145_060_000);
        let custom = DisplayOptions {
            metrics: Metrics::parse(
                "cpu_frequency,upload,battery,uptime,used_memory,available_memory,available_memory_percent",
            ),
            text_size: TextSize::new(TextSize::MAX).unwrap(),
            density: Density::Tight,
        };
        let compact = DisplayOptions {
            text_size: TextSize::new(TextSize::MIN).unwrap(),
            ..custom
        };
        for (mode, display) in [
            (Layout::Floating, custom),
            (Layout::Taskbar, compact),
            (Layout::Taskbar, DisplayOptions::default()),
        ] {
            let (width, height) = mode.size(display);
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
            panel.set_layout(mode, display);
            panel.resize(hwnd, dpi);
            for dark in [false, true] {
                panel.paint(hwnd, dpi, dark, &readings).unwrap();
            }
        }
    }
}
