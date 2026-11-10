//! The WispShot mark: a rounded square in a sky-to-violet gradient with a soft "W" wave
//! and a small spark above it. Same geometry as assets/logo.svg (64-unit grid).
//! Used by the tray icon (software raster, no Direct2D in the tray process) and the
//! settings window (Direct2D).

/// Gradient ends, 0xRRGGBB: bottom-left -> top-right.
pub const C0: u32 = 0x0EA5E9;
pub const C1: u32 = 0x8B5CF6;
/// Rounded square: 4..60 with radius 15.
pub const BOX: (f32, f32, f32) = (4.0, 60.0, 15.0);
/// The wave: three cubic Béziers (start point, then 3 control points each).
pub const WAVE_START: (f32, f32) = (14.0, 25.0);
pub const WAVE: [[(f32, f32); 3]; 3] = [
    [(17.0, 40.0), (21.0, 44.0), (25.0, 37.0)],
    [(28.0, 31.0), (36.0, 31.0), (39.0, 37.0)],
    [(43.0, 44.0), (47.0, 40.0), (50.0, 25.0)],
];
pub const WAVE_WIDTH: f32 = 6.0;
pub const SPARK: (f32, f32, f32) = (32.0, 18.0, 3.6);

/// The wave flattened to a polyline (64-unit grid).
pub fn wave_points() -> Vec<(f32, f32)> {
    let mut pts = vec![WAVE_START];
    let mut p0 = WAVE_START;
    for [c1, c2, p3] in WAVE {
        for i in 1..=16 {
            let t = i as f32 / 16.0;
            let u = 1.0 - t;
            let x = u * u * u * p0.0 + 3.0 * u * u * t * c1.0 + 3.0 * u * t * t * c2.0 + t * t * t * p3.0;
            let y = u * u * u * p0.1 + 3.0 * u * u * t * c1.1 + 3.0 * u * t * t * c2.1 + t * t * t * p3.1;
            pts.push((x, y));
        }
        p0 = p3;
    }
    pts
}

fn seg_dist(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 0.0 { (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
    ((a.0 + t * dx - p.0).powi(2) + (a.1 + t * dy - p.1).powi(2)).sqrt()
}

/// Render the mark into `n`×`n` premultiplied-free ARGB pixels (alpha in the top byte).
/// `rec`: the "recording" variant (dark square, red dot).
pub fn raster(n: usize, rec: bool) -> Vec<u32> {
    let wave = wave_points();
    let ss = 4;
    let k = 64.0 / n as f32;
    let (lo, hi, r) = BOX;
    let (c, h) = ((lo + hi) / 2.0, (hi - lo) / 2.0);
    // small sizes get a bolder stroke and spark so they stay sharp in the tray
    let (ww, sr) = if n <= 24 { (8.0, 4.6) } else { (WAVE_WIDTH, SPARK.2) };
    let mut px = vec![0u32; n * n];
    for y in 0..n {
        for x in 0..n {
            let (mut bg, mut fg) = (0u32, 0u32);
            for sy in 0..ss {
                for sx in 0..ss {
                    let u = (x as f32 + (sx as f32 + 0.5) / ss as f32) * k;
                    let v = (y as f32 + (sy as f32 + 0.5) / ss as f32) * k;
                    let dx = ((u - c).abs() - (h - r)).max(0.0);
                    let dy = ((v - c).abs() - (h - r)).max(0.0);
                    if (dx * dx + dy * dy).sqrt() > r {
                        continue;
                    }
                    bg += 1;
                    let on = if rec {
                        ((u - 32.0).powi(2) + (v - 32.0).powi(2)).sqrt() < 15.0
                    } else {
                        wave.windows(2).any(|w| seg_dist((u, v), w[0], w[1]) <= ww / 2.0)
                            || ((u - SPARK.0).powi(2) + (v - SPARK.1).powi(2)).sqrt() <= sr
                    };
                    if on {
                        fg += 1;
                    }
                }
            }
            if bg == 0 {
                continue;
            }
            // gradient position along the bottom-left -> top-right diagonal
            let (u, v) = ((x as f32 + 0.5) * k, (y as f32 + 0.5) * k);
            let t = (((u - 6.0) - (v - 58.0)) / 104.0).clamp(0.0, 1.0);
            let lerp = |sh: u32| -> u32 {
                let (a, b) = (((C0 >> sh) & 0xFF) as f32, ((C1 >> sh) & 0xFF) as f32);
                (a + (b - a) * t) as u32
            };
            let base = if rec { [0x30, 0x30, 0x34] } else { [lerp(16), lerp(8), lerp(0)] };
            let ink = if rec { [0xFF, 0x3B, 0x3B] } else { [0xFF, 0xFF, 0xFF] };
            let mix = |b: u32, f: u32| (b * (bg - fg) + f * fg) / bg;
            let a = bg * 255 / (ss * ss) as u32;
            px[y * n + x] = (a << 24) | (mix(base[0], ink[0]) << 16) | (mix(base[1], ink[1]) << 8) | mix(base[2], ink[2]);
        }
    }
    px
}
