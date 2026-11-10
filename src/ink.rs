//! Drawing on top of a running recording ("ink"): a layered window over the recorded
//! region. It is *not* excluded from capture, so whatever it shows ends up in the video.
//!
//! Cost model: the window is hidden and its pixel buffer freed while there is nothing to
//! show. Frames are rendered only on input, and on a ~30 fps timer only while a stroke is
//! actually fading out. Outside draw mode the window is click-through.

use crate::gfx::*;
use crate::i18n::t;
use crate::keys::{self, Scope, K};
use crate::layered::{push_opacity, Surface};
use std::cell::RefCell;
use std::rc::Rc;
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// A finished stroke stays this long, then fades out over FADE_MS.
const HOLD_MS: u64 = 3000;
const FADE_MS: u64 = 700;
const TIMER_FADE: usize = 1;
const WM_APP_RENDER: u32 = WM_APP + 70;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum InkTool {
    Pen,
    Marker,
    Arrow,
    Rect,
    Counter,
}

pub const TOOLS: [InkTool; 5] = [InkTool::Pen, InkTool::Marker, InkTool::Arrow, InkTool::Rect, InkTool::Counter];
pub const COLORS: [u32; 4] = [0xFFE5_3935, 0xFFFD_D835, 0xFF43_A047, 0xFF1E_88E5];

impl InkTool {
    fn idx(self) -> usize {
        TOOLS.iter().position(|t| *t == self).unwrap_or(0)
    }

    pub fn key(self) -> K {
        match self {
            InkTool::Pen => K::InkPen,
            InkTool::Marker => K::InkMarker,
            InkTool::Arrow => K::InkArrow,
            InkTool::Rect => K::InkRect,
            InkTool::Counter => K::InkCounter,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            InkTool::Pen => t("draw.pen"),
            InkTool::Marker => t("draw.marker"),
            InkTool::Arrow => t("draw.arrow"),
            InkTool::Rect => t("draw.rect"),
            InkTool::Counter => t("draw.counter"),
        }
    }
}

struct Stroke {
    item: Item,
    /// When it was finished; None = stays until cleared (steps, or fading turned off).
    done: Option<u64>,
}

struct Ink {
    hwnd: HWND,
    g: Rc<Gfx>,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    scale: f32,
    items: Vec<Stroke>,
    cur: Option<Item>,
    start: (f32, f32),
    tool: InkTool,
    /// Colour index per tool (the highlighter starts yellow, everything else red).
    colors: [usize; 5],
    sizes: [f32; 5],
    pub fade: bool,
    drawing: bool,
    prev_fg: HWND,
    surf: Option<Surface>,
    render_queued: bool,
}

thread_local! {
    static INK: RefCell<Option<Ink>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Ink) -> R) -> Option<R> {
    INK.with(|s| s.try_borrow_mut().ok().and_then(|mut s| s.as_mut().map(f)))
}

fn now() -> u64 {
    unsafe { GetTickCount64() }
}

/// Create the (hidden) ink window over `r` (screen pixels).
pub fn create(r: RECT, g: Rc<Gfx>, scale: f32, fade: bool) {
    unsafe {
        let inst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(proc_),
            hInstance: inst.into(),
            lpszClassName: w!("WispRecInk"),
            hCursor: LoadCursorW(None, IDC_CROSS).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        let Ok(hwnd) = CreateWindowExW(
            passive_style(),
            w!("WispRecInk"),
            w!(""),
            WS_POPUP,
            r.left,
            r.top,
            w,
            h,
            None,
            None,
            Some(inst.into()),
            None,
        ) else {
            return;
        };
        let s = scale;
        INK.with(|i| {
            *i.borrow_mut() = Some(Ink {
                hwnd,
                g,
                x: r.left,
                y: r.top,
                w,
                h,
                scale,
                items: vec![],
                cur: None,
                start: (0.0, 0.0),
                tool: InkTool::Pen,
                colors: [0, 1, 0, 0, 0],
                sizes: [4.0 * s, 22.0 * s, 4.0 * s, 3.0 * s, 15.0 * s],
                fade,
                drawing: false,
                prev_fg: HWND::default(),
                surf: None,
                render_queued: false,
            })
        });
    }
}

pub fn destroy() {
    if let Some(Some(h)) = INK.with(|i| i.borrow_mut().take().map(|i| i.hwnd).map(Some)) {
        unsafe {
            let _ = DestroyWindow(h);
        }
    }
}

fn passive_style() -> WINDOW_EX_STYLE {
    WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT
}

pub fn drawing() -> bool {
    with(|i| i.drawing).unwrap_or(false)
}

pub fn tool() -> InkTool {
    with(|i| i.tool).unwrap_or(InkTool::Pen)
}

pub fn color() -> usize {
    with(|i| i.colors[i.tool.idx()]).unwrap_or(0)
}

pub fn fade() -> bool {
    with(|i| i.fade).unwrap_or(true)
}

pub fn has_items() -> bool {
    with(|i| !i.items.is_empty()).unwrap_or(false)
}

/// Enter / leave draw mode (pill button, hotkey, Esc, right click).
pub fn toggle() {
    let Some((hwnd, on)) = with(|i| (i.hwnd, !i.drawing)) else { return };
    unsafe {
        if on {
            let prev = GetForegroundWindow();
            with(|i| {
                i.drawing = true;
                i.prev_fg = prev;
            });
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, (WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW).0 as isize);
            render();
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            let _ = SetFocus(Some(hwnd));
        } else {
            let prev = with(|i| {
                i.drawing = false;
                i.cur = None;
                i.prev_fg
            })
            .unwrap_or_default();
            let _ = ReleaseCapture();
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, passive_style().0 as isize);
            render();
            if !prev.is_invalid() && IsWindow(Some(prev)).as_bool() {
                let _ = SetForegroundWindow(prev);
            }
        }
    }
    crate::recui::raise_pill();
    crate::recui::refresh();
}

pub fn set_tool(t: InkTool) {
    with(|i| i.tool = t);
    crate::recui::refresh();
}

pub fn set_color(c: usize) {
    with(|i| {
        let k = i.tool.idx();
        i.colors[k] = c.min(COLORS.len() - 1);
    });
    crate::recui::refresh();
}

/// Fading on/off. Turning it on starts the clock for every existing stroke.
pub fn toggle_fade() {
    let fade = with(|i| {
        i.fade = !i.fade;
        let t = now();
        for s in &mut i.items {
            if !matches!(s.item, Item::Counter { .. }) {
                s.done = if i.fade { Some(t) } else { None };
            }
        }
        i.fade
    });
    if let Some(f) = fade {
        crate::recui::save_fade(f);
    }
    render();
    crate::recui::refresh();
}

pub fn clear() {
    with(|i| {
        i.items.clear();
        i.cur = None;
    });
    render();
    crate::recui::refresh();
}

pub fn undo() {
    with(|i| i.items.pop());
    render();
    crate::recui::refresh();
}

/// Coalesce many redraw requests (mouse moves) into one frame.
fn queue_render() {
    let Some((h, queued)) = with(|i| (i.hwnd, std::mem::replace(&mut i.render_queued, true))) else { return };
    if !queued {
        unsafe {
            let _ = PostMessageW(Some(h), WM_APP_RENDER, WPARAM(0), LPARAM(0));
        }
    }
}

/// Opacity of a stroke at time t (0 = gone).
fn alpha(s: &Stroke, t: u64) -> f32 {
    match s.done {
        None => 1.0,
        Some(d) if t < d + HOLD_MS => 1.0,
        Some(d) => 1.0 - (t - d - HOLD_MS) as f32 / FADE_MS as f32,
    }
}

fn render() {
    with(|i| {
        i.render_queued = false;
        let t = now();
        i.items.retain(|s| alpha(s, t) > 0.0);
        let empty = i.items.is_empty() && i.cur.is_none();
        unsafe {
            if empty && !i.drawing {
                // nothing to show: hide and give the pixel buffer back
                let _ = ShowWindow(i.hwnd, SW_HIDE);
                i.surf = None;
                let _ = KillTimer(Some(i.hwnd), TIMER_FADE);
                return;
            }
            if i.surf.is_none() {
                i.surf = Surface::new(&i.g, i.w, i.h).ok();
            }
            let Some(surf) = &i.surf else { return };
            let rt: &ID2D1RenderTarget = &surf.rt;
            rt.BeginDraw();
            // draw mode: an almost invisible film so empty areas still take the mouse
            let film = if i.drawing { 1.0 / 255.0 } else { 0.0 };
            rt.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: film }));
            for s in &i.items {
                let a = alpha(s, t);
                if a < 1.0 {
                    let _ = push_opacity(rt, a);
                    let _ = draw_item(&i.g, rt, &s.item, false);
                    rt.PopLayer();
                } else {
                    let _ = draw_item(&i.g, rt, &s.item, false);
                }
            }
            if let Some(c) = &i.cur {
                let _ = draw_item(&i.g, rt, c, false);
            }
            let _ = rt.EndDraw(None, None);
            surf.present(i.hwnd, i.x, i.y);
            if !IsWindowVisible(i.hwnd).as_bool() {
                let _ = ShowWindow(i.hwnd, SW_SHOWNOACTIVATE);
                crate::recui::raise_pill();
            }
            // next wake-up: animate while something fades, else sleep until the next fade starts
            let mut next: Option<u64> = None;
            for s in &i.items {
                if let Some(d) = s.done {
                    let start = d + HOLD_MS;
                    let due = if t >= start { 33 } else { start - t };
                    next = Some(next.map_or(due, |n: u64| n.min(due)));
                }
            }
            match next {
                Some(ms) => {
                    SetTimer(Some(i.hwnd), TIMER_FADE, ms.max(15) as u32, None);
                }
                None => {
                    let _ = KillTimer(Some(i.hwnd), TIMER_FADE);
                }
            }
        }
    });
}

fn begin(x: f32, y: f32) {
    with(|i| {
        let (c, w) = (COLORS[i.colors[i.tool.idx()]], i.sizes[i.tool.idx()]);
        i.start = (x, y);
        match i.tool {
            InkTool::Pen | InkTool::Marker => {
                i.cur = Some(Item::Pen { pts: vec![(x, y)], c, w, marker: i.tool == InkTool::Marker })
            }
            InkTool::Arrow => i.cur = Some(Item::Arrow { a: (x, y), b: (x, y), c, w }),
            InkTool::Rect => i.cur = Some(Item::Rect { r: rc(x, y, x, y), c, w }),
            InkTool::Counter => {
                // steps stay until cleared: they tell the order of things
                let n = i
                    .items
                    .iter()
                    .filter_map(|s| if let Item::Counter { n, .. } = s.item { Some(n) } else { None })
                    .max()
                    .unwrap_or(0)
                    + 1;
                i.items.push(Stroke { item: Item::Counter { x, y, n, c, size: w }, done: None });
            }
        }
    });
    render();
}

fn extend(x: f32, y: f32) {
    let changed = with(|i| {
        let (x0, y0) = i.start;
        let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) < 0 };
        match i.cur.as_mut() {
            Some(Item::Pen { pts, .. }) => {
                let l = *pts.last().unwrap();
                if (l.0 - x).abs() + (l.1 - y).abs() < 1.5 {
                    return false;
                }
                pts.push((x, y));
            }
            Some(Item::Arrow { b, .. }) => *b = (x, y),
            Some(Item::Rect { r, .. }) => {
                let (mut x, mut y) = (x, y);
                if shift {
                    let m = (x - x0).abs().max((y - y0).abs());
                    x = x0 + m * (x - x0).signum();
                    y = y0 + m * (y - y0).signum();
                }
                *r = norm((x0, y0), (x, y));
            }
            _ => return false,
        }
        true
    });
    if changed == Some(true) {
        queue_render();
    }
}

fn finish() {
    with(|i| {
        let Some(it) = i.cur.take() else { return };
        let keep = match &it {
            Item::Rect { r, .. } => r.right - r.left >= 3.0 && r.bottom - r.top >= 3.0,
            Item::Arrow { a, b, .. } => (a.0 - b.0).abs() + (a.1 - b.1).abs() >= 6.0,
            _ => true,
        };
        if keep {
            let done = if i.fade { Some(now()) } else { None };
            i.items.push(Stroke { item: it, done });
        }
    });
    render();
    crate::recui::refresh();
}

fn wheel(up: bool) {
    with(|i| {
        let k = i.tool.idx();
        let (lo, hi, step) = match i.tool {
            InkTool::Marker => (8.0, 80.0, 3.0),
            InkTool::Counter => (8.0, 48.0, 2.0),
            _ => (1.0, 30.0, 1.0),
        };
        let s = i.scale;
        i.sizes[k] = (i.sizes[k] + if up { step * s } else { -step * s }).clamp(lo * s, hi * s);
    });
}

fn xy(lp: LPARAM) -> (f32, f32) {
    ((lp.0 & 0xFFFF) as i16 as f32, ((lp.0 >> 16) & 0xFFFF) as i16 as f32)
}

unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_APP_RENDER => {
            render();
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_FADE => {
            render();
            LRESULT(0)
        }
        WM_LBUTTONDOWN if drawing() => {
            SetCapture(hwnd);
            let (x, y) = xy(lp);
            begin(x, y);
            LRESULT(0)
        }
        WM_MOUSEMOVE if drawing() => {
            let (x, y) = xy(lp);
            extend(x, y);
            LRESULT(0)
        }
        WM_LBUTTONUP if drawing() => {
            let _ = ReleaseCapture();
            finish();
            LRESULT(0)
        }
        WM_RBUTTONUP if drawing() => {
            toggle();
            LRESULT(0)
        }
        WM_MOUSEWHEEL if drawing() => {
            wheel(((wp.0 >> 16) & 0xFFFF) as i16 > 0);
            LRESULT(0)
        }
        WM_KEYDOWN if drawing() => {
            match keys::pressed(Scope::Ink, wp.0 as u16) {
                Some(K::InkPen) => set_tool(InkTool::Pen),
                Some(K::InkMarker) => set_tool(InkTool::Marker),
                Some(K::InkArrow) => set_tool(InkTool::Arrow),
                Some(K::InkRect) => set_tool(InkTool::Rect),
                Some(K::InkCounter) => set_tool(InkTool::Counter),
                Some(K::InkColor1) => set_color(0),
                Some(K::InkColor2) => set_color(1),
                Some(K::InkColor3) => set_color(2),
                Some(K::InkColor4) => set_color(3),
                Some(K::InkFade) => toggle_fade(),
                Some(K::InkUndo) => undo(),
                Some(K::InkClear) => clear(),
                _ if VIRTUAL_KEY(wp.0 as u16) == VK_ESCAPE => toggle(),
                _ => {}
            }
            LRESULT(0)
        }
        WM_MOUSEACTIVATE if !drawing() => LRESULT(MA_NOACTIVATE as isize),
        WM_NCHITTEST if !drawing() => LRESULT(HTTRANSPARENT as isize),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
