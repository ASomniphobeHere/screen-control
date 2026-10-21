//! Direct2D and DirectWrite setup, colours, fonts, and the surface of a layered window.
//! The timer and the popups use it. Sizes are in DIPs (1/96 inch); the surface scales them by
//! the screen's DPI.

use anyhow::{Context, Result};
use windows::Win32::Foundation::{HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_F, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_HOLLOW,
    D2D1_FIGURE_END_OPEN, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_LARGE, D2D1_ARC_SIZE_SMALL, D2D1_CAP_STYLE_ROUND,
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_FEATURE_LEVEL_DEFAULT, D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT,
    D2D1_RENDER_TARGET_USAGE_GDI_COMPATIBLE, D2D1_ROUNDED_RECT, D2D1_STROKE_STYLE_PROPERTIES,
    D2D1_SWEEP_DIRECTION_CLOCKWISE, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1CreateFactory,
    ID2D1DCRenderTarget, ID2D1Factory, ID2D1RenderTarget, ID2D1StrokeStyle,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_FEATURE, DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT,
    DWRITE_LINE_SPACING_METHOD_UNIFORM, DWRITE_TEXT_METRICS, DWRITE_TEXT_RANGE,
    DWriteCreateFactory, IDWriteFactory, IDWriteTextLayout,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC,
    GetMonitorInfoW, HBITMAP, HDC, HGDIOBJ, HMONITOR, MONITORINFO, ReleaseDC, SelectObject,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{ULW_ALPHA, UpdateLayeredWindow};
use windows::core::{HSTRING, w};
use windows_numerics::Vector2;

/// A colour from `0xRRGGBB` and an alpha from 0 to 1.
pub const fn rgba(rgb: u32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((rgb >> 16) & 0xff) as f32 / 255.0,
        g: ((rgb >> 8) & 0xff) as f32 / 255.0,
        b: (rgb & 0xff) as f32 / 255.0,
        a,
    }
}

/// The colours of the dark style.
pub mod colour {
    use super::rgba;
    use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;

    pub const PILL: D2D1_COLOR_F = rgba(0x121214, 0.88);
    pub const TRACK: D2D1_COLOR_F = rgba(0xFFFFFF, 0.16);
    pub const GREEN: D2D1_COLOR_F = rgba(0x3FB950, 1.0);
    pub const YELLOW: D2D1_COLOR_F = rgba(0xE3B341, 1.0);
    pub const RED: D2D1_COLOR_F = rgba(0xFF7B72, 1.0);
    pub const TEXT: D2D1_COLOR_F = rgba(0xF4F4F5, 1.0);
    pub const TEXT_YELLOW: D2D1_COLOR_F = rgba(0xF2CC60, 1.0);
    pub const TEXT_RED: D2D1_COLOR_F = rgba(0xFF9B94, 1.0);
    pub const LABEL: D2D1_COLOR_F = rgba(0xC9CDD3, 1.0);
}

pub const FONT: &str = "Segoe UI";
pub const SEMI_BOLD: DWRITE_FONT_WEIGHT = DWRITE_FONT_WEIGHT(600);
pub const NORMAL: DWRITE_FONT_WEIGHT = DWRITE_FONT_WEIGHT(400);

/// Segoe UI ascent and full height (ascent plus descent), as parts of the font size. Used to
/// place text the way CSS does with a `line-height`.
const ASCENT: f32 = 2210.0 / 2048.0;
const CONTENT: f32 = (2210.0 + 514.0) / 2048.0;

/// The Direct2D and DirectWrite factories. One for the whole helper.
pub struct Draw {
    d2d: ID2D1Factory,
    dwrite: IDWriteFactory,
    round: ID2D1StrokeStyle,
}

/// Text ready to draw, with its size in DIPs.
pub struct Text {
    pub layout: IDWriteTextLayout,
    pub width: f32,
    pub height: f32,
}

/// How to make a piece of text.
pub struct Font {
    pub size: f32,
    pub weight: DWRITE_FONT_WEIGHT,
    /// Digits all have the same width, so the time does not jump.
    pub tabular: bool,
    /// Line height as a part of the size, like CSS `line-height`. `None` uses the font's own.
    pub line_height: Option<f32>,
}

impl Draw {
    pub fn new() -> Result<Draw> {
        let d2d: ID2D1Factory =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None) }
                .context("cannot start Direct2D")?;
        let dwrite: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }
            .context("cannot start DirectWrite")?;
        let round = unsafe {
            d2d.CreateStrokeStyle(
                &D2D1_STROKE_STYLE_PROPERTIES {
                    startCap: D2D1_CAP_STYLE_ROUND,
                    endCap: D2D1_CAP_STYLE_ROUND,
                    ..Default::default()
                },
                None,
            )
        }?;
        Ok(Draw { d2d, dwrite, round })
    }

    /// Lays out one line of text.
    pub fn text(&self, text: &str, font: &Font) -> Result<Text> {
        let wide: Vec<u16> = text.encode_utf16().collect();
        let format = unsafe {
            self.dwrite.CreateTextFormat(
                &HSTRING::from(FONT),
                None,
                font.weight,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                font.size,
                w!("en-us"),
            )
        }?;
        if let Some(line) = font.line_height {
            // CSS puts the extra space (or the missing space) half above and half below.
            let height = line * font.size;
            let baseline = (line - CONTENT) / 2.0 * font.size + ASCENT * font.size;
            unsafe { format.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, height, baseline) }?;
        }
        let layout = unsafe {
            self.dwrite
                .CreateTextLayout(&wide, &format, 10_000.0, 10_000.0)
        }?;
        if font.tabular {
            let typography = unsafe { self.dwrite.CreateTypography() }?;
            unsafe {
                typography.AddFontFeature(DWRITE_FONT_FEATURE {
                    nameTag: DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES,
                    parameter: 1,
                })
            }?;
            let all = DWRITE_TEXT_RANGE {
                startPosition: 0,
                length: wide.len() as u32,
            };
            unsafe { layout.SetTypography(&typography, all) }?;
        }
        let mut metrics = DWRITE_TEXT_METRICS::default();
        unsafe { layout.GetMetrics(&mut metrics) }?;
        Ok(Text {
            layout,
            width: metrics.widthIncludingTrailingWhitespace,
            height: metrics.height,
        })
    }
}

/// Drawing calls for one frame, in DIPs.
pub struct Painter<'a> {
    draw: &'a Draw,
    rt: &'a ID2D1RenderTarget,
}

impl Painter<'_> {
    pub fn round_rect(&self, rect: D2D_RECT_F, radius: f32, colour: D2D1_COLOR_F) -> Result<()> {
        let brush = unsafe { self.rt.CreateSolidColorBrush(&colour, None) }?;
        let rounded = D2D1_ROUNDED_RECT {
            rect,
            radiusX: radius,
            radiusY: radius,
        };
        unsafe { self.rt.FillRoundedRectangle(&rounded, &brush) };
        Ok(())
    }

    /// A soft shadow like CSS `box-shadow: 0 <dy> <blur> <colour>`: rounded rectangles that
    /// grow and fade.
    pub fn shadow(
        &self,
        rect: D2D_RECT_F,
        radius: f32,
        dy: f32,
        blur: f32,
        colour: D2D1_COLOR_F,
    ) -> Result<()> {
        const STEPS: usize = 12;
        // Each layer adds a bit, so the middle reaches the full alpha.
        let alpha = 1.0 - (1.0 - colour.a).powf(1.0 / STEPS as f32);
        for i in 0..STEPS {
            let grow = blur / 2.0 * (1.0 - 2.0 * i as f32 / STEPS as f32);
            let r = D2D_RECT_F {
                left: rect.left - grow,
                top: rect.top + dy - grow,
                right: rect.right + grow,
                bottom: rect.bottom + dy + grow,
            };
            let layer = D2D1_COLOR_F { a: alpha, ..colour };
            self.round_rect(r, (radius + grow).max(0.0), layer)?;
        }
        Ok(())
    }

    pub fn circle(&self, centre: Vector2, radius: f32, colour: D2D1_COLOR_F) -> Result<()> {
        let brush = unsafe { self.rt.CreateSolidColorBrush(&colour, None) }?;
        let ellipse = D2D1_ELLIPSE {
            point: centre,
            radiusX: radius,
            radiusY: radius,
        };
        unsafe { self.rt.FillEllipse(&ellipse, &brush) };
        Ok(())
    }

    /// A ring with round ends from the top, clockwise. `part` is from 0 (nothing) to 1 (full).
    pub fn ring(
        &self,
        centre: Vector2,
        radius: f32,
        width: f32,
        part: f32,
        colour: D2D1_COLOR_F,
    ) -> Result<()> {
        if part <= 0.0 {
            return Ok(());
        }
        let brush = unsafe { self.rt.CreateSolidColorBrush(&colour, None) }?;
        if part >= 0.999 {
            let ellipse = D2D1_ELLIPSE {
                point: centre,
                radiusX: radius,
                radiusY: radius,
            };
            unsafe { self.rt.DrawEllipse(&ellipse, &brush, width, None) };
            return Ok(());
        }
        let angle = part * std::f32::consts::TAU;
        let start = Vector2 {
            X: centre.X,
            Y: centre.Y - radius,
        };
        let end = Vector2 {
            X: centre.X + radius * angle.sin(),
            Y: centre.Y - radius * angle.cos(),
        };
        let path = unsafe { self.draw.d2d.CreatePathGeometry() }?;
        let sink = unsafe { path.Open() }?;
        unsafe {
            sink.BeginFigure(start, D2D1_FIGURE_BEGIN_HOLLOW);
            sink.AddArc(&D2D1_ARC_SEGMENT {
                point: end,
                size: D2D_SIZE_F {
                    width: radius,
                    height: radius,
                },
                rotationAngle: 0.0,
                sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                arcSize: if part > 0.5 {
                    D2D1_ARC_SIZE_LARGE
                } else {
                    D2D1_ARC_SIZE_SMALL
                },
            });
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            sink.Close()?;
            self.rt.DrawGeometry(&path, &brush, width, &self.draw.round);
        }
        Ok(())
    }

    pub fn text(&self, text: &Text, x: f32, y: f32, colour: D2D1_COLOR_F) -> Result<()> {
        let brush = unsafe { self.rt.CreateSolidColorBrush(&colour, None) }?;
        unsafe {
            self.rt.DrawTextLayout(
                Vector2 { X: x, Y: y },
                &text.layout,
                &brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            )
        };
        Ok(())
    }
}

/// The monitor's work area (without the taskbar) in pixels, and its DPI.
pub fn monitor_area(monitor: HMONITOR) -> (RECT, u32) {
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let _ = unsafe { GetMonitorInfoW(monitor, &mut info) };
    let (mut dpi, mut dpi_y) = (96, 96);
    if unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y) }.is_err() {
        dpi = 96;
    }
    (info.rcWork, dpi)
}

/// The picture of a layered window: a 32-bit bitmap with alpha that Direct2D draws into and
/// `UpdateLayeredWindow` shows. Keeps the bitmap and render target between frames.
pub struct Surface {
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    size: SIZE,
    target: Option<ID2D1DCRenderTarget>,
}

impl Surface {
    pub fn new() -> Surface {
        Surface {
            dc: unsafe { CreateCompatibleDC(None) },
            bitmap: HBITMAP::default(),
            old: HGDIOBJ::default(),
            size: SIZE::default(),
            target: None,
        }
    }

    /// Draws a frame of `size` pixels with `paint` and puts it on the window. `pos` moves the
    /// window's top left corner; `None` keeps it where it is.
    pub fn show(
        &mut self,
        draw: &Draw,
        hwnd: HWND,
        pos: Option<POINT>,
        size: SIZE,
        dpi: u32,
        paint: impl FnOnce(&Painter) -> Result<()>,
    ) -> Result<()> {
        if size != self.size {
            self.resize(size)?;
        }
        if self.target.is_none() {
            self.target = Some(self.new_target(draw)?);
        }
        let target = self.target.as_ref().expect("made above");
        let rect = RECT {
            left: 0,
            top: 0,
            right: size.cx,
            bottom: size.cy,
        };
        let result = (|| -> Result<()> {
            unsafe {
                target.BindDC(self.dc, &rect)?;
                target.SetDpi(dpi as f32, dpi as f32);
                target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
                target.BeginDraw();
                target.Clear(Some(&rgba(0, 0.0)));
            }
            let rt: &ID2D1RenderTarget = target;
            let painted = paint(&Painter { draw, rt });
            unsafe { target.EndDraw(None, None) }.context("Direct2D cannot draw")?;
            painted
        })();
        if let Err(e) = result {
            // The next frame starts with a new render target, for example after a driver reset.
            self.target = None;
            return Err(e);
        }
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let screen = unsafe { GetDC(None) };
        let shown = unsafe {
            UpdateLayeredWindow(
                hwnd,
                Some(screen),
                pos.as_ref().map(|p| p as *const POINT),
                Some(&size),
                Some(self.dc),
                Some(&POINT::default()),
                Default::default(),
                Some(&blend),
                ULW_ALPHA,
            )
        };
        unsafe { ReleaseDC(None, screen) };
        shown.context("UpdateLayeredWindow failed")
    }

    fn resize(&mut self, size: SIZE) -> Result<()> {
        self.free_bitmap();
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size.cx,
                // Below zero: rows go from the top down.
                biHeight: -size.cy,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        self.bitmap =
            unsafe { CreateDIBSection(Some(self.dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) }
                .context("cannot make the window bitmap")?;
        self.old = unsafe { SelectObject(self.dc, self.bitmap.into()) };
        self.size = size;
        Ok(())
    }

    fn new_target(&self, draw: &Draw) -> Result<ID2D1DCRenderTarget> {
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            // Hardware if there is a driver, software otherwise.
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 0.0,
            dpiY: 0.0,
            usage: D2D1_RENDER_TARGET_USAGE_GDI_COMPATIBLE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        unsafe { draw.d2d.CreateDCRenderTarget(&props) }.context("cannot make a render target")
    }

    fn free_bitmap(&mut self) {
        if !self.bitmap.is_invalid() {
            unsafe {
                SelectObject(self.dc, self.old);
                let _ = DeleteObject(self.bitmap.into());
            }
            self.bitmap = HBITMAP::default();
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        self.free_bitmap();
        let _ = unsafe { DeleteDC(self.dc) };
    }
}
