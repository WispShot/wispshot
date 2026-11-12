//! Programs for the "keep their own keys" list in Settings: the ones with a window open
//! now, their names (the file description, "SAP Logon") and icons. Only the settings
//! process reads these, while its window is open.

use crate::img::Img;
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::UI::Shell::SHDefExtractIconW;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Programs never offered: the shell's hosts stand for many apps at once.
const NOT_AN_APP: [&str; 5] =
    ["applicationframehost.exe", "textinputhost.exe", "shellexperiencehost.exe", "searchhost.exe", "explorer.exe"];

pub struct Prog {
    /// Lowercase file name ("saplogon.exe").
    pub exe: String,
    pub path: String,
    pub name: String,
    pub icon: Option<Img>,
}

/// The programs with a visible window, by name; icons `size` px.
pub fn running(size: i32) -> Vec<Prog> {
    unsafe extern "system" fn each(h: HWND, lp: LPARAM) -> BOOL {
        let found = &mut *(lp.0 as *mut Vec<String>);
        let ex = GetWindowLongW(h, GWL_EXSTYLE) as u32;
        let owned = GetWindow(h, GW_OWNER).map(|o| !o.is_invalid()).unwrap_or(false);
        if !IsWindowVisible(h).as_bool() || ex & WS_EX_TOOLWINDOW.0 != 0 || owned || crate::capture::is_cloaked(h) {
            return true.into();
        }
        if GetWindowTextLengthW(h) == 0 {
            return true.into();
        }
        if let Some(p) = crate::window_path(h) {
            if !found.iter().any(|f| f.eq_ignore_ascii_case(&p)) {
                found.push(p);
            }
        }
        true.into()
    }
    let mut paths: Vec<String> = vec![];
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut paths as *mut _ as isize));
    }
    let mut out: Vec<Prog> = paths
        .into_iter()
        .filter_map(|path| {
            let exe = file_name(&path);
            (!NOT_AN_APP.contains(&exe.as_str())).then(|| Prog {
                name: describe(&path).unwrap_or_else(|| pretty(&exe)),
                icon: icon(&path, size),
                exe,
                path,
            })
        })
        .collect();
    out.sort_by_key(|p| p.name.to_lowercase());
    out
}

/// Name and icon of a saved program; `path` may be empty (then a running copy is looked up).
pub fn info(exe: &str, path: &str, size: i32) -> (String, Option<Img>, String) {
    let path = if path.is_empty() { find_running(exe).unwrap_or_default() } else { path.to_string() };
    if path.is_empty() {
        return (pretty(exe), None, path);
    }
    (describe(&path).unwrap_or_else(|| pretty(exe)), icon(&path, size), path)
}

fn find_running(exe: &str) -> Option<String> {
    unsafe extern "system" fn each(h: HWND, lp: LPARAM) -> BOOL {
        let ctx = &mut *(lp.0 as *mut (String, Option<String>));
        if let Some(p) = crate::window_path(h) {
            if file_name(&p) == ctx.0 {
                ctx.1 = Some(p);
                return false.into();
            }
        }
        true.into()
    }
    let mut ctx = (exe.to_string(), None);
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut ctx as *mut _ as isize));
    }
    ctx.1
}

pub fn file_name(path: &str) -> String {
    path.rsplit(['\\', '/']).next().unwrap_or(path).to_lowercase()
}

/// "saplogon.exe" -> "Saplogon"
fn pretty(exe: &str) -> String {
    let stem = exe.strip_suffix(".exe").unwrap_or(exe);
    let mut c = stem.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// The FileDescription from the program's version resource.
fn describe(path: &str) -> Option<String> {
    let w = crate::output::wide(path);
    unsafe {
        let n = GetFileVersionInfoSizeW(PCWSTR(w.as_ptr()), None);
        if n == 0 {
            return None;
        }
        let mut buf = vec![0u8; n as usize];
        GetFileVersionInfoW(PCWSTR(w.as_ptr()), Some(0), n, buf.as_mut_ptr() as *mut _).ok()?;
        let mut p: *mut core::ffi::c_void = std::ptr::null_mut();
        let mut len = 0u32;
        let mut langs = vec![];
        if VerQueryValueW(buf.as_ptr() as *const _, w!("\\VarFileInfo\\Translation"), &mut p, &mut len).as_bool()
            && len >= 4
        {
            let t = std::slice::from_raw_parts(p as *const u16, len as usize / 2);
            for pair in t.chunks(2) {
                if pair.len() == 2 {
                    langs.push(format!("{:04x}{:04x}", pair[0], pair[1]));
                }
            }
        }
        langs.extend(["040904b0".to_string(), "040904e4".to_string()]);
        for l in langs {
            let q = crate::output::wide(&format!("\\StringFileInfo\\{l}\\FileDescription"));
            if VerQueryValueW(buf.as_ptr() as *const _, PCWSTR(q.as_ptr()), &mut p, &mut len).as_bool() && len > 1 {
                let s = String::from_utf16_lossy(std::slice::from_raw_parts(p as *const u16, len as usize));
                let s = s.trim_end_matches('\0').trim().to_string();
                if !s.is_empty() {
                    return Some(s);
                }
            }
        }
        None
    }
}

/// The program's icon as straight-alpha BGRA pixels, `size` x `size`.
fn icon(path: &str, size: i32) -> Option<Img> {
    let w = crate::output::wide(path);
    unsafe {
        let mut hicon = HICON::default();
        SHDefExtractIconW(PCWSTR(w.as_ptr()), 0, 0, Some(&mut hicon), None, size as u32).ok().ok()?;
        if hicon.is_invalid() {
            return None;
        }
        let mut ii = ICONINFO::default();
        let got = GetIconInfo(hicon, &mut ii).is_ok();
        let out = got.then(|| {
            let dc = CreateCompatibleDC(None);
            let mut bmi = BITMAPINFO {
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
            let mut px = vec![0u32; (size * size) as usize];
            let ok = !ii.hbmColor.is_invalid()
                && GetDIBits(
                    dc,
                    ii.hbmColor,
                    0,
                    size as u32,
                    Some(px.as_mut_ptr() as *mut _),
                    &mut bmi,
                    DIB_RGB_COLORS,
                ) != 0;
            if ok && px.iter().all(|p| p >> 24 == 0) {
                // an old icon without alpha: its mask says what is transparent
                let mut mask = vec![0u32; (size * size) as usize];
                if GetDIBits(
                    dc,
                    ii.hbmMask,
                    0,
                    size as u32,
                    Some(mask.as_mut_ptr() as *mut _),
                    &mut bmi,
                    DIB_RGB_COLORS,
                ) != 0
                {
                    for (p, m) in px.iter_mut().zip(&mask) {
                        if m & 0xFF_FFFF == 0 {
                            *p |= 0xFF00_0000;
                        }
                    }
                }
            }
            let _ = DeleteDC(dc);
            ok.then_some(Img { w: size, h: size, px })
        });
        if !ii.hbmColor.is_invalid() {
            let _ = DeleteObject(ii.hbmColor.into());
        }
        if !ii.hbmMask.is_invalid() {
            let _ = DeleteObject(ii.hbmMask.into());
        }
        let _ = DestroyIcon(hicon);
        out.flatten()
    }
}
