//! Tray context menu, Fluent style, drawn by hand in the *resident* process.
//!
//! Only GDI: Direct2D would load the GPU driver into the 2 MB tray process. Shapes that
//! need anti-aliasing (rounded rows, the toggle) are rasterised straight into a 32-bit
//! DIB with a signed-distance coverage test; text goes on top with GDI ClearType.
//! The window, fonts and bitmap exist only while the menu is open.

use crate::output::wide;
use std::cell::RefCell;
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

#[derive(Clone)]
pub enum Row {
    Item { id: usize, icon: char, text: String, key: String, danger: bool },
    Toggle { id: usize, icon: char, text: String, on: bool },
    Sep,
}

pub struct Spec {
    pub rows: Vec<Row>,
    /// Shown under the title (e.g. "hotkeys paused").
    pub status: Option<String>,
    /// Called with the chosen command id (on the resident's thread).
    pub on_pick: fn(usize),
    /// Toggle rows: flip the setting and return the new state; the menu stays open.
    pub on_toggle: fn(usize) -> bool,
}

struct Theme {
    bg: u32,
    hover: u32,
    text: u32,
    sub: u32,
    accent: u32,
    danger: u32,
    warn: u32,
    sep: u32,
    off_track: u32,
    off_knob: u32,
}

pub(crate) fn reg_dword(path: PCWSTR, name: PCWSTR) -> Option<u32> {
    let mut v = 0u32;
    let mut n = 4u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            path,
            name,
            RRF_RT_REG_DWORD,
            None,
            Some(&mut v as *mut _ as *mut _),
            Some(&mut n),
        )
        .is_ok()
        .then_some(v)
    }
}

/// WispShot's accent as 0xRRGGBB: the indigo between the two colours of the logo
/// (logo::C0 sky blue and logo::C1 violet), dark enough for white text on it.
pub fn accent() -> u32 {
    0x4F6BED
}

/// Taskbar / menus follow the *system* theme, apps the *app* theme.
pub fn light_theme(system: bool) -> bool {
    // tests: WISP_THEME=light|dark overrides the registry
    match std::env::var("WISP_THEME").as_deref() {
        Ok("light") => return true,
        Ok("dark") => return false,
        _ => {}
    }
    match crate::config::theme() {
        crate::config::Theme::Light => return true,
        crate::config::Theme::Dark => return false,
        crate::config::Theme::System => {}
    }
    let name = if system { w!("SystemUsesLightTheme") } else { w!("AppsUseLightTheme") };
    reg_dword(w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"), name).is_some_and(|v| v != 0)
}

fn theme() -> Theme {
    let a = accent();
    if light_theme(true) {
        Theme {
            bg: 0xF9F9F9,
            hover: 0xEAEAEA,
            text: 0x1B1B1B,
            sub: 0x666666,
            accent: a,
            danger: 0xC42B1C,
            warn: 0x8F6B00,
            sep: 0xE0E0E0,
            off_track: 0xF3F3F3,
            off_knob: 0x5C5C5C,
        }
    } else {
        Theme {
            bg: 0x2C2C2C,
            hover: 0x3D3D3D,
            text: 0xFFFFFF,
            sub: 0xABABAB,
            accent: a,
            danger: 0xFF99A4,
            warn: 0xFCE100,
            sep: 0x404040,
            off_track: 0x353535,
            off_knob: 0xCFCFCF,
        }
    }
}

// ------------------------------------------------------------------ raster

struct Canvas {
    w: i32,
    h: i32,
    px: *mut u32,
}

impl Canvas {
    fn fill(&mut self, c: u32) {
        let n = (self.w * self.h) as usize;
        unsafe { std::slice::from_raw_parts_mut(self.px, n).fill(c) }
    }

    /// Anti-aliased rounded rectangle (pixel coordinates, `a` = 0..1 opacity).
    #[allow(clippy::too_many_arguments)]
    fn rrect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, r: f32, c: u32, a: f32) {
        let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
        let (hw, hh) = ((x1 - x0) / 2.0, (y1 - y0) / 2.0);
        let r = r.min(hw).min(hh);
        let (ya, yb) = ((y0.floor() as i32).max(0), (y1.ceil() as i32).min(self.h));
        let (xa, xb) = ((x0.floor() as i32).max(0), (x1.ceil() as i32).min(self.w));
        let px = unsafe { std::slice::from_raw_parts_mut(self.px, (self.w * self.h) as usize) };
        for y in ya..yb {
            for x in xa..xb {
                let dx = ((x as f32 + 0.5 - cx).abs() - (hw - r)).max(0.0);
                let dy = ((y as f32 + 0.5 - cy).abs() - (hh - r)).max(0.0);
                let d = (dx * dx + dy * dy).sqrt() - r; // < 0 inside
                let cov = (0.5 - d).clamp(0.0, 1.0) * a;
                if cov > 0.0 {
                    let p = &mut px[(y * self.w + x) as usize];
                    *p = mix(*p, c, cov);
                }
            }
        }
    }

    fn circle(&mut self, cx: f32, cy: f32, r: f32, c: u32) {
        self.rrect(cx - r, cy - r, cx + r, cy + r, r, c, 1.0);
    }
}

fn mix(dst: u32, src: u32, a: f32) -> u32 {
    let ch = |sh: u32| -> u32 {
        let (d, s) = (((dst >> sh) & 0xFF) as f32, ((src >> sh) & 0xFF) as f32);
        ((d + (s - d) * a).round() as u32) << sh
    };
    ch(16) | ch(8) | ch(0)
}

/// 0xRRGGBB -> GDI COLORREF (0x00BBGGRR).
fn cref(c: u32) -> COLORREF {
    COLORREF(((c & 0xFF) << 16) | (c & 0xFF00) | ((c >> 16) & 0xFF))
}

// ------------------------------------------------------------------ menu

struct Menu {
    hwnd: HWND,
    spec: Spec,
    t: Theme,
    s: f32,
    w: i32,
    h: i32,
    /// y-range of each row in pixels.
    ys: Vec<(i32, i32)>,
    hover: Option<usize>,
    font: HFONT,
    font_b: HFONT,
    font_s: HFONT,
    font_i: HFONT,
    /// Header logo, `logo_n`² ARGB pixels (rendered once per opening).
    logo: Vec<u32>,
    logo_n: i32,
}

thread_local! {
    static MENU: RefCell<Option<Menu>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Menu) -> R) -> Option<R> {
    MENU.with(|m| m.try_borrow_mut().ok().and_then(|mut m| m.as_mut().map(f)))
}

const W: f32 = 320.0;
const PAD: f32 = 4.0;
const ROW: f32 = 32.0;
const SEP: f32 = 9.0;
const HEAD: f32 = 44.0;
const STATUS: f32 = 26.0;
const LOGO: f32 = 20.0;

fn font_exists(name: &str) -> bool {
    unsafe extern "system" fn cb(_: *const LOGFONTW, _: *const TEXTMETRICW, _: u32, lp: LPARAM) -> i32 {
        *(lp.0 as *mut bool) = true;
        0
    }
    let mut lf = LOGFONTW { lfCharSet: DEFAULT_CHARSET, ..Default::default() };
    let w = wide(name);
    let n = w.len().min(31);
    lf.lfFaceName[..n].copy_from_slice(&w[..n]);
    let mut found = false;
    unsafe {
        let dc = GetDC(None);
        EnumFontFamiliesExW(dc, &lf, Some(cb), LPARAM(&mut found as *mut bool as isize), 0);
        ReleaseDC(None, dc);
    }
    found
}

fn make_font(face: &str, px: i32, weight: i32) -> HFONT {
    let f = wide(face);
    unsafe {
        CreateFontW(
            -px,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            0,
            PCWSTR(f.as_ptr()),
        )
    }
}

pub fn is_open() -> bool {
    MENU.with(|m| m.borrow().is_some())
}

/// Open the menu near (x, y) (the cursor, in screen pixels).
pub fn show(spec: Spec, x: i32, y: i32) {
    if is_open() {
        close();
    }
    unsafe {
        let inst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DROPSHADOW,
            lpfnWndProc: Some(proc_),
            hInstance: inst.into(),
            lpszClassName: w!("WispTrayMenu"),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let mon = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
        let s = crate::monitor_dpi(mon) as f32 / 96.0;
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut mi);
        let wa = mi.rcWork;

        // row geometry
        let mut ys = vec![];
        let mut cy = PAD * s + HEAD * s + if spec.status.is_some() { STATUS * s } else { 0.0 };
        for r in &spec.rows {
            let h = if matches!(r, Row::Sep) { SEP } else { ROW } * s;
            ys.push((cy as i32, (cy + h) as i32));
            cy += h;
        }
        let (w, h) = ((W * s) as i32, (cy + PAD * s) as i32);
        // open towards the screen's inside (the tray is usually bottom-right)
        let mut left = x;
        if left + w > wa.right {
            left = x - w;
        }
        let mut top = y - h;
        if top < wa.top {
            top = y;
        }
        left = left.clamp(wa.left, (wa.right - w).max(wa.left));
        top = top.clamp(wa.top, (wa.bottom - h).max(wa.top));

        let text = if font_exists("Segoe UI Variable Text") { "Segoe UI Variable Text" } else { "Segoe UI" };
        let icons = if font_exists("Segoe Fluent Icons") { "Segoe Fluent Icons" } else { "Segoe MDL2 Assets" };
        let px = |v: f32| (v * s).round() as i32;
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            w!("WispTrayMenu"),
            w!("WispShot"),
            WS_POPUP,
            left,
            top,
            w,
            h,
            None,
            None,
            Some(inst.into()),
            None,
        ) else {
            return;
        };
        let pref = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &pref as *const _ as *const _, 4);
        let logo_n = px(LOGO);
        MENU.with(|m| {
            *m.borrow_mut() = Some(Menu {
                hwnd,
                spec,
                t: theme(),
                s,
                w,
                h,
                ys,
                hover: None,
                font: make_font(text, px(14.0), 400),
                font_b: make_font(text, px(14.0), 600),
                font_s: make_font(text, px(12.0), 400),
                font_i: make_font(icons, px(16.0), 400),
                logo: crate::logo::raster(logo_n as usize, false),
                logo_n,
            })
        });
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(hwnd));
    }
}

pub fn close() {
    if let Some(m) = MENU.with(|m| m.borrow_mut().take()) {
        unsafe {
            for f in [m.font, m.font_b, m.font_s, m.font_i] {
                let _ = DeleteObject(f.into());
            }
            let _ = DestroyWindow(m.hwnd);
        }
        crate::trim_memory();
    }
}

fn paint(m: &Menu, dc: HDC) {
    let s = m.s;
    let t = &m.t;
    unsafe {
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: m.w,
                biHeight: -m.h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let mem = CreateCompatibleDC(Some(dc));
        let Ok(bmp) = CreateDIBSection(Some(mem), &bi, DIB_RGB_COLORS, &mut bits, None, 0) else {
            let _ = DeleteDC(mem);
            return;
        };
        let old = SelectObject(mem, bmp.into());
        let mut cv = Canvas { w: m.w, h: m.h, px: bits as *mut u32 };
        cv.fill(t.bg);
        let (x0, x1) = (PAD * s, m.w as f32 - PAD * s);
        // hover / toggle shapes first, then text
        for (i, r) in m.spec.rows.iter().enumerate() {
            let (ya, yb) = m.ys[i];
            if m.hover == Some(i) && !matches!(r, Row::Sep) {
                cv.rrect(x0, ya as f32 + 1.0, x1, yb as f32 - 1.0, 4.0 * s, t.hover, 1.0);
            }
            match r {
                Row::Sep => {
                    let y = (ya + yb) as f32 / 2.0;
                    cv.rrect(x0 + 8.0 * s, y - 0.5, x1 - 8.0 * s, y + 0.5, 0.0, t.sep, 1.0);
                }
                Row::Toggle { on, .. } => {
                    let (tw, th) = (40.0 * s, 20.0 * s);
                    let tx = x1 - 12.0 * s - tw;
                    let ty = (ya + yb) as f32 / 2.0 - th / 2.0;
                    let knob_r = if *on { 7.0 } else { 6.0 } * s;
                    if *on {
                        cv.rrect(tx, ty, tx + tw, ty + th, th / 2.0, t.accent, 1.0);
                        cv.circle(
                            tx + tw - th / 2.0,
                            ty + th / 2.0,
                            knob_r,
                            if light_theme(true) { 0xFFFFFF } else { 0x000000 },
                        );
                    } else {
                        cv.rrect(tx, ty, tx + tw, ty + th, th / 2.0, t.off_knob, 1.0);
                        cv.rrect(tx + s, ty + s, tx + tw - s, ty + th - s, th / 2.0 - s, t.off_track, 1.0);
                        cv.circle(tx + th / 2.0, ty + th / 2.0, knob_r, t.off_knob);
                    }
                }
                _ => {}
            }
        }
        // header logo in the icon column
        {
            let n = m.logo_n;
            let (lx, ly) = ((16.0 * s).round() as i32, (PAD * s).round() as i32 + ((HEAD * s).round() as i32 - n) / 2);
            let px = std::slice::from_raw_parts_mut(cv.px, (m.w * m.h) as usize);
            for y in 0..n.min(m.h - ly) {
                for x in 0..n.min(m.w - lx) {
                    let c = m.logo[(y * n + x) as usize];
                    if c >> 24 != 0 {
                        let p = &mut px[((ly + y) * m.w + lx + x) as usize];
                        *p = mix(*p, c & 0xFFFFFF, (c >> 24) as f32 / 255.0);
                    }
                }
            }
        }
        let _ = GdiFlush();

        SetBkMode(mem, TRANSPARENT);
        let draw = |text: &str, font: HFONT, color: u32, r: RECT, flags: DRAW_TEXT_FORMAT| {
            let of = SelectObject(mem, font.into());
            SetTextColor(mem, cref(color));
            let mut w: Vec<u16> = text.encode_utf16().collect();
            let mut r = r;
            DrawTextW(mem, &mut w, &mut r, flags | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX);
            SelectObject(mem, of);
        };
        let px = |v: f32| (v * s).round() as i32;
        // header: app name + version, optional status
        let top = px(PAD);
        draw("WispShot", m.font_b, t.text, RECT { left: px(46.0), top, right: m.w, bottom: top + px(HEAD) }, DT_LEFT);
        draw(
            &format!("v{}", env!("CARGO_PKG_VERSION")),
            m.font_s,
            t.sub,
            RECT { left: px(16.0), top, right: m.w - px(16.0), bottom: top + px(HEAD) },
            DT_RIGHT,
        );
        if let Some(st) = &m.spec.status {
            let y = top + px(HEAD) - px(8.0);
            draw(
                "\u{E7BA}",
                m.font_i,
                t.warn,
                RECT { left: px(16.0), top: y, right: px(40.0), bottom: y + px(STATUS) },
                DT_LEFT,
            );
            draw(
                st,
                m.font_s,
                t.sub,
                RECT { left: px(42.0), top: y, right: m.w - px(8.0), bottom: y + px(STATUS) },
                DT_LEFT | DT_END_ELLIPSIS,
            );
        }
        for (i, r) in m.spec.rows.iter().enumerate() {
            let (ya, yb) = m.ys[i];
            let (icon, text, key, color) = match r {
                Row::Item { icon, text, key, danger, .. } => {
                    (*icon, text.as_str(), key.as_str(), if *danger { t.danger } else { t.text })
                }
                Row::Toggle { icon, text, .. } => (*icon, text.as_str(), "", t.text),
                Row::Sep => continue,
            };
            let ic = if matches!(r, Row::Item { danger: true, .. }) { t.danger } else { t.text };
            draw(
                &icon.to_string(),
                m.font_i,
                ic,
                RECT { left: px(16.0), top: ya, right: px(40.0), bottom: yb },
                DT_LEFT,
            );
            // leave room only for what is on the right (shortcut or switch)
            let key_w = if key.is_empty() {
                0
            } else {
                let of = SelectObject(mem, m.font_s.into());
                let mut w: Vec<u16> = key.encode_utf16().collect();
                let mut kr = RECT::default();
                DrawTextW(mem, &mut w, &mut kr, DT_SINGLELINE | DT_CALCRECT | DT_NOPREFIX);
                SelectObject(mem, of);
                kr.right - kr.left + px(12.0)
            };
            let right = if matches!(r, Row::Toggle { .. }) { m.w - px(64.0) } else { m.w - px(16.0) - key_w };
            draw(text, m.font, color, RECT { left: px(46.0), top: ya, right, bottom: yb }, DT_LEFT | DT_END_ELLIPSIS);
            if !key.is_empty() {
                draw(
                    key,
                    m.font_s,
                    t.sub,
                    RECT { left: px(150.0), top: ya, right: m.w - px(16.0), bottom: yb },
                    DT_RIGHT,
                );
            }
        }
        let _ = BitBlt(dc, 0, 0, m.w, m.h, Some(mem), 0, 0, SRCCOPY);
        SelectObject(mem, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
    }
}

fn row_at(y: i32) -> Option<usize> {
    with(|m| m.ys.iter().position(|(a, b)| y >= *a && y < *b).filter(|&i| !matches!(m.spec.rows[i], Row::Sep)))
        .flatten()
}

fn activate(i: usize) {
    let Some(row) = with(|m| (m.spec.rows.get(i).cloned(), m.spec.on_pick, m.spec.on_toggle, m.hwnd)) else { return };
    match row {
        (Some(Row::Item { id, .. }), pick, _, _) => {
            close();
            pick(id);
        }
        (Some(Row::Toggle { id, .. }), _, toggle, hwnd) => {
            let on = toggle(id);
            with(|m| {
                if let Some(Row::Toggle { on: o, .. }) = m.spec.rows.get_mut(i) {
                    *o = on;
                }
            });
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
        }
        _ => {}
    }
}

fn step(down: bool) {
    with(|m| {
        let n = m.spec.rows.len();
        let mut i = m.hover.unwrap_or(if down { n - 1 } else { 0 });
        for _ in 0..n {
            i = if down { (i + 1) % n } else { (i + n - 1) % n };
            if !matches!(m.spec.rows[i], Row::Sep) {
                break;
            }
        }
        m.hover = Some(i);
        unsafe {
            let _ = InvalidateRect(Some(m.hwnd), None, false);
        }
    });
}

unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            MENU.with(|m| {
                if let Ok(m) = m.try_borrow() {
                    if let Some(m) = m.as_ref() {
                        paint(m, dc);
                    }
                }
            });
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEMOVE => {
            let (_, y) = crate::lparam_xy(lp);
            let r = row_at(y);
            if with(|m| std::mem::replace(&mut m.hover, r) != r) == Some(true) {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            let _ = TrackMouseEvent(&mut tme);
            LRESULT(0)
        }
        windows::Win32::UI::Controls::WM_MOUSELEAVE => {
            if with(|m| m.hover.take().is_some()) == Some(true) {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (_, y) = crate::lparam_xy(lp);
            if let Some(i) = row_at(y) {
                activate(i);
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            match VIRTUAL_KEY(wp.0 as u16) {
                VK_ESCAPE => close(),
                VK_DOWN | VK_TAB => step(true),
                VK_UP => step(false),
                VK_RETURN | VK_SPACE => {
                    if let Some(Some(i)) = with(|m| m.hover) {
                        activate(i);
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_ACTIVATE if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE => {
            // clicked elsewhere: close after this message (not from inside it)
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            LRESULT(0)
        }
        WM_CLOSE => {
            close();
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
