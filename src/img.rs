//! Plain BGRA image buffer + CPU effects (blur, pixelate). No dependencies.

#[derive(Clone)]
pub struct Img {
    pub w: i32,
    pub h: i32,
    /// BGRA, 0xAARRGGBB in little-endian u32, top-down rows, stride = w.
    pub px: Vec<u32>,
}

impl Img {
    pub fn new(w: i32, h: i32) -> Self {
        Img { w, h, px: vec![0xFF00_0000; (w.max(0) * h.max(0)) as usize] }
    }

    #[inline]
    pub fn get(&self, x: i32, y: i32) -> u32 {
        let x = x.clamp(0, self.w - 1);
        let y = y.clamp(0, self.h - 1);
        self.px[(y * self.w + x) as usize]
    }

    /// Crop (rect is clamped to the image).
    pub fn crop(&self, x: i32, y: i32, w: i32, h: i32) -> Img {
        let x0 = x.clamp(0, self.w);
        let y0 = y.clamp(0, self.h);
        let x1 = (x + w).clamp(0, self.w);
        let y1 = (y + h).clamp(0, self.h);
        let (cw, ch) = (x1 - x0, y1 - y0);
        let mut out = Vec::with_capacity((cw * ch).max(0) as usize);
        for row in y0..y1 {
            let s = (row * self.w + x0) as usize;
            out.extend_from_slice(&self.px[s..s + cw as usize]);
        }
        Img { w: cw, h: ch, px: out }
    }

    pub fn as_bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.px.as_ptr() as *const u8, self.px.len() * 4) }
    }

    pub fn force_opaque(&mut self) {
        for p in self.px.iter_mut() {
            *p |= 0xFF00_0000;
        }
    }

    /// Smooth (bilinear) integer upscale, written row by row into `dst` (BGRA bytes,
    /// `stride` bytes per row). Screen text is read by OCR far better this way than
    /// with blocky pixels.
    pub fn upscale_into(&self, f: i32, dst: &mut [u8], stride: usize) {
        if self.w <= 0 || self.h <= 0 {
            return;
        }
        // for each output column/row: the two source pixels and the weight (0..256) of the second
        let taps = |n: i32, len: i32| -> Vec<(usize, usize, u32)> {
            (0..n * f)
                .map(|o| {
                    let s = ((2 * o + 1) * 128 / f - 128).clamp(0, (len - 1) * 256);
                    let i = s / 256;
                    ((i) as usize, (i + 1).min(len - 1) as usize, (s % 256) as u32)
                })
                .collect()
        };
        let (xs, ys) = (taps(self.w, self.w), taps(self.h, self.h));
        let mix = |a: u32, b: u32, t: u32| -> u32 {
            let lo = ((a & 0x00FF_00FF) * (256 - t) + (b & 0x00FF_00FF) * t) >> 8;
            let hi = ((a >> 8) & 0x00FF_00FF) * (256 - t) + ((b >> 8) & 0x00FF_00FF) * t;
            (lo & 0x00FF_00FF) | (hi & 0xFF00_FF00)
        };
        let w = self.w as usize;
        let mut row = vec![0u32; xs.len()];
        for (oy, &(y0, y1, ty)) in ys.iter().enumerate() {
            let (r0, r1) = (&self.px[y0 * w..y0 * w + w], &self.px[y1 * w..y1 * w + w]);
            for (o, &(x0, x1, tx)) in row.iter_mut().zip(xs.iter()) {
                *o = mix(mix(r0[x0], r0[x1], tx), mix(r1[x0], r1[x1], tx), ty);
            }
            let out = &mut dst[oy * stride..oy * stride + row.len() * 4];
            for (d, b) in out.iter_mut().zip(row.iter().flat_map(|p| p.to_le_bytes())) {
                *d = b;
            }
        }
    }
}

/// Pixelate (mosaic) in place.
pub fn pixelate(img: &mut Img, block: i32) {
    let b = block.max(2);
    let mut by = 0;
    while by < img.h {
        let mut bx = 0;
        while bx < img.w {
            let (ex, ey) = ((bx + b).min(img.w), (by + b).min(img.h));
            let (mut r, mut g, mut bl, mut n) = (0u32, 0u32, 0u32, 0u32);
            for y in by..ey {
                for x in bx..ex {
                    let p = img.px[(y * img.w + x) as usize];
                    r += (p >> 16) & 255;
                    g += (p >> 8) & 255;
                    bl += p & 255;
                    n += 1;
                }
            }
            let c = 0xFF00_0000 | ((r / n) << 16) | ((g / n) << 8) | (bl / n);
            for y in by..ey {
                for x in bx..ex {
                    img.px[(y * img.w + x) as usize] = c;
                }
            }
            bx += b;
        }
        by += b;
    }
}

/// Approximate gaussian blur: 3 passes of a separable box blur (O(n) per pass,
/// independent of radius). Fast enough to run on multi-megapixel regions.
pub fn blur(img: &mut Img, radius: i32) {
    let r = radius.max(1);
    let (w, h) = (img.w as usize, img.h as usize);
    if w == 0 || h == 0 {
        return;
    }
    // Planar channels for cache-friendly passes.
    let mut ch: [Vec<u32>; 3] = [vec![0; w * h], vec![0; w * h], vec![0; w * h]];
    for (i, p) in img.px.iter().enumerate() {
        ch[0][i] = (p >> 16) & 255;
        ch[1][i] = (p >> 8) & 255;
        ch[2][i] = p & 255;
    }
    let mut tmp = vec![0u32; w * h];
    for c in ch.iter_mut() {
        for _ in 0..3 {
            box_h(c, &mut tmp, w, h, r as usize);
            box_v(&tmp, c, w, h, r as usize);
        }
    }
    for (i, p) in img.px.iter_mut().enumerate().take(w * h) {
        *p = 0xFF00_0000 | (ch[0][i] << 16) | (ch[1][i] << 8) | ch[2][i];
    }
}

fn box_h(src: &[u32], dst: &mut [u32], w: usize, h: usize, r: usize) {
    let d = (2 * r + 1) as u32;
    for y in 0..h {
        let row = &src[y * w..(y + 1) * w];
        let out = &mut dst[y * w..(y + 1) * w];
        let at = |i: isize| row[i.clamp(0, w as isize - 1) as usize];
        let mut acc: u32 = 0;
        for i in -(r as isize)..=(r as isize) {
            acc += at(i);
        }
        for (x, o) in out.iter_mut().enumerate() {
            *o = acc / d;
            acc += at(x as isize + r as isize + 1);
            acc -= at(x as isize - r as isize);
        }
    }
}

fn box_v(src: &[u32], dst: &mut [u32], w: usize, h: usize, r: usize) {
    let d = (2 * r + 1) as u32;
    let mut acc = vec![0u32; w];
    let row = |y: isize| (y.clamp(0, h as isize - 1) as usize) * w;
    for i in -(r as isize)..=(r as isize) {
        let o = row(i);
        for x in 0..w {
            acc[x] += src[o + x];
        }
    }
    for y in 0..h {
        let o = y * w;
        for x in 0..w {
            dst[o + x] = acc[x] / d;
        }
        let add = row(y as isize + r as isize + 1);
        let sub = row(y as isize - r as isize);
        for x in 0..w {
            acc[x] = acc[x] + src[add + x] - src[sub + x];
        }
    }
}
