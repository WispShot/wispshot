//! OCR through the engine built into Windows (Windows.Media.Ocr): no model files,
//! no bundled runtime, nothing loaded until the moment it is used. Runs on a
//! short-lived worker thread so the UI never blocks.

use crate::img::Img;
use crate::output;
use std::sync::RwLock;
use windows::core::*;
use windows::Globalization::Language;
use windows::Graphics::Imaging::*;
use windows::Media::Ocr::*;
use windows::Security::Cryptography::CryptographicBuffer;
use windows::System::UserProfile::GlobalizationPreferences;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WinRT::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub const WM_APP_OCR: u32 = WM_APP + 30;

/// `[capture] ocr_language` from the settings: "auto" (or empty) or a language tag.
static LANGUAGE: RwLock<String> = RwLock::new(String::new());

pub fn set_language(tag: &str) {
    if let Ok(mut l) = LANGUAGE.write() {
        *l = tag.trim().to_string();
    }
}

/// Upscaled images stay under this many pixels (memory and time stay small).
const PIXEL_BUDGET: i64 = 9_000_000;

/// A word "looks native" for a language with extra letters when it has a non-ASCII letter.
/// Text in that language has many such words; English read by its engine has only a few
/// misreads ("Wındow"). Below this share the text is taken to be English.
const NATIVE_SHARE: f32 = 0.12;

fn engine_for(lang: &Language) -> Option<OcrEngine> {
    if OcrEngine::IsLanguageSupported(lang).unwrap_or(false) {
        OcrEngine::TryCreateFromLanguage(lang).ok()
    } else {
        None
    }
}

fn is_english(tag: &str) -> bool {
    let t = tag.to_ascii_lowercase();
    t == "en" || t.starts_with("en-")
}

/// The engine to read with, and an English one to fall back on when the text turns out to
/// be English. The engine Windows picks on its own (the first profile language) reads
/// Turkish badly on an English Windows and English badly on a Turkish one, so with a
/// Latin-script language besides English in the user's list we read with that first.
/// The languages the user works in: the Windows language list, then the keyboard layouts.
/// Many work PCs run an English Windows with a Turkish keyboard added under English, so
/// Turkish is only seen in the keyboard list.
fn user_languages() -> Vec<HSTRING> {
    let mut out: Vec<HSTRING> =
        GlobalizationPreferences::Languages().map(|v| v.into_iter().collect()).unwrap_or_default();
    unsafe {
        use windows::Win32::Globalization::LCIDToLocaleName;
        use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyboardLayoutList, HKL};
        let n = GetKeyboardLayoutList(None);
        let mut hkls = vec![HKL::default(); n.max(0) as usize];
        let n = GetKeyboardLayoutList(Some(&mut hkls)).max(0) as usize;
        for h in &hkls[..n] {
            let langid = (h.0 as usize & 0xFFFF) as u32;
            let mut buf = [0u16; 85];
            let len = LCIDToLocaleName(langid, Some(&mut buf), 0);
            if len > 1 {
                let tag = HSTRING::from_wide(&buf[..len as usize - 1]);
                let base = |t: &HSTRING| t.to_string_lossy().split('-').next().unwrap_or("").to_ascii_lowercase();
                if !out.iter().any(|o| base(o) == base(&tag)) {
                    out.push(tag);
                }
            }
        }
    }
    out
}

fn engines() -> Result<(OcrEngine, Option<OcrEngine>)> {
    let want = LANGUAGE.read().map(|l| l.clone()).unwrap_or_default();
    if !want.is_empty() && !want.eq_ignore_ascii_case("auto") {
        if let Some(e) = Language::CreateLanguage(&HSTRING::from(&want)).ok().and_then(|l| engine_for(&l)) {
            return Ok((e, None));
        }
    }
    let mut native = None;
    let mut english = None;
    for tag in user_languages() {
        let Ok(lang) = Language::CreateLanguage(&tag) else { continue };
        if !OcrEngine::IsLanguageSupported(&lang).unwrap_or(false) {
            continue;
        }
        if is_english(&tag.to_string_lossy()) {
            english.get_or_insert(lang);
        } else if native.is_none() && lang.Script().map(|s| s == "Latn").unwrap_or(false) {
            native = Some(lang);
        }
    }
    let Some(native) = native.and_then(|l| engine_for(&l)) else {
        return Ok((OcrEngine::TryCreateFromUserProfileLanguages()?, None));
    };
    if english.is_none() {
        english = OcrEngine::AvailableRecognizerLanguages().ok().and_then(|v| {
            v.into_iter().find(|l| l.LanguageTag().map(|t| is_english(&t.to_string_lossy())).unwrap_or(false))
        });
    }
    Ok((native, english.and_then(|l| engine_for(&l))))
}

/// The user's main non-English Latin-script language (from the language list or a keyboard),
/// by its own name ("Türkçe"), when Windows has no OCR pack for it: text in it is then read
/// with the wrong letters.
pub fn missing_language() -> Option<String> {
    let want = LANGUAGE.read().map(|l| l.clone()).unwrap_or_default();
    if !want.is_empty() && !want.eq_ignore_ascii_case("auto") {
        return None;
    }
    // the first such language decides; one with a pack means nothing is missing
    for tag in user_languages() {
        let Ok(lang) = Language::CreateLanguage(&tag) else { continue };
        if is_english(&tag.to_string_lossy()) || !lang.Script().map(|s| s == "Latn").unwrap_or(false) {
            continue;
        }
        if OcrEngine::IsLanguageSupported(&lang).unwrap_or(false) {
            return None;
        }
        return lang.NativeName().ok().map(|n| n.to_string_lossy());
    }
    None
}

/// Whether enough of the words have letters beyond ASCII (see NATIVE_SHARE).
fn looks_native(res: &OcrResult) -> bool {
    let (mut words, mut native) = (0, 0);
    for line in res.Lines().into_iter().flatten() {
        for w in line.Words().into_iter().flatten() {
            let t = w.Text().map(|t| t.to_string_lossy()).unwrap_or_default();
            if t.chars().any(|c| c.is_alphabetic()) {
                words += 1;
                if t.chars().any(|c| c.is_alphabetic() && !c.is_ascii()) {
                    native += 1;
                }
            }
        }
    }
    words == 0 || native as f32 >= words as f32 * NATIVE_SHARE
}

/// Run the engine; returns the result and the upscale factor used.
fn run(img: &Img) -> Result<(OcrResult, i32)> {
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
    let max = OcrEngine::MaxImageDimension().unwrap_or(10000) as i32;
    // Screen text is small for the engine: 2-3x (smoothly) reads it much better.
    let fits =
        |f: i32| img.w * f <= max && img.h * f <= max && (img.w as i64 * img.h as i64) * (f * f) as i64 <= PIXEL_BUDGET;
    let f = [3, 2].into_iter().find(|&f| fits(f)).unwrap_or(1);
    let bmp = if f > 1 {
        let bmp = SoftwareBitmap::Create(BitmapPixelFormat::Bgra8, img.w * f, img.h * f)?;
        {
            let buf = bmp.LockBuffer(BitmapBufferAccessMode::Write)?;
            let plane = buf.GetPlaneDescription(0)?;
            let (start, stride) = (plane.StartIndex.max(0) as usize, plane.Stride.max(0) as usize);
            let mref = buf.CreateReference()?;
            let access: IMemoryBufferByteAccess = mref.cast()?;
            let (mut p, mut cap) = (std::ptr::null_mut(), 0u32);
            unsafe { access.GetBuffer(&mut p, &mut cap)? };
            let need = start + stride * (img.h * f) as usize;
            if p.is_null() || stride < (img.w * f * 4) as usize || (cap as usize) < need {
                return Err(Error::from(E_UNEXPECTED));
            }
            let dst = unsafe { std::slice::from_raw_parts_mut(p.add(start), cap as usize - start) };
            img.upscale_into(f, dst, stride);
            mref.Close()?;
            buf.Close()?;
        }
        bmp
    } else {
        let (w, h) = (img.w.min(max), img.h.min(max));
        let cropped;
        let src = if w != img.w || h != img.h {
            cropped = img.crop(0, 0, w, h);
            &cropped
        } else {
            img
        };
        let buf = CryptographicBuffer::CreateFromByteArray(src.as_bytes())?;
        SoftwareBitmap::CreateCopyFromBuffer(&buf, BitmapPixelFormat::Bgra8, src.w, src.h)?
    };
    let (engine, english) = engines()?;
    let res = engine.RecognizeAsync(&bmp)?.join()?;
    if let Some(en) = english {
        if !looks_native(&res) {
            return Ok((en.RecognizeAsync(&bmp)?.join()?, f));
        }
    }
    Ok((res, f))
}

pub fn recognize(img: &Img) -> Result<String> {
    let (res, _) = run(img)?;
    let mut out = String::new();
    for line in res.Lines()? {
        if !out.is_empty() {
            out.push_str("\r\n");
        }
        out.push_str(&line.Text()?.to_string_lossy());
    }
    Ok(out)
}

/// One recognised word and its box in `img` pixels (left, top, right, bottom).
pub type Word = (String, [f32; 4]);

/// The text line by line, word by word, with positions.
pub fn lines(img: &Img) -> Result<Vec<Vec<Word>>> {
    let (res, f) = run(img)?;
    let f = f as f32;
    let mut out = vec![];
    for line in res.Lines()? {
        let mut ws = vec![];
        for w in line.Words()? {
            let r = w.BoundingRect()?;
            ws.push((w.Text()?.to_string_lossy(), [r.X / f, r.Y / f, (r.X + r.Width) / f, (r.Y + r.Height) / f]));
        }
        out.push(ws);
    }
    Ok(out)
}

/// Starts OCR in the background. The result (Some(text) / None on failure) is
/// posted to `notify` as WM_APP_OCR with a boxed String in LPARAM.
pub fn start(img: Img, notify: HWND, anchor: RECT) {
    let h = notify.0 as isize;
    std::thread::spawn(move || {
        let text = match recognize(&img) {
            Ok(t) => t,
            Err(e) => format!("\u{0}{}", e.message()),
        };
        let payload = Box::new((text, anchor));
        unsafe {
            let p = Box::into_raw(payload);
            if PostMessageW(Some(HWND(h as *mut _)), WM_APP_OCR, WPARAM(0), LPARAM(p as isize)).is_err() {
                drop(Box::from_raw(p));
            }
        }
    });
}

// ------------------------------------------------------------ result window

pub fn show_result(text: &str, near: RECT) {
    unsafe {
        let inst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: inst.into(),
            lpszClassName: w!("WispOcr"),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH((COLOR_WINDOW.0 + 1) as isize as *mut _),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let dpi = windows::Win32::UI::HiDpi::GetDpiForSystem() as f32 / 96.0;
        let (w, h) = ((520.0 * dpi) as i32, (320.0 * dpi) as i32);
        let mut x = near.left;
        let mut y = near.bottom + 8;
        let vs = crate::capture::virtual_screen();
        if x + w > vs.right {
            x = vs.right - w;
        }
        if y + h > vs.bottom {
            y = (near.top - h - 8).max(vs.top);
        }
        let title = output::wide(crate::i18n::t("ocr.title"));
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            w!("WispOcr"),
            PCWSTR(title.as_ptr()),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            x,
            y,
            w,
            h,
            None,
            None,
            Some(inst.into()),
            None,
        ) else {
            return;
        };
        let t = output::wide(&text.replace("\r\n", "\n").replace('\n', "\r\n"));
        let edit = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("EDIT"),
            PCWSTR(t.as_ptr()),
            WS_CHILD | WS_VISIBLE | WS_VSCROLL | WINDOW_STYLE((ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN) as u32),
            0,
            0,
            w,
            h,
            Some(hwnd),
            None,
            Some(inst.into()),
            None,
        );
        if let Ok(edit) = edit {
            let font = CreateFontW(
                (-15.0 * dpi) as i32,
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
                w!("Segoe UI"),
            );
            SendMessageW(edit, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, font.0 as isize);
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(edit));
        }
        let _ = SetForegroundWindow(hwnd);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_SIZE => {
            if let Ok(edit) = GetWindow(hwnd, GW_CHILD) {
                let (w, h) = ((lp.0 & 0xFFFF) as i32, ((lp.0 >> 16) & 0xFFFF) as i32);
                let _ = MoveWindow(edit, 0, 0, w, h, true);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            crate::session::poke();
            let f = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
            if f != 0 {
                let _ = DeleteObject(HGDIOBJ(f as *mut _));
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// Called from the app's message loop: Esc closes, Ctrl+A selects all.
pub fn pretranslate(msg: &MSG) -> bool {
    unsafe {
        if msg.message != WM_KEYDOWN {
            return false;
        }
        let root = GetAncestor(msg.hwnd, GA_ROOT);
        if root.is_invalid() {
            return false;
        }
        let mut cls = [0u16; 16];
        let n = GetClassNameW(root, &mut cls) as usize;
        if String::from_utf16_lossy(&cls[..n]) != "WispOcr" {
            return false;
        }
        use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_ESCAPE};
        let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
        if msg.wParam.0 == VK_ESCAPE.0 as usize {
            let _ = DestroyWindow(root);
            return true;
        }
        if ctrl && msg.wParam.0 == b'A' as usize {
            SendMessageW(msg.hwnd, windows::Win32::UI::Controls::EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
            return true;
        }
        false
    }
}
