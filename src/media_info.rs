//! Which files PocketMedia takes, and their pixel size read from the file header
//! (no decoding): PNG, GIF, JPEG and MP4.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use crate::sample::MediaKind;

/// The kind of a supported file name, or None. Only .jpg .jpeg .png
/// .gif .mp4, case-insensitive.
pub fn kind_for(name: &str) -> Option<MediaKind> {
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" | "png" => Some(MediaKind::Picture),
        "gif" => Some(MediaKind::Gif),
        "mp4" => Some(MediaKind::Video),
        _ => None,
    }
}

/// How much of the file is read to find the size.
const HEAD: usize = 256 * 1024;

/// Pixel size from the header, or (0, 0) when it cannot be found.
pub fn dimensions(path: &Path) -> (u32, u32) {
    let Ok(head) = read_head(path) else { return (0, 0) };
    dimensions_of(&head)
}

fn read_head(path: &Path) -> io::Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(HEAD);
    File::open(path)?.take(HEAD as u64).read_to_end(&mut buf)?;
    Ok(buf)
}

pub fn dimensions_of(b: &[u8]) -> (u32, u32) {
    png(b).or_else(|| gif(b)).or_else(|| jpeg(b)).or_else(|| mp4(b)).unwrap_or((0, 0))
}

fn be16(b: &[u8], i: usize) -> Option<u32> {
    Some(u16::from_be_bytes([*b.get(i)?, *b.get(i + 1)?]) as u32)
}
fn be32(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(i..i + 4)?.try_into().ok()?))
}
fn le16(b: &[u8], i: usize) -> Option<u32> {
    Some(u16::from_le_bytes([*b.get(i)?, *b.get(i + 1)?]) as u32)
}

fn png(b: &[u8]) -> Option<(u32, u32)> {
    (b.starts_with(b"\x89PNG\r\n\x1a\n") && b.get(12..16)? == b"IHDR").then_some(())?;
    Some((be32(b, 16)?, be32(b, 20)?))
}

/// The logical screen: the canvas every frame is composed on.
fn gif(b: &[u8]) -> Option<(u32, u32)> {
    (b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a")).then_some(())?;
    Some((le16(b, 6)?, le16(b, 8)?))
}

fn jpeg(b: &[u8]) -> Option<(u32, u32)> {
    (b.get(0..2)? == [0xFF, 0xD8]).then_some(())?;
    let mut i = 2;
    while i + 4 <= b.len() {
        if b[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = b[i + 1];
        // Markers without a length.
        if marker == 0xFF || marker == 0x01 || (0xD0..=0xD9).contains(&marker) {
            i += 2;
            continue;
        }
        let len = be16(b, i + 2)? as usize;
        // SOF0-15, except DHT (C4), JPG (C8) and DAC (CC).
        if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            return Some((be16(b, i + 7)?, be16(b, i + 5)?));
        }
        i += 2 + len;
    }
    None
}

/// The first video track's display size from its `tkhd` box (16.16 fixed point).
fn mp4(b: &[u8]) -> Option<(u32, u32)> {
    (b.get(4..8)? == b"ftyp").then_some(())?;
    let mut from = 0;
    while let Some(pos) = find(&b[from..], b"tkhd").map(|p| p + from) {
        let version = *b.get(pos + 4)?;
        // After the box type: version/flags (4), times and track id, then the matrix.
        let wh = if version == 1 { pos + 4 + 88 } else { pos + 4 + 76 };
        let (w, h) = (be32(b, wh)? >> 16, be32(b, wh + 4)? >> 16);
        if w > 0 && h > 0 {
            return Some((w, h));
        }
        from = pos + 4;
    }
    None
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Test and self-test helpers: smallest headers that carry a size.
pub mod fixtures {
    /// A real, decodable GIF: a `w` x `h` canvas with one 1x1 frame. `tag` makes
    /// the content unique (a comment block).
    pub fn gif(w: u16, h: u16, tag: &str) -> Vec<u8> {
        let mut b = b"GIF89a".to_vec();
        b.extend(w.to_le_bytes());
        b.extend(h.to_le_bytes());
        b.extend([0x80, 0, 0]); // global colour table of 2 entries
        b.extend([0xAB, 0x93, 0xFF, 0x19, 0x1D, 0x26]);
        b.extend([0x21, 0xFE]); // comment
        for chunk in tag.as_bytes().chunks(255) {
            b.push(chunk.len() as u8);
            b.extend(chunk);
        }
        b.push(0);
        b.extend([0x2C, 0, 0, 0, 0, 1, 0, 1, 0, 0]); // 1x1 image at 0,0
        b.extend([2, 2, 0x44, 0x01, 0]); // LZW: clear, 0, end
        b.push(0x3B);
        b
    }

    /// One frame for `gif_frames`: palette indexes (0-127), top row first.
    pub struct GifFrame {
        pub left: u16,
        pub top: u16,
        pub width: u16,
        pub height: u16,
        pub delay_cs: u16,
        /// GIF disposal code: 0/1 keep, 2 clear to background, 3 restore previous.
        pub disposal: u8,
        pub transparent: Option<u8>,
        pub pixels: Vec<u8>,
    }

    impl GifFrame {
        #[allow(clippy::too_many_arguments)]
        pub fn solid(left: u16, top: u16, width: u16, height: u16, index: u8, delay_cs: u16, disposal: u8, transparent: Option<u8>) -> Self {
            let pixels = vec![index & 0x7F; width as usize * height as usize];
            GifFrame { left, top, width, height, delay_cs, disposal, transparent, pixels }
        }
    }

    /// The 128-colour palette `gif_frames` uses, as RGB: 0 black, 1 red, 2 blue,
    /// 3 green, 4 white, then a spread of colours.
    pub fn palette(i: u8) -> [u8; 3] {
        match i {
            0 => [0, 0, 0],
            1 => [255, 0, 0],
            2 => [0, 0, 255],
            3 => [0, 255, 0],
            4 => [255, 255, 255],
            i => [i.wrapping_mul(37), i.wrapping_mul(91), i.wrapping_mul(53)],
        }
    }

    /// A real, looping, animated GIF. The image data uses 8-bit LZW codes with a
    /// clear code every 126 pixels, so it is valid without compressing anything.
    pub fn gif_frames(w: u16, h: u16, frames: &[GifFrame], tag: &str) -> Vec<u8> {
        let mut b = b"GIF89a".to_vec();
        b.extend(w.to_le_bytes());
        b.extend(h.to_le_bytes());
        b.extend([0xF6, 0, 0]); // global colour table of 128 entries
        for i in 0..128u8 {
            b.extend(palette(i));
        }
        b.extend([0x21, 0xFF, 11]);
        b.extend(b"NETSCAPE2.0");
        b.extend([3, 1, 0, 0, 0]); // loop forever
        b.extend([0x21, 0xFE]);
        for chunk in tag.as_bytes().chunks(255) {
            b.push(chunk.len() as u8);
            b.extend(chunk);
        }
        b.push(0);
        for f in frames {
            let packed = (f.disposal & 7) << 2 | f.transparent.is_some() as u8;
            b.extend([0x21, 0xF9, 4, packed]);
            b.extend(f.delay_cs.to_le_bytes());
            b.extend([f.transparent.unwrap_or(0), 0]);
            b.push(0x2C);
            for v in [f.left, f.top, f.width, f.height] {
                b.extend(v.to_le_bytes());
            }
            b.extend([0, 7]); // no local colour table; LZW minimum code size 7
            let mut codes = vec![128u8];
            for (i, &p) in f.pixels.iter().enumerate() {
                if i > 0 && i % 126 == 0 {
                    codes.push(128);
                }
                codes.push(p & 0x7F);
            }
            codes.push(129);
            for chunk in codes.chunks(255) {
                b.push(chunk.len() as u8);
                b.extend(chunk);
            }
            b.push(0);
        }
        b.push(0x3B);
        b
    }

    /// JPEG markers with a frame header only (enough for the size; not decodable).
    pub fn jpeg(w: u16, h: u16, tag: &str) -> Vec<u8> {
        let mut b = vec![0xFF, 0xD8];
        let com = tag.as_bytes();
        b.extend([0xFF, 0xFE]);
        b.extend(((com.len() + 2) as u16).to_be_bytes());
        b.extend(com);
        b.extend([0xFF, 0xC0, 0, 17, 8]);
        b.extend(h.to_be_bytes());
        b.extend(w.to_be_bytes());
        b.extend([3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
        b.extend([0xFF, 0xD9]);
        b
    }

    /// An MP4 box layout with a track header only (enough for the size; not playable).
    pub fn mp4(w: u16, h: u16, tag: &str) -> Vec<u8> {
        let boxed = |kind: &[u8], body: Vec<u8>| {
            let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
            v.extend(kind);
            v.extend(body);
            v
        };
        let mut tkhd = vec![0u8; 84];
        tkhd[76..80].copy_from_slice(&((w as u32) << 16).to_be_bytes());
        tkhd[80..84].copy_from_slice(&((h as u32) << 16).to_be_bytes());
        let trak = boxed(b"trak", boxed(b"tkhd", tkhd));
        let mut out = boxed(b"ftyp", b"isom\0\0\x02\0isomiso2mp41".to_vec());
        out.extend(boxed(b"moov", trak));
        out.extend(boxed(b"free", tag.as_bytes().to_vec()));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_five_formats_any_case() {
        for name in ["a.jpg", "a.JPEG", "a.Png", "a.gif", "clip.MP4"] {
            assert!(kind_for(name).is_some(), "{name}");
        }
        for name in ["a.webp", "a.mov", "a.txt", "png", "a.png.txt", ".gitignore"] {
            assert!(kind_for(name).is_none(), "{name}");
        }
        assert_eq!(kind_for("x.gif"), Some(MediaKind::Gif));
        assert_eq!(kind_for("x.jpeg"), Some(MediaKind::Picture));
    }

    #[test]
    fn sizes_from_headers() {
        assert_eq!(dimensions_of(&fixtures::gif(400, 200, "a")), (400, 200));
        assert_eq!(dimensions_of(&fixtures::jpeg(1920, 1080, "b")), (1920, 1080));
        assert_eq!(dimensions_of(&fixtures::mp4(720, 1280, "c")), (720, 1280));
        let png = crate::test_png::encode_rgba(3, 2, &[0; 24]);
        assert_eq!(dimensions_of(&png), (3, 2));
        assert_eq!(dimensions_of(b"not media"), (0, 0));
        assert_eq!(dimensions_of(&[]), (0, 0));
    }
}
