//! Full-screen frozen-desktop selector + annotation editor.
//! Lives only while capturing; everything is freed on close.

use crate::capture::{self, Shot};
use crate::gfx::*;
use crate::i18n::{t, tf};
use crate::img::{self, Img};
use crate::keys::{self, Scope, K};
use crate::textedit::TextEdit;
use std::cell::RefCell;
use std::rc::Rc;
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub const WM_APP_DONE: u32 = WM_APP + 10;

#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    Capture,
    Record,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Action {
    Copy,
    CopyFile,
    Save,
    Pin,
    Ocr,
    Record,
    CopyColor,
}

pub struct Done {
    pub action: Action,
    pub img: Option<Img>,
    /// Selection in virtual-screen coordinates.
    pub rect: RECT,
    pub text: String,
}

thread_local! {
    pub static DONE: RefCell<Option<Done>> = const { RefCell::new(None) };
    static OPEN: RefCell<Option<HWND>> = const { RefCell::new(None) };
}

pub fn is_open() -> bool {
    OPEN.with(|o| o.borrow().is_some())
}

const ACCENT: u32 = 0xFF2B_8CFF;
const PALETTE: [u32; 8] =
    [0xFFE5_3935, 0xFFFB_8C00, 0xFFFD_D835, 0xFF43_A047, 0xFF1E_88E5, 0xFF8E_24AA, 0xFF21_2121, 0xFFFF_FFFF];

#[derive(Clone, Copy, PartialEq)]
enum Btn {
    Tool(Tool),
    Undo,
    Redo,
    Pin,
    Ocr,
    Record,
    Save,
    CopyFile,
    Copy,
    Close,
    Color(usize),
}

impl Btn {
    fn tip(self) -> String {
        // "Name  (key)", or just the name when the action has no key
        let k = |name: &str, k: K| {
            let l = keys::label(k);
            if l.is_empty() {
                name.to_string()
            } else {
                format!("{}  ({})", name, l)
            }
        };
        match self {
            Btn::Tool(Tool::Rect) => k(t("tool.rect"), K::Rect),
            Btn::Tool(Tool::Ellipse) => k(t("tool.ellipse"), K::Ellipse),
            Btn::Tool(Tool::Arrow) => k(t("tool.arrow"), K::Arrow),
            Btn::Tool(Tool::Line) => k(t("tool.line"), K::Line),
            Btn::Tool(Tool::Pen) => k(t("tool.pen"), K::Pen),
            Btn::Tool(Tool::Marker) => k(t("tool.marker"), K::Marker),
            Btn::Tool(Tool::Text) => k(t("tool.text"), K::Text),
            Btn::Tool(Tool::Counter) => k(t("tool.counter"), K::Counter),
            Btn::Tool(Tool::Mosaic) => k(t("tool.mosaic"), K::Mosaic),
            Btn::Tool(Tool::Blur) => k(t("tool.blur"), K::Blur),
            Btn::Tool(Tool::None) => String::new(),
            Btn::Undo => k(t("editor.undo"), K::Undo),
            Btn::Redo => k(t("editor.redo"), K::Redo),
            Btn::Pin => k(t("editor.pin"), K::PinIt),
            Btn::Ocr => k(t("editor.ocr"), K::Ocr),
            Btn::Record => k(t("editor.record"), K::RecordArea),
            Btn::Save => k(t("editor.save"), K::Save),
            Btn::CopyFile => k(t("editor.copy_file"), K::CopyFile),
            Btn::Copy => tf("editor.copy_tip", &[&keys::label(K::Copy)]),
            Btn::Close => format!("{}  (Esc)", t("editor.close")),
            Btn::Color(_) => t("editor.color_tip").to_string(),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    None,
    Selecting {
        x0: i32,
        y0: i32,
        moved: bool,
    },
    Moving {
        dx: i32,
        dy: i32,
    },
    Resizing {
        edges: u8,
    },
    Drawing {
        x0: f32,
        y0: f32,
    },
    /// Ctrl+drag on an existing drawing: (lx, ly) last mouse position, (tx, ty) total offset.
    MovingItem {
        idx: usize,
        lx: i32,
        ly: i32,
        tx: i32,
        ty: i32,
    },
    /// Selecting inside the open text box with the mouse.
    TextSel,
}

/// Undo / redo entry. Applying one restores the earlier state and yields its own
/// inverse (which goes on the other stack). Every step is reversed exactly and in
/// order, so the indices stored here always point at the right drawing.
enum Op {
    Insert { idx: usize, item: Item },
    Remove { idx: usize },
    Move { idx: usize, dx: f32, dy: f32 },
    Replace { idx: usize, item: Item },
}

struct Ov {
    hwnd: HWND,
    g: Gfx,
    rt: Option<ID2D1HwndRenderTarget>,
    base: Option<ID2D1Bitmap>,
    shot: Shot,
    mode: Mode,
    deep: bool,
    magnifier: bool,
    sel: Option<RECT>,
    drag: Drag,
    mouse: (i32, i32),
    hover: Option<RECT>,
    tool: Tool,
    color: usize,
    sizes: [f32; 11],
    items: Vec<Item>,
    undo: Vec<Op>,
    redo: Vec<Op>,
    /// Drawing chosen with Ctrl+click: Delete removes it, arrows nudge it, F2 edits text.
    picked: Option<usize>,
    /// Text being re-edited: its index (hidden while editing) and the tool to go back to.
    editing: Option<(usize, Tool)>,
    /// Blur / mosaic of the selection, computed on first use (index: 0 mosaic, 1 blur).
    fx: [Option<Rc<Effect>>; 2],
    /// Drawing under the cursor while Ctrl is held (it can be dragged).
    hot: Option<usize>,
    cur: Option<Item>,
    text: Option<TextEdit>,
    tb_hover: Option<Btn>,
    scale: f32,
    fmt_ui: Option<IDWriteTextFormat>,
    fmt_icon: Option<IDWriteTextFormat>,
    closing: bool,
    /// Current message was replayed from the veil.
    forwarded: bool,
    /// First-frame GDI copy of the grab; replaced by Direct2D right after showing.
    gdi: Option<capture::GdiFrame>,
    ready: bool,
    /// Briefly show the tool size next to the cursor after a wheel change.
    size_hint: bool,
}

const WM_APP_INIT: u32 = WM_APP + 11;
const TIMER_SIZE_HINT: usize = 21;

impl Drop for Ov {
    fn drop(&mut self) {
        crate::trace("overlay state dropped");
    }
}

fn tool_idx(t: Tool) -> usize {
    t as usize
}

pub fn open(mode: Mode, magnifier: bool, deep: bool, veil: Option<HWND>) -> bool {
    if is_open() {
        return false;
    }
    crate::trace("open: begin");
    // The screen grab (GDI readback, the slowest step) runs on a worker thread
    // while this thread builds the window, Direct2D target and font caches.
    let vs = capture::virtual_screen();
    let grabber = std::thread::spawn(move || capture::grab_frame(vs));
    // Instant feedback: a dim veil that screen capture cannot see. Normally the
    // resident process already showed it the moment the hotkey fired.
    let veil = veil.filter(|v| unsafe { IsWindow(Some(*v)).as_bool() }).or_else(|| crate::veil::show(vs));
    VEIL.with(|v| *v.borrow_mut() = veil);
    crate::trace("open: veil ready");
    let windows = capture::enum_windows(vs.left, vs.top, deep);
    let Ok(g) = Gfx::new() else {
        let _ = grabber.join();
        destroy_veil();
        return false;
    };
    crate::trace("open: gfx");
    unsafe {
        let inst = GetModuleHandleW(None).unwrap_or_default();
        let cls = w!("WispOverlay");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: inst.into(),
            lpszClassName: cls,
            hCursor: LoadCursorW(None, IDC_CROSS).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let scale = crate::monitor_dpi(MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST)) as f32 / 96.0;
        let mut sizes = [0f32; 11];
        for t in [
            Tool::None,
            Tool::Rect,
            Tool::Ellipse,
            Tool::Arrow,
            Tool::Line,
            Tool::Pen,
            Tool::Marker,
            Tool::Text,
            Tool::Counter,
            Tool::Mosaic,
            Tool::Blur,
        ] {
            sizes[tool_idx(t)] = t.default_size();
        }
        let (ox, oy, w, h) = (vs.left, vs.top, vs.right - vs.left, vs.bottom - vs.top);
        let shot = Shot { ox, oy, img: Img { w, h, px: Vec::new() }, windows };
        let ov = Box::new(Ov {
            hwnd: HWND::default(),
            g,
            rt: None,
            base: None,
            mouse: (pt.x - ox, pt.y - oy),
            shot,
            mode,
            deep,
            magnifier,
            sel: None,
            drag: Drag::None,
            hover: None,
            tool: Tool::None,
            color: 0,
            sizes,
            items: Vec::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            picked: None,
            editing: None,
            fx: [None, None],
            hot: None,
            cur: None,
            text: None,
            tb_hover: None,
            scale,
            fmt_ui: None,
            fmt_icon: None,
            closing: false,
            forwarded: false,
            gdi: None,
            ready: false,
            size_hint: false,
        });
        let raw = Box::into_raw(ov);
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            cls,
            w!("WispShot"),
            WS_POPUP,
            ox,
            oy,
            w,
            h,
            None,
            None,
            Some(inst.into()),
            Some(raw as *const _),
        );
        let Ok(hwnd) = hwnd else {
            drop(Box::from_raw(raw));
            let _ = grabber.join();
            destroy_veil();
            return false;
        };

        // Render the first frame while cloaked: no flash of an empty window.
        let cloak: BOOL = TRUE;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_CLOAK, &cloak as *const _ as *const _, 4);
        no_transitions(hwnd);
        crate::trace("open: window created");
        let ov = &mut *raw;
        let frame = grabber.join().ok().flatten();
        crate::trace("open: grab joined");
        let Some(frame) = frame else {
            let _ = DestroyWindow(hwnd);
            destroy_veil();
            return false;
        };
        ov.gdi = Some(frame);
        ov.update_hover();
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = UpdateWindow(hwnd); // first frame: plain GDI
        crate::trace("open: gdi frame painted");
        let off: BOOL = FALSE;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_CLOAK, &off as *const _ as *const _, 4);
        take_foreground(hwnd);
        VEIL.with(|v| {
            if let Some(v) = *v.borrow() {
                // hidden, not destroyed: input queued on it is still forwarded to us
                let _ = PostMessageW(Some(v), crate::veil::WM_VEIL_HIDE, WPARAM(0), LPARAM(0));
            }
        });
        OPEN.with(|o| *o.borrow_mut() = Some(hwnd));
        crate::trace("open: visible+focused");
        true
    }
}

/// A hotkey alone does not always grant foreground rights. Injecting an empty
/// input event makes this process the "last input" owner (same trick PowerToys uses).
unsafe fn take_foreground(hwnd: HWND) {
    if SetForegroundWindow(hwnd).as_bool() && GetForegroundWindow() == hwnd {
        let _ = SetFocus(Some(hwnd));
        return;
    }
    let inp = INPUT { r#type: INPUT_MOUSE, ..Default::default() };
    SendInput(&[inp], std::mem::size_of::<INPUT>() as i32);
    let _ = SetForegroundWindow(hwnd);
    let _ = SetFocus(Some(hwnd));
}

thread_local! {
    static VEIL: RefCell<Option<HWND>> = const { RefCell::new(None) };
}

/// No DWM show/hide animation: it costs GPU time right when we read the screen.
fn no_transitions(h: HWND) {
    let on: BOOL = TRUE;
    unsafe {
        let _ = DwmSetWindowAttribute(h, DWMWA_TRANSITIONS_FORCEDISABLED, &on as *const _ as *const _, 4);
    }
}

fn destroy_veil() {
    if let Some(h) = VEIL.with(|v| v.borrow_mut().take()) {
        unsafe {
            let _ = PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

fn norm_rect(a: (i32, i32), b: (i32, i32)) -> RECT {
    RECT { left: a.0.min(b.0), top: a.1.min(b.1), right: a.0.max(b.0) + 1, bottom: a.1.max(b.1) + 1 }
}

fn rf(r: &RECT) -> D2D_RECT_F {
    rc(r.left as f32, r.top as f32, r.right as f32, r.bottom as f32)
}

impl Ov {
    fn ensure_rt(&mut self) -> Option<ID2D1HwndRenderTarget> {
        if self.rt.is_none() {
            unsafe {
                let props = D2D1_RENDER_TARGET_PROPERTIES {
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: D2D1_ALPHA_MODE_IGNORE,
                    },
                    dpiX: 96.0,
                    dpiY: 96.0,
                    ..Default::default()
                };
                let hp = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                    hwnd: self.hwnd,
                    pixelSize: D2D_SIZE_U { width: self.shot.img.w as u32, height: self.shot.img.h as u32 },
                    presentOptions: D2D1_PRESENT_OPTIONS_IMMEDIATELY,
                };
                let rt = self.g.d2d.CreateHwndRenderTarget(&props, &hp).ok()?;
                crate::trace("rt created");
                if !self.shot.img.px.is_empty() {
                    self.base = bitmap(&rt, &self.shot.img).ok();
                }
                for it in &self.items {
                    if let Item::Fx { fx, .. } = it {
                        *fx.cache.borrow_mut() = None;
                    }
                }
                for f in self.fx.iter().flatten() {
                    *f.cache.borrow_mut() = None;
                }
                self.rt = Some(rt);
                self.fmt_ui = self.g.text_format(12.0 * self.scale, false).ok();
                self.fmt_icon = self.g.icon_format(16.0 * self.scale).ok();
            }
        }
        self.rt.clone()
    }

    /// Switch from the GDI first frame to Direct2D (runs once, right after showing).
    fn init_d2d(&mut self) {
        if self.ready {
            return;
        }
        self.ready = true;
        if let Some(f) = self.gdi.take() {
            self.shot.img = f.to_img();
        }
        crate::trace("d2d: image copied");
        if std::env::var_os("WISP_NOD2D").is_some() {
            return;
        }
        self.ensure_rt();
        crate::trace("d2d: target + base ready");
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn paint_gdi(&self, dc: HDC) {
        let Some(f) = &self.gdi else { return };
        unsafe {
            let mem = CreateCompatibleDC(Some(dc));
            let old = SelectObject(mem, HGDIOBJ(f.dim as *mut _));
            let _ = BitBlt(dc, 0, 0, f.w, f.h, Some(mem), 0, 0, SRCCOPY);
            if let Some(r) = self.focus() {
                SelectObject(mem, HGDIOBJ(f.bright as *mut _));
                let _ =
                    BitBlt(dc, r.left, r.top, r.right - r.left, r.bottom - r.top, Some(mem), r.left, r.top, SRCCOPY);
                let br = CreateSolidBrush(COLORREF(0x00FF_8C2B));
                let b = (2.0 * self.scale) as i32;
                for e in [
                    RECT { left: r.left - b, top: r.top - b, right: r.right + b, bottom: r.top },
                    RECT { left: r.left - b, top: r.bottom, right: r.right + b, bottom: r.bottom + b },
                    RECT { left: r.left - b, top: r.top, right: r.left, bottom: r.bottom },
                    RECT { left: r.right, top: r.top, right: r.right + b, bottom: r.bottom },
                ] {
                    FillRect(dc, &e, br);
                }
                let _ = DeleteObject(br.into());
            }
            SelectObject(mem, old);
            let _ = DeleteDC(mem);
        }
    }

    fn size(&self) -> f32 {
        self.sizes[tool_idx(self.tool)]
    }

    fn col(&self) -> u32 {
        PALETTE[self.color]
    }

    /// The rect currently "in focus": selection, rubber band, or detected window.
    fn focus(&self) -> Option<RECT> {
        match self.drag {
            Drag::Selecting { x0, y0, moved: true } => Some(norm_rect((x0, y0), self.mouse)),
            _ => self.sel.or(self.hover),
        }
    }

    fn update_hover(&mut self) {
        if self.deep && !self.shot.windows.iter().any(|w| !w.2.is_empty()) {
            capture::fill_children(&mut self.shot.windows, self.shot.ox, self.shot.oy);
        }
        if self.sel.is_none() {
            let (x, y) = self.mouse;
            self.hover = capture::detect(&self.shot.windows, x, y, self.deep).map(|r| RECT {
                left: r.left.max(0),
                top: r.top.max(0),
                right: r.right.min(self.shot.img.w),
                bottom: r.bottom.min(self.shot.img.h),
            });
        }
    }

    fn monitor_rect(&self, x: i32, y: i32) -> RECT {
        unsafe {
            let m = MonitorFromPoint(POINT { x: x + self.shot.ox, y: y + self.shot.oy }, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = GetMonitorInfoW(m, &mut mi);
            let r = mi.rcMonitor;
            RECT {
                left: r.left - self.shot.ox,
                top: r.top - self.shot.oy,
                right: r.right - self.shot.ox,
                bottom: r.bottom - self.shot.oy,
            }
        }
    }

    fn toolbar(&self) -> Vec<(D2D_RECT_F, Btn)> {
        let Some(sel) = self.sel else { return vec![] };
        if !matches!(self.drag, Drag::None | Drag::Drawing { .. }) {
            return vec![];
        }
        let s = self.scale;
        let bs = 32.0 * s;
        let gap = 6.0 * s;
        let mut row: Vec<Option<Btn>> = vec![];
        for t in [
            Tool::Rect,
            Tool::Ellipse,
            Tool::Arrow,
            Tool::Line,
            Tool::Pen,
            Tool::Marker,
            Tool::Text,
            Tool::Counter,
            Tool::Mosaic,
            Tool::Blur,
        ] {
            row.push(Some(Btn::Tool(t)));
        }
        row.extend([
            None,
            Some(Btn::Undo),
            Some(Btn::Redo),
            None,
            Some(Btn::Pin),
            Some(Btn::Ocr),
            Some(Btn::Record),
            Some(Btn::Save),
            Some(Btn::CopyFile),
            None,
            Some(Btn::Close),
            Some(Btn::Copy),
        ]);
        let sep = 9.0 * s;
        let width: f32 = row.iter().map(|b| if b.is_some() { bs } else { sep }).sum();
        let show_colors = !matches!(self.tool, Tool::None | Tool::Mosaic | Tool::Blur);
        let rows_h = if show_colors { bs * 2.0 + gap * 0.5 } else { bs };
        let mon = self.monitor_rect(sel.right - 1, sel.bottom - 1);
        let mut y = sel.bottom as f32 + gap;
        if y + rows_h > mon.bottom as f32 {
            y = sel.top as f32 - gap - rows_h;
            if y < mon.top as f32 {
                y = sel.bottom as f32 - gap - rows_h;
            }
        }
        let mut x = (sel.right as f32 - width).max(mon.left as f32 + gap);
        x = x.min(mon.right as f32 - width - gap);
        let mut out = vec![];
        let x0 = x;
        for b in row {
            match b {
                Some(b) => {
                    out.push((rc(x, y, x + bs, y + bs), b));
                    x += bs;
                }
                None => x += sep,
            }
        }
        if show_colors {
            let y2 = y + bs + gap * 0.5;
            let mut cx = x0;
            for i in 0..PALETTE.len() {
                out.push((rc(cx, y2, cx + bs, y2 + bs), Btn::Color(i)));
                cx += bs;
            }
        }
        out
    }

    fn hit_toolbar(&self, x: i32, y: i32) -> Option<Btn> {
        let (x, y) = (x as f32, y as f32);
        self.toolbar()
            .into_iter()
            .find(|(r, _)| x >= r.left && x < r.right && y >= r.top && y < r.bottom)
            .map(|(_, b)| b)
    }

    fn hit_edges(&self, x: i32, y: i32) -> u8 {
        let Some(s) = self.sel else { return 0 };
        let t = (6.0 * self.scale) as i32;
        if x < s.left - t || x > s.right + t || y < s.top - t || y > s.bottom + t {
            return 0;
        }
        let mut e = 0;
        if (x - s.left).abs() <= t {
            e |= 1;
        }
        if (y - s.top).abs() <= t {
            e |= 2;
        }
        if (x - s.right).abs() <= t {
            e |= 4;
        }
        if (y - s.bottom).abs() <= t {
            e |= 8;
        }
        e
    }

    fn inside_sel(&self, x: i32, y: i32) -> bool {
        self.sel.is_some_and(|s| capture::contains(&s, x, y))
    }

    // ---------------------------------------------------------------- render
    fn render(&mut self) {
        if !self.ready {
            return;
        }
        let Some(rt) = self.ensure_rt() else { return };
        unsafe {
            rt.BeginDraw();
            let _ = self.draw(&rt);
            if let Err(e) = rt.EndDraw(None, None) {
                if e.code() == D2DERR_RECREATE_TARGET {
                    self.rt = None;
                    self.base = None;
                    let _ = InvalidateRect(Some(self.hwnd), None, false);
                }
            }
        }
    }

    fn draw(&self, hrt: &ID2D1HwndRenderTarget) -> Result<()> {
        let rt: &ID2D1RenderTarget = hrt;
        let s = self.scale;
        let (iw, ih) = (self.shot.img.w as f32, self.shot.img.h as f32);
        unsafe {
            if let Some(b) = &self.base {
                rt.DrawBitmap(b, None, 1.0, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None);
            }
            // annotations (clipped to the selection)
            if let Some(sel) = self.sel {
                rt.PushAxisAlignedClip(&rf(&sel), D2D1_ANTIALIAS_MODE_ALIASED);
                for (i, it) in self.items.iter().enumerate() {
                    if self.editing.map(|e| e.0) != Some(i) {
                        draw_item(&self.g, rt, it, true)?;
                    }
                }
                if let Some(c) = &self.cur {
                    draw_item(&self.g, rt, c, true)?;
                }
                if let (Some(te), Some(l)) = (&self.text, self.text_layout()) {
                    let size = self.sizes[tool_idx(Tool::Text)];
                    let hl = brush(rt, 0x802B_8CFF)?;
                    for r in te.sel_rects(&l) {
                        rt.FillRectangle(&r, &hl);
                    }
                    let it = Item::Text { x: te.x, y: te.y, s: te.text(), c: self.col(), size };
                    draw_item(&self.g, rt, &it, true)?;
                    let (cx, cy, ch) = te.caret_pos(&l, te.caret);
                    let dark = brush(rt, 0x9000_0000)?;
                    let b = brush(rt, self.col())?;
                    rt.DrawLine(pt(cx + 0.5, cy + 1.0), pt(cx + 0.5, cy + ch - 1.0), &dark, 3.0, None);
                    rt.DrawLine(pt(cx + 0.5, cy + 1.0), pt(cx + 0.5, cy + ch - 1.0), &b, 1.5, None);
                    let dash = brush(rt, 0x99FF_FFFF)?;
                    rt.DrawRectangle(&te.frame(&l, size * 1.33), &dash, 1.0, None);
                }
                if let (Drag::Drawing { x0, y0 }, Tool::Mosaic | Tool::Blur, None) = (self.drag, self.tool, &self.cur) {
                    let r = norm((x0, y0), (self.mouse.0 as f32, self.mouse.1 as f32));
                    let b = brush(rt, ACCENT)?;
                    rt.DrawRectangle(&r, &b, 1.5, None);
                }
                rt.PopAxisAlignedClip();
            }

            // dim everything outside the focus rect
            let dim = brush(rt, 0x8000_0000)?;
            match self.focus() {
                Some(f) => {
                    let f = rf(&f);
                    rt.FillRectangle(&rc(0.0, 0.0, iw, f.top), &dim);
                    rt.FillRectangle(&rc(0.0, f.bottom, iw, ih), &dim);
                    rt.FillRectangle(&rc(0.0, f.top, f.left, f.bottom), &dim);
                    rt.FillRectangle(&rc(f.right, f.top, iw, f.bottom), &dim);
                    let acc = brush(rt, ACCENT)?;
                    let bw = if self.sel.is_some() { 1.0 * s.max(1.0) } else { 2.0 * s };
                    rt.DrawRectangle(
                        &rc(f.left - bw / 2.0, f.top - bw / 2.0, f.right + bw / 2.0, f.bottom + bw / 2.0),
                        &acc,
                        bw,
                        None,
                    );
                    // size label
                    let mut label = format!("{} × {}", (f.right - f.left) as i32, (f.bottom - f.top) as i32);
                    if self.sel.is_none() && self.drag == Drag::None {
                        label += "   ";
                        label += &if self.mode == Mode::Record {
                            t("editor.hint_record").to_string()
                        } else {
                            tf("editor.hint_capture", &[&keys::label(K::Copy)])
                        };
                    }
                    self.label(rt, &label, f.left, f.top - 24.0 * s, true)?;
                    if self.sel.is_some() {
                        let hs = 3.5 * s.max(1.0);
                        let white = brush(rt, 0xFFFF_FFFF)?;
                        let (mx, my) = ((f.left + f.right) / 2.0, (f.top + f.bottom) / 2.0);
                        for (x, y) in [
                            (f.left, f.top),
                            (mx, f.top),
                            (f.right, f.top),
                            (f.left, my),
                            (f.right, my),
                            (f.left, f.bottom),
                            (mx, f.bottom),
                            (f.right, f.bottom),
                        ] {
                            let r = rc(x - hs, y - hs, x + hs, y + hs);
                            rt.FillRectangle(&r, &white);
                            rt.DrawRectangle(&r, &acc, 1.0, None);
                        }
                    }
                }
                None => rt.FillRectangle(&rc(0.0, 0.0, iw, ih), &dim),
            }

            let show_mag = self.magnifier
                && match self.drag {
                    Drag::Selecting { .. } | Drag::Resizing { .. } => true,
                    Drag::None => self.sel.is_none(),
                    _ => false,
                };
            if show_mag {
                self.draw_magnifier(rt)?;
            }
            // the drawing Ctrl+drag would move (or is moving)
            let hot = match self.drag {
                Drag::MovingItem { idx, .. } => Some(idx),
                _ => self.hot.or(self.picked),
            };
            if let Some(it) = hot.and_then(|i| self.items.get(i)) {
                let b = it.bounds(&self.g);
                let m = 3.0 * s;
                let r = rc(b.left - m, b.top - m, b.right + m, b.bottom + m);
                let dark = brush(rt, 0xA000_0000)?;
                let acc = brush(rt, ACCENT)?;
                rt.DrawRectangle(&r, &dark, 3.0 * s.max(1.0), None);
                rt.DrawRectangle(&r, &acc, 1.5 * s.max(1.0), None);
                if self.drag == Drag::None {
                    let tip =
                        if matches!(it, Item::Text { .. }) { t("editor.picked_text") } else { t("editor.picked") };
                    let below = r.bottom + 6.0 * s;
                    let y = if below + 30.0 * s > ih { r.top - 30.0 * s } else { below };
                    self.label(rt, tip, r.left.max(2.0), y, false)?;
                }
            }
            if hot.is_none() {
                self.draw_brush_preview(rt)?;
            }
            self.draw_toolbar(rt)?;
            if self.mode == Mode::Record && self.sel.is_none() {
                let (mx, my) = self.mouse;
                let m = self.monitor_rect(mx, my);
                self.label(rt, t("editor.pick_record_area"), m.left as f32 + 16.0 * s, m.top as f32 + 16.0 * s, false)?;
            }
        }
        Ok(())
    }

    fn label(&self, rt: &ID2D1RenderTarget, text: &str, x: f32, y: f32, clamp_top: bool) -> Result<()> {
        let Some(f) = &self.fmt_ui else { return Ok(()) };
        let s = self.scale;
        let (tw, th) = self.g.measure(text, f);
        let mut y = y;
        if clamp_top && y < 2.0 {
            y += 28.0 * s;
        }
        let r = rc(x, y, x + tw + 12.0 * s, y + th + 6.0 * s);
        unsafe {
            let bg = brush(rt, 0xE61E_1E1E)?;
            let fg = brush(rt, 0xFFFF_FFFF)?;
            let rr = D2D1_ROUNDED_RECT { rect: r, radiusX: 3.0 * s, radiusY: 3.0 * s };
            rt.FillRoundedRectangle(&rr, &bg);
            let w: Vec<u16> = text.encode_utf16().collect();
            rt.DrawText(
                &w,
                f,
                &rc(x + 6.0 * s, y + 3.0 * s, r.right, r.bottom),
                &fg,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
        Ok(())
    }

    fn draw_magnifier(&self, rt: &ID2D1RenderTarget) -> Result<()> {
        let Some(base) = &self.base else { return Ok(()) };
        let s = self.scale;
        let (mx, my) = self.mouse;
        let n = 15; // source pixels across
        let cell = (8.0 * s).round();
        let size = cell * n as f32;
        let info_h = 40.0 * s;
        let mon = self.monitor_rect(mx, my);
        let mut x = mx as f32 + 20.0 * s;
        let mut y = my as f32 + 20.0 * s;
        if x + size > mon.right as f32 {
            x = mx as f32 - 20.0 * s - size;
        }
        if y + size + info_h > mon.bottom as f32 {
            y = my as f32 - 20.0 * s - size - info_h;
        }
        unsafe {
            let src = rc((mx - n / 2) as f32, (my - n / 2) as f32, (mx + n / 2 + 1) as f32, (my + n / 2 + 1) as f32);
            let dst = rc(x, y, x + size, y + size);
            let bg = brush(rt, 0xFF00_0000)?;
            rt.FillRectangle(&dst, &bg);
            rt.DrawBitmap(base, Some(&dst), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, Some(&src));
            let cross = brush(rt, 0x802B_8CFF)?;
            let c0 = x + cell * (n / 2) as f32;
            let r0 = y + cell * (n / 2) as f32;
            rt.FillRectangle(&rc(c0, y, c0 + cell, r0), &cross);
            rt.FillRectangle(&rc(c0, r0 + cell, c0 + cell, y + size), &cross);
            rt.FillRectangle(&rc(x, r0, c0, r0 + cell), &cross);
            rt.FillRectangle(&rc(c0 + cell, r0, x + size, r0 + cell), &cross);
            let white = brush(rt, 0xFFFF_FFFF)?;
            rt.DrawRectangle(&rc(c0, r0, c0 + cell, r0 + cell), &white, 1.0, None);
            rt.DrawRectangle(&dst, &white, 1.0, None);
            let p = self.shot.img.get(mx, my);
            let info = format!(
                "{}, {}   #{:06X}\n{}",
                mx + self.shot.ox,
                my + self.shot.oy,
                p & 0xFF_FFFF,
                tf("editor.copy_color_hint", &[&keys::label(K::PickColor)])
            );
            let ir = rc(x, y + size, x + size, y + size + info_h);
            let ibg = brush(rt, 0xE61E_1E1E)?;
            rt.FillRectangle(&ir, &ibg);
            let sw = brush(rt, 0xFF00_0000 | p)?;
            let sq = 12.0 * s;
            rt.FillRectangle(&rc(x + 6.0 * s, ir.top + 6.0 * s, x + 6.0 * s + sq, ir.top + 6.0 * s + sq), &sw);
            rt.DrawRectangle(
                &rc(x + 6.0 * s, ir.top + 6.0 * s, x + 6.0 * s + sq, ir.top + 6.0 * s + sq),
                &white,
                1.0,
                None,
            );
            if let Some(f) = &self.fmt_ui {
                let w: Vec<u16> = info.encode_utf16().collect();
                rt.DrawText(
                    &w,
                    f,
                    &rc(x + 24.0 * s, ir.top + 4.0 * s, ir.right, ir.bottom),
                    &white,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }
        }
        Ok(())
    }

    fn draw_toolbar(&self, rt: &ID2D1RenderTarget) -> Result<()> {
        let tb = self.toolbar();
        if tb.is_empty() {
            return Ok(());
        }
        let s = self.scale;
        unsafe {
            let bg = brush(rt, 0xF22A_2A2E)?;
            let hov = brush(rt, 0xFF44_4450)?;
            let act = brush(rt, ACCENT)?;
            let fg = brush(rt, 0xFFF0_F0F0)?;
            let dis = brush(rt, 0xFF70_7070)?;
            // backgrounds per row
            let main_top = tb[0].0.top;
            let mut rows: Vec<D2D_RECT_F> = vec![];
            for (r, _) in &tb {
                if let Some(rr) = rows.iter_mut().find(|rr| rr.top == r.top) {
                    rr.left = rr.left.min(r.left);
                    rr.right = rr.right.max(r.right);
                } else {
                    rows.push(*r);
                }
            }
            for r in rows {
                let pad = 3.0 * s;
                let rr = D2D1_ROUNDED_RECT {
                    rect: rc(r.left - pad, r.top - pad, r.right + pad, r.bottom + pad),
                    radiusX: 6.0 * s,
                    radiusY: 6.0 * s,
                };
                rt.FillRoundedRectangle(&rr, &bg);
            }
            let icon = |ch: &str, r: &D2D_RECT_F, b: &ID2D1SolidColorBrush| {
                if let Some(f) = &self.fmt_icon {
                    let w: Vec<u16> = ch.encode_utf16().collect();
                    rt.DrawText(&w, f, r, b, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
                }
            };
            for (r, b) in &tb {
                let inner = rc(r.left + 2.0 * s, r.top + 2.0 * s, r.right - 2.0 * s, r.bottom - 2.0 * s);
                let active = matches!(b, Btn::Tool(t) if *t == self.tool);
                if active {
                    rt.FillRoundedRectangle(
                        &D2D1_ROUNDED_RECT { rect: inner, radiusX: 4.0 * s, radiusY: 4.0 * s },
                        &act,
                    );
                } else if self.tb_hover == Some(*b) {
                    rt.FillRoundedRectangle(
                        &D2D1_ROUNDED_RECT { rect: inner, radiusX: 4.0 * s, radiusY: 4.0 * s },
                        &hov,
                    );
                }
                let (cx, cy) = ((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0);
                let u = 5.5 * s;
                match b {
                    Btn::Tool(Tool::Rect) => icon("\u{E739}", r, &fg),
                    Btn::Tool(Tool::Ellipse) => icon("\u{EA3A}", r, &fg),
                    Btn::Tool(Tool::Arrow) => {
                        let it = Item::Arrow {
                            a: (cx - u * 1.3, cy + u * 1.3),
                            b: (cx + u * 1.3, cy - u * 1.3),
                            c: 0xFFF0_F0F0,
                            w: 1.6 * s,
                        };
                        draw_item(&self.g, rt, &it, false)?;
                    }
                    Btn::Tool(Tool::Line) => rt.DrawLine(
                        pt(cx - u * 1.3, cy + u * 1.3),
                        pt(cx + u * 1.3, cy - u * 1.3),
                        &fg,
                        1.6 * s,
                        &self.g.round,
                    ),
                    Btn::Tool(Tool::Pen) => icon("\u{E70F}", r, &fg),
                    Btn::Tool(Tool::Marker) => icon("\u{E7E6}", r, &fg),
                    Btn::Tool(Tool::Text) => icon("\u{E8D2}", r, &fg),
                    Btn::Tool(Tool::Counter) => {
                        let e = D2D1_ELLIPSE { point: pt(cx, cy), radiusX: u * 1.45, radiusY: u * 1.45 };
                        rt.DrawEllipse(&e, &fg, 1.3 * s, None);
                        if let Ok(f) = self.g.text_format(11.0 * s, true) {
                            let _ = f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
                            let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
                            rt.DrawText(
                                &[b'1' as u16],
                                &f,
                                r,
                                &fg,
                                D2D1_DRAW_TEXT_OPTIONS_NONE,
                                DWRITE_MEASURING_MODE_NATURAL,
                            );
                        }
                    }
                    Btn::Tool(Tool::Mosaic) => {
                        let c = u * 0.85;
                        for i in 0..3 {
                            for j in 0..3 {
                                let x = cx - c * 1.5 + c * i as f32;
                                let y = cy - c * 1.5 + c * j as f32;
                                let rr = rc(x, y, x + c, y + c);
                                if (i + j) % 2 == 0 {
                                    rt.FillRectangle(&rr, &fg);
                                } else {
                                    rt.DrawRectangle(&rr, &fg, 0.8 * s, None);
                                }
                            }
                        }
                    }
                    Btn::Tool(Tool::Blur) => {
                        for (k, a) in [(1.6f32, 0x40u32), (1.25, 0x80), (0.9, 0xC0), (0.55, 0xFF)] {
                            let bb = brush(rt, (a << 24) | 0xF0F0F0)?;
                            let e = D2D1_ELLIPSE { point: pt(cx, cy), radiusX: u * k, radiusY: u * k };
                            rt.FillEllipse(&e, &bb);
                        }
                    }
                    Btn::Tool(Tool::None) => {}
                    Btn::Undo => icon("\u{E7A7}", r, if self.undo.is_empty() { &dis } else { &fg }),
                    Btn::Redo => icon("\u{E7A6}", r, if self.redo.is_empty() { &dis } else { &fg }),
                    Btn::Pin => icon("\u{E718}", r, &fg),
                    Btn::Ocr => {
                        if let Ok(f) = self.g.text_format(9.5 * s, true) {
                            let _ = f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
                            let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
                            let w: Vec<u16> = "OCR".encode_utf16().collect();
                            rt.DrawText(&w, &f, r, &fg, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
                        }
                        rt.DrawRoundedRectangle(
                            &D2D1_ROUNDED_RECT {
                                rect: rc(cx - u * 1.6, cy - u * 1.1, cx + u * 1.6, cy + u * 1.1),
                                radiusX: 2.0 * s,
                                radiusY: 2.0 * s,
                            },
                            &fg,
                            1.0 * s,
                            None,
                        );
                    }
                    Btn::Record => {
                        let red = brush(rt, 0xFFFF_4040)?;
                        rt.FillEllipse(&D2D1_ELLIPSE { point: pt(cx, cy), radiusX: u * 1.1, radiusY: u * 1.1 }, &red);
                        rt.DrawEllipse(
                            &D2D1_ELLIPSE { point: pt(cx, cy), radiusX: u * 1.6, radiusY: u * 1.6 },
                            &fg,
                            1.2 * s,
                            None,
                        );
                    }
                    Btn::Save => icon("\u{E74E}", r, &fg),
                    Btn::CopyFile => icon("\u{E8A5}", r, &fg),
                    Btn::Copy => {
                        let green = brush(rt, 0xFF4C_D964)?;
                        icon("\u{E73E}", r, &green)
                    }
                    Btn::Close => icon("\u{E711}", r, &fg),
                    Btn::Color(i) => {
                        let cb = brush(rt, PALETTE[*i])?;
                        let rr = rc(cx - u * 1.2, cy - u * 1.2, cx + u * 1.2, cy + u * 1.2);
                        rt.FillRoundedRectangle(
                            &D2D1_ROUNDED_RECT { rect: rr, radiusX: 3.0 * s, radiusY: 3.0 * s },
                            &cb,
                        );
                        if *i == self.color {
                            rt.DrawRoundedRectangle(
                                &D2D1_ROUNDED_RECT {
                                    rect: rc(
                                        rr.left - 3.0 * s,
                                        rr.top - 3.0 * s,
                                        rr.right + 3.0 * s,
                                        rr.bottom + 3.0 * s,
                                    ),
                                    radiusX: 4.0 * s,
                                    radiusY: 4.0 * s,
                                },
                                &fg,
                                1.5 * s,
                                None,
                            );
                        } else if PALETTE[*i] == 0xFF21_2121 {
                            rt.DrawRoundedRectangle(
                                &D2D1_ROUNDED_RECT { rect: rr, radiusX: 3.0 * s, radiusY: 3.0 * s },
                                &dis,
                                1.0,
                                None,
                            );
                        }
                    }
                }
            }
            // size readout on the colour row
            if let Some((r, _)) = tb.iter().rev().find(|(_, b)| matches!(b, Btn::Color(7))) {
                if let Some(f) = &self.fmt_ui {
                    let txt = format!(
                        "  {}: {:.0}",
                        if self.tool == Tool::Text { t("editor.font_size") } else { t("editor.width") },
                        self.size()
                    );
                    let w: Vec<u16> = txt.encode_utf16().collect();
                    let (tw, _) = self.g.measure(&txt, f);
                    let bgr = rc(r.right + 3.0 * s, r.top - 3.0 * s, r.right + tw + 12.0 * s, r.bottom + 3.0 * s);
                    rt.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: bgr, radiusX: 6.0 * s, radiusY: 6.0 * s }, &bg);
                    rt.DrawText(
                        &w,
                        f,
                        &rc(r.right + 4.0 * s, r.top + 8.0 * s, r.right + tw + 12.0 * s, r.bottom),
                        &fg,
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                        DWRITE_MEASURING_MODE_NATURAL,
                    );
                }
            }
            if let Some(h) = self.tb_hover {
                if let Some((r, _)) = tb.iter().find(|(_, b)| *b == h) {
                    let tip = h.tip();
                    if let Some(f) = &self.fmt_ui {
                        let (tw, _) = self.g.measure(&tip, f);
                        let above = r.top <= main_top + 1.0;
                        let y = if above { main_top - 30.0 * s } else { r.bottom + 8.0 * s };
                        let x = (r.left + r.right) / 2.0 - tw / 2.0 - 6.0 * s;
                        self.label(rt, &tip, x.max(2.0), y, false)?;
                    }
                }
            }
        }
        Ok(())
    }

    // ---------------------------------------------------------------- input
    fn finish(&mut self, action: Action) {
        let mut done = Done { action, img: None, rect: RECT::default(), text: String::new() };
        if action == Action::CopyColor {
            let p = self.shot.img.get(self.mouse.0, self.mouse.1);
            done.text = format!("#{:06X}", p & 0xFF_FFFF);
        } else if let Some(sel) = self.sel.or(self.hover) {
            // no click needed: Ctrl+C / Enter etc. take the detected window as it is
            self.sel = Some(sel);
            self.commit_text();
            done.rect = RECT {
                left: sel.left + self.shot.ox,
                top: sel.top + self.shot.oy,
                right: sel.right + self.shot.ox,
                bottom: sel.bottom + self.shot.oy,
            };
            let geo = (sel.left, sel.top, sel.right - sel.left, sel.bottom - sel.top);
            done.img = match action {
                Action::Record => None,
                Action::Ocr => {
                    let mut c = self.shot.img.crop(geo.0, geo.1, geo.2, geo.3);
                    c.force_opaque();
                    Some(c)
                }

                _ => export(&self.g, &self.shot.img, geo, &self.items).ok(),
            };
        } else {
            return;
        }
        DONE.with(|d| *d.borrow_mut() = Some(done));
        self.close();
    }

    /// Deferred close: hide now (feels instant), destroy from the queue so no
    /// code still running on `self` can touch freed state.
    fn close(&mut self) {
        if self.closing {
            return;
        }
        self.closing = true;
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
            let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }

    fn commit_text(&mut self) {
        let editing = self.editing.take();
        let Some(te) = self.text.take() else { return };
        let (x, y, s) = (te.x, te.y, te.text());
        let (c, size) = (self.col(), self.sizes[tool_idx(Tool::Text)]);
        match editing {
            Some((idx, prev)) => {
                self.tool = prev;
                let same = matches!(self.items.get(idx), Some(Item::Text { s: o, c: oc, size: os, .. }) if *o == s && *oc == c && *os == size);
                if s.trim().is_empty() {
                    self.exec(Op::Remove { idx });
                } else if !same {
                    self.exec(Op::Replace { idx, item: Item::Text { x, y, s, c, size } });
                }
            }
            None if !s.trim().is_empty() => self.push(Item::Text { x, y, s, c, size }),
            None => {}
        }
    }

    /// Drop the text being typed; a re-edited text keeps its old content.
    fn cancel_text(&mut self) {
        self.text = None;
        if let Some((_, prev)) = self.editing.take() {
            self.tool = prev;
        }
    }

    /// Re-open the text drawing under (x,y) in the editor.
    fn edit_text_at(&mut self, px: i32, py: i32) -> bool {
        self.commit_text(); // may remove a drawing: look the text up afterwards
        let Some(idx) = self.text_at(px, py) else { return false };
        let Some(Item::Text { x, y, s, c, size }) = self.items.get(idx) else { return false };
        if let Some(ci) = PALETTE.iter().position(|p| p == c) {
            self.color = ci;
        }
        self.sizes[tool_idx(Tool::Text)] = *size;
        self.text = Some(TextEdit::new(*x, *y, s));
        self.editing = Some((idx, self.tool));
        self.tool = Tool::Text;
        self.picked = None;
        self.hot = None;
        self.text_caret_to(px, py, false);
        true
    }

    fn text_layout(&self) -> Option<IDWriteTextLayout> {
        let te = self.text.as_ref()?;
        let f = self.g.text_format(self.sizes[tool_idx(Tool::Text)], true).ok()?;
        te.layout(&self.g, &f)
    }

    /// Is (x,y) on the open text box (with a little slack)?
    fn in_text_box(&self, x: i32, y: i32) -> bool {
        let (Some(te), Some(l)) = (&self.text, self.text_layout()) else { return false };
        let r = te.frame(&l, self.sizes[tool_idx(Tool::Text)] * 1.33);
        let m = 4.0 * self.scale;
        let (x, y) = (x as f32, y as f32);
        x >= r.left - m && x <= r.right + m && y >= r.top - m && y <= r.bottom + m
    }

    fn text_caret_to(&mut self, x: i32, y: i32, extend: bool) {
        let l = self.text_layout();
        if let (Some(te), Some(l)) = (self.text.as_mut(), l) {
            let i = te.hit(&l, x as f32, y as f32);
            te.move_to(i, extend);
        }
    }

    fn text_select_word(&mut self, x: i32, y: i32) {
        self.text_caret_to(x, y, false);
        if let Some(te) = self.text.as_mut() {
            let (a, b) = te.word_at(te.caret);
            te.move_to(a, false);
            te.move_to(b, true);
        }
    }

    /// Top-most text drawing whose box contains (x,y).
    fn text_at(&self, x: i32, y: i32) -> Option<usize> {
        let (fx, fy) = (x as f32, y as f32);
        let m = 3.0 * self.scale;
        self.items.iter().rposition(|it| {
            matches!(it, Item::Text { .. }) && {
                let b = it.bounds(&self.g);
                fx >= b.left - m && fx <= b.right + m && fy >= b.top - m && fy <= b.bottom + m
            }
        })
    }

    fn apply(&mut self, op: Op) -> Option<Op> {
        Some(match op {
            Op::Insert { idx, item } => {
                let idx = idx.min(self.items.len());
                self.items.insert(idx, item);
                Op::Remove { idx }
            }
            Op::Remove { idx } => {
                if idx >= self.items.len() {
                    return None;
                }
                Op::Insert { idx, item: self.items.remove(idx) }
            }
            Op::Move { idx, dx, dy } => {
                self.items.get_mut(idx)?.offset(dx, dy);
                Op::Move { idx, dx: -dx, dy: -dy }
            }
            Op::Replace { idx, item } => Op::Replace { idx, item: std::mem::replace(self.items.get_mut(idx)?, item) },
        })
    }

    /// Do `op` as a new user action.
    fn exec(&mut self, op: Op) {
        if let Some(inv) = self.apply(op) {
            self.undo.push(inv);
            self.redo.clear();
        }
    }

    fn push(&mut self, it: Item) {
        self.exec(Op::Insert { idx: self.items.len(), item: it });
    }

    fn delete_item(&mut self, idx: usize) {
        self.exec(Op::Remove { idx });
        self.picked = None;
        self.hot = None;
        self.update_hot();
    }

    fn undo(&mut self) {
        if let Some(te) = self.text.as_mut() {
            // typing first; once that is all undone, drop the box (a re-edit keeps the old text)
            if !te.undo() {
                self.cancel_text();
            }
            return;
        }
        self.picked = None;
        if let Some(inv) = self.undo.pop().and_then(|op| self.apply(op)) {
            self.redo.push(inv);
        }
    }

    fn redo(&mut self) {
        if let Some(te) = self.text.as_mut() {
            te.redo();
            return;
        }
        self.picked = None;
        if let Some(inv) = self.redo.pop().and_then(|op| self.apply(op)) {
            self.undo.push(inv);
        }
    }

    /// Top-most drawing at (x,y): first one whose stroke is hit, else the smallest
    /// one whose box contains the point (text, filled areas, big shapes from inside).
    fn item_at(&self, x: i32, y: i32) -> Option<usize> {
        let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
        let tol = 4.0 * self.scale;
        if let Some(i) = self.items.iter().rposition(|it| it.hit(fx, fy, tol)) {
            return Some(i);
        }
        let area = |r: &D2D_RECT_F| (r.right - r.left) * (r.bottom - r.top);
        self.items
            .iter()
            .enumerate()
            .map(|(i, it)| (i, it.bounds(&self.g)))
            .filter(|(_, b)| fx >= b.left && fx <= b.right && fy >= b.top && fy <= b.bottom)
            .min_by(|a, b| area(&a.1).total_cmp(&area(&b.1)))
            .map(|(i, _)| i)
    }

    /// Recompute the Ctrl-hover drawing; true if it changed.
    fn update_hot(&mut self) -> bool {
        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) < 0 };
        let (x, y) = self.mouse;
        let hot = if ctrl && self.sel.is_some() && self.text.is_none() && self.drag == Drag::None {
            self.item_at(x, y)
        } else {
            None
        };
        std::mem::replace(&mut self.hot, hot) != hot
    }

    /// The blur / mosaic image for the current tool, covering the selection.
    fn effect(&mut self) -> Option<Rc<Effect>> {
        let sel = self.sel?;
        let i = (self.tool == Tool::Blur) as usize;
        if let Some(f) = &self.fx[i] {
            if f.x <= sel.left && f.y <= sel.top && f.x + f.img.w >= sel.right && f.y + f.img.h >= sel.bottom {
                return Some(f.clone());
            }
        }
        let mut p = self.shot.img.crop(sel.left, sel.top, sel.right - sel.left, sel.bottom - sel.top);
        let k = (12.0 * self.scale.max(1.0)).round() as i32;
        if i == 0 {
            img::pixelate(&mut p, k);
        } else {
            img::blur(&mut p, k);
        }
        let f = Rc::new(Effect { x: sel.left, y: sel.top, img: p, cache: RefCell::new(None) });
        self.fx[i] = Some(f.clone());
        Some(f)
    }

    fn set_tool(&mut self, t: Tool) {
        self.commit_text();
        self.picked = None;
        self.tool = if self.tool == t { Tool::None } else { t };
    }

    fn press(&mut self, b: Btn) {
        match b {
            Btn::Tool(t) => self.set_tool(t),
            Btn::Undo => self.undo(),
            Btn::Redo => self.redo(),
            Btn::Pin => self.finish(Action::Pin),
            Btn::Ocr => self.finish(Action::Ocr),
            Btn::Record => self.finish(Action::Record),
            Btn::Save => self.finish(Action::Save),
            Btn::CopyFile => self.finish(Action::CopyFile),
            Btn::Copy => self.finish(Action::Copy),
            Btn::Close => self.close(),
            Btn::Color(i) => self.color = i,
        }
    }

    fn constrain(&self, x0: f32, y0: f32, x: f32, y: f32, square: bool) -> (f32, f32) {
        if !unsafe { GetKeyState(VK_SHIFT.0 as i32) < 0 } {
            return (x, y);
        }
        let (dx, dy) = (x - x0, y - y0);
        if square {
            let m = dx.abs().max(dy.abs());
            (x0 + m * dx.signum(), y0 + m * dy.signum())
        } else {
            let ang = dy.atan2(dx);
            let snap = (ang / (std::f32::consts::PI / 4.0)).round() * (std::f32::consts::PI / 4.0);
            let len = (dx * dx + dy * dy).sqrt();
            (x0 + len * snap.cos(), y0 + len * snap.sin())
        }
    }

    fn mouse_down(&mut self, x: i32, y: i32) {
        self.mouse = (x, y);
        if let Some(b) = self.hit_toolbar(x, y) {
            self.press(b);
            return;
        }
        if !self.forwarded {
            unsafe { SetCapture(self.hwnd) };
        }
        self.picked = None;
        if self.text.is_some() && self.in_text_box(x, y) {
            let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) < 0 };
            self.text_caret_to(x, y, shift);
            self.drag = Drag::TextSel;
            return;
        }
        // Ctrl + drag on a drawing moves it (any tool); otherwise drawings stay out of the way.
        if unsafe { GetKeyState(VK_CONTROL.0 as i32) < 0 } && self.sel.is_some() {
            self.commit_text();
            if let Some(idx) = self.item_at(x, y) {
                self.hot = Some(idx);
                self.picked = Some(idx);
                self.drag = Drag::MovingItem { idx, lx: x, ly: y, tx: 0, ty: 0 };
                return;
            }
        }
        if self.sel.is_none() {
            self.drag = Drag::Selecting { x0: x, y0: y, moved: false };
            return;
        }
        if self.tool != Tool::None && self.inside_sel(x, y) {
            let (fx, fy) = (x as f32, y as f32);
            let (c, w) = (self.col(), self.size());
            match self.tool {
                Tool::Text => {
                    let had = self.text.is_some();
                    self.commit_text();
                    // clicking an existing text opens it for editing
                    if !had && !self.edit_text_at(x, y) {
                        self.text = Some(TextEdit::new(fx, fy - self.size() * 0.7, ""));
                    }
                    return;
                }
                Tool::Counter => {
                    let n = self
                        .items
                        .iter()
                        .filter_map(|i| if let Item::Counter { n, .. } = i { Some(*n) } else { None })
                        .max()
                        .unwrap_or(0)
                        + 1;
                    self.push(Item::Counter { x: fx, y: fy, n, c, size: w });
                    return;
                }
                Tool::Pen | Tool::Marker => {
                    self.cur = Some(Item::Pen { pts: vec![(fx, fy)], c, w, marker: self.tool == Tool::Marker })
                }
                // freehand brush; Shift + drag selects a rectangle instead (cur stays None)
                Tool::Mosaic | Tool::Blur if unsafe { GetKeyState(VK_SHIFT.0 as i32) >= 0 } => {
                    if let Some(e) = self.effect() {
                        self.cur = Some(Item::Fx { fx: e, shape: FxShape::Stroke { pts: vec![(fx, fy)], w } });
                    }
                }
                _ => {}
            }
            self.drag = Drag::Drawing { x0: fx, y0: fy };
            return;
        }
        let e = self.hit_edges(x, y);
        if e != 0 && (self.tool == Tool::None || !self.inside_sel(x, y)) {
            self.drag = Drag::Resizing { edges: e };
        } else if self.inside_sel(x, y) && self.tool == Tool::None {
            let s = self.sel.unwrap();
            self.drag = Drag::Moving { dx: x - s.left, dy: y - s.top };
        } else if self.items.is_empty() && self.tool == Tool::None {
            self.sel = None;
            self.drag = Drag::Selecting { x0: x, y0: y, moved: false };
        }
    }

    fn mouse_move(&mut self, x: i32, y: i32) {
        self.mouse = (x, y);
        let (iw, ih) = (self.shot.img.w, self.shot.img.h);
        match self.drag {
            Drag::None => {
                self.tb_hover = self.hit_toolbar(x, y);
                self.update_hover();
                self.update_hot();
            }
            Drag::MovingItem { idx, lx, ly, tx, ty } => {
                let (dx, dy) = (x - lx, y - ly);
                if let Some(it) = self.items.get_mut(idx) {
                    it.offset(dx as f32, dy as f32);
                }
                self.drag = Drag::MovingItem { idx, lx: x, ly: y, tx: tx + dx, ty: ty + dy };
            }
            Drag::TextSel => self.text_caret_to(x, y, true),
            Drag::Selecting { x0, y0, moved } => {
                if !moved && ((x - x0).abs() > 3 || (y - y0).abs() > 3) {
                    self.drag = Drag::Selecting { x0, y0, moved: true };
                }
            }
            Drag::Moving { dx, dy } => {
                if let Some(s) = self.sel.as_mut() {
                    let (w, h) = (s.right - s.left, s.bottom - s.top);
                    let nx = (x - dx).clamp(0, iw - w);
                    let ny = (y - dy).clamp(0, ih - h);
                    *s = RECT { left: nx, top: ny, right: nx + w, bottom: ny + h };
                }
            }
            Drag::Resizing { edges } => {
                if let Some(s) = self.sel.as_mut() {
                    let (x, y) = (x.clamp(0, iw), y.clamp(0, ih));
                    if edges & 1 != 0 {
                        s.left = x;
                    }
                    if edges & 2 != 0 {
                        s.top = y;
                    }
                    if edges & 4 != 0 {
                        s.right = x;
                    }
                    if edges & 8 != 0 {
                        s.bottom = y;
                    }
                }
            }
            Drag::Drawing { x0, y0 } => {
                let (fx, fy) = (x as f32, y as f32);
                let (c, w) = (self.col(), self.size());
                match self.tool {
                    Tool::Rect | Tool::Ellipse => {
                        let b = self.constrain(x0, y0, fx, fy, true);
                        let r = norm((x0, y0), b);
                        self.cur = Some(if self.tool == Tool::Rect {
                            Item::Rect { r, c, w }
                        } else {
                            Item::Ellipse { r, c, w }
                        });
                    }
                    Tool::Arrow | Tool::Line => {
                        let b = self.constrain(x0, y0, fx, fy, false);
                        self.cur = Some(if self.tool == Tool::Arrow {
                            Item::Arrow { a: (x0, y0), b, c, w }
                        } else {
                            Item::Line { a: (x0, y0), b, c, w }
                        });
                    }
                    Tool::Mosaic | Tool::Blur | Tool::Pen | Tool::Marker => match self.cur.as_mut() {
                        Some(Item::Fx { shape: FxShape::Stroke { pts, .. }, .. }) | Some(Item::Pen { pts, .. })
                            // skip sub-pixel moves
                            if pts.last().is_none_or(|l| (l.0 - fx).abs() + (l.1 - fy).abs() >= 1.5) => {
                                pts.push((fx, fy));
                            }
                        _ => {}
                    },
                    _ => {}
                }
            }
        }
    }

    fn mouse_up(&mut self, x: i32, y: i32) {
        self.mouse = (x, y);
        unsafe {
            let _ = ReleaseCapture();
        }
        let drag = std::mem::replace(&mut self.drag, Drag::None);
        match drag {
            Drag::Selecting { x0, y0, moved } => {
                let r = if moved { Some(norm_rect((x0, y0), (x, y))) } else { self.hover };
                if let Some(mut r) = r {
                    r.left = r.left.max(0);
                    r.top = r.top.max(0);
                    r.right = r.right.min(self.shot.img.w);
                    r.bottom = r.bottom.min(self.shot.img.h);
                    if r.right - r.left >= 2 && r.bottom - r.top >= 2 {
                        self.sel = Some(r);
                        self.hover = None;
                        if self.mode == Mode::Record {
                            self.finish(Action::Record);
                        }
                    }
                }
            }
            Drag::Resizing { .. } => {
                if let Some(s) = self.sel.as_mut() {
                    let n = RECT {
                        left: s.left.min(s.right),
                        top: s.top.min(s.bottom),
                        right: s.left.max(s.right),
                        bottom: s.top.max(s.bottom),
                    };
                    *s = n;
                    if s.right - s.left < 2 || s.bottom - s.top < 2 {
                        self.sel = None;
                    }
                }
            }
            Drag::MovingItem { idx, tx, ty, .. } => {
                if tx != 0 || ty != 0 {
                    // already moved live: only record how to move it back
                    self.undo.push(Op::Move { idx, dx: -tx as f32, dy: -ty as f32 });
                    self.redo.clear();
                }
                self.update_hot();
            }
            Drag::Drawing { x0, y0 } => match self.tool {
                Tool::Mosaic | Tool::Blur if self.cur.is_none() => {
                    if let Some(sel) = self.sel {
                        let r = norm_rect((x0 as i32, y0 as i32), (x, y));
                        let l = r.left.max(sel.left);
                        let t = r.top.max(sel.top);
                        let rr = r.right.min(sel.right);
                        let b = r.bottom.min(sel.bottom);
                        if rr - l >= 2 && b - t >= 2 {
                            if let Some(fx) = self.effect() {
                                self.push(Item::Fx {
                                    fx,
                                    shape: FxShape::Rect(rc(l as f32, t as f32, rr as f32, b as f32)),
                                });
                            }
                        }
                    }
                }
                _ => {
                    if let Some(it) = self.cur.take() {
                        let keep = match &it {
                            Item::Rect { r, .. } | Item::Ellipse { r, .. } => {
                                r.right - r.left >= 2.0 || r.bottom - r.top >= 2.0
                            }
                            Item::Arrow { a, b, .. } | Item::Line { a, b, .. } => {
                                (a.0 - b.0).abs() + (a.1 - b.1).abs() >= 3.0
                            }
                            _ => true,
                        };
                        if keep {
                            self.push(it);
                        }
                    }
                }
            },
            _ => {}
        }
        self.cur = None;
    }

    fn right_click(&mut self) {
        if self.text.is_some() {
            self.cancel_text();
        } else if self.picked.is_some() {
            self.picked = None;
        } else if self.tool != Tool::None {
            self.tool = Tool::None;
        } else if self.sel.is_some() && self.items.is_empty() {
            self.sel = None;
            self.update_hover();
        } else if self.sel.is_none() {
            self.close();
        }
    }

    fn key(&mut self, vk: u16) -> bool {
        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) < 0 };
        let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) < 0 };
        let vkk = VIRTUAL_KEY(vk);
        if self.text.is_some() {
            let l = self.text_layout();
            let Some(te) = self.text.as_mut() else { return false };
            match vkk {
                VK_ESCAPE => self.commit_text(),
                VK_LEFT | VK_RIGHT => {
                    let right = vkk == VK_RIGHT;
                    let (a, b) = te.range();
                    let to = match (ctrl, right) {
                        _ if te.has_sel() && !shift && !ctrl => {
                            if right {
                                b
                            } else {
                                a
                            }
                        }
                        (true, false) => te.word_left(te.caret),
                        (true, true) => te.word_right(te.caret),
                        (false, false) => te.prev(te.caret),
                        (false, true) => te.next(te.caret),
                    };
                    te.move_to(to, shift);
                }
                VK_UP | VK_DOWN => {
                    if let Some(l) = &l {
                        let to = if vkk == VK_UP && te.line_start(te.caret) == 0 {
                            0
                        } else if vkk == VK_DOWN && te.line_end(te.caret) == te.buf.len() {
                            te.buf.len()
                        } else {
                            te.vertical(l, vkk == VK_DOWN)
                        };
                        te.move_to(to, shift);
                    }
                }
                VK_HOME => {
                    let to = if ctrl { 0 } else { te.line_start(te.caret) };
                    te.move_to(to, shift);
                }
                VK_END => {
                    let to = if ctrl { te.buf.len() } else { te.line_end(te.caret) };
                    te.move_to(to, shift);
                }
                VK_DELETE => te.erase(true, ctrl),
                _ if ctrl => match vk as u8 {
                    b'A' => te.select_all(),
                    b'C' | b'X' => {
                        if te.has_sel() {
                            let t = te.selected().replace('\n', "\r\n");
                            crate::output::copy_text(self.hwnd, &t);
                            if vk == b'X' as u16 {
                                if let Some(te) = self.text.as_mut() {
                                    te.delete_sel();
                                }
                            }
                        }
                    }
                    b'V' => {
                        if let Some(t) = crate::output::clipboard_text(self.hwnd) {
                            if let Some(te) = self.text.as_mut() {
                                te.insert_str(&t);
                            }
                        }
                    }
                    b'Z' if shift => self.redo(),
                    b'Z' => self.undo(),
                    b'Y' => self.redo(),
                    _ => return false,
                },
                _ => return false, // goes to WM_CHAR
            }
            return true;
        }
        if let Some(idx) = self.picked {
            let step = if shift { 10.0 } else { 1.0 };
            let mv = match vkk {
                VK_LEFT => Some((-step, 0.0)),
                VK_RIGHT => Some((step, 0.0)),
                VK_UP => Some((0.0, -step)),
                VK_DOWN => Some((0.0, step)),
                _ => None,
            };
            if let Some((dx, dy)) = mv {
                self.exec(Op::Move { idx, dx, dy });
                return true;
            }
            if keys::pressed(Scope::Editor, vk) == Some(K::EditText) {
                if let Some(b) =
                    self.items.get(idx).filter(|i| matches!(i, Item::Text { .. })).map(|i| i.bounds(&self.g))
                {
                    if self.edit_text_at(((b.left + b.right) / 2.0) as i32, ((b.top + b.bottom) / 2.0) as i32) {
                        if let Some(te) = self.text.as_mut() {
                            te.select_all();
                        }
                    }
                }
                return true;
            }
        }
        if let Some(k) = keys::pressed(Scope::Editor, vk) {
            if self.editor_action(k) {
                return true;
            }
        }
        match vkk {
            VK_DELETE | VK_BACK => match self.picked.or(self.hot) {
                Some(idx) => self.delete_item(idx),
                None => return false,
            },
            VK_ESCAPE if self.picked.is_some() => self.picked = None,
            VK_ESCAPE => {
                if self.tool != Tool::None {
                    self.tool = Tool::None;
                } else {
                    self.close();
                }
            }
            VK_RETURN => self.finish(Action::Copy),
            VK_LEFT | VK_RIGHT | VK_UP | VK_DOWN => {
                let (dx, dy) = match vkk {
                    VK_LEFT => (-1, 0),
                    VK_RIGHT => (1, 0),
                    VK_UP => (0, -1),
                    _ => (0, 1),
                };
                if let Some(s) = self.sel.as_mut() {
                    if shift {
                        s.right = (s.right + dx).max(s.left + 2);
                        s.bottom = (s.bottom + dy).max(s.top + 2);
                    } else {
                        s.left += dx;
                        s.right += dx;
                        s.top += dy;
                        s.bottom += dy;
                    }
                } else {
                    let mut p = POINT::default();
                    unsafe {
                        let _ = GetCursorPos(&mut p);
                        let _ = SetCursorPos(p.x + dx, p.y + dy);
                    }
                }
            }
            // the classic second redo key
            _ if ctrl && shift && vk == b'Z' as u16 => self.redo(),
            _ => return false,
        }
        true
    }

    /// A rebindable editor shortcut (see keys.rs). False when it does nothing here.
    fn editor_action(&mut self, k: K) -> bool {
        let tool = match k {
            K::Rect => Tool::Rect,
            K::Ellipse => Tool::Ellipse,
            K::Arrow => Tool::Arrow,
            K::Line => Tool::Line,
            K::Pen => Tool::Pen,
            K::Marker => Tool::Marker,
            K::Text => Tool::Text,
            K::Counter => Tool::Counter,
            K::Mosaic => Tool::Mosaic,
            K::Blur => Tool::Blur,
            _ => Tool::None,
        };
        if tool != Tool::None {
            if self.sel.is_none() {
                return false;
            }
            self.set_tool(tool);
            return true;
        }
        match k {
            K::Copy => self.finish(Action::Copy),
            K::CopyFile => self.finish(Action::CopyFile),
            K::Save => self.finish(Action::Save),
            K::PinIt => self.finish(Action::Pin),
            K::Ocr => self.finish(Action::Ocr),
            K::RecordArea => self.finish(Action::Record),
            K::Undo => self.undo(),
            K::Redo => self.redo(),
            K::SelectMonitor => {
                let m = self.monitor_rect(self.mouse.0, self.mouse.1);
                let full = RECT { left: 0, top: 0, right: self.shot.img.w, bottom: self.shot.img.h };
                self.sel = Some(if self.sel == Some(m) { full } else { m });
                self.hover = None;
            }
            K::PickColor if self.sel.is_none() => self.finish(Action::CopyColor),
            K::Detect => {
                self.deep = !self.deep;
                self.update_hover();
            }
            _ => return false,
        }
        true
    }

    fn char_in(&mut self, c: u16) {
        let Some(te) = self.text.as_mut() else { return };
        match c {
            8 => te.erase(false, false),
            127 => te.erase(false, true), // Ctrl+Backspace
            13 => te.insert(&[b'\n' as u16], false),
            c if c >= 32 => te.insert(&[c], true), // surrogate halves arrive one by one
            _ => {}
        }
    }

    fn wheel(&mut self, delta: i16) {
        if self.tool == Tool::None {
            return;
        }
        let i = tool_idx(self.tool);
        let step = match self.tool {
            Tool::Text => 2.0,
            Tool::Mosaic | Tool::Blur => 4.0,
            Tool::Marker => 2.0,
            _ => 1.0,
        };
        let (lo, hi) = match self.tool {
            Tool::Text => (8.0, 120.0),
            Tool::Counter => (8.0, 48.0),
            Tool::Mosaic | Tool::Blur => (8.0, 160.0),
            Tool::Marker => (4.0, 60.0),
            _ => (1.0, 40.0),
        };
        let d = if delta > 0 { step } else { -step };
        self.sizes[i] = (self.sizes[i] + d).clamp(lo, hi);
        // Apply to what is being drawn right now, not only to the next mouse move.
        let w = self.sizes[i];
        match self.cur.as_mut() {
            Some(Item::Pen { w: pw, .. }) | Some(Item::Fx { shape: FxShape::Stroke { w: pw, .. }, .. }) => *pw = w,
            Some(_) => {
                let (x, y) = self.mouse;
                self.mouse_move(x, y);
            }
            None => {}
        }
        self.size_hint = true;
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_SIZE_HINT, 900, None);
        }
    }

    /// Outline of the current brush at the cursor, so a wheel change is visible at once.
    fn draw_brush_preview(&self, rt: &ID2D1RenderTarget) -> Result<()> {
        let (mx, my) = self.mouse;
        let drawing = matches!(self.drag, Drag::Drawing { .. });
        if self.sel.is_none()
            || self.tool == Tool::None
            || !(drawing || self.drag == Drag::None && self.inside_sel(mx, my))
        {
            return Ok(());
        }
        let size = self.size();
        let (x, y) = (mx as f32 + 0.5, my as f32 + 0.5);
        let r = match self.tool {
            Tool::Counter => Some(size),
            Tool::Mosaic | Tool::Blur
                if unsafe { GetKeyState(VK_SHIFT.0 as i32) >= 0 } && (!drawing || self.cur.is_some()) =>
            {
                Some(size / 2.0)
            }
            Tool::Rect | Tool::Ellipse | Tool::Arrow | Tool::Line | Tool::Pen | Tool::Marker
                if !drawing || self.size_hint =>
            {
                Some((size / 2.0).max(1.0))
            }
            _ => None,
        };
        unsafe {
            if let Some(r) = r {
                if !(drawing && self.tool == Tool::Counter) {
                    let e = D2D1_ELLIPSE { point: pt(x, y), radiusX: r, radiusY: r };
                    let dark = brush(rt, 0xB000_0000)?;
                    let light = brush(rt, 0xF0FF_FFFF)?;
                    rt.DrawEllipse(&e, &dark, 3.0, None);
                    rt.DrawEllipse(&e, &light, 1.0, None);
                }
            }
            if self.size_hint {
                let name = match self.tool {
                    Tool::Text => t("editor.font_size"),
                    Tool::Mosaic | Tool::Blur => t("editor.brush"),
                    Tool::Counter => t("editor.size"),
                    _ => t("editor.width"),
                };
                let off = r.unwrap_or(0.0) + 14.0 * self.scale;
                self.label(rt, &format!("{}: {:.0}", name, size), x + off, y + off, false)?;
            }
        }
        Ok(())
    }

    fn cursor(&self) -> PCWSTR {
        let (x, y) = self.mouse;
        if self.hit_toolbar(x, y).is_some() {
            return IDC_HAND;
        }
        if self.drag == Drag::TextSel || self.drag == Drag::None && self.in_text_box(x, y) {
            return IDC_IBEAM;
        }
        if let Drag::Moving { .. } = self.drag {
            return IDC_SIZEALL;
        }
        let e = match self.drag {
            Drag::Resizing { edges } => edges,
            Drag::None if self.tool == Tool::None || !self.inside_sel(x, y) => self.hit_edges(x, y),
            _ => 0,
        };
        match e {
            1 | 4 => return IDC_SIZEWE,
            2 | 8 => return IDC_SIZENS,
            3 | 12 => return IDC_SIZENWSE,
            6 | 9 => return IDC_SIZENESW,
            _ => {}
        }
        if self.hot.is_some() || matches!(self.drag, Drag::MovingItem { .. }) {
            return IDC_SIZEALL;
        }
        if self.sel.is_some() && self.inside_sel(x, y) {
            return match self.tool {
                Tool::None => IDC_SIZEALL,
                Tool::Text => IDC_IBEAM,
                _ => IDC_CROSS,
            };
        }
        IDC_CROSS
    }
}

use crate::lparam_xy as xy;

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = &*(lp.0 as *const CREATESTRUCTW);
        let p = cs.lpCreateParams as *mut Ov;
        (*p).hwnd = hwnd;
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, p as isize);
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Ov;
    if p.is_null() {
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    let ov = &mut *p;
    if ov.closing && msg != WM_CLOSE && msg != WM_NCDESTROY && msg != WM_DESTROY {
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    // Any real input before Direct2D is up: finish initialisation first (input is never lost).
    if !ov.ready
        && matches!(
            msg,
            WM_LBUTTONDOWN
                | WM_LBUTTONUP
                | WM_LBUTTONDBLCLK
                | WM_RBUTTONUP
                | WM_MOUSEWHEEL
                | WM_KEYDOWN
                | WM_SYSKEYDOWN
                | WM_CHAR
        )
    {
        ov.init_d2d();
    }
    // Mouse events replayed from the veil: the veil holds the capture until release.
    let forwarded =
        matches!(msg, WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_LBUTTONDBLCLK | WM_RBUTTONDOWN | WM_RBUTTONUP)
            && wp.0 & crate::veil::FORWARDED != 0;
    let wp = if forwarded { WPARAM(wp.0 & !crate::veil::FORWARDED) } else { wp };
    ov.forwarded = forwarded;
    let redraw = |ov: &Ov| {
        let _ = InvalidateRect(Some(ov.hwnd), None, false);
    };
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            if ov.ready {
                ov.render();
            } else {
                ov.paint_gdi(dc);
                let _ = PostMessageW(Some(hwnd), WM_APP_INIT, WPARAM(0), LPARAM(0));
            }
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_APP_INIT => {
            ov.init_d2d();
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_SIZE_HINT => {
            let _ = KillTimer(Some(hwnd), TIMER_SIZE_HINT);
            ov.size_hint = false;
            redraw(ov);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_SETCURSOR => {
            if (lp.0 & 0xFFFF) as u32 == HTCLIENT {
                if let Ok(c) = LoadCursorW(None, ov.cursor()) {
                    SetCursor(Some(c));
                }
                return LRESULT(1);
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = xy(lp);
            ov.mouse_down(x, y);
            redraw(ov);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = xy(lp);
            if (x, y) != ov.mouse || ov.drag != Drag::None {
                ov.mouse_move(x, y);
                redraw(ov);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = xy(lp);
            ov.mouse_up(x, y);
            if IsWindow(Some(hwnd)).as_bool() {
                redraw(ov);
            }
            LRESULT(0)
        }
        WM_LBUTTONDBLCLK => {
            let (x, y) = xy(lp);
            let text = if ov.sel.is_some() { ov.text_at(x, y) } else { None };
            if ov.text.is_some() && ov.in_text_box(x, y) {
                ov.text_select_word(x, y);
                ov.drag = Drag::None;
                redraw(ov);
            } else if text.is_some() {
                // the first click left a dot / number on the text: take it back
                if ov.text.is_none() && matches!(ov.tool, Tool::Pen | Tool::Marker | Tool::Counter) {
                    ov.undo();
                }
                ov.edit_text_at(x, y);
                redraw(ov);
            } else if ov.tool == Tool::None && ov.inside_sel(x, y) {
                ov.finish(Action::Copy);
            } else {
                ov.mouse_down(x, y);
                redraw(ov);
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            ov.right_click();
            if IsWindow(Some(hwnd)).as_bool() {
                redraw(ov);
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let d = ((wp.0 >> 16) & 0xFFFF) as i16;
            ov.wheel(d);
            redraw(ov);
            LRESULT(0)
        }
        WM_KEYDOWN | WM_KEYUP if wp.0 as u16 == VK_CONTROL.0 => {
            // Ctrl shows which drawing can be dragged, without waiting for a mouse move
            if ov.update_hot() {
                redraw(ov);
            }
            if let Ok(c) = LoadCursorW(None, ov.cursor()) {
                SetCursor(Some(c));
            }
            LRESULT(0)
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            if ov.key(wp.0 as u16) {
                if IsWindow(Some(hwnd)).as_bool() {
                    redraw(ov);
                }
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_CHAR => {
            ov.char_in(wp.0 as u16);
            redraw(ov);
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_NCDESTROY => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            drop(Box::from_raw(p));
            OPEN.with(|o| *o.borrow_mut() = None);
            destroy_veil();

            if let Some(app) = crate::app_hwnd() {
                let _ = PostMessageW(Some(app), WM_APP_DONE, WPARAM(0), LPARAM(0));
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
