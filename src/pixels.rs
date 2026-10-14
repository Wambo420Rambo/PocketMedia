//! Small premultiplied BGRA images (thumbnails and preview frames) and the
//! pure pixel work on them: the fitted size, scaling down and rotating.

/// Premultiplied BGRA, top row first, `width * 4` bytes per row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl Pixels {
    pub fn bytes(&self) -> usize {
        self.data.len()
    }
}

/// A zeroed buffer for `width` x `height` 32-bit pixels. Sizes are computed in
/// u64 (a u32 `w * 4 * h` wraps), anything above `max_pixels` is refused, and a
/// failed allocation is an error instead of an abort.
pub fn bgra_buffer(width: u32, height: u32, max_pixels: u64) -> Result<Vec<u8>, String> {
    let pixels = width as u64 * height as u64;
    if pixels > max_pixels {
        return Err(format!("{width} x {height} is too large to preview"));
    }
    let len = pixels.checked_mul(4).and_then(|b| usize::try_from(b).ok()).ok_or_else(|| format!("{width} x {height} is too large to preview"))?;
    let mut data = Vec::new();
    data.try_reserve_exact(len).map_err(|_| format!("not enough memory for {width} x {height} pixels"))?;
    data.resize(len, 0);
    Ok(data)
}

/// The largest size with `src`'s aspect that fits in `max_w` x `max_h`, never
/// larger than `src` itself (no upscaling) and at least 1 x 1.
pub fn fit(src_w: u32, src_h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if src_w == 0 || src_h == 0 || max_w == 0 || max_h == 0 {
        return (0, 0);
    }
    let scale = (max_w as f64 / src_w as f64).min(max_h as f64 / src_h as f64).min(1.0);
    let w = ((src_w as f64 * scale).round() as u32).clamp(1, max_w);
    let h = ((src_h as f64 * scale).round() as u32).clamp(1, max_h);
    (w, h)
}

/// A source image to scale: 32-bit BGRA rows `stride` bytes apart.
pub struct Source<'a> {
    pub data: &'a [u8],
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    /// The fourth byte is not alpha (video RGB32): the result is opaque.
    pub opaque: bool,
    /// The last row in `data` is the top of the picture.
    pub bottom_up: bool,
}

impl Source<'_> {
    fn valid(&self) -> bool {
        let row = self.width as usize * 4;
        self.width > 0
            && self.height > 0
            && self.stride >= row
            && self.data.len() >= self.stride * (self.height as usize - 1) + row
    }
}

/// Which source pixels one output pixel covers, with the partial weights of the
/// first and last one (box filter: every source pixel counts by its overlap).
struct Span {
    first: usize,
    last: usize,
    w_first: f32,
    w_last: f32,
    norm: f32,
}

impl Span {
    fn weight(&self, i: usize) -> f32 {
        if i == self.first {
            self.w_first
        } else if i == self.last {
            self.w_last
        } else {
            1.0
        }
    }
}

fn spans(src: usize, dst: usize) -> Vec<Span> {
    let f = src as f64 / dst as f64;
    (0..dst)
        .map(|i| {
            let a = i as f64 * f;
            let b = ((i + 1) as f64 * f).min(src as f64);
            let first = (a.floor() as usize).min(src - 1);
            let last = ((b.ceil() as usize).max(first + 1) - 1).min(src - 1);
            let (w_first, w_last) =
                if first == last { (b - a, b - a) } else { ((first + 1) as f64 - a, b - last as f64) };
            Span { first, last, w_first: w_first as f32, w_last: w_last as f32, norm: (1.0 / (b - a)) as f32 }
        })
        .collect()
}

/// Scales `src` to `out_w` x `out_h` by averaging the source pixels each output
/// pixel covers (good for shrinking; the only direction PocketMedia scales).
/// Works one output row at a time, so only one row of sums is kept.
pub fn scale(src: &Source, out_w: u32, out_h: u32) -> Option<Pixels> {
    if !src.valid() || out_w == 0 || out_h == 0 {
        return None;
    }
    let (sw, ow) = (src.width as usize, out_w as usize);
    let xs = spans(sw, ow);
    let ys = spans(src.height as usize, out_h as usize);
    let mut acc = vec![0f32; sw * 4];
    let mut out = Vec::with_capacity(ow * out_h as usize * 4);
    for s in &ys {
        acc.iter_mut().for_each(|a| *a = 0.0);
        for sy in s.first..=s.last {
            let w = s.weight(sy);
            let at = if src.bottom_up { src.height as usize - 1 - sy } else { sy } * src.stride;
            let row = &src.data[at..at + sw * 4];
            for (a, &v) in acc.iter_mut().zip(row) {
                *a += v as f32 * w;
            }
        }
        for t in &xs {
            let mut c = [0f32; 4];
            for sx in t.first..=t.last {
                let w = t.weight(sx);
                for (k, ck) in c.iter_mut().enumerate() {
                    *ck += acc[sx * 4 + k] * w;
                }
            }
            let n = s.norm * t.norm;
            let px = c.map(|v| (v * n).round().clamp(0.0, 255.0) as u8);
            if src.opaque {
                out.extend_from_slice(&[px[0], px[1], px[2], 255]);
            } else {
                // Premultiplied: no colour may exceed its alpha.
                out.extend_from_slice(&[px[0].min(px[3]), px[1].min(px[3]), px[2].min(px[3]), px[3]]);
            }
        }
    }
    Some(Pixels { width: out_w, height: out_h, data: out })
}

/// Clockwise rotation a video asks for when it is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rotation {
    None,
    Cw90,
    Cw180,
    Cw270,
}

impl Rotation {
    pub fn from_degrees(d: u32) -> Self {
        match d % 360 {
            90 => Rotation::Cw90,
            180 => Rotation::Cw180,
            270 => Rotation::Cw270,
            _ => Rotation::None,
        }
    }

    /// True when width and height trade places.
    pub fn swaps(self) -> bool {
        matches!(self, Rotation::Cw90 | Rotation::Cw270)
    }
}

/// `p` turned clockwise by `rot`.
pub fn rotate(p: Pixels, rot: Rotation) -> Pixels {
    if rot == Rotation::None {
        return p;
    }
    let (w, h) = (p.width as usize, p.height as usize);
    let (nw, nh) = if rot.swaps() { (h, w) } else { (w, h) };
    let mut out = vec![0u8; p.data.len()];
    for y in 0..h {
        for x in 0..w {
            let (nx, ny) = match rot {
                Rotation::Cw90 => (h - 1 - y, x),
                Rotation::Cw180 => (w - 1 - x, h - 1 - y),
                Rotation::Cw270 => (y, w - 1 - x),
                Rotation::None => (x, y),
            };
            let (s, d) = ((y * w + x) * 4, (ny * nw + nx) * 4);
            out[d..d + 4].copy_from_slice(&p.data[s..s + 4]);
        }
    }
    Pixels { width: nw as u32, height: nh as u32, data: out }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, px: [u8; 4]) -> Vec<u8> {
        px.iter().copied().cycle().take((w * h * 4) as usize).collect()
    }

    #[test]
    fn pixel_buffers_are_sized_in_u64_and_capped() {
        // 65535 x 65535 x 4 wraps to 4,294,443,012 in u32.
        assert!(bgra_buffer(65535, 65535, 1 << 30).is_err());
        assert!(bgra_buffer(65535, 65535, 2048 * 2048).is_err());
        assert!(bgra_buffer(2049, 2048, 2048 * 2048).is_err());
        assert_eq!(bgra_buffer(2048, 2048, 2048 * 2048).map(|b| b.len()), Ok(2048 * 2048 * 4));
        assert_eq!(bgra_buffer(3, 2, 6), Ok(vec![0; 24]));
        assert_eq!(bgra_buffer(0, 5, 6), Ok(vec![]));
        // An allocation that cannot succeed is an error, not an abort.
        assert!(bgra_buffer(u32::MAX, u32::MAX, u64::MAX).is_err());
    }

    #[test]
    fn fit_keeps_aspect_and_never_upscales() {
        assert_eq!(fit(1920, 1080, 128, 72), (128, 72));
        assert_eq!(fit(1080, 1920, 128, 72), (41, 72));
        assert_eq!(fit(100, 100, 128, 72), (72, 72));
        assert_eq!(fit(50, 20, 128, 72), (50, 20), "small media stays at its own size");
        assert_eq!(fit(10_000, 1, 128, 72), (128, 1), "never thinner than one pixel");
        assert_eq!(fit(0, 10, 128, 72), (0, 0));
    }

    #[test]
    fn scaling_a_solid_colour_keeps_it() {
        let data = solid(37, 23, [10, 20, 30, 255]);
        let src = Source { data: &data, width: 37, height: 23, stride: 37 * 4, opaque: false, bottom_up: false };
        let out = scale(&src, 5, 3).unwrap();
        assert_eq!((out.width, out.height), (5, 3));
        assert!(out.data.chunks(4).all(|p| p == [10, 20, 30, 255]), "{:?}", &out.data[..8]);
    }

    #[test]
    fn scaling_averages_what_each_pixel_covers() {
        // 2x1: black and white become one mid grey.
        let data = [0, 0, 0, 255, 255, 255, 255, 255];
        let src = Source { data: &data, width: 2, height: 1, stride: 8, opaque: false, bottom_up: false };
        assert_eq!(scale(&src, 1, 1).unwrap().data, [128, 128, 128, 255]);
        // 3 -> 2: the middle pixel is shared half and half.
        let data = [0, 0, 0, 255, 90, 90, 90, 255, 180, 180, 180, 255];
        let src = Source { data: &data, width: 3, height: 1, stride: 12, opaque: false, bottom_up: false };
        assert_eq!(scale(&src, 2, 1).unwrap().data, [30, 30, 30, 255, 150, 150, 150, 255]);
    }

    #[test]
    fn rows_may_be_padded_and_video_is_opaque() {
        // Two rows of one pixel, each padded to 8 bytes; the 4th byte is junk.
        let data = [100, 100, 100, 0, 9, 9, 9, 9, 200, 200, 200, 7];
        let src = Source { data: &data, width: 1, height: 2, stride: 8, opaque: true, bottom_up: false };
        assert_eq!(scale(&src, 1, 1).unwrap().data, [150, 150, 150, 255]);
        let short = Source { data: &data[..8], width: 1, height: 2, stride: 8, opaque: true, bottom_up: false };
        assert!(scale(&short, 1, 1).is_none(), "too little data is refused");
    }

    #[test]
    fn bottom_up_rows_come_out_top_first() {
        let data = [1, 1, 1, 255, 2, 2, 2, 255];
        let src = Source { data: &data, width: 1, height: 2, stride: 4, opaque: false, bottom_up: true };
        assert_eq!(scale(&src, 1, 2).unwrap().data, [2, 2, 2, 255, 1, 1, 1, 255]);
    }

    #[test]
    fn rotation_moves_corners_clockwise() {
        // 2x1: A B  -> 90° clockwise -> A over B (1x2).
        let p = Pixels { width: 2, height: 1, data: vec![1, 1, 1, 255, 2, 2, 2, 255] };
        let r = rotate(p.clone(), Rotation::Cw90);
        assert_eq!((r.width, r.height), (1, 2));
        assert_eq!(r.data, vec![1, 1, 1, 255, 2, 2, 2, 255]);
        let r = rotate(p.clone(), Rotation::Cw270);
        assert_eq!(r.data, vec![2, 2, 2, 255, 1, 1, 1, 255]);
        let r = rotate(p.clone(), Rotation::Cw180);
        assert_eq!((r.width, r.data[0]), (2, 2));
        assert_eq!(Rotation::from_degrees(90), Rotation::Cw90);
        assert_eq!(Rotation::from_degrees(45), Rotation::None);
    }
}
