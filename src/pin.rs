//! Pinned images: borderless always-on-top windows drawn with plain GDI.
//! Drag to move, wheel = zoom, Ctrl+wheel = opacity, double-click/Esc = close.

use crate::i18n::t;
use crate::img::Img;
use crate::keys::{self, Scope, K};
use crate::output;
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

struct Pin {
    img: Img,
    zoom: f32,
    alpha: u8,
    dib: HBITMAP,
    mem: HDC,
}

const ID_COPY: usize = 1;
const ID_COPYFILE: usize = 2;
const ID_SAVE: usize = 3;
const ID_OCR: usize = 4;
const ID_ZOOM100: usize = 5;
const ID_CLOSE: usize = 6;
const ID_CLOSEALL: usize = 7;

fn class() -> PCWSTR {
    w!("WispPin")
}

pub fn pin(img: Img, x: i32, y: i32) {
    if img.w <= 0 || img.h <= 0 {
        return;
    }
    unsafe {
        let inst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS | CS_DROPSHADOW,
            lpfnWndProc: Some(wndproc),
            hInstance: inst.into(),
            lpszClassName: class(),
            hCursor: LoadCursorW(None, IDC_SIZEALL).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassExW(&wc);

        // Keep the pixels in a DIB section selected into a memory DC: painting is one StretchBlt.
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        ReleaseDC(None, screen);
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: img.w,
                biHeight: -img.h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let Ok(dib) = CreateDIBSection(Some(mem), &bi, DIB_RGB_COLORS, &mut bits, None, 0) else {
            let _ = DeleteDC(mem);
            return;
        };
        std::ptr::copy_nonoverlapping(img.px.as_ptr(), bits as *mut u32, img.px.len());
        SelectObject(mem, dib.into());

        // larger than the screen (a scrolling capture, a big clipboard image): start shrunk to fit
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST), &mut mi);
        let wa = mi.rcWork;
        let (aw, ah) = ((wa.right - wa.left) as f32 * 0.9, (wa.bottom - wa.top) as f32 * 0.9);
        let zoom = (aw / img.w as f32).min(ah / img.h as f32).clamp(0.1, 1.0);
        let (w, h) = (((img.w as f32 * zoom).round() as i32).max(8), ((img.h as f32 * zoom).round() as i32).max(8));
        let x = x.min(wa.right - w).max(wa.left);
        let y = y.min(wa.bottom - h).max(wa.top);
        let st = Box::into_raw(Box::new(Pin { img, zoom, alpha: 255, dib, mem }));
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
            class(),
            w!("WispShot Pin"),
            WS_POPUP,
            x,
            y,
            w,
            h,
            None,
            None,
            Some(inst.into()),
            Some(st as *const _),
        );
        match hwnd {
            Ok(hwnd) => {
                let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
                let _ = ShowWindow(hwnd, SW_SHOW);
                let _ = SetForegroundWindow(hwnd);
            }
            Err(_) => {
                let p = Box::from_raw(st);
                let _ = DeleteDC(p.mem);
                let _ = DeleteObject(p.dib.into());
            }
        }
    }
}

fn apply_zoom(hwnd: HWND, p: &mut Pin, z: f32, anchor: Option<POINT>) {
    let z = z.clamp(0.1, 8.0);
    unsafe {
        let mut r = RECT::default();
        let _ = GetWindowRect(hwnd, &mut r);
        let (ow, oh) = ((r.right - r.left) as f32, (r.bottom - r.top) as f32);
        let nw = ((p.img.w as f32 * z).round() as i32).max(8);
        let nh = ((p.img.h as f32 * z).round() as i32).max(8);
        let (ax, ay) = match anchor {
            Some(a) => (a.x as f32, a.y as f32),
            None => (r.left as f32 + ow / 2.0, r.top as f32 + oh / 2.0),
        };
        let fx = (ax - r.left as f32) / ow.max(1.0);
        let fy = (ay - r.top as f32) / oh.max(1.0);
        let nx = (ax - fx * nw as f32).round() as i32;
        let ny = (ay - fy * nh as f32).round() as i32;
        p.zoom = z;
        let _ = SetWindowPos(hwnd, None, nx, ny, nw, nh, SWP_NOZORDER | SWP_NOACTIVATE);
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

/// `command` from inside the window procedure's key handler.
unsafe fn command_r(hwnd: HWND, id: usize) -> LRESULT {
    command(hwnd, id);
    LRESULT(0)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = &*(lp.0 as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Pin;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    match msg {
        WM_PAINT => {
            let p = &*ptr;
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            let mut r = RECT::default();
            let _ = GetClientRect(hwnd, &mut r);
            if (p.zoom - 1.0).abs() < 0.001 {
                let _ = BitBlt(dc, 0, 0, r.right, r.bottom, Some(p.mem), 0, 0, SRCCOPY);
            } else {
                SetStretchBltMode(dc, if p.zoom < 1.0 { HALFTONE } else { COLORONCOLOR });
                let _ = StretchBlt(dc, 0, 0, r.right, r.bottom, Some(p.mem), 0, 0, p.img.w, p.img.h, SRCCOPY);
            }
            // thin accent border
            let br = CreateSolidBrush(COLORREF(0x00FF_8C2B));
            let _ = FrameRect(dc, &r, br);
            let _ = DeleteObject(br.into());
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_LBUTTONDOWN => {
            // native move loop: smooth, zero work for us
            let _ = ReleaseCapture();
            SendMessageW(hwnd, WM_NCLBUTTONDOWN, Some(WPARAM(HTCAPTION as usize)), Some(LPARAM(0)));
            LRESULT(0)
        }
        WM_LBUTTONDBLCLK | WM_NCLBUTTONDBLCLK | WM_MBUTTONUP => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let d = ((wp.0 >> 16) & 0xFFFF) as i16;
            let p = &mut *ptr;
            if GetKeyState(VK_CONTROL.0 as i32) < 0 {
                let a = (p.alpha as i32 + if d > 0 { 20 } else { -20 }).clamp(40, 255) as u8;
                p.alpha = a;
                let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), a, LWA_ALPHA);
            } else {
                let (x, y) = crate::lparam_xy(lp);
                let anchor = POINT { x, y };
                let f = if d > 0 { 1.1 } else { 1.0 / 1.1 };
                apply_zoom(hwnd, p, p.zoom * f, Some(anchor));
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            match keys::pressed(Scope::Pin, wp.0 as u16) {
                Some(K::PinCopy) => return command_r(hwnd, ID_COPY),
                Some(K::PinCopyFile) => return command_r(hwnd, ID_COPYFILE),
                Some(K::PinSave) => return command_r(hwnd, ID_SAVE),
                Some(K::PinOcr) => return command_r(hwnd, ID_OCR),
                Some(K::PinZoom) => return command_r(hwnd, ID_ZOOM100),
                _ => {}
            }
            match VIRTUAL_KEY(wp.0 as u16) {
                VK_ESCAPE => {
                    let _ = DestroyWindow(hwnd);
                }
                VK_LEFT | VK_RIGHT | VK_UP | VK_DOWN => {
                    let (dx, dy) = match VIRTUAL_KEY(wp.0 as u16) {
                        VK_LEFT => (-1, 0),
                        VK_RIGHT => (1, 0),
                        VK_UP => (0, -1),
                        _ => (0, 1),
                    };
                    let mut r = RECT::default();
                    let _ = GetWindowRect(hwnd, &mut r);
                    let _ = SetWindowPos(hwnd, None, r.left + dx, r.top + dy, 0, 0, SWP_NOSIZE | SWP_NOZORDER);
                }
                VK_NUMPAD0 => command(hwnd, ID_ZOOM100),
                _ => {}
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            let menu = CreatePopupMenu().unwrap_or_default();
            let add = |id: usize, s: &str| {
                let w = output::wide(s);
                let _ = AppendMenuW(menu, MF_STRING, id, PCWSTR(w.as_ptr()));
            };
            let with_key = |label: &str, k: K| format!("{}\t{}", label, keys::label(k));
            add(ID_COPY, &with_key(t("pin.copy"), K::PinCopy));
            add(ID_COPYFILE, &with_key(t("pin.copy_file"), K::PinCopyFile));
            add(ID_SAVE, &with_key(t("pin.save"), K::PinSave));
            add(ID_OCR, &with_key(t("pin.ocr"), K::PinOcr));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            add(ID_ZOOM100, &with_key(t("pin.zoom_100"), K::PinZoom));
            add(ID_CLOSE, &format!("{}\tEsc", t("pin.close")));
            add(ID_CLOSEALL, t("menu.close_pins"));
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let _ = SetForegroundWindow(hwnd);
            let id = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, Some(0), hwnd, None).0 as usize;
            let _ = DestroyMenu(menu);
            if id != 0 {
                command(hwnd, id);
            }
            LRESULT(0)
        }
        WM_NCDESTROY => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            let p = Box::from_raw(ptr);
            let _ = DeleteDC(p.mem);
            let _ = DeleteObject(p.dib.into());
            drop(p);
            crate::session::poke();
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// Actions needing the app (save dialog, OCR) are forwarded to the app window
/// with a copy of the image so the pin's own state is never borrowed across modal loops.
unsafe fn command(hwnd: HWND, id: usize) {
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Pin;
    if ptr.is_null() {
        return;
    }
    match id {
        ID_CLOSE => {
            let _ = DestroyWindow(hwnd);
        }
        ID_CLOSEALL => crate::close_all_pins(),
        ID_ZOOM100 => apply_zoom(hwnd, &mut *ptr, 1.0, None),
        ID_COPY => {
            output::copy_image(hwnd, &(*ptr).img);
        }
        _ => {
            let act = match id {
                ID_COPYFILE => crate::overlay::Action::CopyFile,
                ID_SAVE => crate::overlay::Action::Save,
                _ => crate::overlay::Action::Ocr,
            };
            let mut r = RECT::default();
            let _ = GetWindowRect(hwnd, &mut r);
            crate::overlay::DONE.with(|d| {
                *d.borrow_mut() = Some(crate::overlay::Done {
                    action: act,
                    img: Some((*ptr).img.clone()),
                    rect: r,
                    text: String::new(),
                })
            });
            if let Some(app) = crate::app_hwnd() {
                let _ = PostMessageW(Some(app), crate::overlay::WM_APP_DONE, WPARAM(1), LPARAM(0));
            }
        }
    }
}
