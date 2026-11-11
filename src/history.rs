//! Capture history (off by default): finished captures are kept as PNG files in
//! %LOCALAPPDATA%\WispShot\history, the oldest dropped past a count and size limit.
//! Text found by Windows OCR goes into a `.txt` next to each image, so the history
//! window can search inside the pictures ("the screenshot that said 'invoice'").
//!
//! Nothing runs in the tray process: images are written and indexed by the capture
//! session that made them, and the window is its own short-lived session.

use crate::config::{self, Config};
use crate::gfx::*;
use crate::i18n::{t, tf};
use crate::img::Img;
use crate::output::{self, wide};
use crate::settings::{pal, rrect, title_bar, txt, Pal};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Sent to the session window when a background OCR index finished.
pub const WM_APP_INDEXED: u32 = WM_APP + 41;

pub fn dir() -> PathBuf {
    // tests point this elsewhere so they never touch the user's history
    if let Some(d) = std::env::var_os("WISP_HISTORY_DIR") {
        return PathBuf::from(d);
    }
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("WispShot")
        .join("history")
}

/// History images, oldest first (names are timestamps).
fn files() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir())
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("png")))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn remove(png: &Path) {
    let _ = std::fs::remove_file(png);
    let _ = std::fs::remove_file(png.with_extension("txt"));
}

/// Drop the oldest entries past `keep` images or `max_mb` megabytes.
fn prune(keep: u32, max_mb: u32) {
    let fs = files();
    let sizes: Vec<u64> = fs.iter().map(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)).collect();
    let mut total: u64 = sizes.iter().sum();
    let mut count = fs.len();
    for (p, sz) in fs.iter().zip(&sizes) {
        if count <= keep as usize && total <= max_mb as u64 * 1024 * 1024 {
            break;
        }
        remove(p);
        count -= 1;
        total -= sz;
    }
}

/// Keep a finished capture. Returns where it went (for the OCR index).
pub fn add(img: &Img, cfg: &Config) -> Option<PathBuf> {
    if !cfg.history {
        return None;
    }
    let p = dir().join(output::timestamp_name("WispShot", "png"));
    output::save(img, &p).ok()?;
    prune(cfg.history_keep.max(1), cfg.history_mb.max(1));
    Some(p)
}

/// OCR the image on a worker thread and write the text next to it; posts
/// WM_APP_INDEXED to `notify` when done (success or not).
pub fn index(img: Img, png: PathBuf, notify: HWND) {
    let target = notify.0 as isize;
    std::thread::spawn(move || {
        let mut text = String::new();
        // the OCR engine has a size limit: read a tall (scrolling) capture in slices
        let step = 3000;
        let mut y = 0;
        while y < img.h {
            let h = step.min(img.h - y);
            if let Ok(s) = crate::ocr::recognize(&img.crop(0, y, img.w, h)) {
                if !text.is_empty() && !s.is_empty() {
                    text.push('\n');
                }
                text += s.trim();
            }
            y += h;
        }
        if !text.is_empty() && png.exists() {
            let _ = std::fs::write(png.with_extension("txt"), text);
        }
        unsafe {
            let _ = PostMessageW(Some(HWND(target as *mut _)), WM_APP_INDEXED, WPARAM(0), LPARAM(0));
        }
    });
}

/// Lower-case for searching; folds the Turkish dotted/dotless i so "Fatura" finds "FATURA".
fn fold(s: &str) -> String {
    s.chars()
        .flat_map(|c| match c {
            'İ' | 'I' | 'ı' => vec!['i'],
            c => c.to_lowercase().collect(),
        })
        .filter(|c| *c != '\u{307}')
        .collect()
}

// ================================================================== window

const W: f32 = 860.0;
const H: f32 = 620.0;
const PAD: f32 = 20.0;
const TOP: f32 = 64.0;
const TW: f32 = 184.0;
const TH: f32 = 120.0;
const CELL_H: f32 = TH + 30.0;
const GAP: f32 = 12.0;
const ID_EDIT: usize = 10;
const TIMER_LOAD: usize = 1;
const TIMER_TOAST: usize = 2;
const M_COPY: usize = 1;
const M_COPY_FILE: usize = 2;
const M_PIN: usize = 3;
const M_SHOW: usize = 4;
const M_DELETE: usize = 5;

struct Item {
    path: PathBuf,
    text: String,
    label: String,
    thumb: Option<Option<ID2D1Bitmap>>,
}

#[derive(Clone, Copy, PartialEq)]
enum Hit {
    Item(usize),
    OpenFolder,
    Clear,
    Enable,
}

struct Hs {
    hwnd: HWND,
    g: Gfx,
    rt: Option<ID2D1HwndRenderTarget>,
    dpi: u32,
    light: bool,
    p: Pal,
    edit: HWND,
    font: HFONT,
    edit_bg: HBRUSH,
    enabled: bool,
    items: Vec<Item>,
    /// Indices into `items` that match the search.
    shown: Vec<usize>,
    scroll: f32,
    hover: Option<Hit>,
    hits: Vec<(D2D_RECT_F, Hit)>,
    toast: Option<String>,
    f_body: Option<IDWriteTextFormat>,
    f_sub: Option<IDWriteTextFormat>,
    f_icon: Option<IDWriteTextFormat>,
}

thread_local! {
    static HS: RefCell<Option<Hs>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Hs) -> R) -> Option<R> {
    HS.with(|s| s.try_borrow_mut().ok().and_then(|mut s| s.as_mut().map(f)))
}

/// "WispShot_20261111_175305_520.png" -> localized "11.11.2026 17:53".
fn label(p: &Path) -> String {
    let n = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let d: Vec<&str> = n.split('_').collect();
    match d.as_slice() {
        [_, date, time, ..] if date.len() == 8 && time.len() >= 4 => tf(
            "history.date",
            &[&date[0..4], &date[4..6], &date[6..8], &time[0..2], &time[2..4]],
        ),
        _ => n,
    }
}

fn load_items() -> Vec<Item> {
    files()
        .into_iter()
        .rev()
        .map(|p| Item {
            text: std::fs::read_to_string(p.with_extension("txt")).map(|s| fold(&s)).unwrap_or_default(),
            label: label(&p),
            path: p,
            thumb: None,
        })
        .collect()
}

pub fn open() -> bool {
    unsafe {
        let inst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: inst.into(),
            lpszClassName: w!("WispHistory"),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hIcon: LoadIconW(Some(inst.into()), PCWSTR(1 as _)).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let Ok(g) = Gfx::new() else { return false };
        let cfg = config::load();
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let dpi = crate::monitor_dpi(mon);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut mi);
        let style = WS_OVERLAPPEDWINDOW;
        let k = dpi as f32 / 96.0;
        let mut r = RECT { left: 0, top: 0, right: (W * k) as i32, bottom: (H * k) as i32 };
        let _ = AdjustWindowRectExForDpi(&mut r, style, false, WINDOW_EX_STYLE(0), dpi);
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        let wa = mi.rcWork;
        let x = (wa.left + (wa.right - wa.left - w) / 2).max(wa.left);
        let y = (wa.top + (wa.bottom - wa.top - h) / 2).max(wa.top);
        let title = wide(t("history.window_title"));
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("WispHistory"),
            PCWSTR(title.as_ptr()),
            style | WS_CLIPCHILDREN,
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
        let light = crate::traymenu::light_theme(false);
        let p = pal(light);
        let face = wide("Segoe UI");
        let font = CreateFontW(
            -((14.0 * k) as i32),
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            0,
            PCWSTR(face.as_ptr()),
        );
        let edit = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("EDIT"),
            w!(""),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
            0,
            0,
            10,
            10,
            Some(hwnd),
            Some(HMENU(ID_EDIT as *mut _)),
            Some(inst.into()),
            None,
        )
        .unwrap_or_default();
        SendMessageW(edit, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
        let edit_bg = CreateSolidBrush(COLORREF(bgr(p.card)));
        let items = load_items();
        let shown = (0..items.len()).collect();
        HS.with(|s| {
            *s.borrow_mut() = Some(Hs {
                hwnd,
                f_body: g.text_format(14.0, false).ok(),
                f_sub: g.text_format(12.0, false).ok(),
                f_icon: g.icon_format(16.0).ok(),
                g,
                rt: None,
                dpi,
                light,
                p,
                edit,
                font,
                edit_bg,
                enabled: cfg.history,
                items,
                shown,
                scroll: 0.0,
                hover: None,
                hits: vec![],
                toast: None,
            })
        });
        place_edit();
        title_bar(hwnd, light);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(edit));
    }
    true
}

/// ARGB -> COLORREF (0x00BBGGRR), alpha dropped.
fn bgr(c: u32) -> u32 {
    ((c & 0xFF) << 16) | (c & 0xFF00) | ((c >> 16) & 0xFF)
}

fn k() -> f32 {
    with(|s| s.dpi as f32 / 96.0).unwrap_or(1.0)
}

fn place_edit() {
    with(|s| unsafe {
        let k = s.dpi as f32 / 96.0;
        let show = s.enabled;
        let _ = SetWindowPos(
            s.edit,
            None,
            ((PAD + 34.0) * k) as i32,
            (20.0 * k) as i32,
            (300.0 * k) as i32,
            (24.0 * k) as i32,
            SWP_NOZORDER | if show { SWP_SHOWWINDOW } else { SWP_HIDEWINDOW },
        );
    });
}

fn client_dip(hwnd: HWND) -> (f32, f32) {
    let mut r = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut r);
    }
    let k = k();
    ((r.right - r.left) as f32 / k, (r.bottom - r.top) as f32 / k)
}

fn cols(w: f32) -> usize {
    (((w - 2.0 * PAD + GAP) / (TW + GAP)).floor() as usize).max(1)
}

impl Hs {
    fn filter(&mut self) {
        let mut buf = [0u16; 256];
        let n = unsafe { GetWindowTextW(self.edit, &mut buf) } as usize;
        let q = fold(String::from_utf16_lossy(&buf[..n]).trim());
        // every word of the query, in any order (OCR splits lines and columns its own way)
        let words: Vec<&str> = q.split_whitespace().collect();
        self.shown = (0..self.items.len())
            .filter(|&i| {
                let it = &self.items[i];
                let label = fold(&it.label);
                words.iter().all(|w| it.text.contains(w) || label.contains(w))
            })
            .collect();
        self.scroll = 0.0;
    }

    fn content_h(&self, w: f32) -> f32 {
        let rows = self.shown.len().div_ceil(cols(w));
        TOP + rows as f32 * (CELL_H + GAP) + PAD
    }

    fn scroll_by(&mut self, d: f32) {
        let (w, h) = client_dip(self.hwnd);
        let max = (self.content_h(w) - h).max(0.0);
        self.scroll = (self.scroll + d).clamp(0.0, max);
    }

    fn thumb(&self, path: &Path) -> Option<ID2D1Bitmap> {
        let rt = self.rt.as_ref()?;
        unsafe {
            let f = output::wic().ok()?;
            let pw = wide(&path.to_string_lossy());
            let dec = f
                .CreateDecoderFromFilename(
                    PCWSTR(pw.as_ptr()),
                    None,
                    windows::Win32::Foundation::GENERIC_READ,
                    WICDecodeMetadataCacheOnDemand,
                )
                .ok()?;
            let fr = dec.GetFrame(0).ok()?;
            let (mut w, mut h) = (0, 0);
            fr.GetSize(&mut w, &mut h).ok()?;
            let k = self.dpi as f32 / 96.0;
            let z = ((TW * k) / w as f32).min((TH * k) / h as f32).min(1.0);
            let (tw, th) = (((w as f32 * z) as u32).max(1), ((h as f32 * z) as u32).max(1));
            let sc = f.CreateBitmapScaler().ok()?;
            sc.Initialize(&fr, tw, th, WICBitmapInterpolationModeFant).ok()?;
            let conv = f.CreateFormatConverter().ok()?;
            conv.Initialize(&sc, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)
                .ok()?;
            rt.CreateBitmapFromWicBitmap(&conv, None).ok()
        }
    }

    fn ensure_rt(&mut self) -> Option<()> {
        if self.rt.is_some() {
            return Some(());
        }
        unsafe {
            let mut r = RECT::default();
            let _ = GetClientRect(self.hwnd, &mut r);
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                dpiX: self.dpi as f32,
                dpiY: self.dpi as f32,
                ..Default::default()
            };
            let hp = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd: self.hwnd,
                pixelSize: D2D_SIZE_U { width: r.right as u32, height: r.bottom as u32 },
                presentOptions: D2D1_PRESENT_OPTIONS_NONE,
            };
            self.rt = self.g.d2d.CreateHwndRenderTarget(&props, &hp).ok();
            // thumbnails belong to the old target
            for it in &mut self.items {
                it.thumb = None;
            }
        }
        self.rt.as_ref().map(|_| ())
    }

    fn render(&mut self) {
        if self.ensure_rt().is_none() {
            return;
        }
        let (w, h) = client_dip(self.hwnd);
        // load a few missing thumbnails per frame, keep going on a timer
        let c = cols(w);
        let first_row = ((self.scroll - TOP).max(0.0) / (CELL_H + GAP)) as usize;
        let rows_vis = (h / (CELL_H + GAP)) as usize + 2;
        let mut loaded = 0;
        let mut more = false;
        for &i in self.shown.iter().skip(first_row * c).take(rows_vis * c) {
            if self.items[i].thumb.is_none() {
                if loaded >= 6 {
                    more = true;
                    break;
                }
                let b = self.thumb(&self.items[i].path.clone());
                self.items[i].thumb = Some(b);
                loaded += 1;
            }
        }
        if more {
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_LOAD, 1, None);
            }
        }
        let Some(rt) = self.rt.clone() else { return };
        let rt: &ID2D1RenderTarget = &rt;
        let p = &self.p;
        self.hits.clear();
        unsafe {
            rt.BeginDraw();
            rt.Clear(Some(&color(p.bg)));
            let (Some(fb), Some(fs), Some(fi)) = (self.f_body.clone(), self.f_sub.clone(), self.f_icon.clone()) else {
                let _ = rt.EndDraw(None, None);
                return;
            };
            if !self.enabled {
                let msg = t("history.off");
                let _ = fb.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
                txt(rt, msg, &fb, rc(PAD, h / 2.0 - 60.0, w - PAD, h / 2.0 - 10.0), p.sub);
                let _ = fb.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING);
                let label = t("history.enable");
                let bw = self.g.measure(label, &fb).0 + 32.0;
                let r = rc(w / 2.0 - bw / 2.0, h / 2.0, w / 2.0 + bw / 2.0, h / 2.0 + 34.0);
                rrect(rt, r, 5.0, p.accent, 0);
                let tw = self.g.measure(label, &fb).0;
                txt(rt, label, &fb, rc(r.left + (bw - tw) / 2.0, r.top + 7.0, r.right, r.bottom), p.on_accent);
                self.hits.push((r, Hit::Enable));
                let _ = rt.EndDraw(None, None);
                return;
            }

            // grid
            for (n, &i) in self.shown.iter().enumerate() {
                let (row, col) = (n / c, n % c);
                let x = PAD + col as f32 * (TW + GAP);
                let y = TOP + row as f32 * (CELL_H + GAP) - self.scroll;
                if y + CELL_H < TOP - 8.0 || y > h {
                    continue;
                }
                let cell = rc(x, y, x + TW, y + CELL_H);
                let hov = self.hover == Some(Hit::Item(i));
                rrect(rt, cell, 6.0, if hov { p.btn_hover } else { p.card }, p.card_border);
                if let Some(Some(b)) = &self.items[i].thumb {
                    let sz = b.GetSize();
                    let (bx, by) = (x + (TW - sz.width) / 2.0, y + 6.0 + (TH - 12.0 - sz.height).max(0.0) / 2.0);
                    let dst = rc(bx, by, bx + sz.width.min(TW), by + sz.height.min(TH - 12.0));
                    rt.DrawBitmap(b, Some(&dst), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
                }
                txt(rt, &self.items[i].label, &fs, rc(x + 8.0, y + TH + 4.0, x + TW - 8.0, y + CELL_H), p.sub);
                self.hits.push((cell, Hit::Item(i)));
            }
            if self.shown.is_empty() {
                let msg = if self.items.is_empty() { t("history.empty") } else { t("history.no_match") };
                let _ = fb.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
                txt(rt, msg, &fb, rc(PAD, h / 2.0 - 20.0, w - PAD, h / 2.0 + 20.0), p.sub);
                let _ = fb.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING);
            }

            // top bar over the scrolled grid
            rt.FillRectangle(&rc(0.0, 0.0, w, TOP - 6.0), &brush(rt, p.bg).unwrap());
            rrect(rt, rc(PAD, 16.0, PAD + 342.0, 48.0), 5.0, p.card, p.btn_border);
            txt(rt, "\u{E721}", &fi, rc(PAD + 2.0, 16.0, PAD + 34.0, 48.0), p.sub);
            let count = tf("history.count", &[&self.shown.len().to_string()]);
            txt(rt, &count, &fs, rc(PAD + 356.0, 24.0, PAD + 520.0, 48.0), p.sub);
            let mut bx = w - PAD;
            for (label, hit) in [(t("history.clear"), Hit::Clear), (t("history.open_folder"), Hit::OpenFolder)] {
                let bw = self.g.measure(label, &fb).0 + 28.0;
                let r = rc(bx - bw, 16.0, bx, 48.0);
                let hov = self.hover == Some(hit);
                rrect(rt, r, 5.0, if hov { p.btn_hover } else { p.btn }, p.btn_border);
                let col = if hit == Hit::Clear { p.danger } else { p.text };
                txt(rt, label, &fb, rc(r.left + 14.0, r.top + 6.0, r.right, r.bottom), col);
                self.hits.push((r, hit));
                bx -= bw + 8.0;
            }
            if let Some(m) = &self.toast {
                let tw = self.g.measure(m, &fb).0 + 32.0;
                let r = rc(w / 2.0 - tw / 2.0, h - 56.0, w / 2.0 + tw / 2.0, h - 22.0);
                rrect(rt, r, 6.0, p.text, 0);
                txt(rt, m, &fb, rc(r.left + 16.0, r.top + 7.0, r.right, r.bottom), p.bg);
            }
            if rt.EndDraw(None, None).is_err() {
                self.rt = None;
            }
        }
    }

    fn show_toast(&mut self, m: &str) {
        self.toast = Some(m.to_string());
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_TOAST, 1600, None);
        }
    }

    fn delete(&mut self, i: usize) {
        remove(&self.items[i].path);
        self.items.remove(i);
        self.hover = None;
        self.filter_keep_scroll();
    }

    fn filter_keep_scroll(&mut self) {
        let s = self.scroll;
        self.filter();
        self.scroll = s;
        self.scroll_by(0.0);
    }
}

fn hit(x: i32, y: i32) -> Option<Hit> {
    let k = k();
    let (x, y) = (x as f32 / k, y as f32 / k);
    with(|s| {
        s.hits
            .iter()
            .rev()
            .find(|(r, h)| {
                x >= r.left && x <= r.right && y >= r.top && y <= r.bottom && (!matches!(h, Hit::Item(_)) || y >= TOP - 6.0)
            })
            .map(|(_, h)| *h)
    })
    .flatten()
}

fn redraw(hwnd: HWND) {
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

fn path_of(i: usize) -> Option<PathBuf> {
    with(|s| s.items.get(i).map(|it| it.path.clone())).flatten()
}

fn act(hwnd: HWND, i: usize, cmd: usize) {
    let Some(p) = path_of(i) else { return };
    match cmd {
        M_COPY => {
            let ok = output::load(&p).is_some_and(|img| output::copy_image(hwnd, &img));
            with(|s| s.show_toast(if ok { t("history.copied") } else { t("err.clipboard_busy") }));
        }
        M_COPY_FILE => {
            output::copy_file(hwnd, &p);
            with(|s| s.show_toast(t("history.copied_file")));
        }
        M_PIN => {
            // its own process, so this window can close without taking the pin along
            if let Ok(exe) = std::env::current_exe() {
                let _ = std::process::Command::new(exe).args(["--session", "pinpath", "0"]).arg(&p).spawn();
            }
        }
        M_SHOW => crate::open_path(&p, true),
        M_DELETE => {
            with(|s| s.delete(i));
        }
        _ => {}
    }
    redraw(hwnd);
}

fn menu(hwnd: HWND, i: usize) {
    unsafe {
        let Ok(m) = CreatePopupMenu() else { return };
        for (id, key) in [
            (M_COPY, "history.m_copy"),
            (M_COPY_FILE, "history.m_copy_file"),
            (M_PIN, "history.m_pin"),
            (M_SHOW, "history.m_show"),
            (0, ""),
            (M_DELETE, "history.m_delete"),
        ] {
            if id == 0 {
                let _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
            } else {
                let s = wide(t(key));
                let _ = AppendMenuW(m, MF_STRING, id, PCWSTR(s.as_ptr()));
            }
        }
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON, p.x, p.y, None, hwnd, None);
        let _ = DestroyMenu(m);
        if cmd.0 > 0 {
            act(hwnd, i, cmd.0 as usize);
        }
    }
}

fn clear_all(hwnd: HWND) {
    let body = wide(t("history.clear_confirm"));
    let title = wide(t("history.window_title"));
    let r = unsafe { MessageBoxW(Some(hwnd), PCWSTR(body.as_ptr()), PCWSTR(title.as_ptr()), MB_YESNO | MB_ICONWARNING) };
    if r == IDYES {
        for p in files() {
            remove(&p);
        }
        with(|s| {
            s.items.clear();
            s.filter();
        });
        redraw(hwnd);
    }
}

fn enable(hwnd: HWND) {
    let mut cfg = config::load();
    cfg.history = true;
    let _ = config::save(&cfg);
    crate::ipc_send(crate::IPC_RELOAD, "");
    with(|s| s.enabled = true);
    place_edit();
    with(|s| unsafe {
        let _ = SetFocus(Some(s.edit));
    });
    redraw(hwnd);
}

/// Esc inside the search box closes the window too.
pub fn pretranslate(msg: &MSG) -> bool {
    if msg.message == WM_KEYDOWN && msg.wParam.0 as u16 == VK_ESCAPE.0 {
        if let Some(h) = with(|s| (s.edit == msg.hwnd).then_some(s.hwnd)).flatten() {
            unsafe {
                let _ = DestroyWindow(h);
            }
            return true;
        }
    }
    false
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    use crate::lparam_xy as xy;
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let _ = BeginPaint(hwnd, &mut ps);
            with(|s| s.render());
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_SIZE => {
            with(|s| {
                if let Some(rt) = &s.rt {
                    let _ = rt.Resize(&D2D_SIZE_U { width: (lp.0 & 0xFFFF) as u32, height: ((lp.0 >> 16) & 0xFFFF) as u32 });
                }
                s.scroll_by(0.0);
            });
            redraw(hwnd);
            LRESULT(0)
        }
        WM_GETMINMAXINFO => {
            let mm = &mut *(lp.0 as *mut MINMAXINFO);
            let k = GetDpiForWindow(hwnd).max(96) as f32 / 96.0;
            mm.ptMinTrackSize = POINT { x: (560.0 * k) as i32, y: (360.0 * k) as i32 };
            LRESULT(0)
        }
        WM_TIMER => {
            if wp.0 == TIMER_TOAST {
                with(|s| s.toast = None);
            }
            let _ = KillTimer(Some(hwnd), wp.0);
            redraw(hwnd);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = xy(lp);
            let h = hit(x, y);
            if with(|s| std::mem::replace(&mut s.hover, h) != h) == Some(true) {
                redraw(hwnd);
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
            if with(|s| s.hover.take().is_some()) == Some(true) {
                redraw(hwnd);
            }
            LRESULT(0)
        }
        WM_SETCURSOR if (lp.0 & 0xFFFF) as u32 == HTCLIENT => {
            let mut p = POINT::default();
            let _ = GetCursorPos(&mut p);
            let _ = ScreenToClient(hwnd, &mut p);
            let c = if hit(p.x, p.y).is_some() { IDC_HAND } else { IDC_ARROW };
            if let Ok(c) = LoadCursorW(None, c) {
                SetCursor(Some(c));
            }
            LRESULT(1)
        }
        WM_MOUSEWHEEL => {
            let d = ((wp.0 >> 16) & 0xFFFF) as i16 as f32;
            with(|s| s.scroll_by(-d / 120.0 * 80.0));
            redraw(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = xy(lp);
            match hit(x, y) {
                Some(Hit::Item(i)) => act(hwnd, i, M_COPY),
                Some(Hit::OpenFolder) => crate::open_path(&dir(), false),
                Some(Hit::Clear) => clear_all(hwnd),
                Some(Hit::Enable) => enable(hwnd),
                None => {}
            }
            LRESULT(0)
        }
        WM_LBUTTONDBLCLK => {
            let (x, y) = xy(lp);
            if let Some(Hit::Item(i)) = hit(x, y) {
                act(hwnd, i, M_PIN);
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            let (x, y) = xy(lp);
            if let Some(Hit::Item(i)) = hit(x, y) {
                menu(hwnd, i);
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            match VIRTUAL_KEY(wp.0 as u16) {
                VK_ESCAPE => {
                    let _ = DestroyWindow(hwnd);
                }
                VK_DELETE => {
                    if let Some(Some(Hit::Item(i))) = with(|s| s.hover) {
                        act(hwnd, i, M_DELETE);
                    }
                }
                VK_F if GetKeyState(VK_CONTROL.0 as i32) < 0 => {
                    with(|s| {
                        let _ = SetFocus(Some(s.edit));
                    });
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_COMMAND if (wp.0 & 0xFFFF) == ID_EDIT && ((wp.0 >> 16) & 0xFFFF) as u32 == EN_CHANGE => {
            with(|s| s.filter());
            redraw(hwnd);
            LRESULT(0)
        }
        WM_CTLCOLOREDIT => {
            let dc = HDC(wp.0 as *mut _);
            with(|s| {
                SetTextColor(dc, COLORREF(bgr(s.p.text)));
                SetBkColor(dc, COLORREF(bgr(s.p.card)));
                LRESULT(s.edit_bg.0 as isize)
            })
            .unwrap_or(LRESULT(0))
        }
        WM_SETTINGCHANGE => {
            let light = crate::traymenu::light_theme(false);
            with(|s| {
                if s.light != light {
                    s.light = light;
                    s.p = pal(light);
                    let _ = DeleteObject(s.edit_bg.into());
                    s.edit_bg = CreateSolidBrush(COLORREF(bgr(s.p.card)));
                    title_bar(s.hwnd, light);
                }
            });
            redraw(hwnd);
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_DPICHANGED => {
            with(|s| {
                s.dpi = (wp.0 & 0xFFFF) as u32;
                s.rt = None;
            });
            let r = &*(lp.0 as *const RECT);
            let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER);
            place_edit();
            LRESULT(0)
        }
        WM_DESTROY => {
            if let Some(s) = HS.with(|h| h.borrow_mut().take()) {
                let _ = DeleteObject(s.font.into());
                let _ = DeleteObject(s.edit_bg.into());
            }
            crate::session::poke();
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn fold_turkish() {
        assert_eq!(super::fold("FATURA İstanbul IŞIK"), "fatura istanbul işik");
        assert!(super::fold("Fatura").contains(&super::fold("fATu")));
    }
}
