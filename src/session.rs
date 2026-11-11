//! Short-lived worker process: one capture / recording / pin session.
//! Exits by itself when its last window closes.

use crate::config::{self, Config};
use crate::i18n::{t, tf};
use crate::{ipc_send, notify, IPC_LAST_FILE, IPC_REC_OFF, IPC_REC_ON, WM_APP_REC_STOP};
use crate::{ocr, output, overlay, pin, record, recui};
use std::cell::RefCell;
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::*;

pub const WM_APP_MAYBE_EXIT: u32 = WM_APP + 40;

struct Sess {
    cfg: Config,
    rec: Option<(record::Recorder, recui::RecUi)>,
    rec_start: Option<std::time::Instant>,
    ocr_pending: bool,
    /// History images still being read by OCR in the background.
    indexing: u32,
    /// A modal save dialog is open: its message loop also dispatches our exit checks.
    dialog: bool,
    /// Uploads still on their way.
    uploading: u32,
}

thread_local! {
    static S: RefCell<Option<Sess>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Sess) -> R) -> Option<R> {
    S.with(|s| s.borrow_mut().as_mut().map(f))
}

pub fn run(kind: &str, veil: Option<HWND>, extra: &[String]) {
    crate::trace(&format!("session {} start", kind));
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let inst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(proc_),
            hInstance: inst.into(),
            lpszClassName: w!("WispSession"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            w!("WispSession"),
            w!(""),
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
        crate::set_app_hwnd(hwnd);
        let cfg = config::load();
        S.with(|s| {
            *s.borrow_mut() = Some(Sess {
                cfg: cfg.clone(),
                rec: None,
                rec_start: None,
                ocr_pending: false,
                indexing: 0,
                dialog: false,
                uploading: 0,
            })
        });

        let started = match kind {
            "record" => overlay::open(overlay::Mode::Record, cfg.magnifier, cfg.detect_elements, veil),
            "settings" => crate::settings::open(),
            "history" => crate::history::open(),
            // a history image pinned from the history window
            "pinpath" => match extra.first().and_then(|p| output::load(std::path::Path::new(p))) {
                Some(img) => {
                    let mut p = POINT::default();
                    let _ = GetCursorPos(&mut p);
                    pin::pin(img, p.x - 20, p.y - 20);
                    true
                }
                None => false,
            },
            "delay" => {
                let secs = extra.first().and_then(|s| s.parse().ok()).unwrap_or(cfg.capture_delay);
                let (mag, deep) = (cfg.magnifier, cfg.detect_elements);
                crate::countdown::start(
                    secs,
                    Box::new(move || {
                        if !overlay::open(overlay::Mode::Capture, mag, deep, None) {
                            poke();
                        }
                    }),
                )
            }
            // hand-offs from a capture session: a fresh process that never loaded Direct2D
            "pinfile" | "ocrfile" => match read_handoff(extra) {
                Some((img, r)) => {
                    if kind == "pinfile" {
                        pin::pin(img, r.left, r.top);
                    } else {
                        with(|s| s.ocr_pending = true);
                        ocr::start(img, hwnd, r);
                    }
                    true
                }
                None => false,
            },
            "pin" => match output::clipboard_image(hwnd) {
                Some(img) => {
                    let mut p = POINT::default();
                    let _ = GetCursorPos(&mut p);
                    pin::pin(img, p.x - 20, p.y - 20);
                    true
                }
                None => {
                    notify("WispShot", t("err.no_clipboard_image"));
                    false
                }
            },
            _ => overlay::open(overlay::Mode::Capture, cfg.magnifier, cfg.detect_elements, veil),
        };
        if !started {
            if let Some(v) = veil {
                let _ = PostMessageW(Some(v), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
            return;
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if ocr::pretranslate(&msg) || crate::history::pretranslate(&msg) {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        // never leave a recording unfinished
        stop_recording();
    }
    crate::trace("session end");
}

/// Ask the session to re-check whether anything is still alive.
pub fn poke() {
    if let Some(h) = crate::app_hwnd() {
        unsafe {
            let _ = PostMessageW(Some(h), WM_APP_MAYBE_EXIT, WPARAM(0), LPARAM(0));
        }
    }
}

fn has_windows() -> bool {
    unsafe extern "system" fn each(h: HWND, lp: LPARAM) -> BOOL {
        let mut cls = [0u16; 32];
        let n = GetClassNameW(h, &mut cls) as usize;
        let c = String::from_utf16_lossy(&cls[..n]);
        if matches!(c.as_str(), "WispOverlay" | "WispDelay" | "WispScrollBar" | "WispPin" | "WispOcr" | "WispRecPill")
            || IsWindowVisible(h).as_bool() && c != "WispSession" && c != "WispVeil"
        {
            *(lp.0 as *mut bool) = true;
            return FALSE;
        }
        TRUE
    }
    let mut any = false;
    unsafe {
        let _ = EnumThreadWindows(GetCurrentThreadId(), Some(each), LPARAM(&mut any as *mut bool as isize));
    }
    any
}

fn maybe_exit() {
    let busy =
        with(|s| s.rec.is_some() || s.ocr_pending || s.dialog || s.indexing > 0 || s.uploading > 0).unwrap_or(false);
    if !busy && !overlay::is_open() && !has_windows() {
        unsafe { PostQuitMessage(0) };
    }
}

/// Put a finished capture into the history (when it is on) and read its text.
fn keep(img: &crate::img::Img, cfg: &Config) {
    let Some(p) = crate::history::add(img, cfg) else { return };
    if cfg.history_ocr {
        if let Some(app) = crate::app_hwnd() {
            with(|s| s.indexing += 1);
            crate::history::index(img.clone(), p, app);
        }
    }
}

fn handle_done() {
    let Some(done) = overlay::DONE.with(|d| d.borrow_mut().take()) else { return };
    let Some(app) = crate::app_hwnd() else { return };
    let Some(cfg) = with(|s| s.cfg.clone()) else { return };
    use overlay::Action;
    if !done.from_pin
        && matches!(
            done.action,
            Action::Copy | Action::CopyFile | Action::Save | Action::Pin | Action::Ocr | Action::Upload
        )
    {
        if let Some(img) = &done.img {
            keep(img, &cfg);
        }
    }
    // the share frame goes on what leaves the editor (not on pins, OCR or the history)
    let style = if cfg.frame == crate::frame::Style::Off { crate::frame::Style::Color } else { cfg.frame };
    let framed = |img: &crate::img::Img| if done.frame { crate::frame::apply(img, style) } else { img.clone() };
    match done.action {
        Action::Copy => {
            if let Some(img) = done.img.as_ref().map(framed) {
                let img = &img;
                if !output::copy_image(app, img) {
                    notify("WispShot", t("err.clipboard_busy"));
                }
            }
        }
        Action::CopyColor => {
            output::copy_text(app, &done.text);
        }
        Action::CopyFile => {
            if let Some(img) = done.img.as_ref().map(framed) {
                let img = &img;
                let p = output::temp_dir().join(output::timestamp_name("WispShot", "png"));
                match output::save(img, &p) {
                    Ok(()) => {
                        output::copy_file(app, &p);
                    }
                    Err(_) => notify("WispShot", t("err.temp_file")),
                }
            }
        }
        Action::Save => {
            if let Some(img) = done.img.as_ref().map(framed) {
                let img = &img;
                let name = output::timestamp_name("WispShot", cfg.image_format.ext());
                let target =
                    if cfg.save_ask { save_dialog(app, &cfg.save_dir, &name) } else { Some(cfg.save_dir.join(&name)) };
                if let Some(p) = target {
                    if let Some(d) = p.parent() {
                        let _ = std::fs::create_dir_all(d);
                    }
                    match output::save(img, &p) {
                        Ok(()) => {
                            ipc_send(IPC_LAST_FILE, &p.to_string_lossy());
                            if cfg.save_notify {
                                notify(t("saved.shot"), &tf("saved.click_to_show", &[&p.display().to_string()]));
                            }
                        }
                        Err(_) => notify(t("err.save"), &p.display().to_string()),
                    }
                }
            }
        }
        // Pins and OCR results can stay open for a long time: give them to a fresh
        // process so this one (which holds the GPU driver's caches) can exit now.
        Action::Pin => {
            if let Some(img) = done.img {
                if !handoff("pinfile", &img, done.rect) {
                    pin::pin(img, done.rect.left, done.rect.top);
                }
            }
        }
        Action::Ocr => {
            if let Some(img) = done.img {
                if !handoff("ocrfile", &img, done.rect) {
                    with(|s| s.ocr_pending = true);
                    ocr::start(img, app, done.rect);
                }
            }
        }
        Action::Upload => {
            if let Some(img) = done.img.as_ref().map(framed) {
                upload(app, &img);
            }
        }
        Action::Record => start_recording(done.rect, &cfg),
        Action::Scroll => {
            let ok = crate::scroll::start(
                done.rect,
                Box::new(move |img| {
                    if let Some(img) = img {
                        keep(&img, &cfg);
                        if let Some(app) = crate::app_hwnd() {
                            if output::copy_image(app, &img) {
                                notify(
                                    t("scroll.copied"),
                                    &tf("scroll.copied_body", &[&img.w.to_string(), &img.h.to_string()]),
                                );
                            }
                        }
                        let r = RECT {
                            left: done.rect.left,
                            top: done.rect.top,
                            right: done.rect.left + img.w,
                            bottom: done.rect.top + img.h,
                        };
                        if !handoff("pinfile", &img, r) {
                            pin::pin(img, r.left, r.top);
                        }
                    }
                    poke();
                }),
            );
            if !ok {
                notify("WispShot", t("scroll.failed"));
            }
        }
    }
}

/// Send the capture to the chosen host on a worker thread; the link goes to the
/// clipboard. The first time, the user is told that the link is public.
/// Asked by the editor before the first upload, while it is still open, so "No" keeps
/// the capture and its edits. True when the upload may go ahead.
pub fn confirm_upload(owner: HWND) -> bool {
    use crate::upload::Host;
    use windows::Win32::UI::WindowsAndMessaging::*;
    let now = config::load();
    if now.upload_host == Host::Custom && now.upload_url.is_empty() {
        notify(t("upload.failed"), t("upload.no_url"));
        return false;
    }
    if now.upload_asked {
        return true;
    }
    let host = now.upload_host;
    let keep = if host.hours() > 0 { tf("upload.keep_hours", &[&host.hours().to_string()]) } else { String::new() };
    let body = output::wide(&tf("upload.confirm", &[&host.site(&now.upload_url), &keep]));
    let title = output::wide(t("upload.confirm_title"));
    let r = unsafe {
        MessageBoxW(
            Some(owner),
            PCWSTR(body.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_YESNO | MB_ICONINFORMATION | MB_TOPMOST | MB_SETFOREGROUND,
        )
    };
    if r != IDYES {
        return false;
    }
    // remember it in the ini as it is now (Settings may have changed it while the question was open)
    let mut now = config::load();
    now.upload_asked = true;
    let _ = config::save(&now);
    true
}

/// Sends the capture with the upload settings as they are now (the session's copy may be
/// older than what the user just confirmed). Never sends before the user said yes once.
fn upload(app: HWND, img: &crate::img::Img) {
    use crate::upload::Host;
    let cfg = config::load();
    let host = cfg.upload_host;
    if host == Host::Custom && cfg.upload_url.is_empty() {
        notify(t("upload.failed"), t("upload.no_url"));
        return;
    }
    if !cfg.upload_asked {
        return;
    }
    let site = host.site(&cfg.upload_url);
    let Ok(png) = output::encode(img, output::Fmt::Png) else {
        notify(t("upload.failed"), t("err.temp_file"));
        return;
    };
    notify(t("upload.sending"), &site);
    with(|s| s.uploading += 1);
    let (url, field) = (cfg.upload_url.clone(), cfg.upload_field.clone());
    let target = app.0 as isize;
    let name = output::timestamp_name("WispShot", "png");
    std::thread::spawn(move || {
        let r = crate::upload::upload(host, &url, &field, &png, &name);
        let p = Box::into_raw(Box::new((host, r)));
        unsafe {
            if PostMessageW(Some(HWND(target as *mut _)), crate::upload::WM_APP_UPLOADED, WPARAM(0), LPARAM(p as isize))
                .is_err()
            {
                drop(Box::from_raw(p));
            }
        }
    });
}

/// Raw hand-off file: i32 w, i32 h, then BGRA pixels. Deleted by the receiver.
fn handoff(kind: &str, img: &crate::img::Img, r: RECT) -> bool {
    let dir = output::temp_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(output::timestamp_name("handoff", "raw"));
    let mut data = Vec::with_capacity(8 + img.px.len() * 4);
    data.extend_from_slice(&img.w.to_le_bytes());
    data.extend_from_slice(&img.h.to_le_bytes());
    data.extend_from_slice(img.as_bytes());
    if std::fs::write(&path, data).is_err() {
        return false;
    }
    let Ok(exe) = std::env::current_exe() else { return false };
    let spawned = std::process::Command::new(exe)
        .args(["--session", kind, "0"])
        .arg(&path)
        .args([r.left, r.top, r.right, r.bottom].map(|v| v.to_string()))
        .spawn()
        .is_ok();
    if !spawned {
        let _ = std::fs::remove_file(&path);
    }
    spawned
}

fn read_handoff(extra: &[String]) -> Option<(crate::img::Img, RECT)> {
    let path = std::path::PathBuf::from(extra.first()?);
    let data = std::fs::read(&path).ok();
    let _ = std::fs::remove_file(&path);
    let data = data?;
    let n = |i: usize| extra.get(i).and_then(|s| s.parse::<i32>().ok()).unwrap_or(0);
    let r = RECT { left: n(1), top: n(2), right: n(3), bottom: n(4) };
    let w = i32::from_le_bytes(data.get(0..4)?.try_into().ok()?);
    let h = i32::from_le_bytes(data.get(4..8)?.try_into().ok()?);
    if w <= 0 || h <= 0 || data.len() != 8 + (w as usize * h as usize * 4) {
        return None;
    }
    let px = data[8..].as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    Some((crate::img::Img { w, h, px }, r))
}

fn start_recording(rect: RECT, cfg: &Config) {
    if with(|s| s.rec.is_some()).unwrap_or(true) {
        return;
    }
    // "ask" mode records into the temp folder and moves the file once a location is chosen;
    // a GIF is made from a temporary MP4 there as well
    let dir = if cfg.video_ask || cfg.gif { output::temp_dir() } else { cfg.video_dir.clone() };
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(output::timestamp_name("WispShot", "mp4"));
    // without the bar there would be no way to stop: don't start at all
    let Some(ui) = recui::RecUi::show(rect, cfg) else {
        notify(t("err.recording"), t("err.rec_ui"));
        return;
    };
    let rec = record::Recorder::start(rect, cfg.fps, cfg.cursor, path);
    with(|s| {
        s.rec = Some((rec, ui));
        s.rec_start = Some(std::time::Instant::now());
    });
    ipc_send(IPC_REC_ON, "");
}

fn stop_recording() {
    let Some(Some((rec, ui))) = with(|s| s.rec.take()) else { return };
    ui.close();
    ipc_send(IPC_REC_OFF, "");
    let Some(cfg) = with(|s| s.cfg.clone()) else { return };
    match rec.stop() {
        Ok(p) => {
            let long = with(|s| s.rec_start.take()).flatten().is_some_and(|t| t.elapsed().as_secs() > 10);
            if cfg.gif && long {
                notify("WispShot", t("saved.gif_wait"));
            }
            let p = if cfg.gif { make_gif(p, &cfg) } else { p };
            let (p, kept) = if cfg.video_ask {
                place_video(&p, &cfg)
            } else if p.parent() != Some(cfg.video_dir.as_path()) {
                let dst = cfg.video_dir.join(p.file_name().unwrap_or_default());
                (if output::move_file(&p, &dst).is_ok() { dst } else { p }, false)
            } else {
                (p, false)
            };
            ipc_send(IPC_LAST_FILE, &p.to_string_lossy());
            if kept {
                // dialog cancelled: a recording is never thrown away
                notify(t("saved.video_default"), &tf("saved.click_to_show", &[&p.display().to_string()]));
            } else if cfg.video_notify {
                notify(t("saved.video"), &tf("saved.click_to_show", &[&p.display().to_string()]));
            }
        }
        Err(e) => notify(t("err.recording"), &e),
    }
}

/// Turn the temporary MP4 into a GIF next to it. On failure the MP4 is kept (a
/// recording is never thrown away) and the user is told why.
fn make_gif(mp4: std::path::PathBuf, cfg: &Config) -> std::path::PathBuf {
    let gif = mp4.with_extension("gif");
    match crate::gif::from_mp4(&mp4, &gif, cfg.gif_fps, cfg.gif_width) {
        Ok(()) => {
            let _ = std::fs::remove_file(&mp4);
            gif
        }
        Err(e) => {
            let _ = std::fs::remove_file(&gif);
            notify(t("err.gif"), &e);
            mp4
        }
    }
}

/// Save dialog that keeps the session alive while it is open.
fn save_dialog(owner: HWND, dir: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    with(|s| s.dialog = true);
    // The owner is our invisible 0x0 window; the dialog centres on it, so put it in the
    // middle of the monitor the user is looking at (instead of the top-left corner).
    unsafe {
        use windows::Win32::Graphics::Gdi::*;
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST), &mut mi).as_bool() {
            let w = mi.rcWork;
            // the dialog puts its top-left corner at the owner: offset by about half its size
            let k = windows::Win32::UI::HiDpi::GetDpiForWindow(owner).max(96) as i32;
            let (dw, dh) = (680 * k / 96, 520 * k / 96);
            let x = ((w.left + w.right - dw) / 2).max(w.left);
            let y = ((w.top + w.bottom - dh) / 2).max(w.top);
            let _ = SetWindowPos(owner, None, x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }
    let r = output::save_dialog(owner, dir, name);
    with(|s| s.dialog = false);
    r
}

/// Ask where the finished recording goes. Cancel keeps it in the default video folder.
/// Returns the final path and whether it ended up there because of a cancel.
fn place_video(tmp: &std::path::Path, cfg: &Config) -> (std::path::PathBuf, bool) {
    let name = tmp
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| output::timestamp_name("WispShot", "mp4"));
    let owner = crate::app_hwnd().unwrap_or_default();
    let (dst, kept) = match save_dialog(owner, &cfg.video_dir, &name) {
        Some(p) => (p, false),
        None => (cfg.video_dir.join(&name), true),
    };
    match output::move_file(tmp, &dst) {
        Ok(()) => (dst, kept),
        Err(_) => (tmp.to_path_buf(), kept),
    }
}

pub fn rec_finished() -> bool {
    with(|s| s.rec.as_ref().is_some_and(|r| r.0.finished())).unwrap_or(false)
}

unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        overlay::WM_APP_DONE => {
            handle_done();
            maybe_exit();
            LRESULT(0)
        }
        WM_APP_REC_STOP => {
            stop_recording();
            maybe_exit();
            LRESULT(0)
        }
        WM_APP_MAYBE_EXIT => {
            maybe_exit();
            LRESULT(0)
        }
        crate::upload::WM_APP_UPLOADED => {
            let (host, r) = *Box::from_raw(lp.0 as *mut (crate::upload::Host, std::result::Result<String, String>));
            with(|s| s.uploading = s.uploading.saturating_sub(1));
            match r {
                Ok(link) => {
                    output::copy_text(hwnd, &link);
                    ipc_send(IPC_LAST_FILE, &link);
                    let body = if host.hours() > 0 {
                        tf("upload.done_body", &[&link, &host.hours().to_string()])
                    } else {
                        link.clone()
                    };
                    notify(t("upload.done"), &body);
                }
                Err(e) => notify(t("upload.failed"), &tf("upload.failed_body", &[&e])),
            }
            maybe_exit();
            LRESULT(0)
        }
        crate::history::WM_APP_INDEXED => {
            with(|s| s.indexing = s.indexing.saturating_sub(1));
            maybe_exit();
            LRESULT(0)
        }
        ocr::WM_APP_OCR => {
            let b = Box::from_raw(lp.0 as *mut (String, RECT));
            let (text, rect) = *b;
            with(|s| s.ocr_pending = false);
            if let Some(err) = text.strip_prefix('\u{0}') {
                notify(t("ocr.unavailable"), &tf("ocr.need_language", &[err]));
            } else if text.trim().is_empty() {
                notify("OCR", t("ocr.no_text"));
            } else {
                output::copy_text(hwnd, &text);
                ocr::show_result(&text, rect);
            }
            maybe_exit();
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
