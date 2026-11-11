//! Scrolling capture: the user scrolls a page inside the selected area (or lets
//! "Auto" send wheel notches) and the frames are stitched into one tall image.
//!
//! Stitching compares row hashes: rows that did not move at the top and bottom
//! (a sticky header, a status bar) are found first and kept once; for the rows in
//! between, the shift that makes the most rows match is the scroll distance. Only
//! the stitched strip and the last frame are kept in memory.

use crate::gfx::*;
use crate::i18n::{t, tf};
use crate::img::Img;
use crate::layered::Surface;
use std::cell::RefCell;
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const B: i32 = 3; // frame thickness
const GRAB: usize = 1;
const AUTO: usize = 2;
const HK_DONE: i32 = 1;
const HK_CANCEL: i32 = 2;
/// Tallest result, so a runaway page cannot eat the memory.
const MAX_H: usize = 30000;
const BAR_H: f32 = 36.0;
const ACCENT: u32 = 0xFF2B_8CFF;

// ------------------------------------------------------------- stitching

#[derive(PartialEq, Debug)]
pub enum Step {
    Same,
    Grew,
    Lost,
    Full,
}

pub struct Stitch {
    w: usize,
    h: usize,
    /// The stitched image so far, `w` pixels per row.
    strip: Vec<u32>,
    last: Vec<u64>,
    last_flat: Vec<bool>,
    /// Strip row where row 0 of the last frame sits.
    at: usize,
}

/// Row hashes, leaving out the right edge where a scrollbar thumb moves with the page.
fn hashes(px: &[u32], w: usize, h: usize) -> (Vec<u64>, Vec<bool>) {
    let cols = if w > 120 { w - 24 } else { w };
    let mut hs = Vec::with_capacity(h);
    let mut flat = Vec::with_capacity(h);
    for y in 0..h {
        let row = &px[y * w..y * w + cols];
        let mut hsh = 0xcbf2_9ce4_8422_2325u64;
        for p in row {
            hsh = (hsh ^ (*p & 0xFF_FFFF) as u64).wrapping_mul(0x100_0000_01b3);
        }
        hs.push(hsh);
        flat.push(row.iter().all(|p| (p ^ row[0]) & 0xFF_FFFF == 0));
    }
    (hs, flat)
}

impl Stitch {
    pub fn new(first: &Img) -> Stitch {
        let (w, h) = (first.w as usize, first.h as usize);
        let (last, last_flat) = hashes(&first.px, w, h);
        Stitch { w, h, strip: first.px.clone(), last, last_flat, at: 0 }
    }

    pub fn height(&self) -> usize {
        self.strip.len() / self.w
    }

    pub fn add(&mut self, f: &Img) -> Step {
        let (w, h) = (self.w, self.h);
        if f.w as usize != w || f.h as usize != h {
            return Step::Lost;
        }
        let (hs, flat) = hashes(&f.px, w, h);
        let top = (0..h).take_while(|&i| hs[i] == self.last[i]).count();
        if top == h {
            return Step::Same;
        }
        let bottom = (0..h).rev().take_while(|&i| hs[i] == self.last[i]).count();
        let (lo, hi) = (top, h - bottom);
        let mid = hi - lo;
        // best shift: most matching rows that carry content (blank rows match anything)
        let mut best: Option<(usize, usize)> = None;
        for dy in 1..mid {
            let mut hit = 0;
            let mut total = 0;
            for i in lo..hi - dy {
                if flat[i] && self.last_flat[i + dy] {
                    continue;
                }
                total += 1;
                if hs[i] == self.last[i + dy] {
                    hit += 1;
                }
            }
            if total >= 4 && hit * 100 >= total * 85 && best.is_none_or(|(_, b)| hit > b) {
                best = Some((dy, hit));
            }
        }
        let Some((dy, _)) = best else {
            // nothing moved downwards (changed in place, scrolled up, or too far at once)
            return Step::Lost;
        };
        if self.at + dy + h > MAX_H {
            return Step::Full;
        }
        // new rows from the moving part down go below; the bottom band moves to the new end
        let start = self.at + dy;
        self.strip.truncate((start + top) * w);
        self.strip.extend_from_slice(&f.px[top * w..]);
        self.at = start;
        self.last = hs;
        self.last_flat = flat;
        Step::Grew
    }

    pub fn into_img(self) -> Img {
        let h = self.height() as i32;
        Img { w: self.w as i32, h, px: self.strip }
    }
}

// ------------------------------------------------------------- UI

#[derive(Clone, Copy, PartialEq)]
enum Btn {
    Auto,
    Done,
    Cancel,
}

struct Sc {
    rect: RECT,
    frame: HWND,
    bar: HWND,
    g: Gfx,
    s: f32,
    surf: Option<Surface>,
    stitch: Option<Stitch>,
    lost: bool,
    full: bool,
    auto: bool,
    /// Auto mode: grabs in a row that showed nothing new (the end of the page).
    idle: u32,
    hover: Option<Btn>,
    bar_x: i32,
    bar_y: i32,
    btns: Vec<(D2D_RECT_F, Btn)>,
    on_done: Box<dyn Fn(Option<Img>)>,
}

thread_local! {
    static SC: RefCell<Option<Sc>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Sc) -> R) -> Option<R> {
    SC.with(|s| s.try_borrow_mut().ok().and_then(|mut s| s.as_mut().map(f)))
}

fn exclude(h: HWND) {
    if std::env::var_os("WISP_NOEXCLUDE").is_some() {
        return;
    }
    unsafe {
        if SetWindowDisplayAffinity(h, WDA_EXCLUDEFROMCAPTURE).is_err() {
            let _ = SetWindowDisplayAffinity(h, WDA_MONITOR);
        }
    }
}

/// Start scrolling capture of `r` (screen pixels). `on_done` gets the stitched image,
/// or None when cancelled.
pub fn start(r: RECT, on_done: Box<dyn Fn(Option<Img>)>) -> bool {
    unsafe {
        let inst = GetModuleHandleW(None).unwrap_or_default();
        let frame_wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(frame_proc),
            hInstance: inst.into(),
            lpszClassName: w!("WispScrollFrame"),
            hbrBackground: CreateSolidBrush(COLORREF(0x00FF_8C2B)),
            ..Default::default()
        };
        RegisterClassExW(&frame_wc);
        let bar_wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(bar_proc),
            hInstance: inst.into(),
            lpszClassName: w!("WispScrollBar"),
            hCursor: LoadCursorW(None, IDC_HAND).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassExW(&bar_wc);
        let Ok(g) = Gfx::new() else { return false };
        let (x, y, w, h) = (r.left - B, r.top - B, r.right - r.left + 2 * B, r.bottom - r.top + 2 * B);
        let Ok(frame) = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED | WS_EX_TRANSPARENT,
            w!("WispScrollFrame"),
            w!(""),
            WS_POPUP,
            x,
            y,
            w,
            h,
            None,
            None,
            Some(inst.into()),
            None,
        ) else {
            return false;
        };
        let _ = SetLayeredWindowAttributes(frame, COLORREF(0), 230, LWA_ALPHA);
        let outer = CreateRectRgn(0, 0, w, h);
        let inner = CreateRectRgn(B, B, w - B, h - B);
        CombineRgn(Some(outer), Some(outer), Some(inner), RGN_DIFF);
        let _ = DeleteObject(inner.into());
        SetWindowRgn(frame, Some(outer), false);
        exclude(frame);

        let s = crate::monitor_dpi(MonitorFromPoint(POINT { x: r.left, y: r.top }, MONITOR_DEFAULTTONEAREST))
            as f32
            / 96.0;
        let Ok(bar) = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED,
            w!("WispScrollBar"),
            w!("WispShot"),
            WS_POPUP,
            r.left,
            r.top,
            1,
            1,
            None,
            None,
            Some(inst.into()),
            None,
        ) else {
            let _ = DestroyWindow(frame);
            return false;
        };
        exclude(bar);
        let bar_h = (BAR_H * s) as i32;
        let vs = crate::capture::virtual_screen();
        let mut by = r.top - B - bar_h - 4;
        if by < vs.top {
            by = r.bottom + B + 4;
            if by + bar_h > vs.bottom {
                by = r.top + 4;
            }
        }
        SC.with(|c| {
            *c.borrow_mut() = Some(Sc {
                rect: r,
                frame,
                bar,
                g,
                s,
                surf: None,
                stitch: None,
                lost: false,
                full: false,
                auto: false,
                idle: 0,
                hover: None,
                bar_x: r.left - B,
                bar_y: by,
                btns: vec![],
                on_done,
            })
        });
        let _ = RegisterHotKey(Some(bar), HK_DONE, HOT_KEY_MODIFIERS(0), VK_RETURN.0 as u32);
        let _ = RegisterHotKey(Some(bar), HK_CANCEL, HOT_KEY_MODIFIERS(0), VK_ESCAPE.0 as u32);
        render();
        let _ = ShowWindow(frame, SW_SHOWNOACTIVATE);
        let _ = ShowWindow(bar, SW_SHOWNOACTIVATE);
        SetTimer(Some(bar), GRAB, 70, None);
        true
    }
}

fn grab() {
    let Some(r) = with(|c| c.rect) else { return };
    let Some(img) = crate::capture::grab_pixels(r) else { return };
    let step = with(|c| match &mut c.stitch {
        None => {
            c.stitch = Some(Stitch::new(&img));
            Step::Grew
        }
        Some(st) => st.add(&img),
    });
    let Some(step) = step else { return };
    let redraw = with(|c| {
        let was = (c.lost, c.full);
        match step {
            Step::Grew => {
                c.lost = false;
                c.idle = 0;
            }
            Step::Same => c.idle += 1,
            Step::Lost => {
                // the auto scroller can only mean "the page reached its end" or a redraw
                if c.auto {
                    c.idle += 1;
                } else {
                    c.lost = true;
                }
            }
            Step::Full => c.full = true,
        }
        step == Step::Grew || was != (c.lost, c.full)
    })
    .unwrap_or(false);
    if redraw {
        render();
    }
    let (auto, idle, full) = with(|c| (c.auto, c.idle, c.full)).unwrap_or_default();
    if full || auto && idle >= 12 {
        finish(true);
    }
}

/// One wheel notch down, sent to whatever is under the cursor (only inside the area).
fn wheel() {
    let Some(r) = with(|c| c.rect) else { return };
    unsafe {
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        if p.x < r.left || p.x >= r.right || p.y < r.top || p.y >= r.bottom {
            return;
        }
        let i = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT { mouseData: (-120i32) as u32, dwFlags: MOUSEEVENTF_WHEEL, ..Default::default() },
            },
        };
        SendInput(&[i], std::mem::size_of::<INPUT>() as i32);
    }
}

fn toggle_auto() {
    let Some((on, bar, r)) = with(|c| {
        c.auto = !c.auto;
        c.idle = 0;
        c.lost = false;
        (c.auto, c.bar, c.rect)
    }) else {
        return;
    };
    unsafe {
        if on {
            // the wheel goes to the window under the cursor: put it in the middle of the area
            let _ = SetCursorPos((r.left + r.right) / 2, (r.top + r.bottom) / 2);
            SetTimer(Some(bar), AUTO, 160, None);
        } else {
            let _ = KillTimer(Some(bar), AUTO);
        }
    }
    render();
}

fn finish(keep: bool) {
    let Some(c) = SC.with(|s| s.borrow_mut().take()) else { return };
    unsafe {
        let _ = UnregisterHotKey(Some(c.bar), HK_DONE);
        let _ = UnregisterHotKey(Some(c.bar), HK_CANCEL);
        let _ = DestroyWindow(c.bar);
        let _ = DestroyWindow(c.frame);
    }
    let img = if keep { c.stitch.map(|s| s.into_img()) } else { None };
    (c.on_done)(img);
}

fn render() {
    with(|c| unsafe {
        let s = c.s;
        let (Ok(f), Ok(fb)) = (c.g.text_format(12.5 * s, false), c.g.text_format(12.5 * s, true)) else { return };
        let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
        let _ = fb.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
        let _ = fb.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
        let px = c.stitch.as_ref().map_or(0, |st| st.height());
        let status = if c.full {
            tf("scroll.full", &[&px.to_string()])
        } else if c.lost {
            t("scroll.slower").to_string()
        } else if c.auto {
            tf("scroll.auto_on", &[&px.to_string()])
        } else {
            tf("scroll.status", &[&px.to_string()])
        };
        let labels = [
            (Btn::Auto, if c.auto { t("scroll.auto_stop") } else { t("scroll.auto") }),
            (Btn::Done, t("scroll.done")),
            (Btn::Cancel, t("scroll.cancel")),
        ];
        let pad = 12.0 * s;
        let sw = c.g.measure(&status, &f).0.max(150.0 * s);
        let mut x = pad + sw + pad;
        let h = BAR_H * s;
        c.btns.clear();
        for (b, l) in &labels {
            let bw = c.g.measure(l, &fb).0 + 24.0 * s;
            c.btns.push((rc(x, 4.0 * s, x + bw, h - 4.0 * s), *b));
            x += bw + 4.0 * s;
        }
        let (w, hh) = ((x + pad - 4.0 * s).ceil() as i32, h.ceil() as i32);
        if c.surf.as_ref().is_none_or(|x| x.w != w || x.h != hh) {
            c.surf = Surface::new(&c.g, w, hh).ok();
        }
        let Some(surf) = &c.surf else { return };
        let rt: &ID2D1RenderTarget = &surf.rt;
        rt.BeginDraw();
        rt.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
        let r = h / 2.0;
        let bar = D2D1_ROUNDED_RECT { rect: rc(0.5, 0.5, w as f32 - 0.5, h - 0.5), radiusX: r, radiusY: r };
        if let (Ok(bg), Ok(edge), Ok(fg), Ok(warn), Ok(acc), Ok(hov)) = (
            brush(rt, 0xF01C_1C20),
            brush(rt, 0x30FF_FFFF),
            brush(rt, 0xFFF2_F2F2),
            brush(rt, 0xFFFF_B040),
            brush(rt, ACCENT),
            brush(rt, 0x26FF_FFFF),
        ) {
            rt.FillRoundedRectangle(&bar, &bg);
            rt.DrawRoundedRectangle(&bar, &edge, 1.0, None);
            let st: Vec<u16> = status.encode_utf16().collect();
            rt.DrawText(
                &st,
                &f,
                &rc(pad, 0.0, pad + sw, h),
                if c.lost { &warn } else { &fg },
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
            for ((br, b), (_, l)) in c.btns.iter().zip(&labels) {
                let rr = D2D1_ROUNDED_RECT { rect: *br, radiusX: 6.0 * s, radiusY: 6.0 * s };
                if *b == Btn::Done || *b == Btn::Auto && c.auto {
                    rt.FillRoundedRectangle(&rr, &acc);
                } else if c.hover == Some(*b) {
                    rt.FillRoundedRectangle(&rr, &hov);
                }
                let lw: Vec<u16> = l.encode_utf16().collect();
                rt.DrawText(&lw, &fb, br, &fg, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
            }
        }
        let _ = rt.EndDraw(None, None);
        let vs = crate::capture::virtual_screen();
        let bx = c.bar_x.min(vs.right - w).max(vs.left);
        surf.present(c.bar, bx, c.bar_y);
        c.bar_x = bx;
    });
}

fn hit(lp: LPARAM) -> Option<Btn> {
    let (x, y) = crate::lparam_xy(lp);
    with(|c| c.btns.iter().find(|(r, _)| x as f32 >= r.left && x as f32 <= r.right && y as f32 >= r.top && y as f32 <= r.bottom).map(|(_, b)| *b)).flatten()
}

unsafe extern "system" fn frame_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    DefWindowProcW(hwnd, msg, wp, lp)
}

unsafe extern "system" fn bar_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER if wp.0 == GRAB => {
            grab();
            LRESULT(0)
        }
        WM_TIMER if wp.0 == AUTO => {
            wheel();
            LRESULT(0)
        }
        WM_HOTKEY => {
            finish(wp.0 as i32 == HK_DONE);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let h = hit(lp);
            if with(|c| std::mem::replace(&mut c.hover, h) != h).unwrap_or(false) {
                render();
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            match hit(lp) {
                Some(Btn::Auto) => toggle_auto(),
                Some(Btn::Done) => finish(true),
                Some(Btn::Cancel) => finish(false),
                None => {}
            }
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A "page" of distinct rows, seen through a window of `h` rows starting at `top`,
    /// with a fixed header of `hdr` rows.
    fn view(page: &[u32], w: usize, h: usize, top: usize, hdr: usize) -> Img {
        let mut px = vec![0u32; w * h];
        for y in 0..h {
            let src = if y < hdr { y } else { top + y };
            for x in 0..w {
                px[y * w + x] = page[src * w + x];
            }
        }
        Img { w: w as i32, h: h as i32, px }
    }

    fn page(w: usize, rows: usize) -> Vec<u32> {
        (0..rows * w).map(|i| ((i / w) as u32).wrapping_mul(2654435761) ^ (i % w) as u32).collect()
    }

    #[test]
    fn stitches_scrolled_page() {
        let (w, h) = (40, 50);
        let p = page(w, 400);
        let mut st = Stitch::new(&view(&p, w, h, 0, 0));
        for top in [7, 19, 40, 41, 80, 120] {
            assert_eq!(st.add(&view(&p, w, h, top, 0)), Step::Grew);
        }
        assert_eq!(st.add(&view(&p, w, h, 120, 0)), Step::Same);
        let img = st.into_img();
        assert_eq!(img.h as usize, 120 + h);
        assert_eq!(img.px[..], p[..(120 + h) * w]);
    }

    #[test]
    fn keeps_sticky_header_once() {
        let (w, h, hdr) = (40, 60, 8);
        let p = page(w, 400);
        let mut st = Stitch::new(&view(&p, w, h, 0, hdr));
        for top in [10, 25, 40] {
            assert_eq!(st.add(&view(&p, w, h, top, hdr)), Step::Grew);
        }
        let img = st.into_img();
        assert_eq!(img.h as usize, 40 + h);
        // header, then the page continues where the header left off
        assert_eq!(img.px[..hdr * w], p[..hdr * w]);
        assert_eq!(img.px[(h - 1) * w..h * w], p[(h - 1) * w..h * w]);
        assert_eq!(img.px[(40 + h - 1) * w..], p[(40 + h - 1) * w..(40 + h) * w]);
    }

    #[test]
    fn too_far_is_lost() {
        let (w, h) = (40, 50);
        let p = page(w, 400);
        let mut st = Stitch::new(&view(&p, w, h, 0, 0));
        assert_eq!(st.add(&view(&p, w, h, 60, 0)), Step::Lost);
        assert_eq!(st.add(&view(&p, w, h, 30, 0)), Step::Grew);
    }
}
