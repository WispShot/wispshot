//! `wispshot.exe --selftest <dir>`: runs every native API the app depends on, without
//! showing a window, and writes `selftest.txt` into <dir>. Used by CI on x64 and ARM64
//! machines. Exit code 0 = the core path works (screen grab, Direct2D/DirectWrite,
//! image encoding); OCR and video are reported but may be missing on server images.

use crate::gfx::{self, Gfx, Item};
use crate::img::{self, Img};
use std::fmt::Write as _;
use std::path::Path;
use windows::Win32::Foundation::RECT;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};

pub fn run(dir: &Path) -> i32 {
    let _ = std::fs::create_dir_all(dir);
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let mut log = String::new();
    let mut core_ok = true;
    let _ = writeln!(log, "WispShot {} selftest, {}", env!("CARGO_PKG_VERSION"), std::env::consts::ARCH);

    let mut step = |name: &str, core: bool, r: Result<String, String>| {
        let (tag, msg) = match r {
            Ok(m) => ("PASS", m),
            Err(m) => {
                if core {
                    core_ok = false;
                }
                (if core { "FAIL" } else { "WARN" }, m)
            }
        };
        let _ = writeln!(log, "{tag} {name}: {msg}");
    };

    // 1. GDI screen grab
    let vs = crate::capture::virtual_screen();
    let shot = crate::capture::grab_pixels(vs);
    step(
        "screen grab",
        true,
        shot.as_ref().map(|i| format!("{}x{}", i.w, i.h)).ok_or_else(|| "BitBlt failed".to_string()),
    );
    let base = shot.unwrap_or_else(|| Img::new(320, 240));

    // 2. Direct2D + DirectWrite: annotations rendered over a crop
    let g = Gfx::new().map_err(|e| e.message());
    let annotated = g.as_ref().map_err(|e| e.clone()).and_then(|g| {
        let (w, h) = (base.w.min(640), base.h.min(480));
        let items = vec![
            Item::Rect { r: gfx::rc(10.0, 10.0, 200.0, 120.0), c: 0xFFE53935, w: 3.0 },
            Item::Arrow { a: (300.0, 200.0), b: (210.0, 130.0), c: 0xFFE53935, w: 3.0 },
            Item::Counter { x: 40.0, y: 160.0, n: 1, c: 0xFFE53935, size: 18.0 },
            Item::Text { x: 30.0, y: 200.0, s: "WispShot".into(), c: 0xFFE53935, size: 24.0 },
        ];
        gfx::export(g, &base, (0, 0, w, h), &items).map_err(|e| e.message())
    });
    step("direct2d + directwrite", true, annotated.as_ref().map(|i| format!("{}x{}", i.w, i.h)).map_err(|e| e.clone()));

    // 3. CPU effects
    let mut fx = base.crop(0, 0, base.w.min(400), base.h.min(300));
    img::blur(&mut fx, 12);
    img::pixelate(&mut fx, 12);
    step("blur + pixelate", true, Ok(format!("{}x{}", fx.w, fx.h)));

    // 4. WIC encoding
    let out = annotated.unwrap_or(fx);
    for name in ["selftest.png", "selftest.jpg"] {
        let p = dir.join(name);
        let r = crate::output::save(&out, &p)
            .map_err(|e| e.message())
            .and_then(|_| std::fs::metadata(&p).map_err(|e| e.to_string()))
            .and_then(|m| if m.len() > 0 { Ok(format!("{} bytes", m.len())) } else { Err("empty file".into()) });
        step(name, true, r);
    }

    // 5. OCR on rendered text (needs an OCR language installed: optional)
    let ocr = g.as_ref().map_err(|e| e.clone()).and_then(|g| {
        let mut page = Img::new(720, 140);
        page.px.iter_mut().for_each(|p| *p = 0xFFFF_FFFF);
        let items = vec![Item::Text { x: 20.0, y: 30.0, s: "WispShot 2048".into(), c: 0xFF000000, size: 56.0 }];
        let page = gfx::export(g, &page, (0, 0, 720, 140), &items).map_err(|e| e.message())?;
        // WinRT OCR wants a multithreaded apartment: run it on its own thread like the app does
        let text = std::thread::spawn(move || crate::ocr::recognize(&page).map_err(|e| e.message()))
            .join()
            .map_err(|_| "OCR thread panicked".to_string())??;
        if text.contains("2048") {
            Ok(format!("read {:?}", text.trim()))
        } else {
            Err(format!("unexpected text {:?}", text.trim()))
        }
    });
    step("ocr", false, ocr);

    // 6. Video: Desktop Duplication + Media Foundation H.264 for ~1.5 s
    let p = dir.join("selftest.mp4");
    let rect = RECT {
        left: vs.left,
        top: vs.top,
        right: vs.left + 320.min(vs.right - vs.left),
        bottom: vs.top + 240.min(vs.bottom - vs.top),
    };
    let rec = crate::record::Recorder::start(rect, 30, true, p.clone());
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let r = rec.stop().and_then(|f| std::fs::metadata(&f).map_err(|e| e.to_string())).and_then(|m| {
        if m.len() > 0 {
            Ok(format!("{} bytes", m.len()))
        } else {
            Err("empty file".into())
        }
    });
    step("video (h.264)", false, r);

    let _ = writeln!(log, "{}", if core_ok { "OK" } else { "FAILED" });
    let _ = std::fs::write(dir.join("selftest.txt"), log);
    if core_ok {
        0
    } else {
        1
    }
}
