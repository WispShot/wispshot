//! Share frame: the capture on a background with a margin, rounded corners and a
//! soft shadow, ready to post. Plain CPU work on the finished image.

use crate::img::{self, Img};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Style {
    Off,
    /// Diagonal blue-violet gradient.
    Color,
    Light,
    Dark,
}

impl Style {
    pub const ALL: [Style; 4] = [Style::Off, Style::Color, Style::Light, Style::Dark];

    pub fn ini(self) -> &'static str {
        match self {
            Style::Off => "off",
            Style::Color => "color",
            Style::Light => "light",
            Style::Dark => "dark",
        }
    }

    pub fn parse(s: &str) -> Style {
        match s.trim().to_ascii_lowercase().as_str() {
            "color" | "colour" | "renkli" => Style::Color,
            "light" | "acik" | "açık" => Style::Light,
            "dark" | "koyu" => Style::Dark,
            _ => Style::Off,
        }
    }
}

fn mix(a: u32, b: u32, t: f32) -> u32 {
    let ch = |s: u32| {
        let (x, y) = (((a >> s) & 255) as f32, ((b >> s) & 255) as f32);
        ((x + (y - x) * t).round() as u32) << s
    };
    0xFF00_0000 | ch(16) | ch(8) | ch(0)
}

/// How much of the pixel (x, y) lies inside a w×h rectangle with corner radius r (0..1).
fn coverage(x: i32, y: i32, w: i32, h: i32, r: f32) -> f32 {
    let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
    let cx = px.clamp(r, w as f32 - r);
    let cy = py.clamp(r, h as f32 - r);
    let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
    (r - d + 0.5).clamp(0.0, 1.0)
}

/// The framed image; `Off` returns a copy.
pub fn apply(src: &Img, style: Style) -> Img {
    if style == Style::Off || src.w <= 0 || src.h <= 0 {
        return src.clone();
    }
    let (w, h) = (src.w, src.h);
    let pad = ((w.min(h) as f32 * 0.08) as i32).clamp(32, 96);
    let r = 12.0f32.min(w.min(h) as f32 / 4.0);
    let (ow, oh) = (w + 2 * pad, h + 2 * pad);
    let mut out = Img::new(ow, oh);
    // background
    for y in 0..oh {
        for x in 0..ow {
            out.px[(y * ow + x) as usize] = match style {
                Style::Color => {
                    let t = (x as f32 / ow as f32 * 0.6 + y as f32 / oh as f32 * 0.4).clamp(0.0, 1.0);
                    mix(0xFF4F_6BFF, 0xFFB0_5CFF, t)
                }
                Style::Light => 0xFFF1_F2F5,
                _ => 0xFF1E_1F22,
            };
        }
    }
    // shadow: a blurred copy of the rounded shape, a little lower
    let blur = (pad / 3).max(6);
    let (sx, sy) = (pad, pad + pad / 6);
    let mut mask = Img::new(ow, oh);
    for y in 0..h {
        for x in 0..w {
            let a = (coverage(x, y, w, h, r) * 255.0) as u32;
            let i = ((y + sy) * ow + x + sx) as usize;
            mask.px[i] = 0xFF00_0000 | a << 16 | a << 8 | a;
        }
    }
    img::blur(&mut mask, blur);
    let strength = if style == Style::Dark { 0.6 } else { 0.35 };
    for (o, m) in out.px.iter_mut().zip(&mask.px) {
        let a = (m & 255) as f32 / 255.0 * strength;
        if a > 0.0 {
            *o = mix(*o, 0xFF00_0000, a);
        }
    }
    // the capture with rounded corners
    for y in 0..h {
        for x in 0..w {
            let c = coverage(x, y, w, h, r);
            if c <= 0.0 {
                continue;
            }
            let i = ((y + pad) * ow + x + pad) as usize;
            let p = src.px[(y * w + x) as usize] | 0xFF00_0000;
            out.px[i] = if c >= 1.0 { p } else { mix(out.px[i], p, c) };
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_with_margin_and_keeps_pixels() {
        let mut src = Img::new(200, 100);
        src.px.iter_mut().for_each(|p| *p = 0xFF12_3456);
        let out = apply(&src, Style::Light);
        let pad = 32;
        assert_eq!((out.w, out.h), (200 + 2 * pad, 100 + 2 * pad));
        // the middle of the capture is untouched, the corner of the frame is background
        assert_eq!(out.px[((pad + 50) * out.w + pad + 100) as usize], 0xFF12_3456);
        assert_eq!(out.px[0], 0xFFF1_F2F5);
        // the very corner pixel of the capture is rounded away
        assert_ne!(out.px[(pad * out.w + pad) as usize], 0xFF12_3456);
        assert_eq!(apply(&src, Style::Off).w, 200);
    }
}
