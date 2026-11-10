//! OCR through the engine built into Windows (Windows.Media.Ocr): no model files,
//! no bundled runtime, nothing loaded until the moment it is used. Runs on a
//! short-lived worker thread so the UI never blocks.

use crate::img::Img;
use crate::output;
use windows::core::*;
use windows::Graphics::Imaging::*;
use windows::Media::Ocr::*;
use windows::Security::Cryptography::CryptographicBuffer;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WinRT::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub const WM_APP_OCR: u32 = WM_APP + 30;

fn recognize(img: &Img) -> Result<String> {
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
    let engine = OcrEngine::TryCreateFromUserProfileLanguages()?;
    let max = OcrEngine::MaxImageDimension().unwrap_or(10000) as i32;
    // Small text is recognised far better when upscaled.
    let mut f = 1;
    while img.w * (f + 1) <= max && img.h * (f + 1) <= max && img.h * f < 300 && f < 4 {
        f += 1;
    }
    let scaled;
    let src = if f > 1 {
        scaled = img.upscale(f);
        &scaled
    } else {
        img
    };
    let (w, h) = (src.w.min(max), src.h.min(max));
    let cropped;
    let src = if w != src.w || h != src.h {
        cropped = src.crop(0, 0, w, h);
        &cropped
    } else {
        src
    };
    let buf = CryptographicBuffer::CreateFromByteArray(src.as_bytes())?;
    let bmp = SoftwareBitmap::CreateCopyFromBuffer(&buf, BitmapPixelFormat::Bgra8, src.w, src.h)?;
    let res = engine.RecognizeAsync(&bmp)?.join()?;
    let mut out = String::new();
    for line in res.Lines()? {
        if !out.is_empty() {
            out.push_str("\r\n");
        }
        out.push_str(&line.Text()?.to_string_lossy());
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
            let font = CreateFontW((-15.0 * dpi) as i32, 0, 0, 0, 400, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, w!("Segoe UI"));
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
