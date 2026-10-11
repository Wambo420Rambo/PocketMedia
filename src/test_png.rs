//! Generates the Phase 0 test image in memory and writes it as a real PNG file.
//! No image crates are allowed, so this contains a minimal PNG encoder
//! (uncompressed "stored" deflate blocks). It is only for the risk test.

use std::path::{Path, PathBuf};

pub const WIDTH: u32 = 160;
pub const HEIGHT: u32 = 120;

/// Test image as top-down RGBA rows, fully opaque.
pub struct TestImage {
    pub rgba: Vec<u8>,
}

impl TestImage {
    pub fn generate() -> Self {
        let mut rgba = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let border = x < 4 || y < 4 || x >= WIDTH - 4 || y >= HEIGHT - 4;
                let checker = ((x / 20) + (y / 20)) % 2 == 0;
                let (r, g, b) = if border {
                    (0xB0, 0x8C, 0xFF)
                } else {
                    // Lavender-to-teal diagonal gradient with a soft checker,
                    // so a wrong row order or channel swap is easy to spot.
                    let t = (x + y) as f32 / (WIDTH + HEIGHT) as f32;
                    let base_r = 0x8C as f32 * (1.0 - t) + 0x2A as f32 * t;
                    let base_g = 0x6C as f32 * (1.0 - t) + 0xB8 as f32 * t;
                    let base_b = 0xE8 as f32 * (1.0 - t) + 0xB0 as f32 * t;
                    let k = if checker { 1.0 } else { 0.82 };
                    ((base_r * k) as u8, (base_g * k) as u8, (base_b * k) as u8)
                };
                rgba.extend_from_slice(&[r, g, b, 0xFF]);
            }
        }
        // Red marker in the top-left corner (inside the border) to verify orientation.
        for y in 8..24 {
            for x in 8..24 {
                let i = ((y * WIDTH + x) * 4) as usize;
                rgba[i..i + 4].copy_from_slice(&[0xE0, 0x30, 0x40, 0xFF]);
            }
        }
        Self { rgba }
    }

    /// Bottom-up BGRA rows, as a 32-bit BI_RGB DIB expects.
    pub fn bgra_bottom_up(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.rgba.len());
        let stride = (WIDTH * 4) as usize;
        for row in self.rgba.chunks_exact(stride).rev() {
            for px in row.chunks_exact(4) {
                out.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
            }
        }
        out
    }

    pub fn encode_png(&self) -> Vec<u8> {
        encode_rgba(WIDTH, HEIGHT, &self.rgba)
    }
}

/// Encodes top-down, 8-bit RGBA pixels as a PNG (stored deflate, no compression).
pub fn encode_rgba(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let stride = (width * 4) as usize;
    assert_eq!(rgba.len(), stride * height as usize, "pixel buffer size mismatch");
    let mut raw = Vec::with_capacity((stride + 1) * height as usize);
    for row in rgba.chunks_exact(stride) {
        raw.push(0); // filter: None
        raw.extend_from_slice(row);
    }

    let mut png = Vec::new();
    png.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA, no interlace
    write_chunk(&mut png, b"IHDR", &ihdr);
    write_chunk(&mut png, b"IDAT", &zlib_stored(&raw));
    write_chunk(&mut png, b"IEND", &[]);
    png
}

/// Writes the PNG to `%TEMP%\PocketMedia-Phase0\<pid>\PocketMedia test.png`.
/// Per process, so a second copy never touches the first copy's file.
/// It is a test fixture, not user media, and is removed on exit.
pub fn write_test_file(png: &[u8]) -> std::io::Result<PathBuf> {
    let dir = std::env::temp_dir().join("PocketMedia-Phase0").join(std::process::id().to_string());
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("PocketMedia test.png");
    write_atomically(&path, png)?;
    Ok(path)
}

fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("png.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

fn write_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let mut blocks = data.chunks(65_535).peekable();
    if blocks.peek().is_none() {
        out.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
    }
    while let Some(block) = blocks.next() {
        out.push(if blocks.peek().is_none() { 1 } else { 0 });
        let len = block.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(block);
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn adler32(bytes: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in bytes {
        a = (a + x as u32) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_reference_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn adler32_matches_reference_value() {
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }

    #[test]
    fn png_has_signature_and_correct_header() {
        let png = TestImage::generate().encode_png();
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), WIDTH);
        assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), HEIGHT);
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
    }

    #[test]
    fn stored_deflate_splits_large_input_into_valid_blocks() {
        let data = vec![7u8; 150_000];
        let z = zlib_stored(&data);
        // header + 3 blocks of (5 + payload) + adler
        assert_eq!(z.len(), 2 + 3 * 5 + data.len() + 4);
        assert_eq!(z[2], 0); // first block not final
        assert_eq!(z[2 + 5 + 65_535 + 5 + 65_535], 1); // third block final
    }

    #[test]
    fn dib_rows_are_bottom_up_bgra() {
        let img = TestImage::generate();
        let dib = img.bgra_bottom_up();
        // Red marker at image (10,10) must sit in DIB row HEIGHT-1-10 as BGRA.
        let i = (((HEIGHT - 1 - 10) * WIDTH + 10) * 4) as usize;
        assert_eq!(&dib[i..i + 4], &[0x40, 0x30, 0xE0, 0xFF]);
    }
}
