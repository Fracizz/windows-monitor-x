use ::windows::{
    Win32::{
        Foundation::{HWND, RECT},
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
                DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TEXT_RANGE, DWRITE_WORD_WRAPPING_NO_WRAP,
                DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, IDWriteTextLayout,
                IDWriteTypography,
            },
        },
        UI::WindowsAndMessaging::GetClientRect,
    },
    core::{Result, w},
};
use windows_numerics::Vector2;

use crate::model::Reading;

pub const WIDTH: i32 = 420;
pub const HEIGHT: i32 = 68;
pub const RADIUS: i32 = 11;

const ROW_HEIGHT: f32 = 24.0;
const ROW_BASELINE: f32 = 17.0;
const COLUMN_EDGES: [f32; 2] = [125.0, 244.0];

#[derive(Clone, Copy)]
struct Slot {
    label_x: f32,
    number_x: f32,
    number_end: f32,
    row_y: f32,
}

const SLOTS: [Slot; 6] = [
    Slot {
        label_x: 19.0,
        number_x: 34.0,
        number_end: 76.0,
        row_y: 10.0,
    },
    Slot {
        label_x: 19.0,
        number_x: 34.0,
        number_end: 76.0,
        row_y: 34.0,
    },
    Slot {
        label_x: 141.0,
        number_x: 171.0,
        number_end: 212.0,
        row_y: 10.0,
    },
    Slot {
        label_x: 141.0,
        number_x: 171.0,
        number_end: 212.0,
        row_y: 34.0,
    },
    Slot {
        label_x: 260.0,
        number_x: 309.0,
        number_end: 373.0,
        row_y: 10.0,
    },
    Slot {
        label_x: 260.0,
        number_x: 309.0,
        number_end: 373.0,
        row_y: 34.0,
    },
];

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
    surface: 0xf9fbfe,
    number: 0x142237,
    label: 0x526278,
    border: 0xd0dbe8,
    divider: 0xe1e8f2,
    upload: 0x147466,
    download: 0x285db2,
};
const DARK: Colors = Colors {
    surface: 0x18212d,
    number: 0xedf4ff,
    label: 0xafbdcf,
    border: 0x3b4a5d,
    divider: 0x344153,
    upload: 0x83e4cd,
    download: 0x9ebfff,
};

struct Target {
    window: ID2D1HwndRenderTarget,
    brush: ID2D1SolidColorBrush,
}

struct TextReading {
    displayed: Reading,
    number: IDWriteTextLayout,
    unit: IDWriteTextLayout,
}

pub struct Panel {
    factory: ID2D1Factory,
    fonts: IDWriteFactory,
    number_format: IDWriteTextFormat,
    unit_format: IDWriteTextFormat,
    tabular: IDWriteTypography,
    labels: [IDWriteTextLayout; 4],
    values: [Option<TextReading>; 6],
    target: Option<Target>,
}

impl Panel {
    pub fn new() -> Result<Self> {
        let factory = unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };
        let fonts = unsafe { DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED)? };
        let label_format = text_format(&fonts, 13.0, DWRITE_FONT_WEIGHT_NORMAL)?;
        let number_format = text_format(&fonts, 16.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?;
        let unit_format = text_format(&fonts, 11.5, DWRITE_FONT_WEIGHT_NORMAL)?;
        let tabular = unsafe {
            number_format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING)?;
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
            layout(&fonts, &label_format, "硬盘", 47.0)?,
            layout(&fonts, &label_format, "总流量", 48.0)?,
        ];
        Ok(Self {
            factory,
            fonts,
            number_format,
            unit_format,
            tabular,
            labels,
            values: [const { None }; 6],
            target: None,
        })
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
        let result = unsafe {
            let window = &target.window;
            let brush = &target.brush;
            window.BeginDraw();
            window.Clear(Some(&color(colors.surface)));
            brush.SetColor(&color(colors.border));
            window.DrawRoundedRectangle(
                &D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: 0.75,
                        top: 0.75,
                        right: WIDTH as f32 - 0.75,
                        bottom: HEIGHT as f32 - 0.75,
                    },
                    radiusX: RADIUS as f32 - 0.5,
                    radiusY: RADIUS as f32 - 0.5,
                },
                brush,
                1.0,
                None,
            );
            brush.SetColor(&color(colors.divider));
            for x in COLUMN_EDGES {
                window.DrawLine(point(x, 12.0), point(x, 56.0), brush, 1.0, None);
            }
            brush.SetColor(&color(colors.upload));
            draw_arrow(target, SLOTS[0], true);
            brush.SetColor(&color(colors.download));
            draw_arrow(target, SLOTS[1], false);
            brush.SetColor(&color(colors.label));
            for (slot, label) in SLOTS[2..].iter().zip(&self.labels) {
                window.DrawTextLayout(
                    point(slot.label_x, slot.row_y),
                    label,
                    brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                );
            }
            for (slot, text) in SLOTS.iter().zip(self.values.iter().flatten()) {
                brush.SetColor(&color(colors.number));
                window.DrawTextLayout(
                    point(slot.number_x, slot.row_y),
                    &text.number,
                    brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                );
                brush.SetColor(&color(colors.label));
                window.DrawTextLayout(
                    point(slot.number_end + 4.0, slot.row_y),
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
        for ((cached, reading), slot) in self.values.iter_mut().zip(readings).zip(SLOTS) {
            if cached
                .as_ref()
                .is_some_and(|value| value.displayed == *reading)
            {
                continue;
            }
            let number = layout(
                &self.fonts,
                &self.number_format,
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
            *cached = Some(TextReading {
                displayed: reading.clone(),
                number,
                unit,
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

fn draw_arrow(target: &Target, slot: Slot, up: bool) {
    let mid = slot.row_y + 13.0;
    let tip = mid + if up { -4.5 } else { 4.5 };
    let tail = 2.0 * mid - tip;
    let wing = tip + if up { 3.7 } else { -3.7 };
    let center = point(slot.label_x, tip);
    unsafe {
        target
            .window
            .DrawLine(point(slot.label_x, tail), center, &target.brush, 1.45, None);
        target.window.DrawLine(
            point(slot.label_x - 3.7, wing),
            center,
            &target.brush,
            1.45,
            None,
        );
        target.window.DrawLine(
            point(slot.label_x + 3.7, wing),
            center,
            &target.brush,
            1.45,
            None,
        );
    }
}

fn point(x: f32, y: f32) -> Vector2 {
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
    fn text_fits_fixed_columns() {
        let mut panel = Panel::new().unwrap();
        let sample = Sample {
            upload: Some(999_000.0),
            download: Some(9_990_000_000.0),
            cpu: Some(100.0),
            memory: Some(100.0),
            disk: Some(100.0),
            ..Default::default()
        };
        panel
            .update_text(&readings(sample, 999_990_000_000))
            .unwrap();
        for ((index, slot), text) in SLOTS.iter().enumerate().zip(panel.values.iter().flatten()) {
            let mut number = Default::default();
            let mut unit = Default::default();
            unsafe {
                text.number.GetMetrics(&mut number).unwrap();
                text.unit.GetMetrics(&mut unit).unwrap();
            }
            let column_end = COLUMN_EDGES.get(index / 2).copied().unwrap_or(WIDTH as f32);
            assert!(number.width <= slot.number_end - slot.number_x);
            assert!(slot.number_end + 4.0 + unit.width + 5.0 < column_end);
            assert_eq!(number.lineCount, 1);
            assert_eq!(unit.lineCount, 1);
        }
    }
}
