//! Region screen recording to MP4 (H.264).
//!
//! Pipeline (all on the GPU when possible):
//!   DXGI Desktop Duplication -> CopySubresourceRegion (crop) -> cursor via GDI on the
//!   texture -> Media Foundation SinkWriter with the D3D device manager, which picks
//!   the hardware H.264 encoder (NVENC / QuickSync / AMF). Falls back to system-memory
//!   frames if the GPU path cannot be negotiated.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Performance::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_numerics::Matrix3x2;

pub struct Recorder {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<std::result::Result<PathBuf, String>>>,
}

impl Recorder {
    pub fn start(rect: RECT, fps: u32, cursor: bool, path: PathBuf) -> Recorder {
        let stop = Arc::new(AtomicBool::new(false));
        let s2 = stop.clone();
        let thread = std::thread::spawn(move || run(rect, fps.clamp(5, 60), cursor, path, s2));
        Recorder { stop, thread: Some(thread) }
    }

    pub fn finished(&self) -> bool {
        self.thread.as_ref().is_none_or(|t| t.is_finished())
    }

    pub fn stop(mut self) -> std::result::Result<PathBuf, String> {
        self.stop.store(true, Ordering::SeqCst);
        match self.thread.take() {
            Some(t) => t.join().unwrap_or_else(|_| Err(crate::i18n::t("err.rec_crashed").into())),
            None => Err(crate::i18n::t("err.rec_none").into()),
        }
    }
}

fn qpc() -> i64 {
    let mut v = 0i64;
    unsafe {
        let _ = QueryPerformanceCounter(&mut v);
    }
    v
}
fn qpf() -> i64 {
    let mut v = 0i64;
    unsafe {
        let _ = QueryPerformanceFrequency(&mut v);
    }
    v
}

struct Output {
    adapter: IDXGIAdapter1,
    output: IDXGIOutput1,
    desk: RECT,
    rotation: DXGI_MODE_ROTATION,
}

/// Rotated monitors: Desktop Duplication hands out the panel-native (unrotated)
/// image. This D2D pass maps it back to desktop orientation on the GPU.
struct Rotator {
    dc: ID2D1DeviceContext,
    full: ID3D11Texture2D,
    src: ID2D1Bitmap1,
    dst: ID2D1Bitmap1,
    m: Matrix3x2,
}

impl Rotator {
    /// Transform texture (u,v) -> region (x,y). Mappings verified against GDI captures:
    /// ROTATE270: u = DH-1-y, v = x;  ROTATE90: u = y, v = DW-1-x;  ROTATE180: both flipped.
    #[allow(clippy::too_many_arguments)]
    unsafe fn new(
        device: &ID3D11Device,
        tex_desc: &D3D11_TEXTURE2D_DESC,
        target: &ID3D11Texture2D,
        rot: DXGI_MODE_ROTATION,
        dw: f32,
        dh: f32,
        sx: f32,
        sy: f32,
    ) -> Result<Rotator> {
        let m = match rot {
            DXGI_MODE_ROTATION_ROTATE270 => {
                Matrix3x2 { M11: 0.0, M12: -1.0, M21: 1.0, M22: 0.0, M31: -sx, M32: dh - sy }
            }
            DXGI_MODE_ROTATION_ROTATE90 => {
                Matrix3x2 { M11: 0.0, M12: 1.0, M21: -1.0, M22: 0.0, M31: dw - sx, M32: -sy }
            }
            _ => Matrix3x2 { M11: -1.0, M12: 0.0, M21: 0.0, M22: -1.0, M31: dw - sx, M32: dh - sy },
        };
        let f: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let d2d_dev = f.CreateDevice(&device.cast::<IDXGIDevice>()?)?;
        let dc = d2d_dev.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
        let mut fd = *tex_desc;
        fd.Usage = D3D11_USAGE_DEFAULT;
        fd.BindFlags = D3D11_BIND_SHADER_RESOURCE.0 as u32;
        fd.CPUAccessFlags = 0;
        fd.MiscFlags = 0;
        let mut full = None;
        device.CreateTexture2D(&fd, None, Some(&mut full))?;
        let full = full.ok_or_else(|| Error::from(E_FAIL))?;
        let props = |opt| D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_IGNORE },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: opt,
            colorContext: std::mem::ManuallyDrop::new(None),
        };
        let src =
            dc.CreateBitmapFromDxgiSurface(&full.cast::<IDXGISurface>()?, Some(&props(D2D1_BITMAP_OPTIONS_NONE)))?;
        let dst =
            dc.CreateBitmapFromDxgiSurface(&target.cast::<IDXGISurface>()?, Some(&props(D2D1_BITMAP_OPTIONS_TARGET)))?;
        Ok(Rotator { dc, full, src, dst, m })
    }

    unsafe fn draw(&self, ctx: &ID3D11DeviceContext, desktop: &ID3D11Texture2D) {
        ctx.CopyResource(&self.full, desktop);
        self.dc.SetTarget(&self.dst);
        self.dc.BeginDraw();
        self.dc.SetTransform(&self.m);
        self.dc.DrawBitmap(&self.src, None, 1.0, D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None, None);
        let _ = self.dc.EndDraw(None, None);
    }
}

fn find_output(r: &RECT) -> Result<Output> {
    unsafe {
        let f: IDXGIFactory1 = CreateDXGIFactory1()?;
        let (cx, cy) = ((r.left + r.right) / 2, (r.top + r.bottom) / 2);
        let mut best: Option<Output> = None;
        let mut i = 0;
        while let Ok(a) = f.EnumAdapters1(i) {
            let mut j = 0;
            while let Ok(o) = a.EnumOutputs(j) {
                let d = o.GetDesc()?;
                let dr = d.DesktopCoordinates;
                let hit = cx >= dr.left && cx < dr.right && cy >= dr.top && cy < dr.bottom;
                if hit || best.is_none() {
                    if let Ok(o1) = o.cast::<IDXGIOutput1>() {
                        best = Some(Output { adapter: a.clone(), output: o1, desk: dr, rotation: d.Rotation });
                        if hit {
                            return Ok(best.unwrap());
                        }
                    }
                }
                j += 1;
            }
            i += 1;
        }
        best.ok_or_else(|| Error::from(E_FAIL))
    }
}

fn run(
    rect: RECT,
    fps: u32,
    cursor: bool,
    path: PathBuf,
    stop: Arc<AtomicBool>,
) -> std::result::Result<PathBuf, String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        if MFStartup(MF_VERSION, MFSTARTUP_LITE).is_err() {
            return Err(crate::i18n::t("err.media_foundation").into());
        }
    }
    let r = (|| -> Result<()> {
        match record(rect, fps, cursor, &path, &stop, true) {
            Ok(()) => Ok(()),
            Err(e) => {
                crate::trace(&format!("record: gpu path failed: {:?}", e));
                // GPU negotiation failed early: retry with system-memory frames.
                let _ = std::fs::remove_file(&path);
                if stop.load(Ordering::SeqCst) {
                    return Err(e);
                }
                record(rect, fps, cursor, &path, &stop, false)
            }
        }
    })();
    unsafe {
        let _ = MFShutdown();
        CoUninitialize();
    }
    r.map(|_| path).map_err(|e| e.message().to_string())
}

unsafe fn set_ratio(t: &IMFMediaType, key: &GUID, a: u32, b: u32) -> Result<()> {
    t.SetUINT64(key, ((a as u64) << 32) | b as u64)
}

fn record(rect: RECT, fps: u32, cursor: bool, path: &std::path::Path, stop: &AtomicBool, gpu: bool) -> Result<()> {
    unsafe {
        let out = find_output(&rect)?;
        // clip to the output and make dimensions even (H.264 requirement)
        let l = rect.left.max(out.desk.left);
        let t = rect.top.max(out.desk.top);
        let rr = rect.right.min(out.desk.right);
        let b = rect.bottom.min(out.desk.bottom);
        let w = ((rr - l) & !1).max(2) as u32;
        let h = ((b - t) & !1).max(2) as u32;
        let (sx, sy) = ((l - out.desk.left) as u32, (t - out.desk.top) as u32);

        let mut device: Option<ID3D11Device> = None;
        let mut ctx: Option<ID3D11DeviceContext> = None;
        D3D11CreateDevice(
            &out.adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut ctx),
        )?;
        let device = device.ok_or_else(|| Error::from(E_FAIL))?;
        let ctx = ctx.ok_or_else(|| Error::from(E_FAIL))?;
        if let Ok(mt) = device.cast::<ID3D11Multithread>() {
            let _ = mt.SetMultithreadProtected(true);
        }
        let mut dup = out.output.DuplicateOutput(&device)?;

        // ---- encoder
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 4)?;
        let attrs = attrs.ok_or_else(|| Error::from(E_FAIL))?;
        attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
        attrs.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 0)?;
        let mut _mgr_keep = None;
        if gpu {
            let mut token = 0u32;
            let mut mgr: Option<IMFDXGIDeviceManager> = None;
            MFCreateDXGIDeviceManager(&mut token, &mut mgr)?;
            let mgr = mgr.ok_or_else(|| Error::from(E_FAIL))?;
            mgr.ResetDevice(&device, token)?;
            attrs.SetUnknown(&MF_SINK_WRITER_D3D_MANAGER, &mgr)?;
            _mgr_keep = Some(mgr);
        }
        let wpath = crate::output::wide(&path.to_string_lossy());
        let writer = MFCreateSinkWriterFromURL(PCWSTR(wpath.as_ptr()), None, &attrs)?;

        let outt = MFCreateMediaType()?;
        outt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        outt.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
        let bitrate = ((w as u64 * h as u64 * fps as u64) / 8).clamp(1_000_000, 40_000_000) as u32;
        outt.SetUINT32(&MF_MT_AVG_BITRATE, bitrate)?;
        outt.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        set_ratio(&outt, &MF_MT_FRAME_SIZE, w, h)?;
        set_ratio(&outt, &MF_MT_FRAME_RATE, fps, 1)?;
        set_ratio(&outt, &MF_MT_PIXEL_ASPECT_RATIO, 1, 1)?;
        let stream = writer.AddStream(&outt)?;

        let inp = MFCreateMediaType()?;
        inp.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        inp.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
        inp.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        set_ratio(&inp, &MF_MT_FRAME_SIZE, w, h)?;
        set_ratio(&inp, &MF_MT_FRAME_RATE, fps, 1)?;
        set_ratio(&inp, &MF_MT_PIXEL_ASPECT_RATIO, 1, 1)?;
        inp.SetUINT32(&MF_MT_DEFAULT_STRIDE, w * 4)?;
        writer.SetInputMediaType(stream, &inp, None)?;
        writer.BeginWriting()?;
        crate::trace(&format!("record: writing ({}) {}x{} @{}", if gpu { "gpu" } else { "cpu" }, w, h, fps));

        // ---- textures
        let desc = |usage, bind: u32, cpu: u32, misc: u32| D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: usage,
            BindFlags: bind,
            CPUAccessFlags: cpu,
            MiscFlags: misc,
        };
        let mk = |d: D3D11_TEXTURE2D_DESC| -> Result<ID3D11Texture2D> {
            let mut t = None;
            device.CreateTexture2D(&d, None, Some(&mut t))?;
            t.ok_or_else(|| Error::from(E_FAIL))
        };
        // latest clean desktop crop
        let latest =
            mk(desc(D3D11_USAGE_DEFAULT, (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32, 0, 0))?;
        // composition surface + a small GDI-compatible patch: only the pixels around
        // the cursor ever go through GDI, the full frame stays on the GPU.
        let compose =
            mk(desc(D3D11_USAGE_DEFAULT, (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32, 0, 0))?;
        let mut pd = desc(
            D3D11_USAGE_DEFAULT,
            (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            0,
            D3D11_RESOURCE_MISC_GDI_COMPATIBLE.0 as u32,
        );
        pd.Width = PATCH;
        pd.Height = PATCH;
        let patch = mk(pd)?;
        // ring of frames handed to the encoder (encoder may hold a few)
        const RING: usize = 6;
        let mut ring = Vec::with_capacity(RING);
        for _ in 0..RING {
            ring.push(if gpu {
                mk(desc(D3D11_USAGE_DEFAULT, (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32, 0, 0))?
            } else {
                mk(desc(D3D11_USAGE_STAGING, 0, D3D11_CPU_ACCESS_READ.0 as u32, 0))?
            });
        }
        let src_box = D3D11_BOX { left: sx, top: sy, front: 0, right: sx + w, bottom: sy + h, back: 1 };
        let rotated = !matches!(out.rotation, DXGI_MODE_ROTATION_IDENTITY | DXGI_MODE_ROTATION_UNSPECIFIED);
        let mut rotator: Option<Rotator> = None;

        // Duplication's first frame is empty until something changes on screen:
        // seed the first video frame from a GDI grab of the region instead.
        let mut have = false;
        if let Some(seed) =
            crate::capture::grab_pixels(RECT { left: l, top: t, right: l + w as i32, bottom: t + h as i32 })
        {
            ctx.UpdateSubresource(&latest, 0, None, seed.px.as_ptr() as *const _, w * 4, 0);
            have = true;
        }
        let freq = qpf();
        let start = qpc();
        let interval = freq / fps as i64;
        let mut next_due = start;
        let mut last_write: i64 = -1;
        let mut dirty = false;
        let mut ri = 0usize;
        let mut written = 0u64;
        let ticks_100ns = |q: i64| -> i64 { ((q - start) as i128 * 10_000_000 / freq as i128) as i64 };

        while !stop.load(Ordering::SeqCst) {
            let wait_ms = (((next_due - qpc()).max(0) * 1000) / freq).clamp(0, 100) as u32;
            let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut res: Option<IDXGIResource> = None;
            match dup.AcquireNextFrame(wait_ms, &mut info, &mut res) {
                Ok(()) => {
                    if info.LastPresentTime != 0 || !have {
                        if let Some(res) = res {
                            if let Ok(tex) = res.cast::<ID3D11Texture2D>() {
                                if rotated {
                                    if rotator.is_none() {
                                        let mut td = D3D11_TEXTURE2D_DESC::default();
                                        tex.GetDesc(&mut td);
                                        let (dw, dh) = (
                                            (out.desk.right - out.desk.left) as f32,
                                            (out.desk.bottom - out.desk.top) as f32,
                                        );
                                        rotator = Some(Rotator::new(
                                            &device,
                                            &td,
                                            &latest,
                                            out.rotation,
                                            dw,
                                            dh,
                                            sx as f32,
                                            sy as f32,
                                        )?);
                                    }
                                    if let Some(r) = &rotator {
                                        r.draw(&ctx, &tex);
                                    }
                                } else {
                                    ctx.CopySubresourceRegion(&latest, 0, 0, 0, 0, &tex, 0, Some(&src_box));
                                }
                                have = true;
                            }
                        }
                    }
                    dirty = true; // content or pointer moved
                    let _ = dup.ReleaseFrame();
                }
                Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => {}
                Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                    // mode change / secure desktop: recreate duplication
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    match out.output.DuplicateOutput(&device) {
                        Ok(d) => dup = d,
                        Err(_) => continue,
                    }
                    continue;
                }
                Err(e) => return finalize(&writer, written).and(Err(e)),
            }
            let now = qpc();
            if now < next_due || !have {
                continue;
            }
            // write at most `fps`; when idle, still emit one frame per second so
            // players show a continuous timeline.
            if dirty || last_write < 0 || now - last_write >= freq {
                let dst = &ring[ri];
                ri = (ri + 1) % RING;
                if cursor {
                    ctx.CopyResource(&compose, &latest);
                    draw_cursor(&ctx, &compose, &patch, l, t, w, h);
                    ctx.CopyResource(dst, &compose);
                } else {
                    ctx.CopyResource(dst, &latest);
                }
                let sample = MFCreateSample()?;
                let buf: IMFMediaBuffer = if gpu {
                    let surf_buf = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, dst, 0, false)?;
                    if let Ok(b2) = surf_buf.cast::<IMF2DBuffer>() {
                        let len = b2.GetContiguousLength()?;
                        surf_buf.SetCurrentLength(len)?;
                    }
                    surf_buf
                } else {
                    let size = w * h * 4;
                    let mb = MFCreateMemoryBuffer(size)?;
                    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                    ctx.Map(dst, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
                    let mut p: *mut u8 = std::ptr::null_mut();
                    mb.Lock(&mut p, None, None)?;
                    for y in 0..h as usize {
                        std::ptr::copy_nonoverlapping(
                            (mapped.pData as *const u8).add(y * mapped.RowPitch as usize),
                            p.add(y * w as usize * 4),
                            w as usize * 4,
                        );
                    }
                    mb.Unlock()?;
                    ctx.Unmap(dst, 0);
                    mb.SetCurrentLength(size)?;
                    mb
                };
                sample.AddBuffer(&buf)?;
                let ts = ticks_100ns(now);
                sample.SetSampleTime(ts)?;
                sample.SetSampleDuration(10_000_000 / fps as i64)?;
                writer.WriteSample(stream, &sample)?;
                written += 1;
                last_write = now;
                dirty = false;
            }
            next_due += interval;
            if next_due < now {
                next_due = now + interval;
            }
        }
        crate::trace(&format!("record: {} frames written", written));
        finalize(&writer, written)
    }
}

unsafe fn finalize(writer: &IMFSinkWriter, written: u64) -> Result<()> {
    if written == 0 {
        return Err(Error::new(E_FAIL, crate::i18n::t("err.no_frames")));
    }
    writer.Finalize()
}

const PATCH: u32 = 128;

/// Draw the mouse cursor: copy the 128x128 area under it into a small GDI-compatible
/// texture, draw there, copy back. Cheap regardless of the recording size.
unsafe fn draw_cursor(
    ctx: &ID3D11DeviceContext,
    frame: &ID3D11Texture2D,
    patch: &ID3D11Texture2D,
    ox: i32,
    oy: i32,
    w: u32,
    h: u32,
) {
    let mut ci = CURSORINFO { cbSize: std::mem::size_of::<CURSORINFO>() as u32, ..Default::default() };
    if GetCursorInfo(&mut ci).is_err() || ci.flags.0 & CURSOR_SHOWING.0 == 0 {
        return;
    }
    // hotspot lookup copies the cursor bitmaps: do it only when the cursor changes
    thread_local! {
        static HOT: std::cell::Cell<(isize, i32, i32)> = const { std::cell::Cell::new((0, 0, 0)) };
    }
    let key = ci.hCursor.0 as isize;
    let (k, mut hx, mut hy) = HOT.with(|h| h.get());
    if k != key {
        let mut ii = ICONINFO::default();
        if GetIconInfo(ci.hCursor.into(), &mut ii).is_err() {
            return;
        }
        hx = ii.xHotspot as i32;
        hy = ii.yHotspot as i32;
        if !ii.hbmMask.is_invalid() {
            let _ = windows::Win32::Graphics::Gdi::DeleteObject(ii.hbmMask.into());
        }
        if !ii.hbmColor.is_invalid() {
            let _ = windows::Win32::Graphics::Gdi::DeleteObject(ii.hbmColor.into());
        }
        HOT.with(|h| h.set((key, hx, hy)));
    }
    // cursor top-left in frame coords and the patch rect clipped to the frame
    let cx = ci.ptScreenPos.x - ox - hx;
    let cy = ci.ptScreenPos.y - oy - hy;
    let x0 = cx.max(0);
    let y0 = cy.max(0);
    let x1 = (cx + PATCH as i32).min(w as i32);
    let y1 = (cy + PATCH as i32).min(h as i32);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let (pw, ph) = ((x1 - x0) as u32, (y1 - y0) as u32);
    let src = D3D11_BOX { left: x0 as u32, top: y0 as u32, front: 0, right: x1 as u32, bottom: y1 as u32, back: 1 };
    ctx.CopySubresourceRegion(patch, 0, 0, 0, 0, frame, 0, Some(&src));
    let Ok(surf) = patch.cast::<IDXGISurface1>() else { return };
    if let Ok(dc) = surf.GetDC(false) {
        let _ = DrawIconEx(dc, cx - x0, cy - y0, ci.hCursor.into(), 0, 0, 0, None, DI_NORMAL);
        let _ = surf.ReleaseDC(None);
    }
    let back = D3D11_BOX { left: 0, top: 0, front: 0, right: pw, bottom: ph, back: 1 };
    ctx.CopySubresourceRegion(frame, 0, x0 as u32, y0 as u32, 0, patch, 0, Some(&back));
}
