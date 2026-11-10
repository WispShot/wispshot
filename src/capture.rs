//! Instant full-desktop grab (one BitBlt of the virtual screen) + window/element
//! rectangles for auto-detection.

use crate::img::Img;
use windows::core::BOOL;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub struct Shot {
    /// Virtual-screen origin of the image (can be negative on multi-monitor).
    pub ox: i32,
    pub oy: i32,
    pub img: Img,
    /// Top-level windows in z-order (front first): (hwnd, image-local frame,
    /// child element rects — filled lazily only when element detection is on).
    pub windows: Vec<(isize, RECT, Vec<RECT>)>,
}

pub fn virtual_screen() -> RECT {
    unsafe {
        let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let w = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let h = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        RECT { left: x, top: y, right: x + w, bottom: y + h }
    }
}

// Capture = grab_frame (worker thread) + enum_windows (UI thread); see overlay::open.

/// The raw grab kept as two GDI DIB sections (normal + pre-dimmed) so the first
/// frame can be shown with two plain BitBlts, before Direct2D even exists.
pub struct GdiFrame {
    pub w: i32,
    pub h: i32,
    pub bright: isize,
    bright_bits: usize,
    pub dim: isize,
}
unsafe impl Send for GdiFrame {}

impl GdiFrame {
    pub fn to_img(&self) -> Img {
        let n = (self.w * self.h) as usize;
        let src = unsafe { std::slice::from_raw_parts(self.bright_bits as *const u32, n) };
        Img { w: self.w, h: self.h, px: src.to_vec() }
    }
}

impl Drop for GdiFrame {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(self.bright as *mut _));
            let _ = DeleteObject(HGDIOBJ(self.dim as *mut _));
        }
    }
}

unsafe fn dib(w: i32, h: i32) -> Option<(HBITMAP, *mut u32)> {
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
    let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
    let b = CreateDIBSection(None, &bi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
    if bits.is_null() {
        let _ = DeleteObject(b.into());
        return None;
    }
    Some((b, bits as *mut u32))
}

/// Plain GDI grab of a screen rectangle (correct for rotated monitors too).
pub fn grab_pixels(r: RECT) -> Option<Img> {
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    if w <= 0 || h <= 0 {
        return None;
    }
    unsafe {
        let (b, bits) = dib(w, h)?;
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let old = SelectObject(mem, b.into());
        let ok = BitBlt(mem, 0, 0, w, h, Some(screen), r.left, r.top, SRCCOPY).is_ok();
        let _ = GdiFlush();
        let out =
            ok.then(|| Img { w, h, px: std::slice::from_raw_parts(bits as *const u32, (w * h) as usize).to_vec() });
        SelectObject(mem, old);
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        let _ = DeleteObject(b.into());
        out
    }
}

pub fn grab_frame(r: RECT) -> Option<GdiFrame> {
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    if w <= 0 || h <= 0 {
        return None;
    }
    unsafe {
        let (bright, bbits) = dib(w, h)?;
        let Some((dimb, dbits)) = dib(w, h) else {
            let _ = DeleteObject(bright.into());
            return None;
        };
        let frame = GdiFrame { w, h, bright: bright.0 as isize, bright_bits: bbits as usize, dim: dimb.0 as isize };
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let old = SelectObject(mem, bright.into());
        // No CAPTUREBLT: DWM already composes layered windows, and the flag makes the cursor flicker.
        crate::trace("grab: dibs allocated");
        let ok = BitBlt(mem, 0, 0, w, h, Some(screen), r.left, r.top, SRCCOPY).is_ok();
        let _ = GdiFlush();
        SelectObject(mem, old);
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        if !ok {
            return None;
        }
        crate::trace("grab: bitblt");
        // pre-dim (50%) in parallel
        let n = (w * h) as usize;
        let src = std::slice::from_raw_parts(bbits as *const u32, n);
        let dst = std::slice::from_raw_parts_mut(dbits, n);
        let threads = std::thread::available_parallelism().map_or(2, |n| n.get()).clamp(1, 8);
        let chunk = n.div_ceil(threads);
        std::thread::scope(|s| {
            for (d, sr) in dst.chunks_mut(chunk).zip(src.chunks(chunk)) {
                s.spawn(move || {
                    for (o, i) in d.iter_mut().zip(sr) {
                        *o = (*i >> 1) & 0x007F_7F7F;
                    }
                });
            }
        });
        crate::trace("grab: dimmed");
        Some(frame)
    }
}

struct EnumCtx {
    ox: i32,
    oy: i32,
    deep: bool,
    list: Vec<(isize, RECT, Vec<RECT>)>,
}

fn offset(mut r: RECT, ox: i32, oy: i32) -> RECT {
    r.left -= ox;
    r.right -= ox;
    r.top -= oy;
    r.bottom -= oy;
    r
}

fn children(h: HWND, frame: RECT) -> Vec<RECT> {
    let mut kids: Vec<RECT> = Vec::new();
    let mut kctx = (frame, &mut kids as *mut Vec<RECT>);
    unsafe {
        let _ = EnumChildWindows(Some(h), Some(enum_child), LPARAM(&mut kctx as *mut _ as isize));
    }
    kids
}

/// Fill child-element rects for every window (element detection toggled on).
pub fn fill_children(list: &mut [(isize, RECT, Vec<RECT>)], ox: i32, oy: i32) {
    for (h, frame, kids) in list.iter_mut() {
        let screen =
            RECT { left: frame.left + ox, top: frame.top + oy, right: frame.right + ox, bottom: frame.bottom + oy };
        *kids = children(HWND(*h as *mut _), screen).into_iter().map(|r| offset(r, ox, oy)).collect();
    }
}

fn is_cloaked(h: HWND) -> bool {
    let mut cloaked: u32 = 0;
    unsafe { DwmGetWindowAttribute(h, DWMWA_CLOAKED, &mut cloaked as *mut _ as *mut _, 4).is_ok() && cloaked != 0 }
}

fn frame_rect(h: HWND) -> RECT {
    let mut r = RECT::default();
    unsafe {
        if DwmGetWindowAttribute(
            h,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut r as *mut _ as *mut _,
            std::mem::size_of::<RECT>() as u32,
        )
        .is_err()
        {
            let _ = GetWindowRect(h, &mut r);
        }
    }
    r
}

/// False for windows that are on screen but not in the grabbed image: Wisp's own
/// veil/editor (the veil is a full-screen topmost window, so it used to swallow every
/// detection and a click selected the whole desktop), anything excluded from screen
/// capture, and fully transparent layered windows.
unsafe fn in_screenshot(h: HWND, ex: u32) -> bool {
    let mut cls = [0u16; 16];
    let n = GetClassNameW(h, &mut cls) as usize;
    let c = &cls[..n];
    if c == windows::core::w!("WispVeil").as_wide() || c == windows::core::w!("WispOverlay").as_wide() {
        return false;
    }
    let mut aff = 0u32;
    if GetWindowDisplayAffinity(h, &mut aff).is_ok() && aff != WDA_NONE.0 {
        return false;
    }
    if ex & WS_EX_LAYERED.0 != 0 {
        let (mut key, mut alpha, mut flags) = (COLORREF(0), 0u8, LAYERED_WINDOW_ATTRIBUTES_FLAGS(0));
        if GetLayeredWindowAttributes(h, Some(&mut key), Some(&mut alpha), Some(&mut flags)).is_ok()
            && flags.0 & LWA_ALPHA.0 != 0
            && alpha == 0
        {
            return false;
        }
    }
    true
}

unsafe extern "system" fn enum_top(h: HWND, lp: LPARAM) -> BOOL {
    let ctx = &mut *(lp.0 as *mut EnumCtx);
    if !IsWindowVisible(h).as_bool() || IsIconic(h).as_bool() || is_cloaked(h) {
        return TRUE;
    }
    let ex = GetWindowLongW(h, GWL_EXSTYLE) as u32;
    if ex & WS_EX_TRANSPARENT.0 != 0 && ex & WS_EX_LAYERED.0 != 0 {
        return TRUE; // click-through overlays
    }
    if !in_screenshot(h, ex) {
        return TRUE;
    }
    let r = frame_rect(h);
    if r.right - r.left < 8 || r.bottom - r.top < 8 {
        return TRUE;
    }
    let kids =
        if ctx.deep { children(h, r).into_iter().map(|k| offset(k, ctx.ox, ctx.oy)).collect() } else { Vec::new() };
    ctx.list.push((h.0 as isize, offset(r, ctx.ox, ctx.oy), kids));
    TRUE
}

unsafe extern "system" fn enum_child(h: HWND, lp: LPARAM) -> BOOL {
    let (parent, out) = &mut *(lp.0 as *mut (RECT, *mut Vec<RECT>));
    let out = &mut **out;
    if out.len() > 2000 || !IsWindowVisible(h).as_bool() {
        return TRUE;
    }
    let mut r = RECT::default();
    let _ = GetWindowRect(h, &mut r);
    // clip to the top-level frame
    r.left = r.left.max(parent.left);
    r.top = r.top.max(parent.top);
    r.right = r.right.min(parent.right);
    r.bottom = r.bottom.min(parent.bottom);
    if r.right - r.left >= 6 && r.bottom - r.top >= 6 {
        out.push(r);
    }
    TRUE
}

pub fn enum_windows(ox: i32, oy: i32, deep: bool) -> Vec<(isize, RECT, Vec<RECT>)> {
    let mut ctx = EnumCtx { ox, oy, deep, list: Vec::new() };
    unsafe {
        let _ = EnumWindows(Some(enum_top), LPARAM(&mut ctx as *mut _ as isize));
    }
    ctx.list
}

#[inline]
pub fn contains(r: &RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

/// Smallest element under the point, inside the front-most window containing it.
pub fn detect(windows: &[(isize, RECT, Vec<RECT>)], x: i32, y: i32, deep: bool) -> Option<RECT> {
    for (_, top, kids) in windows {
        if contains(top, x, y) {
            let mut best = *top;
            if deep {
                let area = |r: &RECT| (r.right - r.left) as i64 * (r.bottom - r.top) as i64;
                for k in kids {
                    if contains(k, x, y) && area(k) < area(&best) {
                        best = *k;
                    }
                }
            }
            return Some(best);
        }
    }
    None
}
