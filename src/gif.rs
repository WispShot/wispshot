//! A finished MP4 recording to an animated GIF, using only what Windows ships:
//! Media Foundation decodes, WIC scales, picks the palette and writes the GIF.
//!
//! Two passes over the video: the first collects a few frames to build one shared
//! 256-colour palette (one palette keeps still areas from flickering between
//! frames), the second encodes. Frames that look the same are merged into one
//! longer frame, so a still screen costs almost nothing.

use crate::output::wide;
use std::path::Path;
use windows::core::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::*;
use windows::Win32::System::Variant::{VT_UI1, VT_UI2};

/// Receives (pixels, width, height, ticks) for each distinct frame.
type Sink<'a> = dyn FnMut(&[u8], u32, u32, u32) + 'a;

const STREAM: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
/// Frames kept for building the palette.
const SAMPLES: usize = 16;

/// Convert `src` (MP4) into `dst` (GIF) at `fps`, no wider than `max_w` (0 = any).
pub fn from_mp4(src: &Path, dst: &Path, fps: u32, max_w: u32) -> std::result::Result<(), String> {
    unsafe {
        if MFStartup(MF_VERSION, MFSTARTUP_LITE).is_err() {
            return Err("Media Foundation".into());
        }
        let r = convert(src, dst, fps.clamp(1, 50), max_w).map_err(|e| e.message());
        let _ = MFShutdown();
        r
    }
}

unsafe fn convert(src: &Path, dst: &Path, fps: u32, max_w: u32) -> Result<()> {
    let f = crate::output::wic()?;

    // pass 1: palette from evenly spread frames
    let mut samples: Vec<Vec<u8>> = vec![];
    let mut every = 1usize;
    let mut n = 0usize;
    let (mut sw, mut sh) = (0u32, 0u32);
    frames(&f, src, fps, max_w, &mut |px, w, h, _| {
        if n.is_multiple_of(every) {
            samples.push(half(px, w, h));
            (sw, sh) = ((w / 2).max(1), (h / 2).max(1));
            if samples.len() == SAMPLES {
                // keep every other one and sample half as often from now on
                samples = std::mem::take(&mut samples).into_iter().step_by(2).collect();
                every *= 2;
            }
        }
        n += 1;
    })?;
    if samples.is_empty() {
        return Err(Error::new(windows::Win32::Foundation::E_FAIL, "no frames"));
    }
    let strip: Vec<u8> = samples.concat();
    let sbmp = f.CreateBitmapFromMemory(sw, sh * samples.len() as u32, &GUID_WICPixelFormat32bppBGR, sw * 4, &strip)?;
    let pal = f.CreatePalette()?;
    pal.InitializeFromBitmap(&sbmp, 256, false)?;
    drop(samples);

    // pass 2: encode
    let stream = CreateStreamOnHGlobal(windows::Win32::Foundation::HGLOBAL::default(), true)?;
    let enc = f.CreateEncoder(&GUID_ContainerFormatGif, std::ptr::null())?;
    enc.Initialize(&stream, WICBitmapEncoderNoCache)?;
    enc.SetPalette(&pal)?;
    let mut ticks_done = 0u64;
    let mut err: Option<Error> = None;
    // what the viewer shows so far: each frame only stores the rectangle that changed
    let mut shown: Option<Vec<u8>> = None;
    frames(&f, src, fps, max_w, &mut |px, w, h, ticks| {
        if err.is_some() {
            return;
        }
        // centiseconds, rounded on the running total so the length stays right
        let cs = |t: u64| (t * 100 + fps as u64 / 2) / fps as u64;
        let delay = (cs(ticks_done + ticks as u64) - cs(ticks_done)).clamp(2, 65535) as u16;
        ticks_done += ticks as u64;
        let (x0, y0, x1, y1) = match &shown {
            Some(s) => changed(s, px, w, h).unwrap_or((0, 0, 1, 1)),
            None => (0, 0, w, h),
        };
        let (cw, ch) = (x1 - x0, y1 - y0);
        let mut part = Vec::with_capacity((cw * ch * 4) as usize);
        for y in y0..y1 {
            let i = ((y * w + x0) * 4) as usize;
            part.extend_from_slice(&px[i..i + (cw * 4) as usize]);
        }
        let s = shown.get_or_insert_with(|| px.to_vec());
        for (y, row) in (y0..y1).zip(part.chunks_exact((cw * 4) as usize)) {
            let i = ((y * w + x0) * 4) as usize;
            s[i..i + row.len()].copy_from_slice(row);
        }
        if let Err(e) = write_frame(&f, &enc, &pal, &part, (x0, y0, cw, ch), delay) {
            err = Some(e);
        }
    })?;
    if let Some(e) = err {
        return Err(e);
    }
    enc.Commit()?;

    let mut bytes = stream_bytes(&stream)?;
    add_loop(&mut bytes);
    if let Some(d) = dst.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    std::fs::write(dst, bytes).map_err(|e| Error::new(windows::Win32::Foundation::E_FAIL, e.to_string()))
}

unsafe fn write_frame(
    f: &IWICImagingFactory,
    enc: &IWICBitmapEncoder,
    pal: &IWICPalette,
    px: &[u8],
    (x, y, w, h): (u32, u32, u32, u32),
    delay: u16,
) -> Result<()> {
    let mut frame: Option<IWICBitmapFrameEncode> = None;
    let mut props: Option<IPropertyBag2> = None;
    enc.CreateNewFrame(&mut frame, &mut props)?;
    let frame = frame.ok_or_else(|| Error::from(windows::Win32::Foundation::E_FAIL))?;
    frame.Initialize(props.as_ref())?;
    frame.SetSize(w, h)?;
    let mut pf = GUID_WICPixelFormat8bppIndexed;
    frame.SetPixelFormat(&mut pf)?;
    let bmp = f.CreateBitmapFromMemory(w, h, &GUID_WICPixelFormat32bppBGR, w * 4, px)?;
    let conv = f.CreateFormatConverter()?;
    conv.Initialize(
        &bmp,
        &GUID_WICPixelFormat8bppIndexed,
        // no dithering: screen content (text, flat UI) stays crisp and compresses to less
        // than half the size; error diffusion only helps photos and gradients
        WICBitmapDitherTypeNone,
        pal,
        0.0,
        WICBitmapPaletteTypeCustom,
    )?;
    let mw = frame.GetMetadataQueryWriter()?;
    let set = |name: &str, vt, val: u16| {
        let mut v = PROPVARIANT::default();
        (*v.Anonymous.Anonymous).vt = vt;
        if vt == VT_UI1 {
            (*v.Anonymous.Anonymous).Anonymous.bVal = val as u8;
        } else {
            (*v.Anonymous.Anonymous).Anonymous.uiVal = val;
        }
        let n = wide(name);
        mw.SetMetadataByName(PCWSTR(n.as_ptr()), &v)
    };
    set("/grctlext/Delay", VT_UI2, delay)?;
    // keep the previous frame underneath: this one may cover only part of the picture
    set("/grctlext/Disposal", VT_UI1, 1)?;
    set("/imgdesc/Left", VT_UI2, x as u16)?;
    set("/imgdesc/Top", VT_UI2, y as u16)?;
    frame.WriteSource(&conv, std::ptr::null())?;
    frame.Commit()
}

/// Decode `src` and call `sink(pixels, w, h, ticks)` once per distinct frame, where
/// `ticks` is how many 1/fps steps it stays on screen. Pixels are BGRX, top-down.
unsafe fn frames(f: &IWICImagingFactory, src: &Path, fps: u32, max_w: u32, sink: &mut Sink) -> Result<()> {
    let mut attrs: Option<IMFAttributes> = None;
    MFCreateAttributes(&mut attrs, 1)?;
    let attrs = attrs.ok_or_else(|| Error::from(windows::Win32::Foundation::E_FAIL))?;
    attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)?;
    let path = wide(&src.to_string_lossy());
    let reader = MFCreateSourceReaderFromURL(PCWSTR(path.as_ptr()), &attrs)?;
    let _ = reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false);
    reader.SetStreamSelection(STREAM, true)?;
    let mt = MFCreateMediaType()?;
    mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    mt.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
    reader.SetCurrentMediaType(STREAM, None, &mt)?;
    let mut geo = Geo::read(&reader)?;

    let step = 10_000_000i64 / fps as i64;
    let mut next: Option<i64> = None;
    // the newest decoded frame (scaled), and the frame being held back until we know how long it shows
    let mut prev: Option<Vec<u8>> = None;
    let mut pending: Option<(Vec<u8>, u32)> = None;
    let (ow, oh) = geo.out_size(max_w);
    // merge a frame that looks like the held-back one; otherwise hand that one on
    fn push(sink: &mut Sink, pending: &mut Option<(Vec<u8>, u32)>, px: &[u8], w: u32, h: u32) {
        match pending {
            Some((p, t)) if same(p, px) => *t += 1,
            _ => {
                flush(sink, pending, w, h);
                *pending = Some((px.to_vec(), 1));
            }
        }
    }
    fn flush(sink: &mut Sink, pending: &mut Option<(Vec<u8>, u32)>, w: u32, h: u32) {
        if let Some((p, t)) = pending.take() {
            sink(&p, w, h, t);
        }
    }
    loop {
        let mut flags = 0u32;
        let mut ts = 0i64;
        let mut sample: Option<IMFSample> = None;
        reader.ReadSample(STREAM, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut sample))?;
        if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
            break;
        }
        if flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 as u32 != 0 {
            geo = Geo::read(&reader)?;
            let size = geo.out_size(max_w);
            if size != (ow, oh) {
                // GIF frames all share one size: stop at a mid-stream size change (should not happen)
                break;
            }
        }
        let Some(sample) = sample else { continue };
        let raw = geo.copy(&sample)?;
        let px = scale(f, &raw, geo.w, geo.h, ow, oh)?;
        let n = next.get_or_insert(ts);
        while *n < ts {
            if let Some(p) = &prev {
                push(sink, &mut pending, p, ow, oh);
            }
            *n += step;
        }
        prev = Some(px);
    }
    if let Some(p) = &prev {
        push(sink, &mut pending, p, ow, oh);
    }
    flush(sink, &mut pending, ow, oh);
    Ok(())
}

/// Visible picture inside the decoded buffer.
struct Geo {
    w: u32,
    h: u32,
    x: u32,
    y: u32,
    stride: i32,
    /// Height of the whole decoded buffer (picture plus padding).
    fh: u32,
}

impl Geo {
    unsafe fn read(reader: &IMFSourceReader) -> Result<Geo> {
        let mt = reader.GetCurrentMediaType(STREAM)?;
        let size = mt.GetUINT64(&MF_MT_FRAME_SIZE)?;
        let (fw, fh) = ((size >> 32) as u32, size as u32);
        let stride = mt.GetUINT32(&MF_MT_DEFAULT_STRIDE).map(|s| s as i32).unwrap_or(fw as i32 * 4);
        // H.264 pads to 16 rows; the aperture says which part is the real picture
        let mut area = MFVideoArea::default();
        let ok = mt
            .GetBlob(
                &MF_MT_MINIMUM_DISPLAY_APERTURE,
                std::slice::from_raw_parts_mut(&mut area as *mut _ as *mut u8, std::mem::size_of::<MFVideoArea>()),
                None,
            )
            .is_ok();
        let (x, y, w, h) = if ok && area.Area.cx > 0 && area.Area.cy > 0 {
            (
                area.OffsetX.value.max(0) as u32,
                area.OffsetY.value.max(0) as u32,
                area.Area.cx as u32,
                area.Area.cy as u32,
            )
        } else {
            (0, 0, fw, fh)
        };
        let (x, y) = (x.min(fw.saturating_sub(1)), y.min(fh.saturating_sub(1)));
        Ok(Geo { w: w.min(fw - x).max(1), h: h.min(fh - y).max(1), x, y, stride, fh })
    }

    fn out_size(&self, max_w: u32) -> (u32, u32) {
        if max_w > 0 && self.w > max_w {
            (max_w, ((self.h as u64 * max_w as u64 + self.w as u64 / 2) / self.w as u64).max(1) as u32)
        } else {
            (self.w, self.h)
        }
    }

    /// The visible picture as top-down BGRX rows.
    unsafe fn copy(&self, sample: &IMFSample) -> Result<Vec<u8>> {
        let buf = sample.ConvertToContiguousBuffer()?;
        let mut out = vec![0u8; (self.w * self.h * 4) as usize];
        let row = (self.w * 4) as usize;
        let mut put = |scan0: *const u8, pitch: isize| {
            for y in 0..self.h as usize {
                let s = scan0.offset((y + self.y as usize) as isize * pitch + self.x as isize * 4);
                std::ptr::copy_nonoverlapping(s, out.as_mut_ptr().add(y * row), row);
            }
        };
        if let Ok(b2) = buf.cast::<IMF2DBuffer>() {
            let mut scan0 = std::ptr::null_mut();
            let mut pitch = 0i32;
            b2.Lock2D(&mut scan0, &mut pitch)?;
            put(scan0, pitch as isize);
            let _ = b2.Unlock2D();
        } else {
            let mut p = std::ptr::null_mut();
            buf.Lock(&mut p, None, None)?;
            let pitch = self.stride as isize;
            // a negative default stride means bottom-up rows: the top row is the last one in memory
            let scan0 = if pitch < 0 { p.offset(-pitch * (self.fh as isize - 1)) } else { p };
            put(scan0, pitch);
            let _ = buf.Unlock();
        }
        for px in out.as_chunks_mut::<4>().0 {
            px[3] = 255;
        }
        Ok(out)
    }
}

unsafe fn scale(f: &IWICImagingFactory, px: &[u8], w: u32, h: u32, ow: u32, oh: u32) -> Result<Vec<u8>> {
    if (w, h) == (ow, oh) {
        return Ok(px.to_vec());
    }
    let bmp = f.CreateBitmapFromMemory(w, h, &GUID_WICPixelFormat32bppBGR, w * 4, px)?;
    let s = f.CreateBitmapScaler()?;
    s.Initialize(&bmp, ow, oh, WICBitmapInterpolationModeFant)?;
    let mut out = vec![0u8; (ow * oh * 4) as usize];
    s.CopyPixels(std::ptr::null(), ow * 4, &mut out)?;
    Ok(out)
}

/// Video decoding leaves tiny differences in still areas; ignore those.
const TOLERANCE: u8 = 6;

fn close(a: &[u8], b: &[u8]) -> bool {
    a[0].abs_diff(b[0]) <= TOLERANCE && a[1].abs_diff(b[1]) <= TOLERANCE && a[2].abs_diff(b[2]) <= TOLERANCE
}

fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0).all(|(p, q)| close(p, q))
}

/// Bounding box (x0, y0, x1, y1) of the pixels that changed, None if nothing did.
fn changed(old: &[u8], new: &[u8], w: u32, h: u32) -> Option<(u32, u32, u32, u32)> {
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for y in 0..h {
        let r = (y * w * 4) as usize..((y + 1) * w * 4) as usize;
        let (a, b) = (&old[r.clone()], &new[r]);
        let diff = |x: u32| !close(&a[(x * 4) as usize..], &b[(x * 4) as usize..]);
        let Some(l) = (0..w).find(|&x| diff(x)) else { continue };
        let rr = (l..w).rev().find(|&x| diff(x)).unwrap_or(l);
        x0 = x0.min(l);
        x1 = x1.max(rr + 1);
        y0 = y0.min(y);
        y1 = y + 1;
    }
    (x1 > x0).then_some((x0, y0, x1, y1))
}

/// Every other pixel of every other row (palette samples need not be full size).
fn half(px: &[u8], w: u32, h: u32) -> Vec<u8> {
    let (hw, hh) = ((w / 2).max(1), (h / 2).max(1));
    let mut out = Vec::with_capacity((hw * hh * 4) as usize);
    for y in 0..hh {
        let sy = (y * 2).min(h - 1);
        for x in 0..hw {
            let i = ((sy * w + (x * 2).min(w - 1)) * 4) as usize;
            out.extend_from_slice(&px[i..i + 4]);
        }
    }
    out
}

unsafe fn stream_bytes(stream: &windows::Win32::System::Com::IStream) -> Result<Vec<u8>> {
    use windows::Win32::System::Memory::*;
    let hg = GetHGlobalFromStream(stream)?;
    let mut stat = windows::Win32::System::Com::STATSTG::default();
    stream.Stat(&mut stat, windows::Win32::System::Com::STATFLAG_NONAME)?;
    let p = GlobalLock(hg) as *const u8;
    if p.is_null() {
        return Err(Error::from(windows::Win32::Foundation::E_FAIL));
    }
    let v = std::slice::from_raw_parts(p, stat.cbSize as usize).to_vec();
    let _ = GlobalUnlock(hg);
    Ok(v)
}

/// WIC writes no "loop forever" block: insert the NETSCAPE2.0 extension after the
/// header, screen descriptor and global colour table.
fn add_loop(gif: &mut Vec<u8>) {
    if gif.len() < 13 || &gif[..3] != b"GIF" || gif.windows(11).any(|w| w == b"NETSCAPE2.0") {
        return;
    }
    let packed = gif[10];
    let gct = if packed & 0x80 != 0 { 3 * (1usize << ((packed & 7) + 1)) } else { 0 };
    let at = 13 + gct;
    if at > gif.len() {
        return;
    }
    let mut ext = vec![0x21, 0xFF, 0x0B];
    ext.extend_from_slice(b"NETSCAPE2.0");
    ext.extend_from_slice(&[0x03, 0x01, 0x00, 0x00, 0x00]);
    gif.splice(at..at, ext);
}
