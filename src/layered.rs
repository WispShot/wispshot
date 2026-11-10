//! Per-pixel-alpha window surface: Direct2D draws into a DIB, `UpdateLayeredWindow`
//! shows it. Fully transparent pixels are click-through. Nothing is redrawn unless
//! the owner asks, so an idle surface costs no CPU or GPU time.

use crate::gfx::Gfx;
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub struct Surface {
    pub w: i32,
    pub h: i32,
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    pub rt: ID2D1DCRenderTarget,
}

impl Surface {
    pub fn new(g: &Gfx, w: i32, h: i32) -> Result<Surface> {
        unsafe {
            let bi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let dc = CreateCompatibleDC(None);
            let bmp = CreateDIBSection(Some(dc), &bi, DIB_RGB_COLORS, &mut bits, None, 0)?;
            let old = SelectObject(dc, bmp.into());
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                ..Default::default()
            };
            let rt = g.d2d.CreateDCRenderTarget(&props)?;
            rt.BindDC(dc, &RECT { left: 0, top: 0, right: w, bottom: h })?;
            Ok(Surface { w, h, dc, bmp, old, rt })
        }
    }

    /// Push the finished frame to the window at screen position (x, y).
    pub fn present(&self, hwnd: HWND, x: i32, y: i32) {
        let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
        unsafe {
            let _ = UpdateLayeredWindow(
                hwnd,
                None,
                Some(&POINT { x, y }),
                Some(&SIZE { cx: self.w, cy: self.h }),
                Some(self.dc),
                Some(&POINT::default()),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bmp.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

/// Draw with `opacity` until the matching `PopLayer` (used for fading strokes).
pub unsafe fn push_opacity(rt: &ID2D1RenderTarget, opacity: f32) -> Result<()> {
    let layer = rt.CreateLayer(None)?;
    let inf = f32::INFINITY;
    let p = D2D1_LAYER_PARAMETERS {
        contentBounds: D2D_RECT_F { left: -inf, top: -inf, right: inf, bottom: inf },
        geometricMask: std::mem::ManuallyDrop::new(None),
        maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
        maskTransform: windows_numerics::Matrix3x2::identity(),
        opacity,
        opacityBrush: std::mem::ManuallyDrop::new(None),
        layerOptions: D2D1_LAYER_OPTIONS_NONE,
    };
    rt.PushLayer(&p, &layer);
    Ok(())
}
