//! Clipboard (image / file / text), WIC encoding, save dialog, paths.

use crate::img::Img;
use std::path::{Path, PathBuf};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::StructuredStorage::{CreateStreamOnHGlobal, GetHGlobalFromStream};
use windows::Win32::System::Com::*;
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::Memory::*;
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::*;

const CF_UNICODETEXT: u32 = 13;
const CF_DIB: u32 = 8;
const CF_HDROP: u32 = 15;

/// Image file formats Windows can write without extra codecs.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Fmt {
    Png,
    Jpg,
    Bmp,
    Tiff,
}

impl Fmt {
    pub const ALL: [Fmt; 4] = [Fmt::Png, Fmt::Jpg, Fmt::Bmp, Fmt::Tiff];

    pub fn ext(self) -> &'static str {
        match self {
            Fmt::Png => "png",
            Fmt::Jpg => "jpg",
            Fmt::Bmp => "bmp",
            Fmt::Tiff => "tif",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Fmt::Png => "PNG",
            Fmt::Jpg => "JPG",
            Fmt::Bmp => "BMP",
            Fmt::Tiff => "TIFF",
        }
    }

    /// From an ini value or a file extension.
    pub fn parse(s: &str) -> Option<Fmt> {
        match s.trim().trim_start_matches('.').to_ascii_lowercase().as_str() {
            "png" => Some(Fmt::Png),
            "jpg" | "jpeg" => Some(Fmt::Jpg),
            "bmp" => Some(Fmt::Bmp),
            "tif" | "tiff" => Some(Fmt::Tiff),
            _ => None,
        }
    }
}

pub fn fmt_for(path: &Path) -> Fmt {
    path.extension().and_then(|e| e.to_str()).and_then(Fmt::parse).unwrap_or(Fmt::Png)
}

/// JPEG quality in percent, set from the settings once per process.
static JPEG_QUALITY: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(90);

pub fn set_jpeg_quality(q: u32) {
    JPEG_QUALITY.store(q.clamp(1, 100) as u8, std::sync::atomic::Ordering::Relaxed);
}

pub fn wic() -> Result<IWICImagingFactory> {
    unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
}

/// Encoder option "ImageQuality" (0..1) for JPEG.
unsafe fn set_quality(props: &windows::Win32::System::Com::StructuredStorage::IPropertyBag2, q: f32) {
    use windows::Win32::System::Com::StructuredStorage::PROPBAG2;
    use windows::Win32::System::Variant::*;
    let mut name = wide("ImageQuality");
    let bag = PROPBAG2 { pstrName: PWSTR(name.as_mut_ptr()), ..Default::default() };
    let mut v = VARIANT::default();
    (*v.Anonymous.Anonymous).vt = VT_R4;
    (*v.Anonymous.Anonymous).Anonymous.fltVal = q;
    let _ = props.Write(1, &bag, &v);
}

/// Encode as PNG/JPG/BMP/TIFF. Opaque images are written as 24-bit (smaller files).
pub fn encode(img: &Img, fmt: Fmt) -> Result<Vec<u8>> {
    unsafe {
        let f = wic()?;
        let stream = CreateStreamOnHGlobal(HGLOBAL::default(), true)?;
        let container = match fmt {
            Fmt::Png => GUID_ContainerFormatPng,
            Fmt::Jpg => GUID_ContainerFormatJpeg,
            Fmt::Bmp => GUID_ContainerFormatBmp,
            Fmt::Tiff => GUID_ContainerFormatTiff,
        };
        let enc = f.CreateEncoder(&container, std::ptr::null())?;
        enc.Initialize(&stream, WICBitmapEncoderNoCache)?;
        let mut frame: Option<IWICBitmapFrameEncode> = None;
        let mut props: Option<windows::Win32::System::Com::StructuredStorage::IPropertyBag2> = None;
        enc.CreateNewFrame(&mut frame, &mut props)?;
        let frame = frame.ok_or_else(|| Error::from(E_FAIL))?;
        if let (Fmt::Jpg, Some(p)) = (fmt, &props) {
            set_quality(p, JPEG_QUALITY.load(std::sync::atomic::Ordering::Relaxed) as f32 / 100.0);
        }
        frame.Initialize(props.as_ref())?;
        frame.SetSize(img.w as u32, img.h as u32)?;
        // 24bpp BGR
        let stride = ((img.w * 3 + 3) & !3) as usize;
        let mut buf = vec![0u8; stride * img.h as usize];
        for y in 0..img.h as usize {
            let row = &mut buf[y * stride..];
            for x in 0..img.w as usize {
                let p = img.px[y * img.w as usize + x];
                row[x * 3] = p as u8;
                row[x * 3 + 1] = (p >> 8) as u8;
                row[x * 3 + 2] = (p >> 16) as u8;
            }
        }
        let mut pf = GUID_WICPixelFormat24bppBGR;
        frame.SetPixelFormat(&mut pf)?;
        if pf == GUID_WICPixelFormat24bppBGR {
            frame.WritePixels(img.h as u32, stride as u32, &buf)?;
        } else {
            // Encoder wants something else; let WIC convert from a BGR bitmap.
            let bmp = f.CreateBitmapFromMemory(
                img.w as u32,
                img.h as u32,
                &GUID_WICPixelFormat24bppBGR,
                stride as u32,
                &buf,
            )?;
            frame.WriteSource(&bmp, std::ptr::null())?;
        }
        frame.Commit()?;
        enc.Commit()?;
        let hg = GetHGlobalFromStream(&stream)?;
        let mut stat = STATSTG::default();
        stream.Stat(&mut stat, STATFLAG_NONAME)?;
        let len = stat.cbSize as usize;
        let p = GlobalLock(hg) as *const u8;
        if p.is_null() {
            return Err(Error::from(E_FAIL));
        }
        let v = std::slice::from_raw_parts(p, len).to_vec();
        let _ = GlobalUnlock(hg);
        Ok(v)
    }
}

/// Read an image file (any format WIC knows) as opaque pixels.
pub fn load(path: &Path) -> Option<Img> {
    unsafe {
        let f = wic().ok()?;
        let p = wide(&path.to_string_lossy());
        let dec = f.CreateDecoderFromFilename(PCWSTR(p.as_ptr()), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand).ok()?;
        let fr = dec.GetFrame(0).ok()?;
        let conv = f.CreateFormatConverter().ok()?;
        conv.Initialize(&fr, &GUID_WICPixelFormat32bppBGR, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)
            .ok()?;
        let (mut w, mut h) = (0u32, 0u32);
        conv.GetSize(&mut w, &mut h).ok()?;
        if w == 0 || h == 0 || w > 32768 || h > 65536 {
            return None;
        }
        let mut img = Img::new(w as i32, h as i32);
        let bytes = std::slice::from_raw_parts_mut(img.px.as_mut_ptr() as *mut u8, img.px.len() * 4);
        conv.CopyPixels(std::ptr::null(), w * 4, bytes).ok()?;
        img.force_opaque();
        Some(img)
    }
}

pub fn save(img: &Img, path: &Path) -> Result<()> {
    let bytes = encode(img, fmt_for(path))?;
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(path, bytes).map_err(|_| Error::from(E_FAIL))
}

unsafe fn global_from(bytes: &[u8]) -> Option<HGLOBAL> {
    let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len()).ok()?;
    let p = GlobalLock(h) as *mut u8;
    if p.is_null() {
        let _ = GlobalFree(Some(h));
        return None;
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
    let _ = GlobalUnlock(h);
    Some(h)
}

struct Clip;
impl Clip {
    fn open(owner: HWND) -> Option<Clip> {
        unsafe {
            for _ in 0..10 {
                if OpenClipboard(Some(owner)).is_ok() {
                    let _ = EmptyClipboard();
                    return Some(Clip);
                }
                std::thread::sleep(std::time::Duration::from_millis(15));
            }
        }
        None
    }
    fn put(&self, fmt: u32, bytes: &[u8]) {
        unsafe {
            if let Some(h) = global_from(bytes) {
                if SetClipboardData(fmt, Some(HANDLE(h.0))).is_err() {
                    let _ = GlobalFree(Some(h));
                }
            }
        }
    }
}
impl Drop for Clip {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

fn dib_bytes(img: &Img) -> Vec<u8> {
    let hdr = 40usize;
    let mut v = vec![0u8; hdr + img.px.len() * 4];
    let put32 = |v: &mut Vec<u8>, o: usize, x: u32| v[o..o + 4].copy_from_slice(&x.to_le_bytes());
    put32(&mut v, 0, 40);
    put32(&mut v, 4, img.w as u32);
    put32(&mut v, 8, img.h as u32); // bottom-up: widest compatibility
    v[12] = 1;
    v[14] = 32;
    put32(&mut v, 20, (img.px.len() * 4) as u32);
    let row = img.w as usize * 4;
    let src = img.as_bytes();
    for y in 0..img.h as usize {
        let d = hdr + (img.h as usize - 1 - y) * row;
        v[d..d + row].copy_from_slice(&src[y * row..(y + 1) * row]);
    }
    v
}

/// Image to clipboard: PNG (lossless, preferred by browsers/Office/chat apps) + DIB.
pub fn copy_image(owner: HWND, img: &Img) -> bool {
    let png = encode(img, Fmt::Png).ok();
    let dib = dib_bytes(img);
    let Some(c) = Clip::open(owner) else { return false };
    c.put(CF_DIB, &dib);
    if let Some(png) = png {
        let f = unsafe { RegisterClipboardFormatW(w!("PNG")) };
        c.put(f, &png);
    }
    true
}

/// Puts a file reference on the clipboard (paste into Explorer, mail, chats, uploads).
pub fn copy_file(owner: HWND, path: &Path) -> bool {
    let wide: Vec<u16> = path.as_os_str().encode_wide_z();
    // DROPFILES { pFiles: u32, pt: POINT, fNC: BOOL, fWide: BOOL }
    let mut v = vec![0u8; 20];
    v[0..4].copy_from_slice(&20u32.to_le_bytes());
    v[16..20].copy_from_slice(&1u32.to_le_bytes());
    for c in wide.iter().chain([0u16].iter()) {
        v.extend_from_slice(&c.to_le_bytes());
    }
    let Some(c) = Clip::open(owner) else { return false };
    c.put(CF_HDROP, &v);
    let f = unsafe { RegisterClipboardFormatW(w!("Preferred DropEffect")) };
    c.put(f, &1u32.to_le_bytes()); // DROPEFFECT_COPY
    true
}

pub fn copy_text(owner: HWND, s: &str) -> bool {
    let mut w: Vec<u16> = s.encode_utf16().collect();
    w.push(0);
    let bytes: Vec<u8> = w.iter().flat_map(|c| c.to_le_bytes()).collect();
    let Some(c) = Clip::open(owner) else { return false };
    c.put(CF_UNICODETEXT, &bytes);
    true
}

pub fn clipboard_text(owner: HWND) -> Option<String> {
    unsafe {
        OpenClipboard(Some(owner)).ok()?;
        let mut out = None;
        if let Ok(h) = GetClipboardData(CF_UNICODETEXT) {
            let p = GlobalLock(HGLOBAL(h.0)) as *const u16;
            if !p.is_null() {
                let mut n = 0;
                while *p.add(n) != 0 {
                    n += 1;
                }
                out = Some(String::from_utf16_lossy(std::slice::from_raw_parts(p, n)));
                let _ = GlobalUnlock(HGLOBAL(h.0));
            }
        }
        let _ = CloseClipboard();
        out
    }
}

/// Reads CF_DIB (Windows synthesizes it from bitmaps/DIBV5) into an Img.
pub fn clipboard_image(owner: HWND) -> Option<Img> {
    unsafe {
        OpenClipboard(Some(owner)).ok()?;
        let mut out = None;
        if let Ok(h) = GetClipboardData(CF_DIB) {
            let hg = HGLOBAL(h.0);
            let size = GlobalSize(hg);
            let p = GlobalLock(hg) as *const u8;
            if !p.is_null() {
                out = parse_dib(std::slice::from_raw_parts(p, size));
                let _ = GlobalUnlock(hg);
            }
        }
        let _ = CloseClipboard();
        out
    }
}

fn parse_dib(b: &[u8]) -> Option<Img> {
    let rd32 = |o: usize| -> Option<u32> { Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?)) };
    let hsize = rd32(0)? as usize;
    let w = rd32(4)? as i32;
    let hraw = rd32(8)? as i32;
    let bpp = u16::from_le_bytes(b.get(14..16)?.try_into().ok()?);
    let comp = rd32(16)?;
    let clr_used = rd32(32)? as usize;
    if w <= 0 || hraw == 0 || w > 32768 || hraw.abs() > 32768 {
        return None;
    }
    let h = hraw.abs();
    let mut off = hsize;
    if comp == 3 && hsize == 40 {
        off += 12; // BI_BITFIELDS masks
    }
    if bpp <= 8 {
        off += 4 * if clr_used == 0 { 1usize << bpp } else { clr_used };
    }
    let stride = (w as usize * bpp as usize).div_ceil(32) * 4;
    if b.len() < off + stride * h as usize || !(bpp == 24 || bpp == 32) {
        return None;
    }
    let mut img = Img::new(w, h);
    let mut any_alpha = false;
    for y in 0..h as usize {
        let sy = if hraw > 0 { h as usize - 1 - y } else { y };
        let row = &b[off + sy * stride..];
        for x in 0..w as usize {
            let p = if bpp == 32 {
                let p = u32::from_le_bytes(row[x * 4..x * 4 + 4].try_into().unwrap());
                any_alpha |= p >> 24 != 0;
                p
            } else {
                0xFF00_0000 | (row[x * 3 + 2] as u32) << 16 | (row[x * 3 + 1] as u32) << 8 | row[x * 3] as u32
            };
            img.px[y * w as usize + x] = p;
        }
    }
    if !any_alpha {
        img.force_opaque();
    } else {
        // flatten onto white so pins of transparent images look right
        for p in img.px.iter_mut() {
            let a = *p >> 24;
            let mix = |c: u32| (c * a + 255 * (255 - a)) / 255;
            *p = 0xFF00_0000 | mix((*p >> 16) & 255) << 16 | mix((*p >> 8) & 255) << 8 | mix(*p & 255);
        }
    }
    Some(img)
}

pub trait WideZ {
    fn encode_wide_z(&self) -> Vec<u16>;
}
impl WideZ for std::ffi::OsStr {
    fn encode_wide_z(&self) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        self.encode_wide().chain(std::iter::once(0)).collect()
    }
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn known_folder(id: &GUID) -> Option<PathBuf> {
    unsafe {
        let p = SHGetKnownFolderPath(id, KNOWN_FOLDER_FLAG(0), None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s.map(PathBuf::from)
    }
}

pub fn timestamp_name(prefix: &str, ext: &str) -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!(
        "{}_{:04}{:02}{:02}_{:02}{:02}{:02}_{:03}.{}",
        prefix, t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds, ext
    )
}

pub fn temp_dir() -> PathBuf {
    std::env::temp_dir().join("WispShot")
}

/// Remove temp "copy as file" images older than a day.
pub fn clean_temp() {
    if let Ok(rd) = std::fs::read_dir(temp_dir()) {
        for e in rd.flatten() {
            if let Ok(m) = e.metadata() {
                if m.modified().ok().and_then(|t| t.elapsed().ok()).is_some_and(|d| d.as_secs() > 86400) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }
}

/// Move a file, also across drives (rename, else copy + delete).
pub fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(d) = to.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to)?;
    let _ = std::fs::remove_file(from);
    Ok(())
}

/// Standard "Save As" dialog. The extension of `name` picks the file types: .mp4 or .gif
/// for a recording, otherwise the image types with that one preselected. Returns chosen path.
pub fn save_dialog(owner: HWND, dir: &Path, name: &str) -> Option<PathBuf> {
    unsafe {
        let dlg: IFileSaveDialog = CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let ext = Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        let video = ext == "mp4" || ext == "gif";
        if ext == "mp4" {
            let _ = dlg.SetFileTypes(&[COMDLG_FILTERSPEC { pszName: w!("MP4 video"), pszSpec: w!("*.mp4") }]);
            let _ = dlg.SetDefaultExtension(w!("mp4"));
        } else if ext == "gif" {
            let _ = dlg.SetFileTypes(&[COMDLG_FILTERSPEC { pszName: w!("GIF"), pszSpec: w!("*.gif") }]);
            let _ = dlg.SetDefaultExtension(w!("gif"));
        } else {
            let specs = [
                COMDLG_FILTERSPEC { pszName: w!("PNG"), pszSpec: w!("*.png") },
                COMDLG_FILTERSPEC { pszName: w!("JPEG"), pszSpec: w!("*.jpg;*.jpeg") },
                COMDLG_FILTERSPEC { pszName: w!("BMP"), pszSpec: w!("*.bmp") },
                COMDLG_FILTERSPEC { pszName: w!("TIFF"), pszSpec: w!("*.tif;*.tiff") },
            ];
            let _ = dlg.SetFileTypes(&specs);
            let fmt = Fmt::parse(&ext).unwrap_or(Fmt::Png);
            let i = Fmt::ALL.iter().position(|f| *f == fmt).unwrap_or(0);
            let _ = dlg.SetFileTypeIndex(i as u32 + 1);
            let e = wide(fmt.ext());
            let _ = dlg.SetDefaultExtension(PCWSTR(e.as_ptr()));
        }
        let n = wide(name);
        let _ = dlg.SetFileName(PCWSTR(n.as_ptr()));
        // Separate "last used folder" memory for images and videos (kept by Windows per
        // client GUID); `dir` is only the starting point until something was saved.
        let _ = dlg.SetClientGuid(if video {
            &GUID::from_u128(0x6f1d2c3e_8a4b_4e57_9c1a_2b7d5e0f9a31)
        } else {
            &GUID::from_u128(0x3a9e7b14_52c6_4f0d_a8e2_c4f1b6d90e72)
        });
        // the shell parser wants backslashes (a hand-edited ini may use '/')
        let dir = PathBuf::from(dir.to_string_lossy().replace('/', "\\"));
        let _ = std::fs::create_dir_all(&dir);
        let d = dir.as_os_str().encode_wide_z();
        if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(d.as_ptr()), None) {
            let _ = dlg.SetDefaultFolder(&item);
        }
        dlg.Show(Some(owner)).ok()?;
        let item = dlg.GetResult().ok()?;
        let p = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s.map(PathBuf::from)
    }
}
