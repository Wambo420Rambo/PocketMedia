//! Writes animated GIF files from BGRA frames: each frame gets its own palette
//! of up to 256 colours (median cut), and the picture data is LZW-compressed as
//! the GIF format requires. Used to turn Twitter's GIFs (kept there as MP4
//! videos) back into .gif files. No disk access: it writes to any `Write`.

use std::io::{self, Write};

/// Colours are counted at 5 bits per channel: 32768 boxes.
const BUCKETS: usize = 1 << 15;
const MIN_CODE: u8 = 8;

pub struct GifWriter<W: Write> {
    out: W,
    width: u16,
    height: u16,
    // Reused for every frame.
    count: Vec<u32>,
    sums: Vec<[u64; 3]>,
    map: Vec<u8>,
    tree: Vec<u16>,
}

impl<W: Write> GifWriter<W> {
    /// Starts a GIF of `width` x `height` that loops forever.
    pub fn new(mut out: W, width: u16, height: u16) -> io::Result<Self> {
        if width == 0 || height == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty picture"));
        }
        out.write_all(b"GIF89a")?;
        out.write_all(&width.to_le_bytes())?;
        out.write_all(&height.to_le_bytes())?;
        // No global colour table; each frame brings its own.
        out.write_all(&[0x00, 0, 0])?;
        // Loop forever (the NETSCAPE2.0 application extension, repeat count 0).
        out.write_all(&[0x21, 0xFF, 11])?;
        out.write_all(b"NETSCAPE2.0")?;
        out.write_all(&[3, 1, 0, 0, 0])?;
        Ok(Self {
            out,
            width,
            height,
            count: vec![0; BUCKETS],
            sums: vec![[0; 3]; BUCKETS],
            map: vec![0; BUCKETS],
            tree: Vec::new(),
        })
    }

    /// The picture's width and height.
    pub fn size(&self) -> (u32, u32) {
        (self.width as u32, self.height as u32)
    }

    /// Adds a frame: `bgra` is width x height pixels, 4 bytes each (alpha is
    /// ignored: the frames are opaque). It shows for `delay_cs` hundredths of a second.
    pub fn frame(&mut self, bgra: &[u8], delay_cs: u16) -> io::Result<()> {
        let pixels = self.width as usize * self.height as usize;
        if bgra.len() < pixels * 4 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "the frame is smaller than the picture"));
        }
        let bgra = &bgra[..pixels * 4];
        let palette = self.palette(bgra);
        let indices: Vec<u8> = bgra.chunks_exact(4).map(|p| self.map[bucket(p)]).collect();

        // Graphic control: keep the frame in place (disposal 1), no transparency.
        self.out.write_all(&[0x21, 0xF9, 4, 0x04])?;
        self.out.write_all(&delay_cs.max(2).to_le_bytes())?;
        self.out.write_all(&[0, 0])?;
        // Image descriptor with a local colour table of 256 entries.
        self.out.write_all(&[0x2C, 0, 0, 0, 0])?;
        self.out.write_all(&self.width.to_le_bytes())?;
        self.out.write_all(&self.height.to_le_bytes())?;
        self.out.write_all(&[0x80 | (MIN_CODE - 1)])?;
        let mut table = [0u8; 256 * 3];
        for (i, c) in palette.iter().enumerate() {
            table[i * 3..i * 3 + 3].copy_from_slice(c);
        }
        self.out.write_all(&table)?;
        self.out.write_all(&[MIN_CODE])?;
        let data = lzw(&indices, &mut self.tree);
        for block in data.chunks(255) {
            self.out.write_all(&[block.len() as u8])?;
            self.out.write_all(block)?;
        }
        self.out.write_all(&[0])
    }

    /// Ends the file and returns the writer.
    pub fn finish(mut self) -> io::Result<W> {
        self.out.write_all(&[0x3B])?;
        self.out.flush()?;
        Ok(self.out)
    }

    /// Up to 256 colours for the frame (as RGB), and `map` from each colour box
    /// to its palette entry.
    fn palette(&mut self, bgra: &[u8]) -> Vec<[u8; 3]> {
        self.count.fill(0);
        self.sums.fill([0; 3]);
        for p in bgra.chunks_exact(4) {
            let b = bucket(p);
            self.count[b] += 1;
            let s = &mut self.sums[b];
            s[0] += p[2] as u64;
            s[1] += p[1] as u64;
            s[2] += p[0] as u64;
        }
        // The colour boxes in use, as (bucket, r, g, b at 5 bits).
        let mut used: Vec<(usize, [u8; 3])> =
            (0..BUCKETS).filter(|b| self.count[*b] > 0).map(|b| (b, [(b >> 10) as u8, ((b >> 5) & 31) as u8, (b & 31) as u8])).collect();
        // Median cut: split the box with the most pixels times width along its
        // widest channel, at the pixel-weighted middle, until there are 256.
        let mut boxes: Vec<std::ops::Range<usize>> = Vec::with_capacity(256);
        boxes.push(0..used.len());
        while boxes.len() < 256 {
            let mut best: Option<(u64, usize, usize)> = None;
            for (i, r) in boxes.iter().enumerate() {
                if r.len() < 2 {
                    continue;
                }
                let (axis, width) = widest(&used[r.clone()]);
                let pixels: u64 = used[r.clone()].iter().map(|(b, _)| self.count[*b] as u64).sum();
                let score = pixels * width as u64;
                if best.is_none_or(|(s, _, _)| score > s) {
                    best = Some((score, i, axis));
                }
            }
            let Some((_, i, axis)) = best else { break };
            let r = boxes[i].clone();
            used[r.clone()].sort_unstable_by_key(|(_, c)| c[axis]);
            let total: u64 = used[r.clone()].iter().map(|(b, _)| self.count[*b] as u64).sum();
            let mut acc = 0u64;
            let mut cut = r.start + 1;
            for j in r.clone() {
                acc += self.count[used[j].0] as u64;
                if acc * 2 >= total {
                    cut = (j + 1).clamp(r.start + 1, r.end - 1);
                    break;
                }
            }
            boxes[i] = r.start..cut;
            boxes.push(cut..r.end);
        }
        let mut palette = Vec::with_capacity(boxes.len());
        for (index, r) in boxes.iter().enumerate() {
            let (mut n, mut s) = (0u64, [0u64; 3]);
            for (b, _) in &used[r.clone()] {
                n += self.count[*b] as u64;
                for (k, v) in s.iter_mut().enumerate() {
                    *v += self.sums[*b][k];
                }
                self.map[*b] = index as u8;
            }
            let n = n.max(1);
            palette.push([((s[0] + n / 2) / n) as u8, ((s[1] + n / 2) / n) as u8, ((s[2] + n / 2) / n) as u8]);
        }
        palette
    }
}

/// The colour box of a BGRA pixel: 5 bits each of red, green and blue.
fn bucket(p: &[u8]) -> usize {
    ((p[2] as usize >> 3) << 10) | ((p[1] as usize >> 3) << 5) | (p[0] as usize >> 3)
}

/// The channel with the largest spread in `colors`, and that spread.
fn widest(colors: &[(usize, [u8; 3])]) -> (usize, u8) {
    let mut lo = [u8::MAX; 3];
    let mut hi = [0u8; 3];
    for (_, c) in colors {
        for k in 0..3 {
            lo[k] = lo[k].min(c[k]);
            hi[k] = hi[k].max(c[k]);
        }
    }
    (0..3).map(|k| (k, hi[k] - lo[k])).max_by_key(|(_, w)| *w).unwrap_or((0, 0))
}

/// LZW-compresses palette indices with 8-bit minimum codes, as GIF stores
/// them (codes packed least significant bit first). `tree` is scratch space.
fn lzw(indices: &[u8], tree: &mut Vec<u16>) -> Vec<u8> {
    const CLEAR: u16 = 1 << MIN_CODE;
    const END: u16 = CLEAR + 1;
    // tree[code * 256 + next]: the code for `code` followed by `next`, 0 if none.
    // It is all zeros between calls; only the slots used are cleared again.
    if tree.len() != 4096 * 256 {
        *tree = vec![0; 4096 * 256];
    }
    let mut touched: Vec<u32> = Vec::with_capacity(4096);
    let mut bits = Bits::default();
    let mut size = MIN_CODE as u32 + 1;
    let mut max = END;
    bits.put(CLEAR, size);
    let mut cur: Option<u16> = None;
    for &k in indices {
        let Some(c) = cur else {
            cur = Some(k as u16);
            continue;
        };
        let slot = c as usize * 256 + k as usize;
        if tree[slot] != 0 {
            cur = Some(tree[slot]);
            continue;
        }
        bits.put(c, size);
        max += 1;
        tree[slot] = max;
        touched.push(slot as u32);
        if max as u32 >= 1 << size {
            size += 1;
        }
        if max == 4095 {
            bits.put(CLEAR, size);
            for t in touched.drain(..) {
                tree[t as usize] = 0;
            }
            size = MIN_CODE as u32 + 1;
            max = END;
        }
        cur = Some(k as u16);
    }
    if let Some(c) = cur {
        bits.put(c, size);
        // Reading that code, a decoder adds the entry made before it; when that
        // fills the current code size, it reads the next code one bit wider.
        if max > END && max as u32 + 1 == 1 << size && size < 12 {
            size += 1;
        }
    }
    bits.put(CLEAR, size);
    bits.put(END, MIN_CODE as u32 + 1);
    for t in touched {
        tree[t as usize] = 0;
    }
    bits.finish()
}

#[derive(Default)]
struct Bits {
    out: Vec<u8>,
    acc: u32,
    n: u32,
}

impl Bits {
    fn put(&mut self, code: u16, size: u32) {
        self.acc |= (code as u32) << self.n;
        self.n += size;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push(self.acc as u8);
        }
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plain GIF LZW decoder, to check what `lzw` writes.
    fn unlzw(data: &[u8], min: u32) -> Vec<u8> {
        let clear = 1u32 << min;
        let end = clear + 1;
        let (mut size, mut next) = (min + 1, end + 1);
        let mut dict: Vec<Vec<u8>> = (0..clear).map(|i| vec![i as u8]).collect();
        dict.push(vec![]);
        dict.push(vec![]);
        let (mut acc, mut n, mut pos) = (0u32, 0u32, 0usize);
        let mut out = Vec::new();
        let mut prev: Option<Vec<u8>> = None;
        loop {
            while n < size {
                acc |= (data[pos] as u32) << n;
                pos += 1;
                n += 8;
            }
            let code = acc & ((1 << size) - 1);
            acc >>= size;
            n -= size;
            if code == clear {
                size = min + 1;
                next = end + 1;
                dict.truncate((end + 1) as usize);
                prev = None;
                continue;
            }
            if code == end {
                return out;
            }
            let entry = if (code as usize) < dict.len() {
                dict[code as usize].clone()
            } else {
                let p = prev.clone().unwrap();
                let mut e = p.clone();
                e.push(p[0]);
                e
            };
            out.extend_from_slice(&entry);
            if let Some(p) = prev
                && next < 4096
            {
                let mut e = p;
                e.push(entry[0]);
                dict.push(e);
                next += 1;
                if next >= 1 << size && size < 12 {
                    size += 1;
                }
            }
            prev = Some(entry);
        }
    }

    #[test]
    fn lzw_round_trips_short_long_and_repetitive_data() {
        let mut tree = Vec::new();
        let mut seed = 12345u32;
        let mut noise = || {
            seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
            (seed >> 16) as u8
        };
        let cases: Vec<Vec<u8>> = vec![
            vec![7],
            vec![1, 2],
            vec![0; 10_000],
            (0..70_000).map(|_| noise()).collect(),
            (0..50_000).map(|i| (i / 37 % 5) as u8).collect(),
        ];
        for data in cases {
            assert_eq!(unlzw(&lzw(&data, &mut tree), 8), data, "{} bytes", data.len());
        }
        // Every length up to well past the first code size changes, so the end
        // falls on each boundary at least once.
        let long: Vec<u8> = (0..6000).map(|_| noise() % 3).collect();
        for n in 1..long.len() {
            assert_eq!(unlzw(&lzw(&long[..n], &mut tree), 8), &long[..n], "{n} bytes");
        }
    }

    fn bgra(colors: &[[u8; 3]]) -> Vec<u8> {
        colors.iter().flat_map(|c| [c[2], c[1], c[0], 255]).collect()
    }

    #[test]
    fn a_few_colours_are_kept_exactly() {
        let mut w = GifWriter::new(Vec::new(), 4, 1).unwrap();
        let px = bgra(&[[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 0, 0]]);
        let palette = w.palette(&px);
        assert!(palette.contains(&[255, 0, 0]) && palette.contains(&[0, 255, 0]) && palette.contains(&[0, 0, 255]));
        let idx: Vec<u8> = px.chunks_exact(4).map(|p| w.map[bucket(p)]).collect();
        assert_eq!(idx[0], idx[3]);
        assert_eq!(palette[idx[1] as usize], [0, 255, 0]);
    }

    #[test]
    fn many_colours_get_at_most_256_entries() {
        let colors: Vec<[u8; 3]> = (0..4096u32).map(|i| [(i * 7) as u8, (i * 13) as u8, (i * 29) as u8]).collect();
        let mut w = GifWriter::new(Vec::new(), 64, 64).unwrap();
        let palette = w.palette(&bgra(&colors));
        assert!(palette.len() <= 256 && palette.len() > 200, "{}", palette.len());
    }

    #[test]
    fn the_file_reads_back_as_an_animated_gif() {
        let mut w = GifWriter::new(Vec::new(), 3, 2).unwrap();
        let a = bgra(&[[10, 20, 30]; 6]);
        let b = bgra(&[[200, 100, 0], [0, 0, 0], [255, 255, 255], [1, 2, 3], [4, 5, 6], [7, 8, 9]]);
        w.frame(&a, 4).unwrap();
        w.frame(&b, 7).unwrap();
        assert!(w.frame(&b[..8], 7).is_err(), "a short frame is refused");
        let file = w.finish().unwrap();
        let info = crate::gif::read_info(&mut file.as_slice()).unwrap();
        assert_eq!((info.width, info.height), (3, 2));
        assert_eq!(info.frames.iter().map(|f| f.delay_ms).collect::<Vec<_>>(), vec![40, 70]);
        assert_eq!(file.last(), Some(&0x3B));
    }
}
