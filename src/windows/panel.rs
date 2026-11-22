use ::windows::{
    Win32::{
        Foundation::{COLORREF, HWND, POINT, RECT, SIZE},
        Graphics::{
            Direct2D::{
                Common::{
                    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F,
                    D2D1_PIXEL_FORMAT,
                },
                D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
                D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_PRESENT_OPTIONS_IMMEDIATELY,
                D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_SOFTWARE, D2D1_ROUNDED_RECT,
                D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
                D2D1CreateFactory, ID2D1DCRenderTarget, ID2D1Factory, ID2D1HwndRenderTarget,
                ID2D1RenderTarget, ID2D1SolidColorBrush,
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
            Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
            Gdi::{
                AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
                CreateCompatibleDC, CreateDIBSection, CreateFontIndirectW, CreateSolidBrush,
                DEFAULT_CHARSET, DIB_RGB_COLORS, DT_CENTER, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER,
                DeleteDC, DeleteObject, DrawTextW, FillRect, HBITMAP, HDC, HGDIOBJ, LOGFONTW,
                SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
            },
        },
        UI::WindowsAndMessaging::{GetClientRect, ULW_ALPHA, UpdateLayeredWindow},
    },
    core::{Error, Result, w},
};
use windows_numerics::{Matrix3x2, Vector2};

use super::state::{Density, DisplayOptions, TextSize};
use crate::{
    language::Language,
    model::{Metric, Reading},
};

const ROW_HEIGHT: f32 = 24.0;
const ROW_BASELINE: f32 = 17.0;
const UNIT_GAP: f32 = 4.0;
const PERCENT_GAP: f32 = 2.5;
const ARROW_WING: f32 = 3.7;
const ARROW_INSET: f32 = 2.5;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Layout {
    #[default]
    Floating,
    Taskbar,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutSize {
    width: i32,
    height: i32,
    gap_px: i32,
    text_size: TextSize,
}

impl LayoutSize {
    pub fn pixels(self, dpi: u32) -> (i32, i32) {
        (
            (self.width as f32 * pixel_scale(self.text_size, dpi)).ceil() as i32 + self.gap_px,
            super::scale(self.text_size.scale(self.height), dpi),
        )
    }

    pub fn fitting_dpi(self, dpi: u32, available_px: i32) -> u32 {
        let scalable_width = self.width as f32 * self.text_size.factor();
        let fitting = ((available_px - self.gap_px) as f32 * 96.0 / scalable_width) as u32;
        dpi.min(fitting.max(1))
    }
}

fn pixel_scale(text_size: TextSize, dpi: u32) -> f32 {
    text_size.factor() * dpi as f32 / 96.0
}

impl Layout {
    pub fn size(self, display: DisplayOptions) -> LayoutSize {
        let (empty_width, height) = match self {
            Self::Floating => (132, 68),
            Self::Taskbar => (112, 36),
        };
        let (width, gap_px) = self
            .columns(display)
            .last()
            .map_or((empty_width, 0), |column| {
                (
                    column.left + column.width,
                    column.left_gap_px + column.gap_px,
                )
            });
        LayoutSize {
            width,
            height,
            gap_px,
            text_size: display.text_size,
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

    fn field_widths(self, metric: Metric, language: Language) -> (i32, i32) {
        let (chinese, english) = match metric {
            Metric::Upload | Metric::Download => (10, 10),
            Metric::Cpu | Metric::CpuFrequency => (28, 28),
            Metric::Memory => (29, 33),
            Metric::Disk => (29, 29),
            Metric::Traffic => (57, 40),
            Metric::AvailableMemory | Metric::AvailableMemoryPercent => (57, 63),
            Metric::UsedMemory => (57, 68),
            Metric::Uptime => (57, 48),
            Metric::Battery => (29, 48),
            Metric::DiskRead => (43, 64),
            Metric::DiskWrite => (43, 67),
            Metric::GpuMemoryPercent => (29, 41),
            Metric::UsedGpuMemory => (57, 77),
        };
        let (floating, taskbar) = match metric {
            Metric::Upload | Metric::Download | Metric::DiskRead | Metric::DiskWrite => (59, 57),
            Metric::Cpu
            | Metric::Memory
            | Metric::Disk
            | Metric::Battery
            | Metric::AvailableMemoryPercent
            | Metric::GpuMemoryPercent => (37, 37),
            Metric::Traffic => (51, 49),
            Metric::AvailableMemory | Metric::UsedMemory => (87, 83),
            Metric::CpuFrequency => (66, 64),
            Metric::Uptime if language == Language::English => (62, 62),
            Metric::Uptime => (73, 73),
            Metric::UsedGpuMemory => (78, 75),
        };
        let value = match self {
            Self::Floating => floating,
            Self::Taskbar => taskbar,
        };
        (language.choose(chinese, english), value)
    }

    fn columns(self, display: DisplayOptions) -> impl Iterator<Item = Column> {
        let mut selected = display.metrics.iter();
        let mut left = 0;
        let padding = self.column_padding(display.density);
        let gap_px = match display.density {
            Density::Standard => 8,
            Density::Compact => 6,
            Density::Tight => 4,
        };
        let mut left_gap_px = 0;
        std::iter::from_fn(move || {
            let first = selected.next()?;
            let second = selected.next();
            let (first_label, first_value) = self.field_widths(first, display.language);
            let (second_label, second_value) =
                second.map_or((0, 0), |metric| self.field_widths(metric, display.language));
            // Reserve both rows' widths so sample updates keep the same column positions.
            let label_width = first_label.max(second_label);
            let value_width = first_value.max(second_value);
            // At the outer edge, the network reading already leaves room for its arrow.
            let inset = if left == 0 && is_network_rate(first) && second.is_none_or(is_network_rate)
            {
                0
            } else {
                padding
            };
            let width = inset + label_width + value_width + padding;
            let column = Column {
                left,
                width,
                first,
                second,
                inset,
                label_width,
                left_gap_px,
                gap_px,
                value_width,
            };
            left += width;
            left_gap_px += gap_px;
            Some(column)
        })
    }

    fn slots(self, display: DisplayOptions, scale: f32) -> impl Iterator<Item = (Metric, Slot)> {
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
                Some((column.first, column.slot(first_row, scale))),
                column
                    .second
                    .map(|metric| (metric, column.slot(bottom, scale))),
            ]
            .into_iter()
            .flatten()
        })
    }

    fn dividers(
        self,
        display: DisplayOptions,
        scale: f32,
    ) -> impl Iterator<Item = (Vector2, Vector2)> {
        let (top, bottom) = match self {
            Self::Floating => (12.0, 56.0),
            Self::Taskbar => (5.0, 31.0),
        };
        self.columns(display).skip(1).map(move |column| {
            let x = column.left as f32 + column.left_gap_px as f32 / scale;
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
    label_width: i32,
    left_gap_px: i32,
    gap_px: i32,
    value_width: i32,
}

impl Column {
    fn slot(&self, row_y: f32, scale: f32) -> Slot {
        let label_x = (self.left + self.inset) as f32 + self.left_gap_px as f32 / scale;
        Slot {
            label_x,
            value_x: label_x + self.label_width as f32 + self.gap_px as f32 / scale,
            value_width: self.value_width as f32,
            row_y,
        }
    }
}

struct Slot {
    label_x: f32,
    value_x: f32,
    value_width: f32,
    row_y: f32,
}

impl Slot {
    fn value_right(&self) -> f32 {
        self.value_x + self.value_width
    }

    fn arrow_x(&self) -> f32 {
        self.label_x + ARROW_INSET + ARROW_WING
    }
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

const TRANSPARENT_LIGHT: Colors = Colors {
    surface: 0xf3f3f3,
    number: 0x000000,
    label: 0x000000,
    divider: 0x000000,
    upload: 0x000000,
    download: 0x000000,
    ..LIGHT
};
const TRANSPARENT_DARK: Colors = Colors {
    surface: 0x202020,
    number: 0xffffff,
    label: 0xffffff,
    divider: 0xffffff,
    upload: 0xffffff,
    download: 0xffffff,
    ..DARK
};

#[derive(Clone, Copy)]
pub enum Appearance {
    Solid { dark: bool },
    Transparent { dark: bool },
    TransparentPreview { dark: bool },
}

enum Surface {
    Window(ID2D1HwndRenderTarget),
    Layered(ID2D1DCRenderTarget, LayeredBitmap),
}

impl Surface {
    fn render_target(&self) -> &ID2D1RenderTarget {
        match self {
            Self::Window(target) => target,
            Self::Layered(target, _) => target,
        }
    }
}

struct Target {
    surface: Surface,
    brush: ID2D1SolidColorBrush,
}

struct LayeredBitmap {
    dc: HDC,
    bitmap: HBITMAP,
    size: SIZE,
}

impl LayeredBitmap {
    fn new(size: D2D_SIZE_U) -> Result<Self> {
        let size = SIZE {
            cx: size.width as i32,
            cy: size.height as i32,
        };
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size.cx,
                biHeight: -size.cy,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        unsafe {
            let bitmap = CreateDIBSection(
                None,
                &info,
                DIB_RGB_COLORS,
                &mut std::ptr::null_mut(),
                None,
                0,
            )?;
            let surface = Self {
                dc: CreateCompatibleDC(None),
                bitmap,
                size,
            };
            if surface.dc.is_invalid() || SelectObject(surface.dc, HGDIOBJ(bitmap.0)).is_invalid() {
                return Err(Error::from_thread());
            }
            Ok(surface)
        }
    }

    fn present(&self, hwnd: HWND) -> Result<()> {
        unsafe {
            UpdateLayeredWindow(
                hwnd,
                None,
                None,
                Some(&self.size),
                Some(self.dc),
                Some(&POINT::default()),
                COLORREF(0),
                Some(&BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    BlendFlags: 0,
                    SourceConstantAlpha: 255,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                }),
                ULW_ALPHA,
            )
        }
    }
}

impl Drop for LayeredBitmap {
    fn drop(&mut self) {
        unsafe {
            if !self.dc.is_invalid() {
                let _ = DeleteDC(self.dc);
            }
            let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
        }
    }
}

struct TextReading {
    displayed: Reading,
    number: IDWriteTextLayout,
    unit: IDWriteTextLayout,
    value_right: f32,
    number_x: f32,
    unit_x: f32,
}

struct MetricLabel {
    layout: IDWriteTextLayout,
    ink_left: f32,
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
    label_language: Language,
    label_bold: bool,
    labels: [Option<MetricLabel>; Metric::ALL.len()],
    empty_label: IDWriteTextLayout,
    values: [Option<TextReading>; Metric::ALL.len()],
    target: Option<Target>,
}

impl Panel {
    pub fn new(language: Language) -> Result<Self> {
        let factory = unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };
        let fonts = unsafe { DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED)? };
        let labels = metric_labels(&fonts, language, true)?;
        let number_format = text_format(
            &fonts,
            TextSize::DEFAULT as f32,
            DWRITE_FONT_WEIGHT_SEMI_BOLD,
            language,
        )?;
        let compact_format = text_format(&fonts, 15.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, language)?;
        let percent_format = text_format(&fonts, 14.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, language)?;
        let unit_format = text_format(&fonts, 11.5, DWRITE_FONT_WEIGHT_NORMAL, language)?;
        let tabular = unsafe {
            let typography = fonts.CreateTypography()?;
            typography.AddFontFeature(DWRITE_FONT_FEATURE {
                nameTag: DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES,
                parameter: 1,
            })?;
            typography
        };
        let empty_label = empty_label(&fonts, language)?;
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
            label_language: language,
            label_bold: true,
            labels,
            empty_label,
            values: [const { None }; Metric::ALL.len()],
            target: None,
        })
    }

    pub fn set_label_style(&mut self, language: Language, bold: bool) -> Result<()> {
        if self.label_language != language || self.label_bold != bold {
            let labels = metric_labels(&self.fonts, language, bold)?;
            if self.label_language != language {
                self.empty_label = empty_label(&self.fonts, language)?;
            }
            self.labels = labels;
            self.label_language = language;
            self.label_bold = bold;
        }
        Ok(())
    }

    pub fn set_layout(&mut self, mode: Layout, display: DisplayOptions) {
        if self.mode != mode
            || self.display.metrics != display.metrics
            || self.display.density != display.density
            || self.display.language != display.language
        {
            self.values = [const { None }; Metric::ALL.len()];
        }
        self.mode = mode;
        self.display = display;
    }

    pub fn resize(&mut self, hwnd: HWND, dpi: u32) {
        if let Some(target) = &self.target {
            let keep_target = client_size(hwnd).is_ok_and(|size| match &target.surface {
                Surface::Window(window) => unsafe {
                    window.SetDpi(dpi as f32, dpi as f32);
                    window.Resize(&size).is_ok()
                },
                Surface::Layered(render, bitmap) => {
                    let same_size =
                        size.width == bitmap.size.cx as u32 && size.height == bitmap.size.cy as u32;
                    if same_size {
                        unsafe { render.SetDpi(dpi as f32, dpi as f32) };
                    }
                    same_size
                }
            });
            if !keep_target {
                self.target = None;
            }
        }
    }

    pub fn paint(
        &mut self,
        hwnd: HWND,
        dpi: u32,
        appearance: Appearance,
        readings: &[Reading; Metric::ALL.len()],
    ) -> Result<()> {
        let pixel_scale = pixel_scale(self.display.text_size, dpi);
        self.update_text(readings, pixel_scale)?;
        let layered = matches!(appearance, Appearance::Transparent { .. });
        if !self
            .target
            .as_ref()
            .is_some_and(|target| matches!(target.surface, Surface::Layered(..)) == layered)
        {
            self.target = Some(self.create_target(hwnd, dpi, layered)?);
        }
        let target = self.target.as_ref().expect("render target initialized");
        let colors = match appearance {
            Appearance::Solid { dark: false } => LIGHT,
            Appearance::Solid { dark: true } => DARK,
            Appearance::Transparent { dark: false }
            | Appearance::TransparentPreview { dark: false } => TRANSPARENT_LIGHT,
            Appearance::Transparent { dark: true }
            | Appearance::TransparentPreview { dark: true } => TRANSPARENT_DARK,
        };
        let background = if layered {
            D2D1_COLOR_F::default()
        } else {
            color(colors.surface)
        };
        let solid = matches!(appearance, Appearance::Solid { .. });
        let mut divider = color(colors.divider);
        if !solid {
            divider.a = 0.24;
        }
        let size = self.mode.size(self.display);
        let width = size.width as f32 + size.gap_px as f32 / pixel_scale;
        let factor = self.display.text_size.factor();
        let result = unsafe {
            let window = target.surface.render_target();
            let brush = &target.brush;
            window.BeginDraw();
            window.Clear(Some(&background));
            window.SetTransform(&Matrix3x2::scale(factor, factor));
            if solid && self.mode == Layout::Floating {
                let radius = self.mode.base_radius() as f32 - 0.5;
                brush.SetColor(&color(colors.border));
                window.DrawRoundedRectangle(
                    &D2D1_ROUNDED_RECT {
                        rect: D2D_RECT_F {
                            left: 0.75,
                            top: 0.75,
                            right: width - 0.75,
                            bottom: size.height as f32 - 0.75,
                        },
                        radiusX: radius,
                        radiusY: radius,
                    },
                    brush,
                    1.0,
                    None,
                );
            }
            brush.SetColor(&divider);
            for (start, end) in self.mode.dividers(self.display, pixel_scale) {
                window.DrawLine(start, end, brush, 1.0, None);
            }
            if self.display.metrics.iter().next().is_none() {
                brush.SetColor(&color(colors.label));
                let x = (width - 112.0) / 2.0;
                let y = (size.height as f32 - ROW_HEIGHT) / 2.0;
                window.DrawTextLayout(
                    point(x, y),
                    &self.empty_label,
                    brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                );
            }
            for (metric, slot) in self.mode.slots(self.display, pixel_scale) {
                let index = metric.index();
                let text = self.values[index]
                    .as_ref()
                    .expect("visible reading initialized");
                if let Some(label) = &self.labels[index] {
                    brush.SetColor(&color(colors.label));
                    window.DrawTextLayout(
                        point(slot.label_x - label.ink_left, slot.row_y),
                        &label.layout,
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
                    draw_arrow(window, brush, slot.arrow_x(), slot.row_y, upload);
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
        }
        .and_then(|()| match &target.surface {
            Surface::Window(_) => Ok(()),
            Surface::Layered(_, bitmap) => bitmap.present(hwnd),
        });
        if result.is_err() {
            self.target = None;
        }
        result
    }

    fn update_text(
        &mut self,
        readings: &[Reading; Metric::ALL.len()],
        pixel_scale: f32,
    ) -> Result<()> {
        for (metric, slot) in self.mode.slots(self.display, pixel_scale) {
            let cached = &mut self.values[metric.index()];
            let reading = &readings[metric.index()];
            let right = slot.value_right();
            if cached
                .as_ref()
                .is_some_and(|value| value.displayed == *reading && value.value_right == right)
            {
                continue;
            }
            let number_format = match metric {
                Metric::Cpu
                | Metric::Memory
                | Metric::Disk
                | Metric::Battery
                | Metric::Uptime
                | Metric::GpuMemoryPercent
                | Metric::AvailableMemoryPercent => &self.percent_format,
                _ if self.mode == Layout::Taskbar => &self.compact_format,
                _ => &self.number_format,
            };
            let number = layout(
                &self.fonts,
                number_format,
                &reading.number,
                slot.value_width,
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
            let unit_gap = match reading.unit {
                "" => 0.0,
                "%" => PERCENT_GAP,
                _ => UNIT_GAP,
            };
            let (_, number_ink_right) = text_bounds(&number)?;
            let (unit_ink_left, unit_ink_right) = if reading.unit.is_empty() {
                (0.0, 0.0)
            } else {
                text_bounds(&unit)?
            };
            let reading_ink_right = number_ink_right + unit_gap + unit_ink_right - unit_ink_left;
            *cached = Some(TextReading {
                displayed: reading.clone(),
                number,
                unit,
                value_right: right,
                number_x: right - reading_ink_right,
                unit_x: right - unit_ink_right,
            });
        }
        Ok(())
    }

    fn create_target(&self, hwnd: HWND, dpi: u32, layered: bool) -> Result<Target> {
        let size = client_size(hwnd)?;
        let mut properties = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
            dpiX: dpi as f32,
            dpiY: dpi as f32,
            ..Default::default()
        };
        unsafe {
            let surface = if layered {
                let bitmap = LayeredBitmap::new(size)?;
                properties.pixelFormat = D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                };
                let target = self.factory.CreateDCRenderTarget(&properties)?;
                target.BindDC(
                    bitmap.dc,
                    &RECT {
                        right: bitmap.size.cx,
                        bottom: bitmap.size.cy,
                        ..Default::default()
                    },
                )?;
                target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
                Surface::Layered(target, bitmap)
            } else {
                let target = self.factory.CreateHwndRenderTarget(
                    &properties,
                    &D2D1_HWND_RENDER_TARGET_PROPERTIES {
                        hwnd,
                        pixelSize: size,
                        presentOptions: D2D1_PRESENT_OPTIONS_IMMEDIATELY,
                    },
                )?;
                target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE);
                Surface::Window(target)
            };
            let brush = surface
                .render_target()
                .CreateSolidColorBrush(&color(LIGHT.number), None)?;
            Ok(Target { surface, brush })
        }
    }
}

// The first frame uses GDI before DirectWrite and the system counters are ready.
pub fn paint_starting(hwnd: HWND, dc: HDC, dpi: u32, dark: bool, display: DisplayOptions) {
    let mut rect = RECT::default();
    if unsafe { GetClientRect(hwnd, &mut rect) }.is_err() {
        return;
    }
    let colors = if dark { DARK } else { LIGHT };
    let mut font = LOGFONTW {
        lfHeight: -super::scale(display.text_size.scale(14), dpi),
        lfWeight: 400,
        lfCharSet: DEFAULT_CHARSET,
        ..Default::default()
    };
    for (dst, src) in font.lfFaceName.iter_mut().zip("Segoe UI".encode_utf16()) {
        *dst = src;
    }
    let mut text: Vec<_> = display
        .language
        .choose("正在读取系统指标…", "Reading system metrics…")
        .encode_utf16()
        .collect();
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
    language: Language,
) -> Result<IDWriteTextFormat> {
    unsafe {
        let format = fonts.CreateTextFormat(
            w!("Segoe UI"),
            None,
            weight,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            language.choose(w!("zh-CN"), w!("en-US")),
        )?;
        format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        format.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, ROW_HEIGHT, ROW_BASELINE)?;
        Ok(format)
    }
}

fn metric_labels(
    fonts: &IDWriteFactory,
    language: Language,
    bold: bool,
) -> Result<[Option<MetricLabel>; Metric::ALL.len()]> {
    let weight = if bold {
        DWRITE_FONT_WEIGHT_SEMI_BOLD
    } else {
        DWRITE_FONT_WEIGHT_NORMAL
    };
    let format = text_format(fonts, 14.0, weight, language)?;
    let mut labels = [const { None }; Metric::ALL.len()];
    for metric in Metric::ALL
        .into_iter()
        .filter(|&metric| !is_network_rate(metric))
    {
        let label = match metric {
            Metric::Cpu | Metric::CpuFrequency => "CPU",
            Metric::Memory => language.choose("内存", "RAM"),
            Metric::AvailableMemory | Metric::AvailableMemoryPercent => {
                language.choose("可用内存", "Free RAM")
            }
            Metric::UsedMemory => language.choose("已用内存", "Used RAM"),
            Metric::Disk => language.choose("磁盘", "Disk"),
            Metric::DiskRead => language.choose("磁盘读", "Disk Read"),
            Metric::DiskWrite => language.choose("磁盘写", "Disk Write"),
            Metric::GpuMemoryPercent => language.choose("显存", "VRAM"),
            Metric::UsedGpuMemory => language.choose("已用显存", "Used VRAM"),
            Metric::Battery => language.choose("电量", "Battery"),
            Metric::Traffic => language.choose("累计流量", "Traffic"),
            _ => super::metric_name(metric, language),
        };
        let layout = layout(fonts, &format, label, 80.0)?;
        let (ink_left, _) = text_bounds(&layout)?;
        labels[metric.index()] = Some(MetricLabel { layout, ink_left });
    }
    Ok(labels)
}

fn empty_label(fonts: &IDWriteFactory, language: Language) -> Result<IDWriteTextLayout> {
    let format = text_format(fonts, 12.0, DWRITE_FONT_WEIGHT_NORMAL, language)?;
    let text = language.choose("未选择监控项", "No metrics selected");
    let label = layout(fonts, &format, text, 112.0)?;
    unsafe { label.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)? };
    Ok(label)
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

#[cfg(test)]
fn text_right_edge(text: &IDWriteTextLayout) -> Result<f32> {
    Ok(text_bounds(text)?.1)
}

fn text_bounds(text: &IDWriteTextLayout) -> Result<(f32, f32)> {
    let overhang = unsafe { text.GetOverhangMetrics()? };
    Ok((
        -overhang.left,
        unsafe { text.GetMaxWidth() } + overhang.right,
    ))
}

fn draw_arrow(
    window: &ID2D1RenderTarget,
    brush: &ID2D1SolidColorBrush,
    x: f32,
    row_y: f32,
    up: bool,
) {
    let mid = row_y + 13.0;
    let tip = mid + if up { -4.5 } else { 4.5 };
    let tail = 2.0 * mid - tip;
    let wing = tip + if up { ARROW_WING } else { -ARROW_WING };
    let center = point(x, tip);
    unsafe {
        window.DrawLine(point(x, tail), center, brush, 1.45, None);
        window.DrawLine(point(x - ARROW_WING, wing), center, brush, 1.45, None);
        window.DrawLine(point(x + ARROW_WING, wing), center, brush, 1.45, None);
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

    fn assert_row_alignment(panel: &Panel, metric: Metric, slot: &Slot, scale: f32) -> f32 {
        let text = panel.values[metric.index()].as_ref().unwrap();
        let right = if text.displayed.unit.is_empty() {
            text.number_x + text_right_edge(&text.number).unwrap()
        } else {
            text.unit_x + text_right_edge(&text.unit).unwrap()
        };
        assert!((right - slot.value_right()).abs() < 0.01);
        let label_end = match &panel.labels[metric.index()] {
            Some(label) => slot.label_x - label.ink_left + text_right_edge(&label.layout).unwrap(),
            None => slot.arrow_x() + ARROW_WING,
        };
        let gap = match panel.display.density {
            Density::Standard => 8.0,
            Density::Compact => 6.0,
            Density::Tight => 4.0,
        };
        let (number_ink_left, _) = text_bounds(&text.number).unwrap();
        assert!(
            label_end + gap / scale <= text.number_x + number_ink_left + 0.01,
            "{metric:?} overlaps the label gap"
        );
        right
    }

    #[test]
    fn network_arrows_share_a_left_edge_for_different_rate_widths() {
        let display = DisplayOptions {
            metrics: Metrics::parse("upload,download"),
            density: Density::Tight,
            ..Default::default()
        };
        let sample = Sample {
            upload: Some(20_000.0),
            download: Some(999_000_000.0),
            ..Default::default()
        };
        let values = readings(sample, 0, display.language);
        let mut panel = Panel::new(display.language).unwrap();
        for mode in [Layout::Floating, Layout::Taskbar] {
            panel.set_layout(mode, display);
            panel.update_text(&values, 1.0).unwrap();
            let mut slots = mode.slots(display, 1.0);
            let (upload, top) = slots.next().unwrap();
            let (download, bottom) = slots.next().unwrap();
            assert_eq!(top.arrow_x(), bottom.arrow_x());
            assert_eq!(
                assert_row_alignment(&panel, upload, &top, 1.0),
                top.value_right()
            );
            assert_eq!(
                assert_row_alignment(&panel, download, &bottom, 1.0),
                bottom.value_right()
            );
        }
    }

    #[test]
    fn label_weight_can_change_without_resizing_text() {
        let mut panel = Panel::new(Language::Chinese).unwrap();
        let verify = |panel: &Panel, expected| {
            for label in panel.labels.iter().flatten() {
                let mut weight = DWRITE_FONT_WEIGHT::default();
                let mut size = 0.0;
                unsafe {
                    label.layout.GetFontWeight(0, &mut weight, None).unwrap();
                    label.layout.GetFontSize(0, &mut size, None).unwrap();
                }
                assert_eq!(weight, expected);
                assert_eq!(size, 14.0);
            }
        };
        verify(&panel, DWRITE_FONT_WEIGHT_SEMI_BOLD);
        panel.set_label_style(Language::Chinese, false).unwrap();
        verify(&panel, DWRITE_FONT_WEIGHT_NORMAL);
        panel.set_label_style(Language::Chinese, true).unwrap();
        verify(&panel, DWRITE_FONT_WEIGHT_SEMI_BOLD);
    }

    #[test]
    fn text_fits_in_both_layouts() {
        let mut panel = Panel::new(Language::Chinese).unwrap();
        let sample = Sample {
            upload: Some(1.0),
            download: Some(999_000_000.0),
            cpu: Some(100.0),
            memory: Some(PhysicalMemory {
                total: 2_000_000_000_000,
                available: 999_400_000_000,
            }),
            disk: Some(100.0),
            cpu_mhz: Some(10_000.0),
            uptime: Some(std::time::Duration::from_secs(999 * 86_400 + 23 * 3_600)),
            battery: Some(100.0),
            ..Default::default()
        };
        let selections = [
            (Metrics::default(), [357, 328]),
            (
                Metrics::parse("upload,download,cpu_frequency,memory,disk,traffic"),
                [386, 355],
            ),
            (
                Metrics::parse(
                    "uptime,upload,available_memory,cpu_frequency,battery,download,traffic",
                ),
                [646, 598],
            ),
            (
                Metrics::parse("traffic,disk,upload,download,cpu_frequency,available_memory"),
                [453, 415],
            ),
            (
                Metrics::parse("available_memory_percent,used_memory,available_memory"),
                [376, 348],
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
                    .update_text(&readings(sample, 999_000_000, Language::Chinese), 1.0)
                    .unwrap();
                assert_eq!(mode.size(display).pixels(96).0, expected_width);
                assert!(
                    mode.slots(display, 1.0)
                        .map(|(metric, _)| metric)
                        .eq(metrics.iter())
                );
                for (metric, slot) in mode.slots(display, 1.0) {
                    let text = panel.values[metric.index()].as_ref().unwrap();
                    let gap = if text.displayed.unit == "%" {
                        PERCENT_GAP
                    } else {
                        UNIT_GAP
                    };
                    if !text.displayed.unit.is_empty() {
                        let number_right = text.number_x + text_right_edge(&text.number).unwrap();
                        let unit_left = text.unit_x + text_bounds(&text.unit).unwrap().0;
                        assert!((unit_left - number_right - gap).abs() < 0.01);
                    }
                    assert_row_alignment(&panel, metric, &slot, 1.0);
                }
            }
        }
    }

    #[test]
    fn density_preserves_pixel_gaps_at_different_dpis_and_text_sizes() {
        let mut panel = Panel::new(Language::Chinese).unwrap();
        let values = readings(Sample::default(), 16_000_000, Language::Chinese);
        for (mode, widths) in [
            (Layout::Floating, [357, 321, 290]),
            (Layout::Taskbar, [328, 297, 271]),
        ] {
            let height = mode.size(DisplayOptions::default()).pixels(96).1;
            for ((density, gap), width) in [
                (Density::Standard, 8),
                (Density::Compact, 6),
                (Density::Tight, 4),
            ]
            .into_iter()
            .zip(widths)
            {
                let display = DisplayOptions {
                    density,
                    ..Default::default()
                };
                assert_eq!(mode.size(display).pixels(96), (width, height));
                for (dpi, text_size) in [(96, 16), (168, 15), (192, 24), (120, 10)] {
                    let display = DisplayOptions {
                        text_size: TextSize::new(text_size).unwrap(),
                        ..display
                    };
                    let scale = pixel_scale(display.text_size, dpi);
                    let size = mode.size(display);
                    let width = size.pixels(dpi).0;
                    let available = width * 3 / 4;
                    let preview_dpi = size.fitting_dpi(dpi, available);
                    assert!(size.pixels(preview_dpi).0 <= available);
                    for column in mode.columns(display) {
                        let slot = column.slot(0.0, scale);
                        let gap_px =
                            (slot.value_x - slot.label_x - column.label_width as f32) * scale;
                        assert!((gap_px - gap as f32).abs() < 0.01);
                    }
                    panel.set_layout(mode, display);
                    panel.update_text(&values, scale).unwrap();
                    for (metric, slot) in mode.slots(display, scale) {
                        assert_row_alignment(&panel, metric, &slot, scale);
                    }
                }
            }
        }
    }

    #[test]
    fn individual_metrics_fit_in_both_languages() {
        let sample = Sample {
            upload: Some(999_000_000.0),
            download: Some(999_000_000.0),
            cpu: Some(100.0),
            memory: Some(PhysicalMemory {
                total: 64_000_000_000_000,
                available: 32_000_000_000_000,
            }),
            disk: Some(100.0),
            cpu_mhz: Some(10_000.0),
            uptime: Some(std::time::Duration::from_secs(999 * 86_400 + 23 * 3_600)),
            battery: Some(100.0),
            disk_read: Some(999_000_000.0),
            disk_write: Some(999_000_000.0),
            gpu_memory: Some(crate::model::GpuMemory {
                used: 1_000_000_000_000,
                total: 1_000_000_000_000,
            }),
            ..Default::default()
        };
        for language in [Language::Chinese, Language::English] {
            let mut panel = Panel::new(language).unwrap();
            let mut values = readings(sample, 999_000_000, language);
            for metric in [Metric::Memory, Metric::AvailableMemoryPercent] {
                values[metric.index()].number = "100".into();
            }
            for mode in [Layout::Floating, Layout::Taskbar] {
                for metric in Metric::ALL {
                    let display = DisplayOptions {
                        language,
                        metrics: Metrics::parse(metric.key()),
                        ..Default::default()
                    };
                    panel.set_layout(mode, display);
                    panel.update_text(&values, 1.0).unwrap();
                    let (_, slot) = mode.slots(display, 1.0).next().unwrap();
                    assert_row_alignment(&panel, metric, &slot, 1.0);
                }
            }
        }
    }

    #[test]
    fn readings_stay_against_column_edges_when_digits_and_units_change() {
        let mut panel = Panel::new(Language::Chinese).unwrap();
        for mode in [Layout::Floating, Layout::Taskbar] {
            for density in [Density::Standard, Density::Compact, Density::Tight] {
                let display = DisplayOptions {
                    density,
                    ..Default::default()
                };
                panel.set_layout(mode, display);
                let padding = mode.column_padding(density) as f32;
                let columns: Vec<_> = mode.columns(display).collect();
                let slots: Vec<_> = mode.slots(display, 1.0).collect();
                for (cpu, disk, upload, total) in [
                    (9.0, 1.0, 322_000.0, 8_000_000_000),
                    (100.0, 100.0, 999_000_000.0, 125_000_000_000),
                    (4.0, 0.0, 22.0, 975_000_000),
                ] {
                    let sample = Sample {
                        cpu: Some(cpu),
                        disk: Some(disk),
                        upload: Some(upload),
                        memory: Some(PhysicalMemory {
                            total: 100,
                            available: 38,
                        }),
                        ..Default::default()
                    };
                    panel
                        .update_text(&readings(sample, total, display.language), 1.0)
                        .unwrap();
                    for (index, rows) in slots.chunks_exact(2).enumerate() {
                        let [(first, top), (second, bottom)] = rows else {
                            unreachable!();
                        };
                        let top_right = assert_row_alignment(&panel, *first, top, 1.0);
                        let bottom_right = assert_row_alignment(&panel, *second, bottom, 1.0);
                        let column = &columns[index];
                        let column_end =
                            (column.left + column.width + column.left_gap_px + column.gap_px)
                                as f32;
                        assert!((column_end - top_right - padding).abs() < 0.01);
                        assert!((column_end - bottom_right - padding).abs() < 0.01);
                        if (*first, *second) == (Metric::Cpu, Metric::Memory) {
                            let digit_right = |metric: Metric| {
                                let text = panel.values[metric.index()].as_ref().unwrap();
                                text.number_x + text_right_edge(&text.number).unwrap()
                            };
                            assert!((digit_right(*first) - digit_right(*second)).abs() < 0.01);
                        }
                        if index != 0 {
                            assert_eq!(
                                top.label_x,
                                (column.left + column.left_gap_px) as f32 + padding
                            );
                            assert_eq!(bottom.label_x, top.label_x);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn draws_both_layouts_at_175_percent_scaling() {
        use ::windows::{
            Win32::{
                Graphics::Gdi::{DIBSECTION, GdiFlush, GetObjectW},
                UI::WindowsAndMessaging::{
                    CreateWindowExW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SetWindowPos,
                    WINDOW_EX_STYLE, WS_POPUP,
                },
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
        let mut panel = Panel::new(Language::Chinese).unwrap();
        let sample = Sample {
            upload: Some(409_000.0),
            download: Some(26_000.0),
            cpu: Some(1.0),
            memory: Some(PhysicalMemory {
                total: 100,
                available: 38,
            }),
            disk: Some(1.0),
            cpu_mhz: Some(4_230.0),
            ..Default::default()
        };
        let readings = readings(sample, 8_000_000_000, Language::Chinese);
        let custom = DisplayOptions {
            language: Language::Chinese,
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
        let taskbar = DisplayOptions {
            density: Density::Tight,
            text_size: TextSize::new(15).unwrap(),
            ..Default::default()
        };
        let taskbar_compact = DisplayOptions {
            density: Density::Compact,
            ..taskbar
        };
        for (mode, display) in [
            (Layout::Floating, custom),
            (Layout::Taskbar, compact),
            (Layout::Taskbar, DisplayOptions::default()),
            (Layout::Taskbar, taskbar_compact),
            (Layout::Taskbar, taskbar),
        ] {
            let (width, height) = mode.size(display).pixels(dpi);
            unsafe {
                SetWindowPos(
                    hwnd,
                    None,
                    0,
                    0,
                    width,
                    height,
                    SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOZORDER,
                )
            }
            .unwrap();
            panel.set_layout(mode, display);
            panel.resize(hwnd, dpi);
            super::super::set_panel_layered(hwnd, false);
            for dark in [false, true] {
                panel
                    .paint(hwnd, dpi, Appearance::Solid { dark }, &readings)
                    .unwrap();
            }
            super::super::set_panel_layered(hwnd, true);
            for dark in [false, true] {
                panel
                    .paint(hwnd, dpi, Appearance::Transparent { dark }, &readings)
                    .unwrap();
                let Some(Target {
                    surface: Surface::Layered(_, bitmap),
                    ..
                }) = &panel.target
                else {
                    panic!("transparent theme requires a layered render target");
                };
                let mut dib = DIBSECTION::default();
                let bytes = size_of::<DIBSECTION>() as i32;
                unsafe {
                    assert!(GdiFlush().as_bool());
                    assert_eq!(
                        GetObjectW(HGDIOBJ(bitmap.bitmap.0), bytes, Some((&raw mut dib).cast())),
                        bytes
                    );
                }
                let pixels = unsafe {
                    std::slice::from_raw_parts(
                        dib.dsBm.bmBits.cast::<u32>(),
                        (bitmap.size.cx * bitmap.size.cy) as usize,
                    )
                };
                assert_eq!(pixels[0], 0);
                assert_eq!(pixels[bitmap.size.cx as usize - 1], 0);
                assert_eq!(pixels[pixels.len() - bitmap.size.cx as usize], 0);
                assert_eq!(pixels[pixels.len() - 1], 0);
                assert!(pixels.contains(&if dark { 0xffffffff } else { 0xff000000 }));
                let pixel_scale = pixel_scale(display.text_size, dpi);
                let (start, end) = mode.dividers(display, pixel_scale).next().unwrap();
                let x = (start.X * pixel_scale) as usize;
                let y = ((start.Y + end.Y) * 0.5 * pixel_scale) as usize;
                let divider_alpha = pixels[y * bitmap.size.cx as usize + x] >> 24;
                assert!((2..=127).contains(&divider_alpha));
                if display == taskbar || display == taskbar_compact {
                    let pitch = bitmap.size.cx as usize;
                    let (top_row, bottom_row) =
                        pixels.split_at(pitch * (bitmap.size.cy as usize / 2));
                    let visible_right = |row: &[u32], left: usize, right: usize| {
                        row.chunks_exact(pitch)
                            .filter_map(|line| {
                                line[left..right]
                                    .iter()
                                    .rposition(|pixel| pixel >> 24 >= 128)
                            })
                            .max()
                            .unwrap()
                            + left
                    };
                    let top_right = visible_right(top_row, 0, pitch);
                    let bottom_right = visible_right(bottom_row, 0, pitch);
                    assert!(
                        top_right.abs_diff(bottom_right) <= 1,
                        "1% ends at {top_right}px, 8 GB ends at {bottom_right}px"
                    );
                    let (_, cpu_slot) = mode
                        .slots(display, pixel_scale)
                        .find(|(m, _)| *m == Metric::Cpu)
                        .unwrap();
                    let cpu = panel.values[Metric::Cpu.index()].as_ref().unwrap();
                    let number_right = cpu.number_x + text_right_edge(&cpu.number).unwrap();
                    let left = (cpu_slot.label_x * pixel_scale) as usize;
                    let split = ((number_right + PERCENT_GAP / 2.0) * pixel_scale) as usize;
                    let limit = (cpu_slot.value_right() * pixel_scale).ceil() as usize;
                    let top_digits = visible_right(top_row, left, split);
                    let bottom_digits = visible_right(bottom_row, left, split);
                    assert!(
                        top_digits.abs_diff(bottom_digits) <= 1,
                        "CPU digits end at {top_digits}px, RAM digits end at {bottom_digits}px"
                    );
                    assert_eq!(
                        visible_right(top_row, split, limit),
                        visible_right(bottom_row, split, limit),
                        "CPU and RAM percent signs must have the same right edge"
                    );
                    let padding = mode.column_padding(display.density) as f32 * pixel_scale;
                    for (start, _) in mode.dividers(display, pixel_scale) {
                        let divider_x = start.X * pixel_scale;
                        let left = divider_x.ceil() as usize;
                        for row in [top_row, bottom_row] {
                            let reading_end = visible_right(row, 0, divider_x.floor() as usize);
                            assert!(
                                (divider_x - reading_end as f32 - padding).abs() <= 2.0,
                                "reading ends at {reading_end}px, right divider at {divider_x}px"
                            );
                            let label_start = row
                                .chunks_exact(pitch)
                                .filter_map(|line| {
                                    line[left..].iter().position(|pixel| pixel >> 24 >= 128)
                                })
                                .min()
                                .unwrap()
                                + left;
                            assert!(
                                (label_start as f32 - divider_x - padding).abs() <= 2.0,
                                "label starts at {label_start}px, divider at {divider_x}px"
                            );
                        }
                    }
                }
            }
        }
    }
}
