//! Recording chrome: a click-through red frame around the region and a small
//! "● REC 00:12  ✎ Çiz  ■" pill that unfolds into the drawing toolbar (see ink.rs).
//! Frame and pill are excluded from capture (WDA_EXCLUDEFROMCAPTURE); the ink is not.

use crate::config::Config;
use crate::gfx::*;
use crate::i18n::t;
use crate::keys::{self, K};
use crate::ink::{self, InkTool, COLORS, TOOLS};
use crate::layered::Surface;
use std::cell::RefCell;
use std::rc::Rc;
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub struct RecUi {
    frame: HWND,
    pill: HWND,
}

const B: i32 = 3; // frame thickness
/// Sent by the resident process (hotkey / tray) to stop the recording.
pub const WM_REC_STOP_REQUEST: u32 = WM_APP + 60;
const HK_DRAW: i32 = 1;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;

fn exclude(h: HWND) {
    // tests: WISP_NOEXCLUDE=1 keeps the chrome visible to screenshots
    if std::env::var_os("WISP_NOEXCLUDE").is_some() {
        return;
    }
    unsafe {
        // WDA_EXCLUDEFROMCAPTURE needs Win10 2004+; fall back to WDA_MONITOR.
        if SetWindowDisplayAffinity(h, WDA_EXCLUDEFROMCAPTURE).is_err() {
            let _ = SetWindowDisplayAffinity(h, WDA_MONITOR);
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Btn {
    Draw,
    Tool(InkTool),
    Color(usize),
    Fade,
    Clear,
    Stop,
}

impl Btn {
    fn tip(self) -> String {
        let k = |name: &str, k: K| {
            let l = keys::label(k);
            if l.is_empty() {
                name.to_string()
            } else {
                format!("{}  ({})", name, l)
            }
        };
        match self {
            Btn::Draw if ink::drawing() => t("rec.tip_done").into(),
            Btn::Draw => k(t("rec.tip_draw"), K::Draw),
            Btn::Tool(tool) => k(tool.name(), tool.key()),
            Btn::Color(i) => k(t("rec.tip_color"), [K::InkColor1, K::InkColor2, K::InkColor3, K::InkColor4][i]),
            Btn::Fade if ink::fade() => k(t("rec.tip_fade_on"), K::InkFade),
            Btn::Fade => k(t("rec.tip_fade_off"), K::InkFade),
            Btn::Clear => k(t("rec.tip_clear"), K::InkClear),
            Btn::Stop => t("rec.tip_stop").into(),
        }
    }
}

struct Pill {
    hwnd: HWND,
    g: Rc<Gfx>,
    s: f32,
    /// Bar anchor: right edge and top in screen pixels.
    right: i32,
    top: i32,
    left_min: i32,
    start: u64,
    hover: Option<Btn>,
    surf: Option<Surface>,
    fmt_ui: Option<IDWriteTextFormat>,
    fmt_icon: Option<IDWriteTextFormat>,
}

thread_local! {
    static PILL: RefCell<Option<Pill>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Pill) -> R) -> Option<R> {
    PILL.with(|p| p.try_borrow_mut().ok().and_then(|mut p| p.as_mut().map(f)))
}

impl RecUi {
    pub fn show(r: RECT, cfg: &Config) -> Option<RecUi> {
        unsafe {
            let inst = GetModuleHandleW(None).unwrap_or_default();
            for (cls, proc_) in [(w!("WispRecFrame"), Some(frame_proc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT)), (w!("WispRecPill"), Some(pill_proc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT))] {
                let wc = WNDCLASSEXW {
                    cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                    lpfnWndProc: proc_,
                    hInstance: inst.into(),
                    lpszClassName: cls,
                    hCursor: LoadCursorW(None, IDC_HAND).unwrap_or_default(),
                    ..Default::default()
                };
                RegisterClassExW(&wc);
            }
            let (x, y, w, h) = (r.left - B, r.top - B, r.right - r.left + 2 * B, r.bottom - r.top + 2 * B);
            let frame = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED | WS_EX_TRANSPARENT,
                w!("WispRecFrame"),
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
            )
            .ok()?;
            let _ = SetLayeredWindowAttributes(frame, COLORREF(0), 230, LWA_ALPHA);
            let outer = CreateRectRgn(0, 0, w, h);
            let inner = CreateRectRgn(B, B, w - B, h - B);
            CombineRgn(Some(outer), Some(outer), Some(inner), RGN_DIFF);
            let _ = DeleteObject(inner.into());
            SetWindowRgn(frame, Some(outer), false); // system owns the region now
            exclude(frame);
            let _ = ShowWindow(frame, SW_SHOWNOACTIVATE);

            let s = crate::monitor_dpi(MonitorFromPoint(POINT { x: r.left, y: r.top }, MONITOR_DEFAULTTONEAREST)) as f32 / 96.0;
            let g = Rc::new(Gfx::new().unwrap_or_else(|_| std::process::abort()));

            // the ink goes under the pill: create it first
            ink::create(r, g.clone(), s, cfg.ink_fade);

            let bar_h = (BAR_H * s) as i32;
            let vs = crate::capture::virtual_screen();
            let mut top = r.top - B - bar_h - 4;
            if top < vs.top {
                top = r.bottom + B + 4;
                if top + bar_h + (TIP_H * s) as i32 > vs.bottom {
                    top = r.top + 4;
                }
            }
            let pill = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED,
                w!("WispRecPill"),
                w!("WispShot REC"),
                WS_POPUP,
                r.right,
                top,
                1,
                1,
                None,
                None,
                Some(inst.into()),
                None,
            );
            let Ok(pill) = pill else {
                ink::destroy();
                let _ = DestroyWindow(frame);
                return None;
            };
            exclude(pill);
            PILL.with(|p| {
                *p.borrow_mut() = Some(Pill {
                    hwnd: pill,
                    fmt_ui: g.text_format(12.5 * s, true).ok(),
                    fmt_icon: g.icon_format(15.0 * s).ok(),
                    g,
                    s,
                    right: (r.right + B).min(vs.right),
                    top,
                    left_min: vs.left,
                    start: GetTickCount64(),
                    hover: None,
                    surf: None,
                })
            });
            if let Some(hk) = cfg.hotkey(K::Draw) {
                let _ = RegisterHotKey(Some(pill), HK_DRAW, hk.mods, hk.vk);
            }
            render();
            let _ = ShowWindow(pill, SW_SHOWNOACTIVATE);
            SetTimer(Some(pill), 1, 500, None);
            Some(RecUi { frame, pill })
        }
    }

    pub fn close(self) {
        ink::destroy();
        PILL.with(|p| p.borrow_mut().take());
        unsafe {
            let _ = DestroyWindow(self.pill);
            let _ = DestroyWindow(self.frame);
        }
    }
}

/// Redraw the pill (tool / colour / mode changed).
pub fn refresh() {
    render();
}

/// Keep the pill above the ink window.
pub fn raise_pill() {
    if let Some(h) = with(|p| p.hwnd) {
        unsafe {
            let _ = SetWindowPos(h, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
    }
}

/// Remember the fade choice for the next recording.
pub fn save_fade(on: bool) {
    let mut cfg = crate::config::load();
    if cfg.ink_fade != on {
        cfg.ink_fade = on;
        let _ = crate::config::save(&cfg);
    }
}

const BAR_H: f32 = 36.0;
const TIP_H: f32 = 30.0;
const BS: f32 = 30.0; // button
const CS: f32 = 24.0; // colour swatch
const SEP: f32 = 9.0;
const TIME_W: f32 = 86.0;
const DRAW_W: f32 = 58.0;

/// Buttons in bar-local coordinates (DIPs × scale) and the bar width.
fn layout(s: f32) -> (Vec<(D2D_RECT_F, Btn)>, f32) {
    let pad = 4.0 * s;
    let mut x = pad + TIME_W * s;
    let mut out = vec![];
    let mut put = |x: &mut f32, w: f32, b: Btn| {
        let h = if matches!(b, Btn::Color(_)) { CS * s } else { BS * s };
        let y = (BAR_H * s - h) / 2.0;
        out.push((rc(*x, y, *x + w, y + h), b));
        *x += w;
    };
    if ink::drawing() {
        for t in TOOLS {
            put(&mut x, BS * s, Btn::Tool(t));
        }
        x += SEP * s;
        for i in 0..COLORS.len() {
            put(&mut x, CS * s, Btn::Color(i));
            x += 2.0 * s;
        }
        x += SEP * s;
        put(&mut x, BS * s, Btn::Fade);
        put(&mut x, BS * s, Btn::Clear);
        x += SEP * s;
    }
    put(&mut x, DRAW_W * s, Btn::Draw);
    put(&mut x, BS * s, Btn::Stop);
    (out, x + pad)
}

fn render() {
    with(|p| unsafe {
        let s = p.s;
        let (btns, bw) = layout(s);
        let (w, h) = (bw.ceil() as i32, ((BAR_H + TIP_H) * s).ceil() as i32);
        if p.surf.as_ref().map_or(true, |f| f.w != w || f.h != h) {
            p.surf = Surface::new(&p.g, w, h).ok();
        }
        let Some(surf) = &p.surf else { return };
        let rt: &ID2D1RenderTarget = &surf.rt;
        rt.BeginDraw();
        rt.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
        let _ = draw(p, rt, &btns, bw);
        let _ = rt.EndDraw(None, None);
        let x = (p.right - w).max(p.left_min);
        surf.present(p.hwnd, x, p.top);
    });
}

fn draw(p: &Pill, rt: &ID2D1RenderTarget, btns: &[(D2D_RECT_F, Btn)], bw: f32) -> Result<()> {
    let s = p.s;
    unsafe {
        let bar = rc(0.5, 0.5, bw - 0.5, BAR_H * s - 0.5);
        let r = BAR_H * s / 2.0;
        rt.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: bar, radiusX: r, radiusY: r }, &brush(rt, 0xF01C_1C20)?);
        rt.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: bar, radiusX: r, radiusY: r }, &brush(rt, 0x30FF_FFFF)?, 1.0, None);
        let fg = brush(rt, 0xFFF2_F2F2)?;
        let dim = brush(rt, 0xFF9A_9AA2)?;
        let acc = brush(rt, 0xFF2B_8CFF)?;
        let hov = brush(rt, 0x26FF_FFFF)?;
        let red = brush(rt, 0xFFFF_4545)?;

        // ● REC 00:12  (the dot pulses with the clock)
        let secs = (GetTickCount64() - p.start) / 1000;
        let cy = BAR_H * s / 2.0;
        let dot = if secs % 2 == 0 { &red } else { &brush(rt, 0xFFB0_3030)? };
        rt.FillEllipse(&D2D1_ELLIPSE { point: pt(18.0 * s, cy), radiusX: 5.0 * s, radiusY: 5.0 * s }, dot);
        if let Some(f) = &p.fmt_ui {
            let t: Vec<u16> = format!("REC {:02}:{:02}", secs / 60, secs % 60).encode_utf16().collect();
            let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
            rt.DrawText(&t, f, &rc(30.0 * s, 0.0, (TIME_W + 8.0) * s, BAR_H * s), &fg, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
        }
        // divider after the clock
        rt.DrawLine(pt((TIME_W + 2.0) * s, cy - 9.0 * s), pt((TIME_W + 2.0) * s, cy + 9.0 * s), &brush(rt, 0x40FF_FFFF)?, 1.0, None);

        let icon = |ch: &str, r: &D2D_RECT_F, b: &ID2D1SolidColorBrush| {
            if let Some(f) = &p.fmt_icon {
                let w: Vec<u16> = ch.encode_utf16().collect();
                rt.DrawText(&w, f, r, b, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
            }
        };
        let cur_tool = ink::tool();
        let cur_color = ink::color();
        for (r, b) in btns {
            let inner = rc(r.left + 2.0 * s, r.top + 2.0 * s, r.right - 2.0 * s, r.bottom - 2.0 * s);
            let rr = D2D1_ROUNDED_RECT { rect: inner, radiusX: 6.0 * s, radiusY: 6.0 * s };
            let active = matches!(b, Btn::Tool(t) if *t == cur_tool) || (*b == Btn::Draw && ink::drawing());
            if active {
                rt.FillRoundedRectangle(&rr, &acc);
            } else if p.hover == Some(*b) && !matches!(b, Btn::Color(_)) {
                rt.FillRoundedRectangle(&rr, &hov);
            }
            let (cx, cy) = ((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0);
            let u = 5.5 * s;
            match b {
                Btn::Draw => {
                    let (ch, label) = if ink::drawing() { ("\u{E73E}", t("rec.done")) } else { ("\u{E70F}", t("rec.draw")) };
                    icon(ch, &rc(r.left + 2.0 * s, r.top, r.left + 24.0 * s, r.bottom), &fg);
                    if let Some(f) = &p.fmt_ui {
                        let t: Vec<u16> = label.encode_utf16().collect();
                        rt.DrawText(&t, f, &rc(r.left + 24.0 * s, r.top, r.right, r.bottom), &fg, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
                    }
                }
                Btn::Tool(InkTool::Pen) => icon("\u{E70F}", r, &fg),
                Btn::Tool(InkTool::Marker) => icon("\u{E7E6}", r, &fg),
                Btn::Tool(InkTool::Rect) => icon("\u{E739}", r, &fg),
                Btn::Tool(InkTool::Arrow) => {
                    let it = Item::Arrow { a: (cx - u * 1.2, cy + u * 1.2), b: (cx + u * 1.2, cy - u * 1.2), c: 0xFFF2_F2F2, w: 1.6 * s };
                    draw_item(&p.g, rt, &it, false)?;
                }
                Btn::Tool(InkTool::Counter) => {
                    rt.DrawEllipse(&D2D1_ELLIPSE { point: pt(cx, cy), radiusX: u * 1.45, radiusY: u * 1.45 }, &fg, 1.3 * s, None);
                    if let Ok(f) = p.g.text_format(11.0 * s, true) {
                        let _ = f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
                        let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
                        rt.DrawText(&[b'1' as u16], &f, r, &fg, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
                    }
                }
                Btn::Color(i) => {
                    let e = D2D1_ELLIPSE { point: pt(cx, cy), radiusX: 7.0 * s, radiusY: 7.0 * s };
                    rt.FillEllipse(&e, &brush(rt, COLORS[*i])?);
                    if *i == cur_color {
                        let ring = D2D1_ELLIPSE { point: pt(cx, cy), radiusX: 10.0 * s, radiusY: 10.0 * s };
                        rt.DrawEllipse(&ring, &fg, 1.5 * s, None);
                    } else if p.hover == Some(*b) {
                        let ring = D2D1_ELLIPSE { point: pt(cx, cy), radiusX: 10.0 * s, radiusY: 10.0 * s };
                        rt.DrawEllipse(&ring, &dim, 1.0 * s, None);
                    }
                }
                Btn::Fade => icon(if ink::fade() { "\u{E916}" } else { "\u{E718}" }, r, &fg),
                Btn::Clear => icon("\u{E74D}", r, if ink::has_items() { &fg } else { &dim }),
                Btn::Stop => {
                    let k = 5.5 * s;
                    let sq = D2D1_ROUNDED_RECT { rect: rc(cx - k, cy - k, cx + k, cy + k), radiusX: 2.0 * s, radiusY: 2.0 * s };
                    rt.FillRoundedRectangle(&sq, &red);
                }
            }
        }
        // tooltip under the bar (the window is taller than the bar; this strip is see-through)
        if let (Some(h), Some(f)) = (p.hover, &p.fmt_ui) {
            if let Some((r, _)) = btns.iter().find(|(_, b)| *b == h) {
                let tip = h.tip();
                let (tw, th) = p.g.measure(&tip, f);
                let (w, hh) = (tw + 16.0 * s, th + 8.0 * s);
                let x = ((r.left + r.right) / 2.0 - w / 2.0).clamp(0.0, (bw - w).max(0.0));
                let y = BAR_H * s + 4.0 * s;
                let box_ = rc(x, y, x + w, y + hh);
                rt.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: box_, radiusX: 5.0 * s, radiusY: 5.0 * s }, &brush(rt, 0xF01C_1C20)?);
                let t: Vec<u16> = tip.encode_utf16().collect();
                let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR);
                rt.DrawText(&t, f, &rc(x + 8.0 * s, y + 4.0 * s, x + w, y + hh), &fg, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
            }
        }
    }
    Ok(())
}

fn hit(x: i32, y: i32) -> Option<Btn> {
    let s = with(|p| p.s)?;
    let (btns, _) = layout(s);
    let (x, y) = (x as f32, y as f32);
    btns.into_iter().find(|(r, _)| x >= r.left && x < r.right && y >= r.top && y < r.bottom).map(|(_, b)| b)
}

fn press(b: Btn) {
    match b {
        Btn::Draw => ink::toggle(),
        Btn::Tool(t) => ink::set_tool(t),
        Btn::Color(i) => ink::set_color(i),
        Btn::Fade => ink::toggle_fade(),
        Btn::Clear => ink::clear(),
        Btn::Stop => stop(),
    }
    // the bar changed width: what is under the cursor now?
    let mut pt = POINT::default();
    if let Some(h) = with(|p| p.hwnd) {
        unsafe {
            let _ = GetCursorPos(&mut pt);
            let _ = ScreenToClient(h, &mut pt);
        }
        let hv = hit(pt.x, pt.y);
        with(|p| p.hover = hv);
    }
    render();
}

fn stop() {
    if let Some(app) = crate::app_hwnd() {
        unsafe {
            let _ = PostMessageW(Some(app), crate::WM_APP_REC_STOP, WPARAM(0), LPARAM(0));
        }
    }
}

unsafe extern "system" fn frame_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            let br = CreateSolidBrush(COLORREF(0x003B_3BFF));
            FillRect(dc, &ps.rcPaint, br);
            let _ = DeleteObject(br.into());
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe extern "system" fn pill_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    use crate::lparam_xy as xy;
    match msg {
        WM_TIMER => {
            render();
            if crate::session::rec_finished() {
                stop();
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = xy(lp);
            let h = hit(x, y);
            if with(|p| std::mem::replace(&mut p.hover, h) != h) == Some(true) {
                render();
            }
            let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
            let _ = TrackMouseEvent(&mut tme);
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            if with(|p| p.hover.take().is_some()) == Some(true) {
                render();
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = xy(lp);
            if let Some(b) = hit(x, y) {
                press(b);
            }
            LRESULT(0)
        }
        WM_HOTKEY if wp.0 as i32 == HK_DRAW => {
            ink::toggle();
            LRESULT(0)
        }
        WM_REC_STOP_REQUEST => {
            stop();
            LRESULT(0)
        }
        WM_SETCURSOR => {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let _ = ScreenToClient(hwnd, &mut pt);
            let c = if hit(pt.x, pt.y).is_some() { IDC_HAND } else { IDC_ARROW };
            if let Ok(c) = LoadCursorW(None, c) {
                SetCursor(Some(c));
            }
            LRESULT(1)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_NCDESTROY => {
            crate::session::poke();
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
