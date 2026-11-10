//! Direct2D/DirectWrite helpers and the annotation model shared by the editor
//! (GPU, HWND target) and the exporter (WIC target).

use crate::img::Img;
use std::cell::RefCell;
use std::rc::Rc;
use windows::core::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::*;
use windows_numerics::Matrix3x2;

pub struct Gfx {
    pub d2d: ID2D1Factory,
    pub dw: IDWriteFactory,
    pub round: ID2D1StrokeStyle,
    pub flat: ID2D1StrokeStyle,
    pub icon_font: Vec<u16>,
}

impl Gfx {
    pub fn new() -> Result<Gfx> {
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let mk = |cap, join| {
                d2d.CreateStrokeStyle(
                    &D2D1_STROKE_STYLE_PROPERTIES {
                        startCap: cap,
                        endCap: cap,
                        dashCap: cap,
                        lineJoin: join,
                        miterLimit: 10.0,
                        dashStyle: D2D1_DASH_STYLE_SOLID,
                        dashOffset: 0.0,
                    },
                    None,
                )
            };
            let round = mk(D2D1_CAP_STYLE_ROUND, D2D1_LINE_JOIN_ROUND)?;
            let flat = mk(D2D1_CAP_STYLE_SQUARE, D2D1_LINE_JOIN_ROUND)?;
            // Segoe Fluent Icons (Win11) or Segoe MDL2 Assets (Win10)
            let mut icon_font = crate::output::wide("Segoe MDL2 Assets");
            let mut coll = None;
            if dw.GetSystemFontCollection(&mut coll, false).is_ok() {
                if let Some(c) = coll {
                    let mut idx = 0u32;
                    let mut exists = BOOL(0);
                    if c.FindFamilyName(w!("Segoe Fluent Icons"), &mut idx, &mut exists).is_ok() && exists.as_bool() {
                        icon_font = crate::output::wide("Segoe Fluent Icons");
                    }
                }
            }
            Ok(Gfx { d2d, dw, round, flat, icon_font })
        }
    }

    pub fn text_format(&self, size: f32, bold: bool) -> Result<IDWriteTextFormat> {
        unsafe {
            self.dw.CreateTextFormat(
                w!("Segoe UI"),
                None,
                if bold { DWRITE_FONT_WEIGHT_SEMI_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL },
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                w!(""),
            )
        }
    }

    pub fn icon_format(&self, size: f32) -> Result<IDWriteTextFormat> {
        unsafe {
            let f = self.dw.CreateTextFormat(
                PCWSTR(self.icon_font.as_ptr()),
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                w!(""),
            )?;
            f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            Ok(f)
        }
    }

    /// Measures a text block: (width, height).
    pub fn measure(&self, s: &str, fmt: &IDWriteTextFormat) -> (f32, f32) {
        let w: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            if let Ok(l) = self.dw.CreateTextLayout(&w, fmt, 100000.0, 100000.0) {
                let mut m = DWRITE_TEXT_METRICS::default();
                if l.GetMetrics(&mut m).is_ok() {
                    return (m.widthIncludingTrailingWhitespace, m.height);
                }
            }
        }
        (0.0, 0.0)
    }
}

#[inline]
pub fn pt(x: f32, y: f32) -> windows_numerics::Vector2 {
    windows_numerics::Vector2 { X: x, Y: y }
}
#[inline]
pub fn rc(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}
#[inline]
pub fn color(argb: u32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((argb >> 16) & 255) as f32 / 255.0,
        g: ((argb >> 8) & 255) as f32 / 255.0,
        b: (argb & 255) as f32 / 255.0,
        a: ((argb >> 24) & 255) as f32 / 255.0,
    }
}

pub fn brush(rt: &ID2D1RenderTarget, argb: u32) -> Result<ID2D1SolidColorBrush> {
    unsafe { rt.CreateSolidColorBrush(&color(argb), None) }
}

pub fn bitmap(rt: &ID2D1RenderTarget, img: &Img) -> Result<ID2D1Bitmap> {
    unsafe {
        rt.CreateBitmap(
            D2D_SIZE_U { width: img.w as u32, height: img.h as u32 },
            Some(img.px.as_ptr() as *const _),
            (img.w * 4) as u32,
            &D2D1_BITMAP_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_IGNORE,
                },
                dpiX: 96.0,
                dpiY: 96.0,
            },
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    None,
    Rect,
    Ellipse,
    Arrow,
    Line,
    Pen,
    Marker,
    Text,
    Counter,
    Mosaic,
    Blur,
}

impl Tool {
    pub fn default_size(self) -> f32 {
        match self {
            Tool::Marker => 16.0,
            Tool::Text => 20.0,
            Tool::Counter => 14.0,
            // brush diameter (Shift+drag paints a rectangle instead)
            Tool::Mosaic | Tool::Blur => 32.0,
            _ => 3.0,
        }
    }
}

pub enum Item {
    Rect {
        r: D2D_RECT_F,
        c: u32,
        w: f32,
    },
    Ellipse {
        r: D2D_RECT_F,
        c: u32,
        w: f32,
    },
    Arrow {
        a: (f32, f32),
        b: (f32, f32),
        c: u32,
        w: f32,
    },
    Line {
        a: (f32, f32),
        b: (f32, f32),
        c: u32,
        w: f32,
    },
    Pen {
        pts: Vec<(f32, f32)>,
        c: u32,
        w: f32,
        marker: bool,
    },
    Text {
        x: f32,
        y: f32,
        s: String,
        c: u32,
        size: f32,
    },
    Counter {
        x: f32,
        y: f32,
        n: u32,
        c: u32,
        size: f32,
    },
    /// Blur / mosaic: pixels of a pre-computed effect image shown through a shape.
    /// The effect covers the whole selection, so a moved shape re-samples its new spot.
    Fx {
        fx: Rc<Effect>,
        shape: FxShape,
    },
}

pub enum FxShape {
    Rect(D2D_RECT_F),
    Stroke { pts: Vec<(f32, f32)>, w: f32 },
}

/// The selection with blur or mosaic applied, placed at x,y (image coords).
pub struct Effect {
    pub x: i32,
    pub y: i32,
    pub img: Img,
    /// GPU copy (editor only); dropped when the render target is recreated.
    pub cache: RefCell<Option<ID2D1Bitmap>>,
}

fn seg_dist(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 0.0 { (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
    let (qx, qy) = (a.0 + t * dx - p.0, a.1 + t * dy - p.1);
    (qx * qx + qy * qy).sqrt()
}

fn poly_dist(p: (f32, f32), pts: &[(f32, f32)]) -> f32 {
    match pts {
        [] => f32::MAX,
        [a] => seg_dist(p, *a, *a),
        _ => pts.windows(2).map(|w| seg_dist(p, w[0], w[1])).fold(f32::MAX, f32::min),
    }
}

impl Item {
    /// Bounding box (image coords), including stroke width.
    pub fn bounds(&self, g: &Gfx) -> D2D_RECT_F {
        let grow = |r: D2D_RECT_F, m: f32| rc(r.left - m, r.top - m, r.right + m, r.bottom + m);
        let of = |pts: &[(f32, f32)], m: f32| {
            let mut r = rc(f32::MAX, f32::MAX, f32::MIN, f32::MIN);
            for p in pts {
                r = rc(r.left.min(p.0), r.top.min(p.1), r.right.max(p.0), r.bottom.max(p.1));
            }
            grow(r, m)
        };
        match self {
            Item::Rect { r, w, .. } | Item::Ellipse { r, w, .. } => grow(*r, w / 2.0),
            Item::Arrow { a, b, w, .. } => of(&[*a, *b], w * 2.0 + 4.0),
            Item::Line { a, b, w, .. } => of(&[*a, *b], w / 2.0),
            Item::Pen { pts, w, .. } => of(pts, w / 2.0),
            Item::Text { x, y, s, size, .. } => {
                let (tw, th) = g.text_format(*size, true).map(|f| g.measure(s, &f)).unwrap_or((*size, *size));
                rc(*x, *y, x + tw.max(4.0), y + th.max(*size))
            }
            Item::Counter { x, y, size, .. } => rc(x - size, y - size, x + size, y + size),
            Item::Fx { shape: FxShape::Rect(r), .. } => *r,
            Item::Fx { shape: FxShape::Stroke { pts, w }, .. } => of(pts, w / 2.0),
        }
    }

    /// Is (x,y) on the visible part of the item (within `tol`)?
    pub fn hit(&self, x: f32, y: f32, tol: f32) -> bool {
        let p = (x, y);
        let inside =
            |r: &D2D_RECT_F, m: f32| x >= r.left - m && x <= r.right + m && y >= r.top - m && y <= r.bottom + m;
        match self {
            Item::Rect { r, w, .. } => {
                let m = w / 2.0 + tol;
                inside(r, m) && !(x > r.left + m && x < r.right - m && y > r.top + m && y < r.bottom - m)
            }
            Item::Ellipse { r, w, .. } => {
                let (cx, cy) = ((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0);
                let (rx, ry) = (((r.right - r.left) / 2.0).max(1.0), ((r.bottom - r.top) / 2.0).max(1.0));
                let d = (((x - cx) / rx).powi(2) + ((y - cy) / ry).powi(2)).sqrt();
                (d - 1.0).abs() * rx.min(ry) <= w / 2.0 + tol
            }
            Item::Arrow { a, b, w, .. } => seg_dist(p, *a, *b) <= w * 1.5 + tol,
            Item::Line { a, b, w, .. } => seg_dist(p, *a, *b) <= w / 2.0 + tol,
            Item::Pen { pts, w, .. } => poly_dist(p, pts) <= w / 2.0 + tol,
            Item::Text { .. } => false, // bounds-based, see Item::bounds
            Item::Counter { x: cx, y: cy, size, .. } => ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() <= size + tol,
            Item::Fx { shape: FxShape::Rect(r), .. } => inside(r, tol),
            Item::Fx { shape: FxShape::Stroke { pts, w }, .. } => poly_dist(p, pts) <= w / 2.0 + tol,
        }
    }

    pub fn offset(&mut self, dx: f32, dy: f32) {
        let mv = |r: &mut D2D_RECT_F| *r = rc(r.left + dx, r.top + dy, r.right + dx, r.bottom + dy);
        let mvp = |p: &mut (f32, f32)| *p = (p.0 + dx, p.1 + dy);
        match self {
            Item::Rect { r, .. } | Item::Ellipse { r, .. } | Item::Fx { shape: FxShape::Rect(r), .. } => mv(r),
            Item::Arrow { a, b, .. } | Item::Line { a, b, .. } => {
                mvp(a);
                mvp(b);
            }
            Item::Pen { pts, .. } | Item::Fx { shape: FxShape::Stroke { pts, .. }, .. } => pts.iter_mut().for_each(mvp),
            Item::Text { x, y, .. } | Item::Counter { x, y, .. } => {
                *x += dx;
                *y += dy;
            }
        }
    }
}

pub fn norm(a: (f32, f32), b: (f32, f32)) -> D2D_RECT_F {
    rc(a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1))
}

fn with_alpha(c: u32, a: u32) -> u32 {
    (c & 0x00FF_FFFF) | (a << 24)
}

/// Draw one annotation. `cache_bitmaps` = keep the GPU bitmap for patches (editor only).
pub fn draw_item(g: &Gfx, rt: &ID2D1RenderTarget, it: &Item, cache_bitmaps: bool) -> Result<()> {
    unsafe {
        match it {
            Item::Rect { r, c, w } => {
                let b = brush(rt, *c)?;
                rt.DrawRectangle(r, &b, *w, &g.flat);
            }
            Item::Ellipse { r, c, w } => {
                let b = brush(rt, *c)?;
                let e = D2D1_ELLIPSE {
                    point: pt((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0),
                    radiusX: (r.right - r.left) / 2.0,
                    radiusY: (r.bottom - r.top) / 2.0,
                };
                rt.DrawEllipse(&e, &b, *w, None);
            }
            Item::Line { a, b: bb, c, w } => {
                let b = brush(rt, *c)?;
                rt.DrawLine(pt(a.0, a.1), pt(bb.0, bb.1), &b, *w, &g.round);
            }
            Item::Arrow { a, b: bb, c, w } => {
                let b = brush(rt, *c)?;
                let (dx, dy) = (bb.0 - a.0, bb.1 - a.1);
                let len = (dx * dx + dy * dy).sqrt();
                if len < 1.0 {
                    return Ok(());
                }
                let (ux, uy) = (dx / len, dy / len);
                let head = (w * 3.5 + 8.0).min(len);
                let hw = head * 0.55;
                let base = (bb.0 - ux * head, bb.1 - uy * head);
                let shaft_end = (bb.0 - ux * head * 0.8, bb.1 - uy * head * 0.8);
                rt.DrawLine(pt(a.0, a.1), pt(shaft_end.0, shaft_end.1), &b, *w, &g.round);
                let geo = g.d2d.CreatePathGeometry()?;
                let sink = geo.Open()?;
                sink.BeginFigure(pt(bb.0, bb.1), D2D1_FIGURE_BEGIN_FILLED);
                sink.AddLines(&[pt(base.0 - uy * hw, base.1 + ux * hw), pt(base.0 + uy * hw, base.1 - ux * hw)]);
                sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                sink.Close()?;
                rt.FillGeometry(&geo, &b, None);
            }
            Item::Pen { pts, c, w, marker } => {
                let col = if *marker { with_alpha(*c, 0x66) } else { *c };
                let b = brush(rt, col)?;
                if pts.len() == 1 {
                    let e = D2D1_ELLIPSE { point: pt(pts[0].0, pts[0].1), radiusX: w / 2.0, radiusY: w / 2.0 };
                    rt.FillEllipse(&e, &b);
                    return Ok(());
                }
                let geo = g.d2d.CreatePathGeometry()?;
                let sink = geo.Open()?;
                sink.BeginFigure(pt(pts[0].0, pts[0].1), D2D1_FIGURE_BEGIN_HOLLOW);
                let rest: Vec<_> = pts[1..].iter().map(|p| pt(p.0, p.1)).collect();
                sink.AddLines(&rest);
                sink.EndFigure(D2D1_FIGURE_END_OPEN);
                sink.Close()?;
                rt.DrawGeometry(&geo, &b, *w, if *marker { &g.flat } else { &g.round });
            }
            Item::Text { x, y, s, c, size } => {
                let b = brush(rt, *c)?;
                let f = g.text_format(*size, true)?;
                let w: Vec<u16> = s.encode_utf16().collect();
                rt.DrawText(
                    &w,
                    &f,
                    &rc(*x, *y, x + 100000.0, y + 100000.0),
                    &b,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }
            Item::Counter { x, y, n, c, size } => {
                let r = size;
                let b = brush(rt, *c)?;
                let e = D2D1_ELLIPSE { point: pt(*x, *y), radiusX: *r, radiusY: *r };
                rt.FillEllipse(&e, &b);
                let wb = brush(rt, 0xFFFF_FFFF)?;
                rt.DrawEllipse(&e, &wb, (r / 8.0).max(1.5), None);
                let f = g.text_format(r * 1.15, true)?;
                f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
                f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
                let s: Vec<u16> = n.to_string().encode_utf16().collect();
                rt.DrawText(
                    &s,
                    &f,
                    &rc(x - r, y - r, x + r, y + r),
                    &wb,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }
            Item::Fx { fx, shape } => {
                let bmp = if cache_bitmaps {
                    let mut c = fx.cache.borrow_mut();
                    if c.is_none() {
                        *c = Some(bitmap(rt, &fx.img)?);
                    }
                    c.clone().unwrap()
                } else {
                    bitmap(rt, &fx.img)?
                };
                let bp = D2D1_BITMAP_BRUSH_PROPERTIES {
                    extendModeX: D2D1_EXTEND_MODE_CLAMP,
                    extendModeY: D2D1_EXTEND_MODE_CLAMP,
                    interpolationMode: D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                };
                let br = rt.CreateBitmapBrush(&bmp, Some(&bp), None)?;
                br.SetTransform(&Matrix3x2::translation(fx.x as f32, fx.y as f32));
                match shape {
                    FxShape::Rect(r) => rt.FillRectangle(r, &br),
                    FxShape::Stroke { pts, w } if pts.len() == 1 => {
                        let e = D2D1_ELLIPSE { point: pt(pts[0].0, pts[0].1), radiusX: w / 2.0, radiusY: w / 2.0 };
                        rt.FillEllipse(&e, &br);
                    }
                    FxShape::Stroke { pts, w } => {
                        let geo = g.d2d.CreatePathGeometry()?;
                        let sink = geo.Open()?;
                        sink.BeginFigure(pt(pts[0].0, pts[0].1), D2D1_FIGURE_BEGIN_HOLLOW);
                        let rest: Vec<_> = pts[1..].iter().map(|p| pt(p.0, p.1)).collect();
                        sink.AddLines(&rest);
                        sink.EndFigure(D2D1_FIGURE_END_OPEN);
                        sink.Close()?;
                        rt.DrawGeometry(&geo, &br, *w, &g.round);
                    }
                }
            }
        }
    }
    Ok(())
}

/// Render base crop + annotations into a CPU image (software WIC target).
pub fn export(g: &Gfx, base: &Img, sel: (i32, i32, i32, i32), items: &[Item]) -> Result<Img> {
    let (sx, sy, sw, sh) = sel;
    let mut crop = base.crop(sx, sy, sw, sh);
    if items.is_empty() {
        crop.force_opaque();
        return Ok(crop);
    }

    unsafe {
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let wb =
            wic.CreateBitmap(crop.w as u32, crop.h as u32, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnLoad)?;
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            ..Default::default()
        };
        let rt = g.d2d.CreateWicBitmapRenderTarget(&wb, &props)?;
        rt.BeginDraw();
        let bm = bitmap(&rt, &crop)?;
        rt.DrawBitmap(&bm, None, 1.0, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None);
        rt.SetTransform(&Matrix3x2::translation(-sx as f32, -sy as f32));
        for it in items {
            draw_item(g, &rt, it, false)?;
        }
        rt.EndDraw(None, None)?;
        let mut out = Img::new(crop.w, crop.h);
        let r = WICRect { X: 0, Y: 0, Width: crop.w, Height: crop.h };
        let bytes = std::slice::from_raw_parts_mut(out.px.as_mut_ptr() as *mut u8, out.px.len() * 4);
        wb.CopyPixels(&r, (crop.w * 4) as u32, bytes)?;
        out.force_opaque();
        Ok(out)
    }
}
