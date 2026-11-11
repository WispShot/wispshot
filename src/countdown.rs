//! Delayed capture: a small "Screenshot in 3" pill counts down, then the normal
//! capture editor opens. The pill never takes focus and lets clicks through, so a
//! menu or tooltip opened meanwhile stays open, and screen capture cannot see it.
//! Pressing the delayed-capture key again cancels (the resident process closes us).

use crate::gfx::*;
use crate::i18n::tf;
use crate::layered::Surface;
use std::cell::RefCell;
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

const TICK: usize = 1;
const GO: usize = 2;
const H: f32 = 40.0;

struct Cd {
    hwnd: HWND,
    g: Gfx,
    s: f32,
    left: u32,
    /// Top centre of the pill, screen pixels.
    cx: i32,
    top: i32,
    surf: Option<Surface>,
    on_done: Box<dyn Fn()>,
}

thread_local! {
    static CD: RefCell<Option<Cd>> = const { RefCell::new(None) };
}

/// Show the pill for `secs` seconds, then hide it and call `on_done`.
pub fn start(secs: u32, on_done: Box<dyn Fn()>) -> bool {
    unsafe {
        let inst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(proc_),
            hInstance: inst.into(),
            lpszClassName: w!("WispDelay"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let Ok(g) = Gfx::new() else { return false };
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        let mon = MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut mi);
        let s = crate::monitor_dpi(mon) as f32 / 96.0;
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED | WS_EX_TRANSPARENT,
            w!("WispDelay"),
            w!("WispShot"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(inst.into()),
            None,
        ) else {
            return false;
        };
        if std::env::var_os("WISP_NOEXCLUDE").is_none()
            && SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE).is_err()
        {
            let _ = SetWindowDisplayAffinity(hwnd, WDA_MONITOR);
        }
        let w = &mi.rcWork;
        CD.with(|c| {
            *c.borrow_mut() = Some(Cd {
                hwnd,
                g,
                s,
                left: secs.max(1),
                cx: (w.left + w.right) / 2,
                top: w.top + (16.0 * s) as i32,
                surf: None,
                on_done,
            })
        });
        render();
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(Some(hwnd), TICK, 1000, None);
        true
    }
}

fn render() {
    CD.with(|c| {
        let mut c = c.borrow_mut();
        let Some(c) = c.as_mut() else { return };
        let s = c.s;
        let label = tf("delay.pill", &[&c.left.to_string()]);
        let Ok(f) = c.g.text_format(14.0 * s, true) else { return };
        let Ok(fi) = c.g.icon_format(16.0 * s) else { return };
        let tw = c.g.measure(&label, &f).0;
        let (w, h) = ((tw + 58.0 * s).ceil() as i32, (H * s).ceil() as i32);
        if c.surf.as_ref().is_none_or(|x| x.w != w || x.h != h) {
            c.surf = Surface::new(&c.g, w, h).ok();
        }
        let Some(surf) = &c.surf else { return };
        let rt: &ID2D1RenderTarget = &surf.rt;
        unsafe {
            rt.BeginDraw();
            rt.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
            let r = h as f32 / 2.0;
            let bar = D2D1_ROUNDED_RECT { rect: rc(0.5, 0.5, w as f32 - 0.5, h as f32 - 0.5), radiusX: r, radiusY: r };
            if let (Ok(bg), Ok(edge), Ok(fg)) =
                (brush(rt, 0xF01C_1C20), brush(rt, 0x30FF_FFFF), brush(rt, 0xFFF2_F2F2))
            {
                rt.FillRoundedRectangle(&bar, &bg);
                rt.DrawRoundedRectangle(&bar, &edge, 1.0, None);
                // stopwatch icon
                let ic: Vec<u16> = "\u{E916}".encode_utf16().collect();
                rt.DrawText(
                    &ic,
                    &fi,
                    &rc(10.0 * s, 0.0, 40.0 * s, h as f32),
                    &fg,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
                let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
                let t: Vec<u16> = label.encode_utf16().collect();
                rt.DrawText(
                    &t,
                    &f,
                    &rc(40.0 * s, 0.0, w as f32, h as f32),
                    &fg,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }
            let _ = rt.EndDraw(None, None);
        }
        surf.present(c.hwnd, c.cx - w / 2, c.top);
    });
}

unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER if wp.0 == TICK => {
            let left = CD.with(|c| c.borrow_mut().as_mut().map(|c| {
                c.left = c.left.saturating_sub(1);
                c.left
            }));
            if left == Some(0) {
                let _ = KillTimer(Some(hwnd), TICK);
                // off screen first, in case capture exclusion is not supported
                let _ = ShowWindow(hwnd, SW_HIDE);
                SetTimer(Some(hwnd), GO, 60, None);
            } else {
                render();
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == GO => {
            let _ = KillTimer(Some(hwnd), GO);
            let cd = CD.with(|c| c.borrow_mut().take());
            let _ = DestroyWindow(hwnd);
            if let Some(cd) = cd {
                (cd.on_done)();
            }
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_DESTROY => {
            // cancelled (WM_CLOSE from the resident process): let the session exit
            if CD.with(|c| c.borrow_mut().take()).is_some() {
                crate::session::poke();
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
