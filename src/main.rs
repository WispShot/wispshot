//! WispShot — a screenshot tool that is (almost) not there.
//!
//! Two kinds of process, same executable:
//! - **resident**: tray icon + global hotkeys only. One hidden window blocked in
//!   GetMessage: no polling, ~2 MB private memory. The only hook is an
//!   out-of-context foreground-change event (for "pause in full screen").
//! - **session**: `wispshot.exe --session capture|record|pin`, spawned per action. Owns
//!   everything heavy (Direct2D, WIC, OCR, H.264 encoder, pins) and exits
//!   as soon as its last window closes, so GPU-driver caches and all other
//!   memory go back to the OS. A crash there never takes the tray down.

#![windows_subsystem = "windows"]

mod apps;
mod capture;
mod config;
mod countdown;
mod frame;
mod gfx;
mod gif;
mod history;
mod i18n;
mod img;
mod ink;
mod keys;
mod layered;
mod logo;
mod ocr;
mod output;
mod overlay;
mod pin;
mod record;
mod recui;
mod redact;
mod scroll;
mod selftest;
mod session;
mod settings;
mod textedit;
mod traymenu;
mod update;
mod upload;
mod veil;

use config::Config;
use i18n::{t, tf};
use keys::K;
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::*;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

static APP: AtomicIsize = AtomicIsize::new(0);
static IS_SESSION: AtomicBool = AtomicBool::new(false);

/// The hidden window of *this* process (resident or session).
pub fn app_hwnd() -> Option<HWND> {
    let v = APP.load(Ordering::Relaxed);
    (v != 0).then_some(HWND(v as *mut _))
}

pub fn set_app_hwnd(h: HWND) {
    APP.store(h.0 as isize, Ordering::Relaxed);
}

/// Opt-in timing trace (set WISP_TRACE=1): appends to %TEMP%\wispshot-trace.log.
pub fn trace(what: &str) {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    static T0: OnceLock<std::time::Instant> = OnceLock::new();
    if !*ON.get_or_init(|| std::env::var_os("WISP_TRACE").is_some()) {
        return;
    }
    let t = T0.get_or_init(std::time::Instant::now).elapsed();
    use std::io::Write;
    if let Ok(mut f) =
        std::fs::OpenOptions::new().create(true).append(true).open(std::env::temp_dir().join("wispshot-trace.log"))
    {
        let _ = writeln!(f, "[{}] {:>10.2} ms  {}", std::process::id(), t.as_secs_f64() * 1000.0, what);
    }
}

/// Give unused pages back to the OS.
pub fn trim_memory() {
    unsafe {
        let _ = SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
    }
}

/// Effective DPI of a monitor (96 = 100 %).
pub fn monitor_dpi(mon: HMONITOR) -> u32 {
    let (mut dx, mut dy) = (96u32, 96u32);
    unsafe {
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
    }
    dx.max(96)
}

/// Client coordinates from a mouse message's LPARAM.
pub fn lparam_xy(lp: LPARAM) -> (i32, i32) {
    ((lp.0 & 0xFFFF) as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as i16 as i32)
}

const WM_TRAY: u32 = WM_APP + 1;
pub const WM_APP_CMD: u32 = WM_APP + 2;
pub const WM_APP_REC_STOP: u32 = WM_APP + 3;
const HK_CAPTURE: i32 = 1;
const HK_RECORD: i32 = 2;
const HK_PIN: i32 = 3;
const HK_DELAY: i32 = 4;

const CMD_CAPTURE: usize = 100;
const CMD_RECORD: usize = 101;
const CMD_PIN: usize = 102;
const CMD_SHOTS: usize = 103;
const CMD_VIDEOS: usize = 104;
const CMD_SETTINGS: usize = 105;
/// Re-read the ini (sent by the settings window after every change).
const CMD_RELOAD: usize = 106;
const CMD_AUTOSTART: usize = 107;
const CMD_CLOSEPINS: usize = 108;
const CMD_EXIT: usize = 109;
/// `--menu`: open the tray menu at the cursor (tests, scripts)
const CMD_MENU: usize = 111;
/// Capture after `capture_delay` seconds; again cancels the countdown.
const CMD_DELAY: usize = 112;
const CMD_HISTORY: usize = 113;
const CMD_SPONSOR: usize = 114;
/// Download and install the newer version the daily check found.
const CMD_UPDATE: usize = 115;

// session -> resident messages (WM_COPYDATA dwData)
pub const IPC_NOTIFY: usize = 1;
pub const IPC_REC_ON: usize = 2;
pub const IPC_REC_OFF: usize = 3;
pub const IPC_LAST_FILE: usize = 4;
/// settings window saved wispshot.ini: reload it
pub const IPC_RELOAD: usize = 5;
/// settings window is reading keys for a hotkey field: "0" release ours, "1" take them back
pub const IPC_HOTKEYS: usize = 6;
/// A session read text in a language Windows has no OCR pack for (its name follows).
pub const IPC_OCR_HINT: usize = 7;
/// `--update check` found a newer version (it follows).
pub const IPC_UPDATE: usize = 8;
/// The OCR language hint was shown since the tray started (once is enough).
static OCR_HINTED: AtomicBool = AtomicBool::new(false);

struct State {
    cfg: Config,
    last_file: Option<PathBuf>,
    taskbar_created: u32,
    icon: HICON,
    icon_rec: HICON,
    recording: bool,
    /// Hotkeys released because a full-screen app, or an app from `pause_in_apps`, is in front.
    suspended: bool,
    /// The `pause_in_apps` program in front (its exe name), when that is the reason.
    paused_app: Option<String>,
    /// The shortcuts that program keeps for itself; empty: all of them.
    paused_keys: Vec<K>,
    /// Released while the settings window records a new hotkey.
    held: bool,
    fg_hook: HWINEVENTHOOK,
    /// Size/position changes of the foreground process: a game usually takes focus
    /// first and goes full screen a moment later, with no further focus change.
    loc_hook: HWINEVENTHOOK,
    loc_pid: u32,
    /// A newer version the update check found ("0.4.1").
    update: Option<String>,
}

/// While suspended, re-check this often (a game can leave full screen without a focus change).
const TIMER_FULLSCREEN: usize = 2;
/// A second delayed-capture press came before the countdown window existed: retry the cancel.
const TIMER_CANCEL_DELAY: usize = 3;
/// Hourly check for history images past their age limit.
const TIMER_EXPIRE: usize = 4;
/// Ask for a newer version two minutes after start, then once a day.
const TIMER_UPDATE: usize = 5;
/// The balloon click target of an update notice.
const UPDATE_MARK: &str = "wispshot:update";

thread_local! {
    /// When the last countdown session was started (it takes a moment to show its window).
    static DELAY_SPAWNED: std::cell::Cell<Option<std::time::Instant>> = const { std::cell::Cell::new(None) };
}

fn delay_starting() -> bool {
    DELAY_SPAWNED.with(|d| d.get()).is_some_and(|t| t.elapsed().as_millis() < 1500)
}

thread_local! {
    static ST: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    ST.with(|s| s.borrow_mut().as_mut().map(f))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).map(|a| a.to_ascii_lowercase()).collect();
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    if args.first().map(|s| s.as_str()) == Some("--selftest") {
        let dir = std::env::args()
            .nth(2)
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("WispShot-selftest"));
        std::process::exit(selftest::run(&dir));
    }
    if args.first().map(|s| s.as_str()) == Some("--update") {
        // like a session: its notices go through the tray
        IS_SESSION.store(true, Ordering::Relaxed);
        std::process::exit(update::run(args.get(1).map(|s| s.as_str()).unwrap_or("check")));
    }
    if args.first().map(|s| s.as_str()) == Some("--session") {
        IS_SESSION.store(true, Ordering::Relaxed);
        let veil = args.get(2).and_then(|s| s.parse::<isize>().ok()).filter(|v| *v != 0).map(|v| HWND(v as *mut _));
        // paths may contain upper-case letters: re-read the raw (not lower-cased) extras
        let extra: Vec<String> = std::env::args().skip(4).collect();
        session::run(args.get(1).map(|s| s.as_str()).unwrap_or("capture"), veil, &extra);
        return;
    }
    resident(args);
}

// ======================================================================= resident

fn resident(args: Vec<String>) {
    let cmd = match args.first().map(|s| s.as_str()) {
        Some("--capture") | Some("-c") => CMD_CAPTURE,
        Some("--record") | Some("-r") => CMD_RECORD,
        Some("--pin") => CMD_PIN,
        Some("--exit") => CMD_EXIT,
        Some("--menu") => CMD_MENU,
        _ => 0,
    };
    unsafe {
        // single instance: forward the command to the running copy
        let _mutex = CreateMutexW(None, true, w!("Local\\WispShot.SingleInstance"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            if let Ok(h) = FindWindowW(w!("WispApp"), None) {
                let _ = PostMessageW(Some(h), WM_APP_CMD, WPARAM(if cmd == 0 { CMD_CAPTURE } else { cmd }), LPARAM(0));
            }
            return;
        }
        if cmd == CMD_EXIT {
            return;
        }
        migrate_autostart();
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);

        let inst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(app_proc),
            hInstance: inst.into(),
            lpszClassName: w!("WispApp"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            w!("WispApp"),
            w!("Wisp"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(inst.into()),
            None,
        ) else {
            return;
        };
        set_app_hwnd(hwnd);
        // let sessions (another process) send us WM_COPYDATA even under UIPI differences
        let _ = ChangeWindowMessageFilterEx(hwnd, WM_COPYDATA, MSGFLT_ALLOW, None);

        let cfg = config::load();
        history::expire(cfg.history_days);
        SetTimer(Some(hwnd), TIMER_EXPIRE, 3_600_000, None);
        SetTimer(Some(hwnd), TIMER_UPDATE, 120_000, None);
        watch_ini(hwnd);
        let small = GetSystemMetrics(SM_CXSMICON).max(16);
        let state = State {
            cfg,
            last_file: None,
            taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
            icon: make_icon(small, false),
            icon_rec: make_icon(small, true),
            recording: false,
            suspended: false,
            paused_app: None,
            paused_keys: vec![],
            held: false,
            fg_hook: HWINEVENTHOOK::default(),
            loc_hook: HWINEVENTHOOK::default(),
            loc_pid: 0,
            update: None,
        };
        ST.with(|s| *s.borrow_mut() = Some(state));
        tray(NIM_ADD);
        register_hotkeys(true);
        watch_fullscreen();
        output::clean_temp();
        update::clean_temp();
        if cmd != 0 {
            let _ = PostMessageW(Some(hwnd), WM_APP_CMD, WPARAM(cmd), LPARAM(0));
        }
        trim_memory();

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        tray(NIM_DELETE);
    }
}

fn register_hotkeys(warn: bool) {
    let Some(app) = app_hwnd() else { return };
    let Some((cfg, suspended, recording, held, only)) =
        with(|s| (s.cfg.clone(), s.suspended, s.recording, s.held, s.paused_keys.clone()))
    else {
        return;
    };
    let mut failed = vec![];
    unsafe {
        for (id, k) in
            [(HK_CAPTURE, K::Capture), (HK_DELAY, K::CaptureDelayed), (HK_RECORD, K::Record), (HK_PIN, K::PinClipboard)]
        {
            let (hk, txt) = (cfg.hotkey(k), cfg.key_label(k));
            let _ = UnregisterHotKey(Some(app), id);
            // in a full-screen game the keys belong to the game; a running recording can still be stopped
            let paused = suspended && (only.is_empty() || only.contains(&k));
            if held || paused && !(id == HK_RECORD && recording) {
                continue;
            }
            if let Some(hk) = hk {
                if RegisterHotKey(Some(app), id, hk.mods, hk.vk).is_err() {
                    failed.push(txt);
                }
            }
        }
    }
    if warn && !failed.is_empty() {
        notify(t("hotkey_taken.title"), &tf("hotkey_taken.body", &[&failed.join(", ")]));
    }
}

// ------------------------------------------------------------ full screen

/// Install or remove the foreground-change hook to match the config, then re-check.
/// Apply hand edits of wispshot.ini without a restart. A thread sleeps on the folder's
/// change notification (no polling) and asks for a reload when the file's time changes.
fn watch_ini(app: HWND) {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::*;
    let path = config::ini_path();
    let Some(dir) = path.parent().map(|d| d.to_path_buf()) else { return };
    let target = app.0 as isize;
    let stamp = |p: &std::path::Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let _ = std::thread::Builder::new().stack_size(64 * 1024).spawn(move || unsafe {
        let wdir: Vec<u16> = dir.as_os_str().encode_wide().chain(Some(0)).collect();
        let Ok(h) = FindFirstChangeNotificationW(
            PCWSTR(wdir.as_ptr()),
            false,
            FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_FILE_NAME,
        ) else {
            return;
        };
        let mut last = stamp(&path);
        while WaitForSingleObject(h, INFINITE) == WAIT_OBJECT_0 {
            // editors write in steps: let the save finish before reading it
            std::thread::sleep(std::time::Duration::from_millis(400));
            let now = stamp(&path);
            // an empty file is a save in progress; reading it would write the defaults over it
            let empty = std::fs::metadata(&path).map(|m| m.len() == 0).unwrap_or(true);
            if now != last && !empty {
                last = now;
                let _ = PostMessageW(Some(HWND(target as *mut _)), WM_APP_CMD, WPARAM(CMD_RELOAD), LPARAM(0));
            }
            if FindNextChangeNotification(h).is_err() {
                break;
            }
        }
        let _ = FindCloseChangeNotification(h);
    });
}

fn watch_fullscreen() {
    let Some((on, hook)) = with(|s| (s.cfg.pause_fullscreen || !s.cfg.pause_apps.is_empty(), s.fg_hook)) else {
        return;
    };
    unsafe {
        if on && hook.is_invalid() {
            let h = SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                None,
                Some(on_foreground),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            );
            with(|s| s.fg_hook = h);
        } else if !on && !hook.is_invalid() {
            let _ = UnhookWinEvent(hook);
            with(|s| s.fg_hook = HWINEVENTHOOK::default());
        }
    }
    watch_foreground_size();
    check_fullscreen();
}

unsafe extern "system" fn on_foreground(_: HWINEVENTHOOK, _: u32, _: HWND, _: i32, _: i32, _: u32, _: u32) {
    watch_foreground_size();
    check_fullscreen();
}

/// Follow location changes of the foreground window's process only (a global hook would fire constantly).
fn watch_foreground_size() {
    let Some((on, hook, pid)) = with(|s| (s.cfg.pause_fullscreen, s.loc_hook, s.loc_pid)) else { return };
    unsafe {
        let mut want = 0u32;
        if on {
            GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut want));
            if want == GetCurrentProcessId() {
                want = 0;
            }
        }
        if want == pid && (want == 0) == hook.is_invalid() {
            return;
        }
        if !hook.is_invalid() {
            let _ = UnhookWinEvent(hook);
        }
        let h = if want != 0 {
            SetWinEventHook(
                EVENT_OBJECT_LOCATIONCHANGE,
                EVENT_OBJECT_LOCATIONCHANGE,
                None,
                Some(on_location),
                want,
                0,
                WINEVENT_OUTOFCONTEXT,
            )
        } else {
            HWINEVENTHOOK::default()
        };
        with(|s| {
            s.loc_hook = h;
            s.loc_pid = if h.is_invalid() { 0 } else { want };
        });
    }
}

unsafe extern "system" fn on_location(_: HWINEVENTHOOK, _: u32, hwnd: HWND, obj: i32, child: i32, _: u32, _: u32) {
    if obj == OBJID_WINDOW.0 && child == CHILDID_SELF as i32 && hwnd == GetForegroundWindow() {
        check_fullscreen();
    }
}

fn check_fullscreen() {
    let Some((on, apps, was, was_app, was_keys)) = with(|s| {
        (s.cfg.pause_fullscreen, s.cfg.pause_apps.clone(), s.suspended, s.paused_app.clone(), s.paused_keys.clone())
    }) else {
        return;
    };
    let app = if apps.is_empty() { None } else { foreground_exe().and_then(|e| apps.into_iter().find(|a| a.exe == e)) };
    let full = app.is_none() && on && foreground_is_fullscreen();
    let now = full || app.is_some();
    let keys = app.as_ref().map(|a| a.keys.clone()).unwrap_or_default();
    let name = app.map(|a| a.exe);
    if now == was && name == was_app && keys == was_keys {
        return;
    }
    with(|s| {
        s.suspended = now;
        s.paused_app = name;
        s.paused_keys = keys;
    });
    register_hotkeys(false);
    tray(NIM_MODIFY);
    if let Some(app) = app_hwnd() {
        unsafe {
            // leaving one of the listed apps always moves the focus; full screen may not
            if full {
                SetTimer(Some(app), TIMER_FULLSCREEN, 2000, None);
            } else {
                let _ = KillTimer(Some(app), TIMER_FULLSCREEN);
            }
        }
    }
}

/// The exe name of the foreground window's program, lowercase ("saplogon.exe").
fn foreground_exe() -> Option<String> {
    window_exe(unsafe { GetForegroundWindow() })
}

/// The exe name of a window's program, lowercase; None for our own windows.
pub(crate) fn window_exe(hwnd: HWND) -> Option<String> {
    window_path(hwnd).map(|p| p.rsplit('\\').next().unwrap_or(&p).to_lowercase())
}

/// The full path of a window's program; None for our own windows.
pub(crate) fn window_path(hwnd: HWND) -> Option<String> {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 || pid == GetCurrentProcessId() {
            return None;
        }
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut n = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut n).is_ok();
        let _ = CloseHandle(h);
        ok.then(|| String::from_utf16_lossy(&buf[..n as usize]))
    }
}

/// A game or video player covering its whole monitor (exclusive or borderless).
/// Maximized windows keep their caption and are not counted; neither is the desktop.
fn foreground_is_fullscreen() -> bool {
    unsafe {
        if let Ok(QUNS_RUNNING_D3D_FULL_SCREEN) = SHQueryUserNotificationState() {
            return true;
        }
        let h = GetForegroundWindow();
        if h.is_invalid() {
            return false;
        }
        let mut cls = [0u16; 64];
        let n = GetClassNameW(h, &mut cls) as usize;
        let cls = String::from_utf16_lossy(&cls[..n]);
        if matches!(cls.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd")
            || cls.starts_with("Wisp")
        {
            return false;
        }
        let style = GetWindowLongW(h, GWL_STYLE) as u32;
        if style & WS_CAPTION.0 == WS_CAPTION.0 {
            return false;
        }
        let mut r = RECT::default();
        if GetWindowRect(h, &mut r).is_err() {
            return false;
        }
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST), &mut mi).as_bool() {
            return false;
        }
        let m = mi.rcMonitor;
        r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom
    }
}

// ------------------------------------------------------------------- tray

fn tray(op: NOTIFY_ICON_MESSAGE) {
    let Some(app) = app_hwnd() else { return };
    let Some((icon, tip)) = with(|s| {
        let tip = if s.recording {
            tf("tray.tip_recording", &[&s.cfg.key_label(K::Record)])
        } else if let Some(app) = &s.paused_app {
            tf("tray.tip_paused_app", &[app])
        } else if s.suspended {
            t("tray.tip_paused").to_string()
        } else {
            tf("tray.tip", &[&s.cfg.key_label(K::Capture)])
        };
        (if s.recording { s.icon_rec } else { s.icon }, tip)
    }) else {
        return;
    };
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: app,
        uID: 1,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
        uCallbackMessage: WM_TRAY,
        hIcon: icon,
        ..Default::default()
    };
    copy_w(&mut nid.szTip, &tip);
    unsafe {
        let _ = Shell_NotifyIconW(op, &nid);
        if op == NIM_ADD {
            nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            let _ = Shell_NotifyIconW(NIM_SETVERSION, &nid);
        }
    }
}

fn copy_w(dst: &mut [u16], s: &str) {
    let w: Vec<u16> = s.encode_utf16().take(dst.len() - 1).collect();
    dst[..w.len()].copy_from_slice(&w);
    dst[w.len()] = 0;
}

/// Balloon/toast from the tray icon. From a session process this is forwarded
/// to the resident instance (which owns the icon).
pub fn notify(title: &str, text: &str) {
    if IS_SESSION.load(Ordering::Relaxed) {
        ipc_send(IPC_NOTIFY, &format!("{}\u{1}{}", title, text));
        return;
    }
    let Some(app) = app_hwnd() else { return };
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: app,
        uID: 1,
        uFlags: NIF_INFO,
        dwInfoFlags: NIIF_INFO | NIIF_NOSOUND,
        ..Default::default()
    };
    copy_w(&mut nid.szInfoTitle, title);
    copy_w(&mut nid.szInfo, text);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

/// An upload link as a notice may open it: http(s), one word, no quotes.
fn is_link(s: &str) -> bool {
    (s.starts_with("https://") || s.starts_with("http://")) && !s.chars().any(|c| c == '"' || c.is_whitespace())
}

/// `wispshot.exe --update check|install` in its own process (WinHTTP stays out of the tray).
fn spawn_update(what: &str) {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).args(["--update", what]).spawn();
    }
}

/// Session -> resident message.
pub fn ipc_send(kind: usize, text: &str) {
    unsafe {
        let Ok(res) = FindWindowW(w!("WispApp"), None) else { return };
        let w: Vec<u16> = text.encode_utf16().collect();
        let cds = COPYDATASTRUCT { dwData: kind, cbData: (w.len() * 2) as u32, lpData: w.as_ptr() as *mut _ };
        let mut out = 0usize;
        let _ = SendMessageTimeoutW(
            res,
            WM_COPYDATA,
            WPARAM(0),
            LPARAM(&cds as *const _ as isize),
            SMTO_ABORTIFHUNG,
            1000,
            Some(&mut out),
        );
    }
}

/// Procedural tray icon (no resource compiler needed): the WispShot mark from logo.rs,
/// or its recording variant (dark square, red dot).
fn make_icon(size: i32, rec: bool) -> HICON {
    let px = logo::raster(size as usize, rec);
    unsafe {
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                biHeight: -size,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let dc = GetDC(None);
        let color = CreateDIBSection(Some(dc), &bi, DIB_RGB_COLORS, &mut bits, None, 0).unwrap_or_default();
        ReleaseDC(None, dc);
        if !bits.is_null() {
            std::ptr::copy_nonoverlapping(px.as_ptr(), bits as *mut u32, px.len());
        }
        let mask = CreateBitmap(size, size, 1, 1, None);
        let ii = ICONINFO { fIcon: TRUE, xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&ii).unwrap_or_default();
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        icon
    }
}

/// Re-sync the tray icon with reality (a session may have died without telling us).
fn sync_recording() -> bool {
    let rec = session_window("WispRecPill").is_some();
    if with(|s| std::mem::replace(&mut s.recording, rec)) != Some(rec) {
        tray(NIM_MODIFY);
    }
    rec
}

fn tray_menu() {
    use traymenu::Row;
    let Some((cfg, suspended)) = with(|s| (s.cfg.clone(), s.suspended)) else { return };
    let recording = sync_recording();
    let item = |id: usize, icon: char, text: &str, key: &str| Row::Item {
        id,
        icon,
        text: text.to_string(),
        key: key.to_string(),
        danger: false,
    };
    let mut rows = vec![];
    if let Some(Some(v)) = with(|s| s.update.clone()) {
        rows.push(item(CMD_UPDATE, '\u{E896}', &tf("menu.update", &[&v]), ""));
        rows.push(Row::Sep);
    }
    rows.extend([
        item(CMD_CAPTURE, '\u{E722}', t("menu.capture"), &cfg.key_label(K::Capture)),
        item(
            CMD_DELAY,
            '\u{E916}',
            &tf("menu.capture_delayed", &[&cfg.capture_delay.to_string()]),
            &cfg.key_label(K::CaptureDelayed),
        ),
    ]);
    rows.push(if recording {
        Row::Item {
            id: CMD_RECORD,
            icon: '\u{E71A}',
            text: t("menu.stop_recording").into(),
            key: cfg.key_label(K::Record),
            danger: true,
        }
    } else {
        item(CMD_RECORD, '\u{E714}', t("menu.record"), &cfg.key_label(K::Record))
    });
    rows.extend([
        item(CMD_PIN, '\u{E718}', t("menu.pin_clipboard"), &cfg.key_label(K::PinClipboard)),
        item(CMD_CLOSEPINS, '\u{E77A}', t("menu.close_pins"), ""),
        item(CMD_HISTORY, '\u{E81C}', t("menu.history"), ""),
        Row::Sep,
        item(CMD_SHOTS, '\u{E91B}', t("menu.shots_folder"), ""),
        item(CMD_VIDEOS, '\u{E8B7}', t("menu.videos_folder"), ""),
        Row::Sep,
        item(CMD_SETTINGS, '\u{E713}', t("menu.settings"), ""),
        Row::Toggle { id: CMD_AUTOSTART, icon: '\u{E7F4}', text: t("menu.autostart").into(), on: autostart_enabled() },
        item(CMD_SPONSOR, '\u{EB51}', t("menu.sponsor"), ""),
        Row::Sep,
        item(CMD_EXIT, '\u{E7E8}', t("menu.exit"), ""),
    ]);
    let status = suspended.then(|| t("menu.paused").to_string());
    let mut pt = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    traymenu::show(
        traymenu::Spec {
            rows,
            status,
            // posted: the menu is gone before a capture freezes the screen
            on_pick: |id| {
                if let Some(app) = app_hwnd() {
                    unsafe {
                        let _ = PostMessageW(Some(app), WM_APP_CMD, WPARAM(id), LPARAM(0));
                    }
                }
            },
            on_toggle: |id| {
                if id == CMD_AUTOSTART {
                    set_autostart(!autostart_enabled());
                }
                autostart_enabled()
            },
        },
        pt.x,
        pt.y,
    );
}

// ------------------------------------------------------------- autostart

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");

/// Pre-0.2 builds registered "Wisp": re-register under the new name for this exe.
fn migrate_autostart() {
    unsafe {
        if RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, w!("Wisp"), RRF_RT_REG_SZ, None, None, None).is_err() {
            return;
        }
        set_autostart(true);
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, Some(0), KEY_SET_VALUE, &mut key).is_ok() {
            let _ = RegDeleteValueW(key, w!("Wisp"));
            let _ = RegCloseKey(key);
        }
    }
}

pub fn autostart_enabled() -> bool {
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, w!("WispShot"), RRF_RT_REG_SZ, None, None, None).is_ok() }
}

pub fn set_autostart(on: bool) {
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, Some(0), KEY_SET_VALUE, &mut key).is_err() {
            return;
        }
        if on {
            let exe = std::env::current_exe().unwrap_or_default();
            let v = output::wide(&format!("\"{}\"", exe.display()));
            let bytes = std::slice::from_raw_parts(v.as_ptr() as *const u8, v.len() * 2);
            let _ = RegSetValueExW(key, w!("WispShot"), Some(0), REG_SZ, Some(bytes));
        } else {
            let _ = RegDeleteValueW(key, w!("WispShot"));
        }
        let _ = RegCloseKey(key);
    }
}

pub fn open_path(p: &std::path::Path, select: bool) {
    let _ = std::fs::create_dir_all(if select { p.parent().unwrap_or(p) } else { p });
    let args = if select { format!("/select,\"{}\"", p.display()) } else { format!("\"{}\"", p.display()) };
    let a = output::wide(&args);
    unsafe {
        ShellExecuteW(None, w!("open"), w!("explorer.exe"), PCWSTR(a.as_ptr()), None, SW_SHOWNORMAL);
    }
}

// ------------------------------------------------------------- sessions

/// A window of the given class belonging to any Wisp session.
fn session_window(class: &str) -> Option<HWND> {
    let c = output::wide(class);
    unsafe { FindWindowW(PCWSTR(c.as_ptr()), None).ok() }
}

fn spawn_session(kind: &str) {
    // one interactive capture at a time
    if session_window("WispOverlay").is_some()
        || session_window("WispVeil").is_some_and(|v| unsafe { IsWindowVisible(v).as_bool() })
    {
        return;
    }
    // the veil appears immediately; the session process takes over from it
    let veil = if kind == "pin" { None } else { veil::show(capture::virtual_screen()) };
    if let Ok(exe) = std::env::current_exe() {
        let mut c = std::process::Command::new(exe);
        c.args(["--session", kind, &veil.map_or(0, |v| v.0 as isize).to_string()]);
        if let Err(e) = c.spawn() {
            if let Some(v) = veil {
                unsafe {
                    let _ = DestroyWindow(v);
                }
            }
            notify("WispShot", &tf("err.session", &[&e.to_string()]));
        }
    }
}

/// Ends a running delayed-capture countdown; false if there was none.
fn cancel_countdown() -> bool {
    match session_window("WispDelay") {
        Some(h) => unsafe {
            let _ = PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0));
            true
        },
        None => false,
    }
}

/// Starts a countdown session; it opens the capture editor by itself when the time is up.
fn spawn_delayed(secs: u32) {
    if session_window("WispOverlay").is_some()
        || session_window("WispVeil").is_some_and(|v| unsafe { IsWindowVisible(v).as_bool() })
    {
        return;
    }
    if let Ok(exe) = std::env::current_exe() {
        let mut c = std::process::Command::new(exe);
        c.args(["--session", "delay", "0", &secs.to_string()]);
        DELAY_SPAWNED.with(|d| d.set(Some(std::time::Instant::now())));
        if let Err(e) = c.spawn() {
            notify("WispShot", &tf("err.session", &[&e.to_string()]));
        }
    }
}

/// Posts WM_CLOSE to every pin (each may live in its own session process).
pub fn close_all_pins() {
    unsafe extern "system" fn each(h: HWND, _: LPARAM) -> BOOL {
        let mut cls = [0u16; 16];
        let n = GetClassNameW(h, &mut cls) as usize;
        if String::from_utf16_lossy(&cls[..n]) == "WispPin" {
            let _ = PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        TRUE
    }
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(0));
    }
}

// ------------------------------------------------------------- commands

fn command(id: usize) {
    let Some(app) = app_hwnd() else { return };
    let Some(cfg) = with(|s| s.cfg.clone()) else { return };
    match id {
        CMD_CAPTURE => {
            // the countdown just ran out and is opening the editor itself: nothing to do
            if session_window("WispDelay").is_some_and(|h| unsafe { !IsWindowVisible(h).as_bool() }) {
                return;
            }
            // the capture key during a countdown: take it now
            cancel_countdown();
            spawn_session("capture")
        }
        CMD_DELAY => {
            if cancel_countdown() {
                DELAY_SPAWNED.with(|d| d.set(None));
            } else if delay_starting() {
                // pressed twice quickly: cancel as soon as the countdown shows up
                unsafe {
                    SetTimer(Some(app), TIMER_CANCEL_DELAY, 100, None);
                }
            } else {
                spawn_delayed(cfg.capture_delay);
            }
        }
        CMD_RECORD => match session_window("WispRecPill") {
            Some(pill) => unsafe {
                // the session may ask where to save: let it come to the front
                let _ = AllowSetForegroundWindow(ASFW_ANY);
                let _ = PostMessageW(Some(pill), recui::WM_REC_STOP_REQUEST, WPARAM(0), LPARAM(0));
            },
            None => spawn_session("record"),
        },
        CMD_PIN => spawn_session("pin"),
        CMD_CLOSEPINS => close_all_pins(),
        CMD_SHOTS => open_path(&cfg.save_dir, false),
        CMD_VIDEOS => open_path(&cfg.video_dir, false),
        CMD_UPDATE => spawn_update("install"),
        CMD_SPONSOR => unsafe {
            ShellExecuteW(None, w!("open"), w!("https://github.com/sponsors/efekaputlu"), None, None, SW_SHOWNORMAL);
        },
        CMD_HISTORY => match session_window("WispHistory") {
            Some(h) => unsafe {
                let _ = ShowWindow(h, SW_RESTORE);
                let _ = SetForegroundWindow(h);
            },
            None => unsafe {
                let _ = AllowSetForegroundWindow(ASFW_ANY);
                if let Ok(exe) = std::env::current_exe() {
                    let _ = std::process::Command::new(exe).args(["--session", "history", "0"]).spawn();
                }
            },
        },
        CMD_SETTINGS => match session_window("WispSettings") {
            Some(h) => unsafe {
                let _ = ShowWindow(h, SW_RESTORE);
                let _ = SetForegroundWindow(h);
            },
            None => unsafe {
                // a settings window is its own short-lived process, like every other UI
                let _ = AllowSetForegroundWindow(ASFW_ANY);
                if let Ok(exe) = std::env::current_exe() {
                    let _ = std::process::Command::new(exe).args(["--session", "settings", "0"]).spawn();
                }
            },
        },
        CMD_RELOAD => {
            let cfg = config::load();
            history::expire(cfg.history_days);
            with(|s| s.cfg = cfg);
            register_hotkeys(true);
            watch_fullscreen();
            tray(NIM_MODIFY);
        }
        CMD_AUTOSTART => set_autostart(!autostart_enabled()),
        CMD_MENU => tray_menu(),
        CMD_EXIT => unsafe {
            if let Some(pill) = session_window("WispRecPill") {
                let _ = PostMessageW(Some(pill), recui::WM_REC_STOP_REQUEST, WPARAM(0), LPARAM(0));
            }
            // Settings is its own process: it goes with the tray
            if let Some(w) = session_window("WispSettings") {
                let _ = PostMessageW(Some(w), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
            let _ = DestroyWindow(app);
        },
        _ => {}
    }
    trim_memory();
}

unsafe extern "system" fn app_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_HOTKEY => {
            // the hooks can miss a game that went full screen without any event we see; never act on
            // a press meant for it (re-checking also releases the keys for the next press)
            check_fullscreen();
            let stop_rec = wp.0 as i32 == HK_RECORD && with(|s| s.recording) == Some(true);
            let k = match wp.0 as i32 {
                HK_CAPTURE => K::Capture,
                HK_DELAY => K::CaptureDelayed,
                HK_RECORD => K::Record,
                _ => K::PinClipboard,
            };
            let paused = with(|s| s.suspended && (s.paused_keys.is_empty() || s.paused_keys.contains(&k)));
            if paused == Some(true) && !stop_rec {
                return LRESULT(0);
            }
            match wp.0 as i32 {
                HK_CAPTURE => command(CMD_CAPTURE),
                HK_DELAY => command(CMD_DELAY),
                HK_RECORD => command(CMD_RECORD),
                HK_PIN => command(CMD_PIN),
                _ => {}
            }
            LRESULT(0)
        }
        WM_TRAY => {
            match (lp.0 & 0xFFFF) as u32 {
                NIN_SELECT | 0x401 => {
                    if sync_recording() {
                        command(CMD_RECORD); // stop
                    } else {
                        // let the taskbar finish its click before freezing the screen
                        SetTimer(Some(hwnd), 1, 150, None);
                    }
                }
                WM_CONTEXTMENU => tray_menu(),
                NIN_BALLOONUSERCLICK => {
                    if let Some(Some(p)) = with(|s| s.last_file.clone()) {
                        let s = p.to_string_lossy();
                        if s == UPDATE_MARK {
                            command(CMD_UPDATE);
                        } else if s.starts_with("https://") || s.starts_with("http://") || s.starts_with("ms-settings:")
                        {
                            // an uploaded capture: open its link
                            let u = output::wide(&s);
                            ShellExecuteW(None, w!("open"), PCWSTR(u.as_ptr()), None, None, SW_SHOWNORMAL);
                        } else {
                            open_path(&p, true);
                        }
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        // exclusive full screen switches the display mode
        WM_DISPLAYCHANGE => {
            check_fullscreen();
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_FULLSCREEN => {
            check_fullscreen();
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_UPDATE => {
            SetTimer(Some(hwnd), TIMER_UPDATE, 86_400_000, None);
            update::clean_temp();
            if with(|s| s.cfg.check_updates) == Some(true) {
                spawn_update("check");
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_EXPIRE => {
            history::expire(with(|s| s.cfg.history_days).unwrap_or(0));
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_CANCEL_DELAY => {
            if cancel_countdown() || !delay_starting() {
                let _ = KillTimer(Some(hwnd), wp.0);
                DELAY_SPAWNED.with(|d| d.set(None));
            }
            LRESULT(0)
        }
        WM_TIMER => {
            let _ = KillTimer(Some(hwnd), wp.0);
            if wp.0 == 1 {
                command(CMD_CAPTURE);
            }
            LRESULT(0)
        }
        WM_APP_CMD => {
            command(wp.0);
            LRESULT(0)
        }
        WM_COPYDATA => {
            let cds = &*(lp.0 as *const COPYDATASTRUCT);
            let s = if cds.lpData.is_null() {
                String::new()
            } else {
                String::from_utf16_lossy(std::slice::from_raw_parts(cds.lpData as *const u16, cds.cbData as usize / 2))
            };
            match cds.dwData {
                IPC_NOTIFY => {
                    let (t, b) = s.split_once('\u{1}').unwrap_or(("WispShot", s.as_str()));
                    notify(t, b);
                }
                IPC_UPDATE if update::newer(&s) => {
                    // one notice per new version; the tray menu keeps offering it
                    if with(|st| st.update.replace(s.clone()) != Some(s.clone())) == Some(true) {
                        with(|st| st.last_file = Some(PathBuf::from(UPDATE_MARK)));
                        notify(&tf("update.ready_title", &[&s]), t("update.ready"));
                    }
                }
                IPC_OCR_HINT if !OCR_HINTED.swap(true, Ordering::Relaxed) => {
                    // a click on the notice opens the language settings
                    with(|st| st.last_file = Some(PathBuf::from("ms-settings:regionlanguage")));
                    notify(&tf("ocr.hint_title", &[&s]), &tf("ocr.hint_body", &[&s]));
                }
                IPC_REC_ON | IPC_REC_OFF => {
                    with(|st| st.recording = cds.dwData == IPC_REC_ON);
                    if with(|st| st.suspended) == Some(true) {
                        register_hotkeys(false); // the stop key follows the recording
                    }
                    tray(NIM_MODIFY);
                }
                // what a click on the next notice opens: only an upload link or a file that exists
                // (any program on the desktop can send this message)
                IPC_LAST_FILE
                    if is_link(&s) || {
                        let p = std::path::Path::new(&s);
                        p.is_absolute() && !s.contains('"') && p.exists()
                    } =>
                {
                    with(|st| st.last_file = Some(PathBuf::from(&s)));
                }
                IPC_HOTKEYS => {
                    with(|st| st.held = s == "0");
                    register_hotkeys(false);
                }
                IPC_RELOAD => {
                    let _ = PostMessageW(Some(hwnd), WM_APP_CMD, WPARAM(CMD_RELOAD), LPARAM(0));
                }
                _ => {}
            }
            LRESULT(1)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => {
            let tc = with(|s| s.taskbar_created).unwrap_or(0);
            if msg == tc && tc != 0 {
                // Explorer restarted: put the icon back (stability)
                tray(NIM_ADD);
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
    }
}
