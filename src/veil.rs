//! The "veil": an instant, capture-excluded dim layer shown the moment the hotkey
//! fires, before the capture session has even started. It is owned by the resident
//! process; input that lands on it is buffered and replayed on the editor window
//! (possibly in another process) once that exists.

use std::cell::RefCell;
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::*;

/// session -> veil: editor is visible now, hide (and flush buffered input).
pub const WM_VEIL_HIDE: u32 = WM_APP + 70;
/// Marks mouse messages replayed from the veil (wParam bit; MK_* flags never use it).
pub const FORWARDED: usize = 1 << 30;
const WATCHDOG: usize = 7;

thread_local! {
    static PENDING: RefCell<Vec<(u32, WPARAM, LPARAM)>> = const { RefCell::new(Vec::new()) };
}

fn editor() -> Option<HWND> {
    unsafe { FindWindowW(w!("WispOverlay"), None).ok() }
}

pub fn show(vs: RECT) -> Option<HWND> {
    if std::env::var_os("WISP_NOVEIL").is_some() {
        return None;
    }
    unsafe {
        let inst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(proc_),
            hInstance: inst.into(),
            lpszClassName: w!("WispVeil"),
            hCursor: LoadCursorW(None, IDC_CROSS).unwrap_or_default(),
            hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let h = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_NOACTIVATE,
            w!("WispVeil"),
            w!(""),
            WS_POPUP,
            vs.left,
            vs.top,
            vs.right - vs.left,
            vs.bottom - vs.top,
            None,
            None,
            Some(inst.into()),
            None,
        )
        .ok()?;
        // Only safe if capture really excludes it (Windows 10 2004+); otherwise no veil.
        if SetWindowDisplayAffinity(h, WDA_EXCLUDEFROMCAPTURE).is_err() {
            let _ = DestroyWindow(h);
            return None;
        }
        let _ = SetLayeredWindowAttributes(h, COLORREF(0), 128, LWA_ALPHA);
        let on: BOOL = TRUE;
        let _ = DwmSetWindowAttribute(h, DWMWA_TRANSITIONS_FORCEDISABLED, &on as *const _ as *const _, 4);
        // No activation: changing the active window makes apps repaint, slowing the grab.
        let _ = ShowWindow(h, SW_SHOWNA);
        // Safety net if the session never shows up (crash, failed start).
        SetTimer(Some(h), WATCHDOG, 3000, None);
        Some(h)
    }
}

fn flush() {
    let Some(ed) = editor() else { return };
    let q = PENDING.with(|p| std::mem::take(&mut *p.borrow_mut()));
    for (m, w, l) in q {
        unsafe {
            let _ = PostMessageW(Some(ed), m, w, l);
        }
    }
}

unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_LBUTTONDBLCLK | WM_RBUTTONDOWN | WM_RBUTTONUP
        | WM_MOUSEWHEEL | WM_KEYDOWN | WM_KEYUP | WM_CHAR | WM_SYSKEYDOWN => {
            // A drag that starts on the veil stays with the veil until the button is
            // released (explicit capture), so no event is lost in the cross-process handoff.
            if matches!(msg, WM_LBUTTONDOWN | WM_RBUTTONDOWN) {
                SetCapture(hwnd);
            }
            let wp = if matches!(
                msg,
                WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_LBUTTONDBLCLK | WM_RBUTTONDOWN | WM_RBUTTONUP
            ) {
                WPARAM(wp.0 | FORWARDED)
            } else {
                wp
            };
            // keep order: everything goes through the queue
            PENDING.with(|p| p.borrow_mut().push((msg, wp, lp)));
            flush();
            if matches!(msg, WM_LBUTTONUP | WM_RBUTTONUP) {
                let _ = ReleaseCapture();
            }
            LRESULT(0)
        }
        WM_VEIL_HIDE => {
            let _ = ShowWindow(hwnd, SW_HIDE);
            flush();
            LRESULT(0)
        }
        WM_TIMER => {
            if editor().is_none() {
                let _ = DestroyWindow(hwnd);
            } else if !IsWindowVisible(hwnd).as_bool() {
                let _ = KillTimer(Some(hwnd), WATCHDOG);
            }
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_DESTROY => {
            PENDING.with(|p| p.borrow_mut().clear());
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
