//! Settings window (its own short-lived `--session settings` process), drawn with
//! Direct2D in a Fluent style: navigation on the left, cards on the right.
//! Every change is applied at once: written to wispshot.ini and the tray reloads it.

use crate::config::{self, Config, Theme};
use crate::gfx::*;
use crate::i18n::{self, t, tf};
use crate::img::Img;
use crate::keys::{self, Chord, Scope, DEFS, K};
use crate::output::{self, WideZ};
use std::cell::RefCell;
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const W: f32 = 820.0;
const H: f32 = 600.0;
const NAV_W: f32 = 232.0;
/// Page content scrolls below the fixed page title.
const VIEW_TOP: f32 = 76.0;
/// The Shortcuts page also keeps its row of section buttons fixed under the title.
const JUMP_H: f32 = 40.0;
/// Sections of the Shortcuts page, for the jump bar (i18n keys).
const JUMPS: [&str; 5] = ["keys.jump_apps", "keys.jump_global", "keys.jump_editor", "keys.jump_draw", "keys.jump_pin"];
const TIMER_TOAST: usize = 1;
/// The update check (its own thread) has a result in UPD_RESULT.
const WM_UPDATE_DONE: u32 = WM_APP + 7;
static UPD_RESULT: std::sync::Mutex<Option<std::result::Result<String, String>>> = std::sync::Mutex::new(None);

/// What "Check now" found; shown on the Updates card itself, not only as a notice.
#[derive(Clone, PartialEq)]
enum Upd {
    Idle,
    Checking,
    Latest,
    New(String),
    Failed(String),
}
const KBD_KEY: PCWSTR = w!("Control Panel\\Keyboard");
const SNIP_VALUE: PCWSTR = w!("PrintScreenKeyForSnippingEnabled");
const VK_PRINT: u16 = 0x2C;

/// Windows opens its screen capture on Print Screen (on when the value is missing, as on Windows 11).
fn snipping_on() -> bool {
    crate::traymenu::reg_dword(KBD_KEY, SNIP_VALUE).unwrap_or(1) != 0
}

fn set_snipping(on: bool) -> bool {
    let v: u32 = on as u32;
    unsafe {
        windows::Win32::System::Registry::RegSetKeyValueW(
            windows::Win32::System::Registry::HKEY_CURRENT_USER,
            KBD_KEY,
            SNIP_VALUE,
            windows::Win32::System::Registry::REG_DWORD.0,
            Some(&v as *const u32 as *const _),
            4,
        )
        .is_ok()
    }
}

/// The shortcuts the tray holds (and a program from the list can keep for itself).
const TRAY_KEYS: [K; 4] = [K::Capture, K::CaptureDelayed, K::Record, K::PinClipboard];

/// A Direct2D bitmap of a straight-alpha icon.
fn icon_bitmap(rt: &ID2D1RenderTarget, img: &Img) -> Option<ID2D1Bitmap> {
    let px: Vec<u32> = img
        .px
        .iter()
        .map(|&p| {
            let a = p >> 24;
            let m = |sh: u32| (((p >> sh) & 0xFF) * a / 255) << sh;
            (a << 24) | m(16) | m(8) | m(0)
        })
        .collect();
    unsafe {
        rt.CreateBitmap(
            D2D_SIZE_U { width: img.w as u32, height: img.h as u32 },
            Some(px.as_ptr() as *const _),
            (img.w * 4) as u32,
            &D2D1_BITMAP_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
            },
        )
        .ok()
    }
}

/// Some system-wide shortcut is Print Screen on its own (the Windows setting is only about that key).
fn uses_print(cfg: &Config) -> bool {
    DEFS.iter()
        .filter(|d| d.scope == Scope::Global)
        .any(|d| keys::parse(cfg.key(d.k)).map(|c| c.vk == VK_PRINT && c.mods == 0).unwrap_or(false))
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Tg {
    PauseFs,
    SaveNotify,
    VideoNotify,
    Cursor,
    InkFade,
    Magnifier,
    Detect,
    History,
    Autostart,
    Updates,
    /// Print Screen takes the screenshot (and Windows lets go of it).
    PrintKey,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Sg {
    SaveMode,
    VideoMode,
    Fps,
    Delay,
    HistoryDays,
    ImageFmt,
    VideoFmt,
    Frame,
    Upload,
    Language,
    Theme,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Act {
    Nav(usize),
    Key(K),
    Tog(Tg),
    Seg(Sg, usize),
    Browse(bool),
    OpenDir(bool),
    OpenIni,
    ResetKeys,
    /// Add a program whose own keys win over the shortcuts (`pause_in_apps`).
    AddApp,
    RemoveApp(usize),
    /// A program from the open-programs list.
    PickApp(usize),
    BrowseApp,
    CancelPick,
    /// While program i is in front, shortcut K goes to the program (true) or to WispShot.
    AppKey(usize, K, bool),
    /// Show or hide the shortcut lines of program i.
    OpenApp(usize),
    /// Scroll to a section of the Shortcuts page.
    Jump(usize),
    /// Put a tool / an action on the editor toolbar or take it off (index into BAR_TOOLS / BAR_ACTIONS).
    BarTool(usize),
    BarAction(usize),
    CheckUpdates,
    /// Install the version the check found (a portable copy opens the download page).
    InstallUpdate,
    Sponsor,
}

const PAGES: [(&str, char); 6] = [
    ("page.shortcuts", '\u{E765}'),
    ("page.screenshots", '\u{E722}'),
    ("page.video", '\u{E714}'),
    ("page.capture", '\u{F407}'),
    ("page.general", '\u{E713}'),
    ("page.about", '\u{E946}'),
];
const FPS: [u32; 4] = [15, 24, 30, 60];
const DELAYS: [u32; 3] = [3, 5, 10];
/// History age limits in days (0: never delete by age).
const DAYS: [u32; 5] = [0, 1, 7, 30, 90];
const GROUPS: [(Scope, &str); 4] = [
    (Scope::Global, "keys.group_global"),
    (Scope::Editor, "keys.group_editor"),
    (Scope::Ink, "keys.group_draw"),
    (Scope::Pin, "keys.group_pin"),
];
/// Text reserve of a folder card (its subtitle is a path, trimmed from the middle).
const FOLDER_RESERVE: f32 = 210.0;

pub(crate) struct Pal {
    pub bg: u32,
    pub nav: u32,
    pub card: u32,
    pub card_border: u32,
    pub text: u32,
    pub sub: u32,
    pub accent: u32,
    pub on_accent: u32,
    pub hover: u32,
    pub selected: u32,
    pub btn: u32,
    pub btn_hover: u32,
    pub btn_border: u32,
    pub off_border: u32,
    pub danger: u32,
    pub divider: u32,
}

pub(crate) fn pal(light: bool) -> Pal {
    let a = crate::traymenu::accent();
    if light {
        Pal {
            bg: 0xFFF3_F3F3,
            nav: 0xFFEB_EBEB,
            card: 0xFFFF_FFFF,
            card_border: 0x14000000,
            text: 0xE400_0000,
            sub: 0x9E00_0000,
            accent: 0xFF00_0000 | a,
            on_accent: 0xFFFF_FFFF,
            hover: 0x0A00_0000,
            selected: 0x0F00_0000,
            btn: 0xFFFB_FBFB,
            btn_hover: 0xFFF6_F6F6,
            btn_border: 0x2600_0000,
            off_border: 0x7300_0000,
            danger: 0xFFC4_2B1C,
            divider: 0x0F00_0000,
        }
    } else {
        // dark mode uses a lighter shade of the accent, like Windows does
        let l = |c: u32, sh: u32| ((((c >> sh) & 0xFF) as f32 * 0.45 + 255.0 * 0.55) as u32) << sh;
        let al = l(a, 16) | l(a, 8) | l(a, 0);
        Pal {
            bg: 0xFF20_2020,
            nav: 0xFF1A_1A1A,
            card: 0xFF2B_2B2B,
            card_border: 0x1AFF_FFFF,
            text: 0xFFFF_FFFF,
            sub: 0x9EFF_FFFF,
            accent: 0xFF00_0000 | al,
            on_accent: 0xFF00_0000,
            hover: 0x14FF_FFFF,
            selected: 0x1FFF_FFFF,
            btn: 0xFF37_3737,
            btn_hover: 0xFF3D_3D3D,
            btn_border: 0x1FFF_FFFF,
            off_border: 0x9EFF_FFFF,
            danger: 0xFFFF_99A4,
            divider: 0x14FF_FFFF,
        }
    }
}

struct Fonts {
    title: IDWriteTextFormat,
    body: IDWriteTextFormat,
    body_b: IDWriteTextFormat,
    sub: IDWriteTextFormat,
    /// Small text that wraps onto more lines (from the top of its box).
    wrap: IDWriteTextFormat,
    path: IDWriteTextFormat,
    icon: IDWriteTextFormat,
    hero: IDWriteTextFormat,
}

struct St {
    hwnd: HWND,
    g: Gfx,
    rt: Option<ID2D1HwndRenderTarget>,
    dpi: u32,
    light: bool,
    p: Pal,
    f: Option<Fonts>,
    cfg: Config,
    autostart: bool,
    page: usize,
    hover: Option<Act>,
    press: Option<Act>,
    /// Shortcut field waiting for keys.
    recording: Option<K>,
    toast: Option<(String, bool)>,
    hits: Vec<(D2D_RECT_F, Act)>,
    tray_mb: Option<f64>,
    scroll: f32,
    /// Height of the current page's content (for the scroll limit).
    content_h: f32,
    /// Name, icon and path of each `pause_apps` entry (same order).
    app_info: Vec<(String, Option<Img>, String)>,
    /// The open-programs list while "Add program" is open.
    picking: Option<Vec<crate::apps::Prog>>,
    /// The saved program whose shortcut lines are shown (one at a time).
    open_app: Option<usize>,
    /// Where each Shortcuts section starts in the scrolling content (set while drawing).
    marks: std::cell::Cell<[f32; 5]>,
    upd: Upd,
}

thread_local! {
    static ST: RefCell<Option<St>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut St) -> R) -> Option<R> {
    ST.with(|s| s.try_borrow_mut().ok().and_then(|mut s| s.as_mut().map(f)))
}

fn family(g: &Gfx, name: &str) -> bool {
    unsafe {
        let mut coll = None;
        if g.dw.GetSystemFontCollection(&mut coll, false).is_err() {
            return false;
        }
        let Some(c) = coll else { return false };
        let (mut i, mut ok) = (0u32, BOOL(0));
        let w = output::wide(name);
        c.FindFamilyName(PCWSTR(w.as_ptr()), &mut i, &mut ok).is_ok() && ok.as_bool()
    }
}

fn fonts(g: &Gfx) -> Result<Fonts> {
    let text = if family(g, "Segoe UI Variable Text") { "Segoe UI Variable Text" } else { "Segoe UI" };
    let disp = if family(g, "Segoe UI Variable Display") { "Segoe UI Variable Display" } else { "Segoe UI" };
    let mk = |fam: &str, size: f32, bold: bool| -> Result<IDWriteTextFormat> {
        let w = output::wide(fam);
        unsafe {
            let f = g.dw.CreateTextFormat(
                PCWSTR(w.as_ptr()),
                None,
                if bold { DWRITE_FONT_WEIGHT_SEMI_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL },
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                w!(""),
            )?;
            f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            Ok(f)
        }
    };
    let path = mk(text, 12.0, false)?;
    unsafe {
        // long paths keep their end: C:\Users\…\Pictures\WispShot
        let sign = g.dw.CreateEllipsisTrimmingSign(&path)?;
        let t = DWRITE_TRIMMING {
            granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
            delimiter: '\\' as u32,
            delimiterCount: 2,
        };
        path.SetTrimming(&t, &sign)?;
    }
    let icon = g.icon_format(16.0)?;
    let wrap = mk(text, 12.0, false)?;
    unsafe {
        wrap.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
        wrap.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
    }
    Ok(Fonts {
        wrap,
        title: mk(disp, 28.0, true)?,
        body: mk(text, 14.0, false)?,
        body_b: mk(text, 14.0, true)?,
        sub: mk(text, 12.0, false)?,
        path,
        icon,
        hero: mk(disp, 20.0, true)?,
    })
}

pub fn open() -> bool {
    unsafe {
        let inst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: inst.into(),
            lpszClassName: w!("WispSettings"),
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
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let k = dpi as f32 / 96.0;
        let mut r = RECT { left: 0, top: 0, right: (W * k) as i32, bottom: (H * k) as i32 };
        let _ = AdjustWindowRectExForDpi(&mut r, style, false, WINDOW_EX_STYLE(0), dpi);
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        let wa = mi.rcWork;
        let x = (wa.left + (wa.right - wa.left - w) / 2).max(wa.left);
        let y = (wa.top + (wa.bottom - wa.top - h) / 2).max(wa.top);
        let title = output::wide(t("settings.window_title"));
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("WispSettings"),
            PCWSTR(title.as_ptr()),
            style,
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
        let f = fonts(&g).ok();
        ST.with(|s| {
            *s.borrow_mut() = Some(St {
                hwnd,
                g,
                rt: None,
                dpi,
                light,
                p: pal(light),
                f,
                cfg,
                autostart: crate::autostart_enabled(),
                page: 0,
                hover: None,
                press: None,
                recording: None,
                toast: None,
                hits: vec![],
                tray_mb: tray_memory(),
                scroll: 0.0,
                content_h: 0.0,
                app_info: vec![],
                picking: None,
                open_app: None,
                upd: Upd::Idle,
                marks: std::cell::Cell::new([0.0; 5]),
            })
        });
        with(|s| s.refresh_apps());
        title_bar(hwnd, light);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
    true
}

/// Title bar in the same colour as the window (Windows 11; ignored elsewhere).
pub(crate) fn title_bar(hwnd: HWND, light: bool) {
    unsafe {
        let dark: BOOL = (!light).into();
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const _, 4);
        let c: u32 = if light { 0x00F3_F3F3 } else { 0x0020_2020 };
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_CAPTION_COLOR, &c as *const _ as *const _, 4);
    }
}

/// Private memory of the running tray process, in MB.
fn tray_memory() -> Option<f64> {
    use windows::Win32::System::ProcessStatus::*;
    use windows::Win32::System::Threading::*;
    unsafe {
        let h = FindWindowW(w!("WispApp"), None).ok()?;
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        let p = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut c = PROCESS_MEMORY_COUNTERS_EX::default();
        let ok = GetProcessMemoryInfo(
            p,
            &mut c as *mut _ as *mut _,
            std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        )
        .is_ok();
        let _ = CloseHandle(p);
        ok.then(|| c.PrivateUsage as f64 / (1024.0 * 1024.0))
    }
}

// ================================================================== drawing

pub(crate) fn txt(rt: &ID2D1RenderTarget, s: &str, f: &IDWriteTextFormat, r: D2D_RECT_F, c: u32) {
    let w: Vec<u16> = s.encode_utf16().collect();
    unsafe {
        if let Ok(b) = brush(rt, c) {
            rt.DrawText(&w, f, &r, &b, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
        }
    }
}

pub(crate) fn rrect(rt: &ID2D1RenderTarget, r: D2D_RECT_F, rad: f32, fill: u32, stroke: u32) {
    let rr = D2D1_ROUNDED_RECT { rect: r, radiusX: rad, radiusY: rad };
    unsafe {
        if fill >> 24 != 0 {
            if let Ok(b) = brush(rt, fill) {
                rt.FillRoundedRectangle(&rr, &b);
            }
        }
        if stroke >> 24 != 0 {
            let r2 = D2D1_ROUNDED_RECT {
                rect: rc(r.left + 0.5, r.top + 0.5, r.right - 0.5, r.bottom - 0.5),
                radiusX: rad,
                radiusY: rad,
            };
            if let Ok(b) = brush(rt, stroke) {
                rt.DrawRoundedRectangle(&r2, &b, 1.0, None);
            }
        }
    }
}

/// A thin bar in the logo's gradient (sky blue at the top to violet), for the selected page.
fn brand_bar(rt: &ID2D1RenderTarget, r: D2D_RECT_F) {
    use crate::logo::{C0, C1};
    unsafe {
        let stops = [
            D2D1_GRADIENT_STOP { position: 0.0, color: color(0xFF00_0000 | C0) },
            D2D1_GRADIENT_STOP { position: 1.0, color: color(0xFF00_0000 | C1) },
        ];
        let Ok(sc) = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP) else { return };
        let props =
            D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: pt(r.left, r.top), endPoint: pt(r.left, r.bottom) };
        if let Ok(g) = rt.CreateLinearGradientBrush(&props, None, &sc) {
            rt.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: 1.5, radiusY: 1.5 }, &g);
        }
    }
}

fn circle(rt: &ID2D1RenderTarget, x: f32, y: f32, r: f32, c: u32) {
    unsafe {
        if let Ok(b) = brush(rt, c) {
            rt.FillEllipse(&D2D1_ELLIPSE { point: pt(x, y), radiusX: r, radiusY: r }, &b);
        }
    }
}

type Hits = Vec<(D2D_RECT_F, Act)>;

impl St {
    fn toggle_val(&self, t: Tg) -> bool {
        let c = &self.cfg;
        match t {
            Tg::PauseFs => c.pause_fullscreen,
            Tg::SaveNotify => c.save_notify,
            Tg::VideoNotify => c.video_notify,
            Tg::Cursor => c.cursor,
            Tg::InkFade => c.ink_fade,
            Tg::Magnifier => c.magnifier,
            Tg::Detect => c.detect_elements,
            Tg::History => c.history,
            Tg::Autostart => self.autostart,
            Tg::Updates => c.check_updates,
            Tg::PrintKey => keys::parse(c.key(K::Capture)).map(|k| k.vk == VK_PRINT && k.mods == 0).unwrap_or(false),
        }
    }

    fn seg_val(&self, s: Sg) -> Option<usize> {
        match s {
            Sg::SaveMode => Some(!self.cfg.save_ask as usize),
            Sg::VideoMode => Some(!self.cfg.video_ask as usize),
            Sg::Fps => FPS.iter().position(|f| *f == self.cfg.fps),
            Sg::Delay => DELAYS.iter().position(|d| *d == self.cfg.capture_delay),
            Sg::HistoryDays => DAYS.iter().position(|d| *d == self.cfg.history_days),
            Sg::ImageFmt => output::Fmt::ALL.iter().position(|f| *f == self.cfg.image_format),
            Sg::VideoFmt => Some(self.cfg.gif as usize),
            Sg::Frame => crate::frame::Style::ALL.iter().position(|f| *f == self.cfg.frame),
            Sg::Upload => match self.cfg.upload_host {
                crate::upload::Host::Custom => Some(crate::upload::Host::SHOWN.len()),
                h => crate::upload::Host::SHOWN.iter().position(|x| *x == h),
            },
            Sg::Language => {
                if self.cfg.language.eq_ignore_ascii_case("system") {
                    Some(0)
                } else {
                    i18n::LANGS.iter().position(|l| l.0.eq_ignore_ascii_case(&self.cfg.language)).map(|i| i + 1)
                }
            }
            Sg::Theme => Some(self.cfg.theme as usize),
        }
    }

    fn render(&mut self) {
        unsafe {
            if self.rt.is_none() {
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
            }
            let Some(hrt) = self.rt.clone() else { return };
            let rt: &ID2D1RenderTarget = &hrt;
            rt.BeginDraw();
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            let mut hits = vec![];
            self.content_h = self.draw(rt, &mut hits);
            self.hits = hits;
            if rt.EndDraw(None, None).is_err() {
                self.rt = None;
            }
        }
    }

    /// Draws everything; returns the height of the scrolling page content.
    fn draw(&self, rt: &ID2D1RenderTarget, hits: &mut Hits) -> f32 {
        let Some(f) = &self.f else { return 0.0 };
        let p = &self.p;
        unsafe {
            let c = |v: u32| D2D1_COLOR_F {
                r: ((v >> 16) & 0xFF) as f32 / 255.0,
                g: ((v >> 8) & 0xFF) as f32 / 255.0,
                b: (v & 0xFF) as f32 / 255.0,
                a: 1.0,
            };
            rt.Clear(Some(&c(p.bg)));
            if let Ok(b) = brush(rt, p.nav) {
                rt.FillRectangle(&rc(0.0, 0.0, NAV_W, H), &b);
            }
        }
        // ---- navigation
        self.logo(rt, 20.0, 22.0, 28.0);
        txt(rt, "WispShot", &f.body_b, rc(58.0, 18.0, NAV_W, 36.0), p.text);
        txt(rt, t("settings.title"), &f.sub, rc(58.0, 36.0, NAV_W, 52.0), p.sub);
        let mut y = 80.0;
        for (i, (name, icon)) in PAGES.iter().enumerate() {
            let r = rc(8.0, y, NAV_W - 8.0, y + 38.0);
            let a = Act::Nav(i);
            let bg = if self.page == i {
                p.selected
            } else if self.hover == Some(a) {
                p.hover
            } else {
                0
            };
            rrect(rt, r, 5.0, bg, 0);
            if self.page == i {
                brand_bar(rt, rc(8.0, y + 11.0, 11.0, y + 27.0));
            }
            txt(rt, &icon.to_string(), &f.icon, rc(20.0, y, 44.0, y + 38.0), p.text);
            txt(rt, t(name), &f.body, rc(54.0, y, NAV_W - 12.0, y + 38.0), p.text);
            hits.push((r, a));
            y += 42.0;
        }
        txt(rt, &format!("v{}", env!("CARGO_PKG_VERSION")), &f.sub, rc(20.0, H - 36.0, NAV_W, H - 16.0), p.sub);

        // ---- page: fixed title, scrolling body
        let x0 = NAV_W + 32.0;
        let x1 = W - 32.0;
        txt(rt, t(PAGES[self.page].0), &f.title, rc(x0, 18.0, x1, 64.0), p.text);
        if self.page == 0 {
            let w = self.g.measure(t("keys.reset"), &f.body).0 + 32.0;
            self.button(rt, rc(x1 - w, 26.0, x1, 58.0), t("keys.reset"), Act::ResetKeys, hits);
        }
        let vt = self.view_top();
        if self.page == 0 {
            self.jump_bar(rt, x0, hits);
        }
        let view = rc(NAV_W, vt, W, H);
        unsafe { rt.PushAxisAlignedClip(&view, D2D1_ANTIALIAS_MODE_ALIASED) };
        let mut body: Hits = vec![];
        let top = vt + 8.0 - self.scroll;
        let mut marks = [0.0f32; 5];
        let mut y = top;
        let card = |st: &St,
                    y: &mut f32,
                    title: &str,
                    sub: &str,
                    reserve: f32,
                    hits: &mut Hits,
                    ctl: &mut dyn FnMut(&St, D2D_RECT_F, &mut Hits)| {
            // a wide control (many options) goes under the text, so the text keeps the whole width
            let stacked = reserve >= 380.0;
            let reserve = if stacked { 18.0 } else { reserve };
            let h = if stacked {
                116.0
            } else if sub.is_empty() {
                56.0
            } else {
                70.0
            };
            let r = rc(x0, *y, x1, *y + h);
            rrect(rt, r, 6.0, p.card, p.card_border);
            if sub.is_empty() {
                txt(rt, title, &f.body, rc(x0 + 18.0, *y, x1 - reserve, *y + h), p.text);
            } else {
                txt(rt, title, &f.body, rc(x0 + 18.0, *y + 13.0, x1 - reserve, *y + 35.0), p.text);
                let ff = if reserve == FOLDER_RESERVE { &f.path } else { &f.sub };
                txt(rt, sub, ff, rc(x0 + 18.0, *y + 35.0, x1 - reserve, *y + 55.0), p.sub);
            }
            ctl(st, if stacked { rc(r.left, r.top + 64.0, r.right, r.bottom - 6.0) } else { r }, hits);
            *y += h + 4.0;
        };
        match self.page {
            0 => {
                marks[0] = y - top;
                card(
                    self,
                    &mut y,
                    t("keys.pause_fullscreen"),
                    t("keys.pause_fullscreen_sub"),
                    140.0,
                    &mut body,
                    &mut |st, r, h| st.toggle(rt, r, Tg::PauseFs, h),
                );
                card(self, &mut y, t("keys.print"), t("keys.print_sub"), 140.0, &mut body, &mut |st, r, h| {
                    st.toggle(rt, r, Tg::PrintKey, h)
                });
                // programs that keep their own keys: the card, then the open-programs picker, then the list
                let label = if self.picking.is_some() { t("common.cancel") } else { t("keys.apps_add") };
                let bw = self.g.measure(label, &f.body).0 + 32.0;
                card(self, &mut y, t("keys.apps"), t("keys.apps_sub"), bw + 40.0, &mut body, &mut |st, r, h| {
                    let cy = (r.top + r.bottom) / 2.0;
                    let a = if st.picking.is_some() { Act::CancelPick } else { Act::AddApp };
                    st.button(rt, rc(r.right - 18.0 - bw, cy - 16.0, r.right - 18.0, cy + 16.0), label, a, h);
                });
                if let Some(list) = &self.picking {
                    y = self.draw_picker(rt, list, y, &mut body);
                }
                if !self.cfg.pause_apps.is_empty() {
                    y = self.draw_apps(rt, y, &mut body);
                }
                txt(rt, t("keys.how_to"), &f.sub, rc(x0 + 4.0, y + 2.0, x1, y + 26.0), p.sub);
                y += 30.0;
                for (gi, (scope, title)) in GROUPS.iter().enumerate() {
                    let scope = *scope;
                    marks[gi + 1] = y - top;
                    txt(rt, t(title), &f.body_b, rc(x0 + 4.0, y + 6.0, x1, y + 32.0), p.text);
                    y += 36.0;
                    let rows: Vec<&keys::Def> = DEFS.iter().filter(|d| d.scope == scope).collect();
                    let rh = 46.0;
                    rrect(rt, rc(x0, y, x1, y + rh * rows.len() as f32), 6.0, p.card, p.card_border);
                    for (i, d) in rows.iter().enumerate() {
                        let r = rc(x0, y, x1, y + rh);
                        if i > 0 {
                            rrect(rt, rc(x0 + 1.0, y, x1 - 1.0, y + 1.0), 0.0, p.divider, 0);
                        }
                        // only rows on screen cost anything
                        if r.bottom > vt && r.top < H {
                            txt(rt, t(label_key(d.k)), &f.body, rc(x0 + 18.0, y, x1 - 190.0, y + rh), p.text);
                            self.hotkey(rt, r, d.k, &mut body);
                        }
                        y += rh;
                    }
                    y += 8.0;
                }
                txt(rt, t("keys.fixed"), &f.sub, rc(x0 + 4.0, y, x1, y + 22.0), p.sub);
                y += 26.0;
            }
            1 => {
                card(self, &mut y, t("shots.save"), t("shots.save_sub"), 300.0, &mut body, &mut |st, r, h| {
                    st.seg(rt, r, Sg::SaveMode, &[t("common.ask"), t("common.to_folder")], h)
                });
                let d = self.cfg.save_dir.display().to_string();
                card(self, &mut y, t("common.folder"), &d, FOLDER_RESERVE, &mut body, &mut |st, r, h| {
                    st.folder(rt, r, false, h)
                });
                card(self, &mut y, t("shots.notify"), t("shots.notify_sub"), 140.0, &mut body, &mut |st, r, h| {
                    st.toggle(rt, r, Tg::SaveNotify, h)
                });
                let fmts: Vec<&str> = output::Fmt::ALL.iter().map(|f| f.label()).collect();
                card(self, &mut y, t("shots.format"), t("shots.format_sub"), 300.0, &mut body, &mut |st, r, h| {
                    st.seg(rt, r, Sg::ImageFmt, &fmts, h)
                });
                let frames: Vec<(String, String)> = [
                    ("frame.s_off", "frame.d_off"),
                    ("frame.s_color", "frame.d_color"),
                    ("frame.s_light", "frame.d_light"),
                    ("frame.s_dark", "frame.d_dark"),
                ]
                .iter()
                .map(|(a, b)| (t(a).to_string(), t(b).to_string()))
                .collect();
                y = self.radio_list(rt, y, t("shots.frame"), t("shots.frame_sub"), Sg::Frame, &frames, true, &mut body);
                let sub = tf("shots.upload_sub", &[&keys::display(self.cfg.key(K::Upload))]);
                let mut hosts: Vec<(String, String)> = [
                    ("upload.s_uguu", "upload.d_uguu"),
                    ("upload.s_litter24", "upload.d_litter24"),
                    ("upload.s_litter72", "upload.d_litter72"),
                ]
                .iter()
                .map(|(a, b)| (t(a).to_string(), t(b).to_string()))
                .collect();
                if self.cfg.upload_host == crate::upload::Host::Custom {
                    hosts.push((t("upload.s_custom").to_string(), tf("upload.d_custom", &[&self.cfg.upload_url])));
                }
                y = self.radio_list(rt, y, t("shots.upload"), &sub, Sg::Upload, &hosts, false, &mut body);
            }
            2 => {
                card(self, &mut y, t("video.save"), t("video.save_sub"), 300.0, &mut body, &mut |st, r, h| {
                    st.seg(rt, r, Sg::VideoMode, &[t("common.ask"), t("common.to_folder")], h)
                });
                let d = self.cfg.video_dir.display().to_string();
                card(self, &mut y, t("common.folder"), &d, FOLDER_RESERVE, &mut body, &mut |st, r, h| {
                    st.folder(rt, r, true, h)
                });
                card(self, &mut y, t("video.notify"), "", 140.0, &mut body, &mut |st, r, h| {
                    st.toggle(rt, r, Tg::VideoNotify, h)
                });
                card(self, &mut y, t("video.format"), t("video.format_sub"), 230.0, &mut body, &mut |st, r, h| {
                    st.seg(rt, r, Sg::VideoFmt, &["MP4", "GIF"], h)
                });
                card(self, &mut y, t("video.fps"), t("video.fps_sub"), 230.0, &mut body, &mut |st, r, h| {
                    st.seg(rt, r, Sg::Fps, &["15", "24", "30", "60"], h)
                });
                card(self, &mut y, t("video.cursor"), "", 140.0, &mut body, &mut |st, r, h| {
                    st.toggle(rt, r, Tg::Cursor, h)
                });
                card(self, &mut y, t("video.fade"), t("video.fade_sub"), 140.0, &mut body, &mut |st, r, h| {
                    st.toggle(rt, r, Tg::InkFade, h)
                });
            }
            3 => {
                card(
                    self,
                    &mut y,
                    t("capture.magnifier"),
                    t("capture.magnifier_sub"),
                    140.0,
                    &mut body,
                    &mut |st, r, h| st.toggle(rt, r, Tg::Magnifier, h),
                );
                let sub = tf("capture.detect_sub", &[&keys::display(self.cfg.key(K::Detect))]);
                card(self, &mut y, t("capture.detect"), &sub, 140.0, &mut body, &mut |st, r, h| {
                    st.toggle(rt, r, Tg::Detect, h)
                });
                let secs: Vec<String> = DELAYS.iter().map(|d| tf("capture.delay_secs", &[&d.to_string()])).collect();
                let secs: Vec<&str> = secs.iter().map(|s| s.as_str()).collect();
                card(self, &mut y, t("capture.delay"), t("capture.delay_sub"), 230.0, &mut body, &mut |st, r, h| {
                    st.seg(rt, r, Sg::Delay, &secs, h)
                });
                let sub = tf("capture.history_sub", &[&self.cfg.history_keep.to_string()]);
                card(self, &mut y, t("capture.history"), &sub, 140.0, &mut body, &mut |st, r, h| {
                    st.toggle(rt, r, Tg::History, h)
                });
                let ages = [
                    t("capture.age_never"),
                    t("capture.age_day"),
                    t("capture.age_week"),
                    t("capture.age_month"),
                    t("capture.age_3months"),
                ];
                card(
                    self,
                    &mut y,
                    t("capture.history_age"),
                    t("capture.history_age_sub"),
                    400.0,
                    &mut body,
                    &mut |st, r, h| st.seg(rt, r, Sg::HistoryDays, &ages, h),
                );
                y = self.draw_bar_choice(rt, y, &mut body);
            }
            4 => {
                let mut langs: Vec<&str> = vec![t("general.system")];
                langs.extend(i18n::LANGS.iter().map(|l| l.1));
                card(self, &mut y, t("general.language"), "", 330.0, &mut body, &mut |st, r, h| {
                    st.seg(rt, r, Sg::Language, &langs, h)
                });
                card(self, &mut y, t("general.theme"), "", 330.0, &mut body, &mut |st, r, h| {
                    st.seg(rt, r, Sg::Theme, &[t("general.system"), t("general.light"), t("general.dark")], h)
                });
                card(
                    self,
                    &mut y,
                    t("general.autostart"),
                    t("general.autostart_sub"),
                    140.0,
                    &mut body,
                    &mut |st, r, h| st.toggle(rt, r, Tg::Autostart, h),
                );
                card(
                    self,
                    &mut y,
                    t("general.updates"),
                    t("general.updates_sub"),
                    140.0,
                    &mut body,
                    &mut |st, r, h| st.toggle(rt, r, Tg::Updates, h),
                );
                card(self, &mut y, t("general.ini"), t("general.ini_sub"), 130.0, &mut body, &mut |st, r, h| {
                    let b = rc(
                        r.right - 18.0 - 90.0,
                        (r.top + r.bottom) / 2.0 - 16.0,
                        r.right - 18.0,
                        (r.top + r.bottom) / 2.0 + 16.0,
                    );
                    st.button(rt, b, t("common.open"), Act::OpenIni, h);
                });
            }
            _ => {
                rrect(rt, rc(x0, y, x1, y + 112.0), 6.0, p.card, p.card_border);
                self.logo(rt, x0 + 24.0, y + 26.0, 60.0);
                txt(rt, "WispShot", &f.hero, rc(x0 + 104.0, y + 22.0, x1, y + 50.0), p.text);
                txt(
                    rt,
                    &tf("about.version", &[env!("CARGO_PKG_VERSION")]),
                    &f.sub,
                    rc(x0 + 104.0, y + 50.0, x1, y + 68.0),
                    p.sub,
                );
                txt(rt, t("about.tagline"), &f.body, rc(x0 + 104.0, y + 70.0, x1 - 16.0, y + 92.0), p.text);
                y += 116.0;
                let mem = match self.tray_mb {
                    Some(m) => tf("about.memory_sub", &[&format!("{:.1}", m)]),
                    None => t("about.memory_off").to_string(),
                };
                card(self, &mut y, t("about.memory"), &mem, 20.0, &mut body, &mut |_, _, _| {});
                let (sub, label, act) = match &self.upd {
                    Upd::Idle => (t("about.updates_sub").to_string(), t("about.check_updates"), Act::CheckUpdates),
                    Upd::Checking => (t("update.checking").to_string(), t("about.check_updates"), Act::CheckUpdates),
                    Upd::Latest => (
                        tf("update.up_to_date", &[env!("CARGO_PKG_VERSION")]),
                        t("about.check_updates"),
                        Act::CheckUpdates,
                    ),
                    Upd::New(v) => {
                        let label = if crate::update::installed() { t("about.install") } else { t("about.download") };
                        (tf("about.new_version", &[v]), label, Act::InstallUpdate)
                    }
                    Upd::Failed(e) => (tf("about.check_failed", &[e]), t("about.try_again"), Act::CheckUpdates),
                };
                let bw = self.g.measure(label, &f.body).0 + 32.0;
                card(self, &mut y, t("about.updates"), &sub, bw + 40.0, &mut body, &mut |st, r, h| {
                    let cy = (r.top + r.bottom) / 2.0;
                    st.button(rt, rc(r.right - 18.0 - bw, cy - 16.0, r.right - 18.0, cy + 16.0), label, act, h);
                });
                card(self, &mut y, t("about.license"), t("about.license_sub"), 20.0, &mut body, &mut |_, _, _| {});
                card(self, &mut y, t("about.author"), "Efe Kaputlu", 20.0, &mut body, &mut |_, _, _| {});
                card(self, &mut y, t("about.support"), t("about.support_sub"), 150.0, &mut body, &mut |st, r, h| {
                    let label = t("about.sponsor");
                    let bw = st.g.measure(label, &f.body).0 + 32.0;
                    let b = rc(
                        r.right - 18.0 - bw,
                        (r.top + r.bottom) / 2.0 - 16.0,
                        r.right - 18.0,
                        (r.top + r.bottom) / 2.0 + 16.0,
                    );
                    st.button(rt, b, label, Act::Sponsor, h);
                });
            }
        }
        let content_h = y - top + 8.0;
        unsafe { rt.PopAxisAlignedClip() };
        // only what is visible can be clicked
        for (r, a) in body {
            if r.bottom > vt && r.top < H {
                hits.push((rc(r.left, r.top.max(vt), r.right, r.bottom.min(H)), a));
            }
        }
        // thin scroll indicator
        self.marks.set(marks);
        let view_h = H - vt;
        if content_h > view_h {
            let bar_h = (view_h * view_h / content_h).max(32.0);
            let pos = vt + (view_h - bar_h) * (self.scroll / (content_h - view_h)).clamp(0.0, 1.0);
            rrect(rt, rc(W - 8.0, pos + 4.0, W - 4.0, pos + bar_h - 4.0), 2.0, p.sub, 0);
        }
        // a small card like the others: a mark in the accent (or red) circle, then the message
        if let Some((msg, bad)) = &self.toast {
            let (tw, _) = self.g.measure(msg, &f.body);
            let r = rc(x1 - tw - 64.0, H - 66.0, x1, H - 24.0);
            rrect(rt, rc(r.left - 1.0, r.top + 2.0, r.right + 1.0, r.bottom + 5.0), 9.0, 0x22000000, 0);
            rrect(rt, r, 8.0, p.card, p.card_border);
            let (cx, cy) = (r.left + 24.0, (r.top + r.bottom) / 2.0);
            circle(rt, cx, cy, 10.0, if *bad { p.danger } else { p.accent });
            let ic = if *bad { '\u{E711}' } else { '\u{E73E}' };
            if let Ok(small) = self.g.icon_format(11.0) {
                unsafe {
                    let _ = small.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
                }
                txt(rt, &ic.to_string(), &small, rc(cx - 10.0, cy - 10.0, cx + 10.0, cy + 10.0), p.on_accent);
            }
            txt(rt, msg, &f.body, rc(r.left + 44.0, r.top, r.right - 10.0, r.bottom), p.text);
        }
        content_h
    }

    /// The WispShot mark (logo.rs) with its rounded square filling x..x+s.
    fn logo(&self, rt: &ID2D1RenderTarget, x: f32, y: f32, s: f32) {
        use crate::logo::*;
        let k = s / (BOX.1 - BOX.0);
        let m = |p: (f32, f32)| pt(x + (p.0 - BOX.0) * k, y + (p.1 - BOX.0) * k);
        let col = |c: u32| D2D1_COLOR_F {
            r: ((c >> 16) & 0xFF) as f32 / 255.0,
            g: ((c >> 8) & 0xFF) as f32 / 255.0,
            b: (c & 0xFF) as f32 / 255.0,
            a: 1.0,
        };
        unsafe {
            let stops = [
                D2D1_GRADIENT_STOP { position: 0.0, color: col(C0) },
                D2D1_GRADIENT_STOP { position: 1.0, color: col(C1) },
            ];
            let Ok(sc) = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP) else {
                return;
            };
            let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: m((6.0, 58.0)), endPoint: m((58.0, 6.0)) };
            let Ok(grad) = rt.CreateLinearGradientBrush(&props, None, &sc) else { return };
            let rr = D2D1_ROUNDED_RECT { rect: rc(x, y, x + s, y + s), radiusX: BOX.2 * k, radiusY: BOX.2 * k };
            rt.FillRoundedRectangle(&rr, &grad);
            let Ok(white) = brush(rt, 0xFFFF_FFFF) else { return };
            if let Ok(geo) = self.g.d2d.CreatePathGeometry() {
                if let Ok(sink) = geo.Open() {
                    sink.BeginFigure(m(WAVE_START), D2D1_FIGURE_BEGIN_HOLLOW);
                    for [c1, c2, p3] in WAVE {
                        sink.AddBezier(&D2D1_BEZIER_SEGMENT { point1: m(c1), point2: m(c2), point3: m(p3) });
                    }
                    sink.EndFigure(D2D1_FIGURE_END_OPEN);
                    if sink.Close().is_ok() {
                        rt.DrawGeometry(&geo, &white, WAVE_WIDTH * k, &self.g.round);
                    }
                }
            }
            rt.FillEllipse(
                &D2D1_ELLIPSE { point: m((SPARK.0, SPARK.1)), radiusX: SPARK.2 * k, radiusY: SPARK.2 * k },
                &white,
            );
        }
    }

    /// A program icon (straight alpha) at x, y, `s` px; a plain tile when there is none.
    fn app_icon(&self, rt: &ID2D1RenderTarget, icon: Option<&Img>, x: f32, y: f32, s: f32) {
        let r = rc(x, y, x + s, y + s);
        match icon.and_then(|i| icon_bitmap(rt, i)) {
            Some(b) => unsafe {
                rt.DrawBitmap(&b, Some(&r), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
            },
            None => {
                rrect(rt, r, 4.0, self.p.btn, self.p.btn_border);
                if let Some(f) = &self.f {
                    txt(rt, "\u{ED35}", &f.icon, rc(x + 2.0, y, x + s, y + s), self.p.sub);
                }
            }
        }
    }

    /// Which tools and actions sit on the editor toolbar: two rows of chips, lit = on the bar.
    fn draw_bar_choice(&self, rt: &ID2D1RenderTarget, y: f32, hits: &mut Hits) -> f32 {
        let Some(f) = &self.f else { return y };
        let p = &self.p;
        let (x0, x1) = (NAV_W + 32.0, W - 32.0);
        // lay the chips out first to know the card's height
        let place = |names: &[(&'static str, &'static str)], start: f32| -> (Vec<(D2D_RECT_F, String)>, f32) {
            let (mut x, mut cy) = (x0 + 18.0, start);
            let mut out = vec![];
            for (_, key) in names {
                let label = t(key).split(" (").next().unwrap_or("").to_string();
                let w = self.g.measure(&label, &f.sub).0 + 22.0;
                if x + w > x1 - 18.0 {
                    x = x0 + 18.0;
                    cy += 32.0;
                }
                out.push((rc(x, cy, x + w, cy + 26.0), label));
                x += w + 6.0;
            }
            (out, cy + 26.0)
        };
        let two = self.g.measure(t("capture.bar_sub"), &f.sub).0 > x1 - x0 - 36.0;
        let head = if two { 84.0 } else { 64.0 };
        let (tools, end_t) = place(&config::BAR_TOOLS, y + head + 22.0);
        let (acts, end_a) = place(&config::BAR_ACTIONS, end_t + 34.0);
        let h = end_a - y + 16.0;
        rrect(rt, rc(x0, y, x1, y + h), 6.0, p.card, p.card_border);
        txt(rt, t("capture.bar"), &f.body, rc(x0 + 18.0, y + 13.0, x1 - 18.0, y + 35.0), p.text);
        txt(
            rt,
            t("capture.bar_sub"),
            if two { &f.wrap } else { &f.sub },
            rc(x0 + 18.0, y + 37.0, x1 - 18.0, y + head - 4.0),
            p.sub,
        );
        let groups = [
            (t("capture.bar_tools"), tools, &self.cfg.bar_tools, &config::BAR_TOOLS[..], y + head),
            (t("capture.bar_actions"), acts, &self.cfg.bar_actions, &config::BAR_ACTIONS[..], end_t + 12.0),
        ];
        for (gi, (title, chips, on, table, ty)) in groups.into_iter().enumerate() {
            txt(rt, title, &f.sub, rc(x0 + 18.0, ty, x1 - 18.0, ty + 20.0), p.sub);
            for (i, (r, label)) in chips.iter().enumerate() {
                let lit = on.iter().any(|n| n == table[i].0);
                let a = if gi == 0 { Act::BarTool(i) } else { Act::BarAction(i) };
                if lit {
                    rrect(rt, *r, 13.0, p.accent, 0);
                    txt(rt, label, &f.sub, rc(r.left + 11.0, r.top, r.right, r.bottom), p.on_accent);
                } else {
                    let bg = if self.hover == Some(a) { p.btn_hover } else { p.btn };
                    rrect(rt, *r, 13.0, bg, p.btn_border);
                    txt(rt, label, &f.sub, rc(r.left + 11.0, r.top, r.right, r.bottom), p.text);
                }
                hits.push((*r, a));
            }
        }
        y + h + 4.0
    }

    /// Top of the scrolling area on the current page.
    fn view_top(&self) -> f32 {
        if self.page == 0 {
            VIEW_TOP + JUMP_H
        } else {
            VIEW_TOP
        }
    }

    /// The row of section buttons under the Shortcuts title; the section in view is lit.
    fn jump_bar(&self, rt: &ID2D1RenderTarget, x0: f32, hits: &mut Hits) {
        let Some(f) = &self.f else { return };
        let p = &self.p;
        let marks = self.marks.get();
        let here = (0..5).rev().find(|&i| marks[i] <= self.scroll + 24.0).unwrap_or(0);
        let (y0, y1) = (VIEW_TOP + 2.0, VIEW_TOP + 30.0);
        let mut x = x0;
        for (i, key) in JUMPS.iter().enumerate() {
            let label = t(key);
            let w = self.g.measure(label, &f.sub).0 + 26.0;
            let r = rc(x, y0, x + w, y1);
            let a = Act::Jump(i);
            if i == here {
                rrect(rt, r, 14.0, p.accent, 0);
                txt(rt, label, &f.sub, rc(r.left + 13.0, r.top, r.right, r.bottom), p.on_accent);
            } else {
                rrect(rt, r, 14.0, if self.hover == Some(a) { p.btn_hover } else { p.btn }, p.btn_border);
                txt(rt, label, &f.sub, rc(r.left + 13.0, r.top, r.right, r.bottom), p.text);
            }
            hits.push((r, a));
            x += w + 6.0;
        }
    }

    /// The open-programs list under "Add program"; returns the new y.
    fn draw_picker(&self, rt: &ID2D1RenderTarget, list: &[crate::apps::Prog], y: f32, hits: &mut Hits) -> f32 {
        let Some(f) = &self.f else { return y };
        let p = &self.p;
        let (x0, x1) = (NAV_W + 32.0, W - 32.0);
        let rh = 44.0;
        let foot = 52.0;
        let head = 34.0;
        let h = head + rh * list.len() as f32 + foot;
        rrect(rt, rc(x0, y, x1, y + h), 6.0, p.card, p.accent);
        let title = if list.is_empty() { t("keys.apps_none_open") } else { t("keys.apps_open") };
        txt(rt, title, &f.sub, rc(x0 + 18.0, y + 10.0, x1 - 18.0, y + head), p.sub);
        let mut ry = y + head;
        for (i, prog) in list.iter().enumerate() {
            let r = rc(x0 + 6.0, ry, x1 - 6.0, ry + rh);
            let added = self.cfg.pause_apps.iter().any(|a| a.exe == prog.exe);
            let a = Act::PickApp(i);
            if !added && self.hover == Some(a) {
                rrect(rt, r, 4.0, p.hover, 0);
            }
            self.app_icon(rt, prog.icon.as_ref(), x0 + 18.0, ry + 10.0, 24.0);
            let col = if added { p.sub } else { p.text };
            txt(rt, &prog.name, &f.body, rc(x0 + 54.0, ry, x1 - 200.0, ry + rh), col);
            let right = if added { t("keys.apps_on_list").to_string() } else { prog.exe.clone() };
            let tw = self.g.measure(&right, &f.sub).0;
            txt(rt, &right, &f.sub, rc(x1 - 18.0 - tw, ry, x1 - 12.0, ry + rh), p.sub);
            if !added {
                hits.push((r, a));
            }
            ry += rh;
        }
        let label = t("keys.apps_browse");
        let bw = self.g.measure(label, &f.body).0 + 32.0;
        self.button(rt, rc(x0 + 18.0, ry + 10.0, x0 + 18.0 + bw, ry + 42.0), label, Act::BrowseApp, hits);
        y + h + 4.0
    }

    /// The saved programs: icon, name and Remove, then one line per shortcut with a switch:
    /// on, the key goes to the program while it is in front; returns the new y.
    fn draw_apps(&self, rt: &ID2D1RenderTarget, mut y: f32, hits: &mut Hits) -> f32 {
        let Some(f) = &self.f else { return y };
        let p = &self.p;
        let (x0, x1) = (NAV_W + 32.0, W - 32.0);
        let (head, line, pad) = (64.0, 32.0, 8.0);
        let bound: Vec<K> = TRAY_KEYS.iter().copied().filter(|k| !keys::display(self.cfg.key(*k)).is_empty()).collect();
        let apps = &self.cfg.pause_apps;
        let open = |i: usize| self.open_app == Some(i);
        let total: f32 =
            (0..apps.len()).map(|i| head + if open(i) { line * bound.len() as f32 + pad } else { 0.0 }).sum();
        rrect(rt, rc(x0, y, x1, y + total), 6.0, p.card, p.card_border);
        let rw = self.g.measure(t("keys.apps_remove"), &f.body).0 + 28.0;
        let key_w =
            bound.iter().map(|k| self.g.measure(&keys::display(self.cfg.key(*k)), &f.sub).0).fold(0.0, f32::max) + 20.0;
        for (i, a) in apps.iter().enumerate() {
            if i > 0 {
                rrect(rt, rc(x0 + 1.0, y, x1 - 1.0, y + 1.0), 0.0, p.divider, 0);
            }
            let is_open = open(i);
            // the header opens and closes the program's lines
            let hr = rc(x0 + 6.0, y + 4.0, x1 - rw - 30.0, y + head - 4.0);
            {
                let act = Act::OpenApp(i);
                if self.hover == Some(act) {
                    rrect(rt, hr, 4.0, p.hover, 0);
                }
                let chev = if is_open { '\u{E70E}' } else { '\u{E70D}' };
                txt(rt, &chev.to_string(), &f.icon, rc(hr.right - 26.0, y, hr.right, y + head), p.sub);
                hits.push((hr, act));
            }
            let (name, icon) = match self.app_info.get(i) {
                Some((n, ic, _)) => (n.as_str(), ic.as_ref()),
                None => (a.exe.as_str(), None),
            };
            self.app_icon(rt, icon, x0 + 18.0, y + 12.0, 24.0);
            txt(rt, name, &f.body, rc(x0 + 54.0, y + 8.0, hr.right - 30.0, y + 32.0), p.text);
            let b = rc(x1 - 18.0 - rw, y + 8.0, x1 - 18.0, y + 36.0);
            self.button(rt, b, t("keys.apps_remove"), Act::RemoveApp(i), hits);
            let sub = if is_open {
                t("keys.apps_keys").to_string()
            } else {
                let off: Vec<String> =
                    bound.iter().filter(|k| a.pauses(**k)).map(|k| keys::display(self.cfg.key(*k))).collect();
                tf("keys.apps_summary", &[&off.join(", ")])
            };
            txt(rt, &sub, &f.sub, rc(x0 + 54.0, y + 36.0, hr.right - 30.0, y + 58.0), p.sub);
            let mut ly = y + head;
            if is_open {
                // one line per shortcut: what it does, its key, and who gets it: [WispShot | Program]
                let (wisp, prog) = ("WispShot", t("keys.apps_to_program"));
                let seg_w = self.g.measure(wisp, &f.sub).0.max(self.g.measure(prog, &f.sub).0) + 24.0;
                for k in &bound {
                    let k = *k;
                    let to_prog = a.pauses(k);
                    // the row switches; a click on the chosen half changes nothing
                    let act = Act::AppKey(i, k, !to_prog);
                    let r = rc(x0 + 46.0, ly, x1 - 12.0, ly + line);
                    if self.hover == Some(act) {
                        rrect(rt, r, 4.0, p.hover, 0);
                    }
                    let seg = rc(r.right - 6.0 - 2.0 * seg_w, r.top + 4.0, r.right - 6.0, r.bottom - 4.0);
                    rrect(rt, seg, 4.0, p.btn, p.btn_border);
                    let halves = [(wisp, !to_prog), (prog, to_prog)];
                    for (j, (name, chosen)) in halves.iter().enumerate() {
                        let hl = seg.left + seg_w * j as f32;
                        let hr = rc(hl, seg.top, hl + seg_w, seg.bottom);
                        if *chosen {
                            rrect(
                                rt,
                                rc(hr.left + 2.0, hr.top + 2.0, hr.right - 2.0, hr.bottom - 2.0),
                                3.0,
                                p.accent,
                                0,
                            );
                        }
                        let nw = self.g.measure(name, &f.sub).0;
                        let col = if *chosen { p.on_accent } else { p.sub };
                        txt(rt, name, &f.sub, rc((hr.left + hr.right - nw) / 2.0, hr.top, hr.right, hr.bottom), col);
                    }
                    let label = keys::display(self.cfg.key(k));
                    let cap = rc(seg.left - 14.0 - key_w, r.top + 4.0, seg.left - 14.0, r.bottom - 4.0);
                    rrect(rt, cap, 4.0, p.btn, p.btn_border);
                    let lw = self.g.measure(&label, &f.sub).0;
                    txt(
                        rt,
                        &label,
                        &f.sub,
                        rc((cap.left + cap.right - lw) / 2.0, cap.top, cap.right, cap.bottom),
                        p.text,
                    );
                    txt(rt, t(label_key(k)), &f.sub, rc(r.left + 8.0, r.top, cap.left - 10.0, r.bottom), p.text);
                    hits.push((r, act));
                    let chosen_half = if to_prog {
                        rc(seg.left + seg_w, seg.top, seg.right, seg.bottom)
                    } else {
                        rc(seg.left, seg.top, seg.left + seg_w, seg.bottom)
                    };
                    hits.push((chosen_half, Act::AppKey(i, k, to_prog)));
                    ly += line;
                }
                ly += pad;
            }
            y = ly;
        }
        y + 4.0
    }

    /// Names and icons of the saved programs (after a change to the list).
    fn refresh_apps(&mut self) {
        let size = (24.0 * self.dpi as f32 / 96.0).round() as i32;
        self.app_info = self.cfg.pause_apps.iter().map(|a| crate::apps::info(&a.exe, &a.path, size)).collect();
    }

    fn add_app(&mut self, path: &str) {
        let Some(mut app) = config::PauseApp::from_path(path) else { return };
        self.picking = None;
        if self.cfg.pause_apps.iter().any(|a| a.exe == app.exe) {
            self.show_toast(&tf("keys.apps_already", &[&app.exe]), false);
            return;
        }
        if app.path.is_empty() {
            app.path = path.to_string();
        }
        self.cfg.pause_apps.push(app);
        self.open_app = Some(self.cfg.pause_apps.len() - 1);
        self.refresh_apps();
        let name = self.app_info.last().map(|i| i.0.clone()).unwrap_or_default();
        self.commit(&tf("keys.apps_added", &[&name]));
    }

    /// A card with a title and a sentence, then one row per option: a radio mark, its name
    /// and what it means (and, for the share frame, a small picture of the result).
    #[allow(clippy::too_many_arguments)]
    fn radio_list(
        &self,
        rt: &ID2D1RenderTarget,
        y: f32,
        title: &str,
        sub: &str,
        s: Sg,
        opts: &[(String, String)],
        swatch: bool,
        hits: &mut Hits,
    ) -> f32 {
        let Some(f) = &self.f else { return y };
        let p = &self.p;
        let (x0, x1) = (NAV_W + 32.0, W - 32.0);
        // the sentence may take two lines
        let two = self.g.measure(sub, &f.sub).0 > x1 - x0 - 36.0;
        let (head, rh) = (if two { 84.0 } else { 64.0 }, 52.0);
        let h = head + rh * opts.len() as f32 + 8.0;
        rrect(rt, rc(x0, y, x1, y + h), 6.0, p.card, p.card_border);
        txt(rt, title, &f.body, rc(x0 + 18.0, y + 13.0, x1 - 18.0, y + 35.0), p.text);
        txt(rt, sub, if two { &f.wrap } else { &f.sub }, rc(x0 + 18.0, y + 37.0, x1 - 18.0, y + head - 4.0), p.sub);
        let sel = self.seg_val(s);
        let mut ry = y + head;
        for (i, (name, desc)) in opts.iter().enumerate() {
            let r = rc(x0 + 6.0, ry, x1 - 6.0, ry + rh);
            let a = Act::Seg(s, i);
            if self.hover == Some(a) && sel != Some(i) {
                rrect(rt, r, 4.0, p.hover, 0);
            }
            let cy = (r.top + r.bottom) / 2.0;
            let cx = x0 + 28.0;
            if sel == Some(i) {
                circle(rt, cx, cy, 9.0, p.accent);
                circle(rt, cx, cy, 4.0, p.on_accent);
            } else {
                circle(rt, cx, cy, 9.0, p.off_border);
                circle(rt, cx, cy, 8.0, p.card);
            }
            let mut tx = x0 + 50.0;
            if swatch {
                self.frame_swatch(rt, i, rc(tx, cy - 14.0, tx + 40.0, cy + 14.0));
                tx += 54.0;
            }
            txt(rt, name, &f.body, rc(tx, ry + 7.0, x1 - 18.0, ry + 28.0), p.text);
            txt(rt, desc, &f.sub, rc(tx, ry + 27.0, x1 - 18.0, ry + 47.0), p.sub);
            hits.push((r, a));
            ry += rh;
        }
        y + h + 4.0
    }

    /// A tiny picture of a share frame style: the screenshot (a light tile) on its background.
    fn frame_swatch(&self, rt: &ID2D1RenderTarget, i: usize, r: D2D_RECT_F) {
        let shot = rc(r.left + 8.0, r.top + 6.0, r.right - 8.0, r.bottom - 6.0);
        let tile = if self.light { 0xFFFF_FFFF } else { 0xFFE6_E6E6 };
        match i {
            0 => {
                rrect(rt, r, 4.0, 0, self.p.btn_border);
                rrect(rt, rc(r.left + 2.0, r.top + 2.0, r.right - 2.0, r.bottom - 2.0), 2.0, tile, 0);
            }
            _ => {
                if i == 1 {
                    unsafe {
                        let stops = [
                            D2D1_GRADIENT_STOP { position: 0.0, color: color(0xFF4F_6BFF) },
                            D2D1_GRADIENT_STOP { position: 1.0, color: color(0xFFB0_5CFF) },
                        ];
                        if let Ok(sc) = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)
                        {
                            let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                                startPoint: pt(r.left, r.bottom),
                                endPoint: pt(r.right, r.top),
                            };
                            if let Ok(g) = rt.CreateLinearGradientBrush(&props, None, &sc) {
                                rt.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: 4.0, radiusY: 4.0 }, &g);
                            }
                        }
                    }
                } else {
                    let bg = if i == 2 { 0xFFF1_F2F5 } else { 0xFF1E_1F22 };
                    rrect(rt, r, 4.0, bg, self.p.btn_border);
                }
                rrect(rt, rc(shot.left + 1.0, shot.top + 2.0, shot.right + 1.0, shot.bottom + 2.0), 3.0, 0x40000000, 0);
                rrect(rt, shot, 3.0, tile, 0);
            }
        }
    }

    fn toggle(&self, rt: &ID2D1RenderTarget, card: D2D_RECT_F, tg: Tg, hits: &mut Hits) {
        let Some(f) = &self.f else { return };
        let p = &self.p;
        let on = self.toggle_val(tg);
        let cy = (card.top + card.bottom) / 2.0;
        let tr = rc(card.right - 18.0 - 40.0, cy - 10.0, card.right - 18.0, cy + 10.0);
        let hov = self.hover == Some(Act::Tog(tg));
        if on {
            rrect(rt, tr, 10.0, p.accent, 0);
            circle(rt, tr.right - 10.0, cy, if hov { 7.5 } else { 7.0 }, p.on_accent);
        } else {
            rrect(rt, tr, 10.0, if hov { p.hover } else { 0 }, p.off_border);
            circle(rt, tr.left + 10.0, cy, if hov { 6.0 } else { 5.0 }, p.off_border);
        }
        txt(
            rt,
            if on { t("common.on") } else { t("common.off") },
            &f.body,
            rc(tr.left - 70.0, card.top, tr.left - 12.0, card.bottom),
            p.text,
        );
        // the whole card toggles: a bigger target than the switch
        hits.push((card, Act::Tog(tg)));
    }

    fn seg(&self, rt: &ID2D1RenderTarget, card: D2D_RECT_F, s: Sg, opts: &[&str], hits: &mut Hits) {
        let Some(f) = &self.f else { return };
        let p = &self.p;
        let cy = (card.top + card.bottom) / 2.0;
        let ws: Vec<f32> = opts.iter().map(|o| self.g.measure(o, &f.body).0.max(18.0) + 28.0).collect();
        let total: f32 = ws.iter().sum();
        let mut x = card.right - 18.0 - total;
        rrect(rt, rc(x, cy - 16.0, x + total, cy + 16.0), 5.0, p.btn, p.btn_border);
        let sel = self.seg_val(s);
        for (i, (o, w)) in opts.iter().zip(&ws).enumerate() {
            let r = rc(x, cy - 16.0, x + w, cy + 16.0);
            let a = Act::Seg(s, i);
            let inner = rc(r.left + 3.0, r.top + 3.0, r.right - 3.0, r.bottom - 3.0);
            if sel == Some(i) {
                rrect(rt, inner, 4.0, p.accent, 0);
            } else if self.hover == Some(a) {
                rrect(rt, inner, 4.0, p.hover, 0);
            }
            let tw = self.g.measure(o, &f.body).0;
            txt(
                rt,
                o,
                &f.body,
                rc(r.left + (w - tw) / 2.0, r.top, r.right, r.bottom),
                if sel == Some(i) { p.on_accent } else { p.text },
            );
            hits.push((r, a));
            x += w;
        }
    }

    fn button(&self, rt: &ID2D1RenderTarget, r: D2D_RECT_F, label: &str, a: Act, hits: &mut Hits) {
        let Some(f) = &self.f else { return };
        let p = &self.p;
        let bg = if self.press == Some(a) {
            p.hover
        } else if self.hover == Some(a) {
            p.btn_hover
        } else {
            p.btn
        };
        rrect(rt, r, 5.0, bg, p.btn_border);
        let tw = self.g.measure(label, &f.body).0;
        txt(rt, label, &f.body, rc((r.left + r.right - tw) / 2.0, r.top, r.right, r.bottom), p.text);
        hits.push((r, a));
    }

    fn folder(&self, rt: &ID2D1RenderTarget, card: D2D_RECT_F, video: bool, hits: &mut Hits) {
        let cy = (card.top + card.bottom) / 2.0;
        let r2 = rc(card.right - 18.0 - 64.0, cy - 16.0, card.right - 18.0, cy + 16.0);
        let r1 = rc(r2.left - 8.0 - 90.0, cy - 16.0, r2.left - 8.0, cy + 16.0);
        self.button(rt, r1, t("common.browse"), Act::Browse(video), hits);
        self.button(rt, r2, t("common.open"), Act::OpenDir(video), hits);
    }

    fn hotkey(&self, rt: &ID2D1RenderTarget, row: D2D_RECT_F, k: K, hits: &mut Hits) {
        let Some(f) = &self.f else { return };
        let p = &self.p;
        let cy = (row.top + row.bottom) / 2.0;
        let rec = self.recording == Some(k);
        let shown = keys::display(self.cfg.key(k));
        let label = if rec {
            t("keys.press").to_string()
        } else if shown.is_empty() {
            t("keys.none").to_string()
        } else {
            shown.clone()
        };
        let tw = self.g.measure(&label, &f.body_b).0;
        let w = (tw + 32.0).max(140.0);
        let r = rc(row.right - 18.0 - w, cy - 15.0, row.right - 18.0, cy + 15.0);
        let a = Act::Key(k);
        let bg = if self.hover == Some(a) && !rec { p.btn_hover } else { p.btn };
        rrect(rt, r, 5.0, bg, if rec { p.accent } else { p.btn_border });
        if rec {
            rrect(rt, rc(r.left + 1.0, r.top + 1.0, r.right - 1.0, r.bottom - 1.0), 4.0, 0, p.accent);
        }
        let col = if rec {
            p.accent
        } else if shown.is_empty() {
            p.sub
        } else {
            p.text
        };
        txt(rt, &label, &f.body_b, rc((r.left + r.right - tw) / 2.0, r.top, r.right, r.bottom), col);
        hits.push((r, a));
    }

    // ------------------------------------------------------------- actions

    fn commit(&mut self, toast: &str) {
        // Print Screen is no longer ours: give it back to Windows if we took it
        if self.cfg.snipping_restore && !uses_print(&self.cfg) && set_snipping(true) {
            self.cfg.snipping_restore = false;
        }
        match config::save(&self.cfg) {
            Ok(()) => {
                crate::ipc_send(crate::IPC_RELOAD, "");
                self.show_toast(toast, false);
            }
            Err(e) => self.show_toast(&tf("settings.save_failed", &[&e.to_string()]), true),
        }
    }

    fn show_toast(&mut self, msg: &str, bad: bool) {
        self.toast = Some((msg.to_string(), bad));
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_TOAST, if bad { 4000 } else { 1600 }, None);
        }
    }

    fn act(&mut self, a: Act) {
        if let Some(k) = self.recording {
            if a != Act::Key(k) {
                self.stop_recording();
            }
        }
        match a {
            Act::Nav(i) => {
                if self.page != i {
                    self.page = i;
                    self.scroll = 0.0;
                }
            }
            Act::Key(k) => {
                if self.recording == Some(k) {
                    self.stop_recording();
                } else {
                    self.recording = Some(k);
                    // the tray's own hotkeys would swallow the keys we want to read
                    crate::ipc_send(crate::IPC_HOTKEYS, "0");
                }
            }
            Act::Tog(tg) => {
                let v = !self.toggle_val(tg);
                let c = &mut self.cfg;
                match tg {
                    Tg::PauseFs => c.pause_fullscreen = v,
                    Tg::SaveNotify => c.save_notify = v,
                    Tg::VideoNotify => c.video_notify = v,
                    Tg::Cursor => c.cursor = v,
                    Tg::InkFade => c.ink_fade = v,
                    Tg::Magnifier => c.magnifier = v,
                    Tg::Detect => c.detect_elements = v,
                    Tg::History => c.history = v,
                    Tg::Updates => c.check_updates = v,
                    Tg::PrintKey => {
                        if v {
                            if keys::clash(&c.keys, K::Capture, Chord { vk: VK_PRINT, mods: 0 }).is_some() {
                                self.show_toast(t("keys.print_taken"), true);
                                return;
                            }
                            c.set_key(K::Capture, "PrintScreen".into());
                            if snipping_on() {
                                if set_snipping(false) {
                                    c.snipping_restore = true;
                                } else {
                                    self.show_toast(t("snip.failed"), true);
                                }
                            }
                        } else {
                            // back to the default; commit() gives the key back to Windows
                            c.set_key(K::Capture, keys::def(K::Capture).default.into());
                        }
                    }
                    Tg::Autostart => {
                        crate::set_autostart(v);
                        self.autostart = crate::autostart_enabled();
                        self.show_toast(
                            if self.autostart { t("general.autostart_on") } else { t("general.autostart_off") },
                            false,
                        );
                        return;
                    }
                }
                self.commit(t("settings.saved"));
            }
            Act::Seg(s, i) => {
                if self.seg_val(s) == Some(i) {
                    return;
                }
                match s {
                    Sg::SaveMode => self.cfg.save_ask = i == 0,
                    Sg::VideoMode => self.cfg.video_ask = i == 0,
                    Sg::Fps => self.cfg.fps = FPS[i],
                    Sg::Delay => self.cfg.capture_delay = DELAYS[i],
                    Sg::HistoryDays => self.cfg.history_days = DAYS[i],
                    Sg::ImageFmt => self.cfg.image_format = output::Fmt::ALL[i],
                    Sg::VideoFmt => self.cfg.gif = i == 1,
                    Sg::Frame => self.cfg.frame = crate::frame::Style::ALL[i],
                    Sg::Upload => match crate::upload::Host::SHOWN.get(i) {
                        Some(h) => self.cfg.upload_host = *h,
                        None => return, // the custom row is set in the ini
                    },
                    Sg::Language => {
                        self.cfg.language = if i == 0 { "system".into() } else { i18n::LANGS[i - 1].0.into() };
                        i18n::set(&self.cfg.language);
                        let title = output::wide(t("settings.window_title"));
                        unsafe {
                            let _ = SetWindowTextW(self.hwnd, PCWSTR(title.as_ptr()));
                        }
                    }
                    Sg::Theme => {
                        self.cfg.theme = [Theme::System, Theme::Light, Theme::Dark][i];
                        config::set_theme(self.cfg.theme);
                        self.apply_theme();
                    }
                }
                self.commit(t("settings.saved"));
            }
            Act::Browse(_) | Act::BrowseApp => {} // handled in wndproc: dialogs must not run inside a borrow
            Act::AddApp => {
                let size = (24.0 * self.dpi as f32 / 96.0).round() as i32;
                self.picking = Some(crate::apps::running(size));
            }
            Act::RemoveApp(i) => {
                if i < self.cfg.pause_apps.len() {
                    let name = self.app_info.get(i).map(|n| n.0.clone()).unwrap_or_default();
                    self.cfg.pause_apps.remove(i);
                    self.open_app = None;
                    self.refresh_apps();
                    self.commit(&tf("keys.apps_removed", &[&name]));
                }
            }
            Act::PickApp(i) => {
                if let Some(path) = self.picking.as_ref().and_then(|l| l.get(i)).map(|p| p.path.clone()) {
                    self.add_app(&path);
                }
            }
            Act::CancelPick => self.picking = None,
            Act::BarTool(i) | Act::BarAction(i) => {
                let (list, table) = match a {
                    Act::BarTool(_) => (&mut self.cfg.bar_tools, &config::BAR_TOOLS[..]),
                    _ => (&mut self.cfg.bar_actions, &config::BAR_ACTIONS[..]),
                };
                let Some((name, _)) = table.get(i) else { return };
                let on = list.iter().any(|n| n == name);
                let keep: Vec<String> = table
                    .iter()
                    .filter(|(n, _)| if n == name { !on } else { list.iter().any(|x| x == n) })
                    .map(|(n, _)| n.to_string())
                    .collect();
                *list = keep;
                self.commit(t("settings.saved"));
            }
            Act::OpenApp(i) => self.open_app = if self.open_app == Some(i) { None } else { Some(i) },
            Act::Jump(i) => {
                self.scroll = 0.0;
                let to = self.marks.get()[i.min(4)];
                self.scroll_by(if i == 0 { 0.0 } else { to - 4.0 });
            }
            Act::AppKey(i, k, to_prog) => {
                let Some(app) = self.cfg.pause_apps.get_mut(i) else { return };
                if app.pauses(k) == to_prog {
                    return;
                }
                let mut on: Vec<K> = TRAY_KEYS.iter().copied().filter(|x| app.pauses(*x)).collect();
                if let Some(at) = on.iter().position(|x| *x == k) {
                    if on.len() == 1 {
                        self.show_toast(t("keys.apps_keep_one"), true);
                        return;
                    }
                    on.remove(at);
                } else {
                    on.push(k);
                }
                // all of them is the plain form ("saplogon.exe" with nothing after it)
                app.keys = if TRAY_KEYS.iter().all(|x| on.contains(x)) {
                    vec![]
                } else {
                    TRAY_KEYS.iter().copied().filter(|x| on.contains(x)).collect()
                };
                self.commit(t("settings.saved"));
            }
            Act::CheckUpdates => {
                if self.upd == Upd::Checking {
                    return;
                }
                self.upd = Upd::Checking;
                // this window is its own short-lived process, so WinHTTP never reaches the tray
                let hwnd = self.hwnd.0 as isize;
                std::thread::spawn(move || {
                    let r = crate::update::latest();
                    if let Ok(mut slot) = UPD_RESULT.lock() {
                        *slot = Some(r);
                    }
                    unsafe {
                        let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_UPDATE_DONE, WPARAM(0), LPARAM(0));
                    }
                });
            }
            Act::InstallUpdate => {
                if let Ok(exe) = std::env::current_exe() {
                    let _ = std::process::Command::new(exe).args(["--update", "install"]).spawn();
                }
            }
            Act::OpenDir(video) => {
                crate::open_path(if video { &self.cfg.video_dir } else { &self.cfg.save_dir }, false)
            }
            Act::OpenIni => open_ini(),
            Act::Sponsor => unsafe {
                ShellExecuteW(
                    None,
                    w!("open"),
                    w!("https://github.com/sponsors/efekaputlu"),
                    None,
                    None,
                    SW_SHOWNORMAL,
                );
            },
            Act::ResetKeys => {
                self.cfg.keys = DEFS.iter().map(|d| d.default.to_string()).collect();
                self.commit(t("keys.reset_done"));
            }
        }
    }

    fn apply_theme(&mut self) {
        self.light = crate::traymenu::light_theme(false);
        self.p = pal(self.light);
        title_bar(self.hwnd, self.light);
    }

    fn stop_recording(&mut self) {
        if self.recording.take().is_some() {
            crate::ipc_send(crate::IPC_HOTKEYS, "1");
        }
    }

    /// A key arrived while a shortcut field is recording. True = consumed.
    fn record_key(&mut self, vk: u16) -> bool {
        let Some(k) = self.recording else { return false };
        let key = VIRTUAL_KEY(vk);
        if matches!(
            key,
            VK_CONTROL
                | VK_SHIFT
                | VK_MENU
                | VK_LWIN
                | VK_RWIN
                | VK_LCONTROL
                | VK_RCONTROL
                | VK_LSHIFT
                | VK_RSHIFT
                | VK_LMENU
                | VK_RMENU
        ) {
            return true; // wait for the real key
        }
        let c = Chord::now(vk);
        if c.mods == 0 && key == VK_ESCAPE {
            self.stop_recording();
            return true;
        }
        let text = if c.mods == 0 && key == VK_BACK {
            String::new()
        } else {
            let Some(text) = c.text().filter(|_| keys::bindable(vk)) else {
                self.show_toast(t("keys.unsupported"), true);
                return true;
            };
            // a bare letter as a system-wide hotkey would take that letter away from every app
            let typing = (0x30..=0x5A).contains(&vk) || vk == 0x20 || (0xBA..=0xE2).contains(&vk);
            if keys::def(k).scope == Scope::Global && c.mods == 0 && typing {
                self.show_toast(t("keys.need_modifier"), true);
                return true;
            }
            if let Some(other) = keys::clash(&self.cfg.keys, k, c) {
                self.show_toast(&tf("keys.taken", &[t(label_key(other))]), true);
                return true;
            }
            text
        };
        let cleared = text.is_empty();
        self.cfg.set_key(k, text);
        self.stop_recording();
        self.commit(if cleared { t("keys.removed") } else { t("keys.saved") });
        true
    }

    fn scroll_by(&mut self, dy: f32) {
        let max = (self.content_h - (H - self.view_top())).max(0.0);
        self.scroll = (self.scroll + dy).clamp(0.0, max);
    }
}

/// i18n key of an action's name: "key.<ini name>".
fn label_key(k: K) -> &'static str {
    use std::sync::OnceLock;
    static NAMES: OnceLock<Vec<&'static str>> = OnceLock::new();
    // the table is static: these few strings are built once and kept
    NAMES.get_or_init(|| DEFS.iter().map(|d| &*Box::leak(format!("key.{}", d.ini).into_boxed_str())).collect())
        [k as usize]
}

/// Folder picker starting at `cur`.
fn browse(owner: HWND, cur: &std::path::Path) -> Option<std::path::PathBuf> {
    unsafe {
        let dlg = CoCreateInstance::<_, IFileOpenDialog>(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        if let Ok(o) = dlg.GetOptions() {
            let _ = dlg.SetOptions(o | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM);
        }
        if cur.is_dir() {
            let d = cur.as_os_str().encode_wide_z();
            if let Ok(it) = SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(d.as_ptr()), None) {
                let _ = dlg.SetFolder(&it);
            }
        }
        dlg.Show(Some(owner)).ok()?;
        let it = dlg.GetResult().ok()?;
        let p = it.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s.map(std::path::PathBuf::from)
    }
}

/// File picker for a program (.exe); returns its path.
fn browse_exe(owner: HWND) -> Option<String> {
    unsafe {
        let dlg = CoCreateInstance::<_, IFileOpenDialog>(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let name = output::wide(t("keys.apps_programs"));
        let spec = [windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC {
            pszName: PCWSTR(name.as_ptr()),
            pszSpec: w!("*.exe"),
        }];
        let _ = dlg.SetFileTypes(&spec);
        if let Ok(o) = dlg.GetOptions() {
            let _ = dlg.SetOptions(o | FOS_FORCEFILESYSTEM | FOS_FILEMUSTEXIST);
        }
        dlg.Show(Some(owner)).ok()?;
        let it = dlg.GetResult().ok()?;
        let p = it.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    }
}

/// Open wispshot.ini with the default .ini handler (notepad.exe is deliberately not launched).
fn open_ini() {
    let path = config::ini_path();
    let w = path.as_os_str().encode_wide_z();
    let r = unsafe { ShellExecuteW(None, w!("open"), PCWSTR(w.as_ptr()), None, None, SW_SHOWNORMAL) };
    if r.0 as isize <= 32 {
        crate::open_path(&path, true);
    }
}

fn hit(x: i32, y: i32) -> Option<Act> {
    with(|s| {
        let k = s.dpi as f32 / 96.0;
        let (x, y) = (x as f32 / k, y as f32 / k);
        s.hits.iter().rev().find(|(r, _)| x >= r.left && x < r.right && y >= r.top && y < r.bottom).map(|(_, a)| *a)
    })
    .flatten()
}

fn redraw(hwnd: HWND) {
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

/// Hover follows the content when it scrolls under a still mouse.
fn refresh_hover(hwnd: HWND) {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
        let _ = ScreenToClient(hwnd, &mut p);
    }
    let h = hit(p.x, p.y);
    with(|s| s.hover = h);
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
        WM_MOUSEWHEEL => {
            let d = ((wp.0 >> 16) & 0xFFFF) as i16 as f32;
            with(|s| s.scroll_by(-d / 120.0 * 60.0));
            // repaint first (new hit rects), then see what is under the mouse now
            with(|s| s.render());
            refresh_hover(hwnd);
            redraw(hwnd);
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
        WM_LBUTTONDOWN => {
            let (x, y) = xy(lp);
            let h = hit(x, y);
            with(|s| s.press = h);
            redraw(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = xy(lp);
            let h = hit(x, y);
            let pressed = with(|s| s.press.take()).flatten();
            match h {
                Some(Act::Browse(video)) if pressed == h => {
                    // the folder dialog runs its own message loop: no borrow held across it
                    let cur = with(|s| if video { s.cfg.video_dir.clone() } else { s.cfg.save_dir.clone() })
                        .unwrap_or_default();
                    if let Some(p) = browse(hwnd, &cur) {
                        with(|s| {
                            if video {
                                s.cfg.video_dir = p;
                            } else {
                                s.cfg.save_dir = p;
                            }
                            s.commit(t("settings.folder_changed"));
                        });
                    }
                }
                Some(Act::BrowseApp) if pressed == h => {
                    if let Some(path) = browse_exe(hwnd) {
                        with(|s| s.add_app(&path));
                    }
                }
                Some(a) if pressed == Some(a) => {
                    with(|s| s.act(a));
                }
                _ => {
                    // a click on empty space ends shortcut recording
                    with(|s| s.stop_recording());
                }
            }
            redraw(hwnd);
            LRESULT(0)
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let vk = wp.0 as u16;
            if with(|s| s.record_key(vk)) == Some(true) {
                redraw(hwnd);
                return LRESULT(0);
            }
            match VIRTUAL_KEY(vk) {
                VK_ESCAPE if msg == WM_KEYDOWN => {
                    let _ = DestroyWindow(hwnd);
                    return LRESULT(0);
                }
                // Ctrl+Tab / Ctrl+Shift+Tab: next / previous page
                VK_TAB if GetKeyState(VK_CONTROL.0 as i32) < 0 => {
                    let back = GetKeyState(VK_SHIFT.0 as i32) < 0;
                    with(|s| {
                        s.page = (s.page + if back { PAGES.len() - 1 } else { 1 }) % PAGES.len();
                        s.scroll = 0.0;
                    });
                    redraw(hwnd);
                    return LRESULT(0);
                }
                VK_PRIOR | VK_NEXT | VK_HOME | VK_END if msg == WM_KEYDOWN => {
                    let page = H - with(|s| s.view_top()).unwrap_or(VIEW_TOP) - 40.0;
                    with(|s| match VIRTUAL_KEY(vk) {
                        VK_PRIOR => s.scroll_by(-page),
                        VK_NEXT => s.scroll_by(page),
                        VK_HOME => s.scroll_by(-1e6),
                        _ => s.scroll_by(1e6),
                    });
                    redraw(hwnd);
                    return LRESULT(0);
                }
                _ => {}
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_SYSCHAR if with(|s| s.recording.is_some()) == Some(true) => LRESULT(0),
        WM_UPDATE_DONE => {
            let r = UPD_RESULT.lock().ok().and_then(|mut s| s.take());
            with(|s| {
                s.upd = match r {
                    Some(Ok(v)) if crate::update::newer(&v) => {
                        // the tray menu offers it too
                        crate::ipc_send(crate::IPC_UPDATE, &v);
                        Upd::New(v)
                    }
                    Some(Ok(_)) => Upd::Latest,
                    Some(Err(e)) => Upd::Failed(e),
                    None => Upd::Idle,
                }
            });
            redraw(hwnd);
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_TOAST => {
            let _ = KillTimer(Some(hwnd), TIMER_TOAST);
            with(|s| s.toast = None);
            redraw(hwnd);
            LRESULT(0)
        }
        WM_ACTIVATE if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE => {
            with(|s| s.stop_recording());
            redraw(hwnd);
            LRESULT(0)
        }
        WM_SETTINGCHANGE => {
            // light / dark switch while the window is open ("System" theme)
            with(|s| s.apply_theme());
            redraw(hwnd);
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_DPICHANGED => {
            with(|s| {
                s.dpi = (wp.0 & 0xFFFF) as u32;
                s.rt = None;
            });
            let r = &*(lp.0 as *const RECT);
            let _ = SetWindowPos(
                hwnd,
                None,
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            redraw(hwnd);
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            with(|s| s.stop_recording());
            ST.with(|s| s.borrow_mut().take());
            crate::session::poke();
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
