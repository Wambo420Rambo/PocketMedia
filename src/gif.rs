//! What the GIF format itself says about animation, read without decoding any
//! pixels: the canvas size and, per frame, where it goes, how long it shows and
//! what happens to it afterwards (disposal). WIC decodes each frame's pixels;
//! `Composer` puts them on the canvas the way browsers do.

use std::io::{self, Read};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposal {
    /// Leave the frame in place (GIF disposal 0 and 1).
    Keep,
    /// Clear the frame's area to transparent before the next frame (2).
    Background,
    /// Put back what was under the frame (3).
    Previous,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameInfo {
    pub left: u32,
    pub top: u32,
    pub width: u32,
    pub height: u32,
    /// How long the frame shows, after the browsers' rule for tiny delays.
    pub delay_ms: u32,
    pub disposal: Disposal,
}

#[derive(Debug, PartialEq, Eq)]
pub struct GifInfo {
    pub width: u32,
    pub height: u32,
    pub frames: Vec<FrameInfo>,
}

/// Browsers show frames with a delay of 0 or 10 ms for 100 ms; so does PocketMedia.
pub fn delay_ms(centiseconds: u16) -> u32 {
    if centiseconds <= 1 { 100 } else { centiseconds as u32 * 10 }
}

fn byte(r: &mut impl Read) -> io::Result<u8> {
    let mut b = [0u8; 1];
    r.read_exact(&mut b)?;
    Ok(b[0])
}

fn le16(r: &mut impl Read) -> io::Result<u16> {
    let mut b = [0u8; 2];
    r.read_exact(&mut b)?;
    Ok(u16::from_le_bytes(b))
}

fn skip(r: &mut impl Read, n: u64) -> io::Result<()> {
    let copied = io::copy(&mut r.take(n), &mut io::sink())?;
    if copied == n { Ok(()) } else { Err(io::ErrorKind::UnexpectedEof.into()) }
}

/// Skips data sub-blocks up to and including the empty block that ends them.
fn skip_sub_blocks(r: &mut impl Read) -> io::Result<()> {
    loop {
        match byte(r)? {
            0 => return Ok(()),
            n => skip(r, n as u64)?,
        }
    }
}

/// Reads the frame table. A file cut short keeps the frames read so far.
pub fn read_info(r: &mut impl Read) -> Result<GifInfo, String> {
    let bad = |e: io::Error| format!("not a readable GIF: {e}");
    let mut sig = [0u8; 6];
    r.read_exact(&mut sig).map_err(bad)?;
    if &sig != b"GIF87a" && &sig != b"GIF89a" {
        return Err("not a GIF file".into());
    }
    let width = le16(r).map_err(bad)? as u32;
    let height = le16(r).map_err(bad)? as u32;
    let packed = byte(r).map_err(bad)?;
    skip(r, 2).map_err(bad)?; // background colour index, aspect
    if packed & 0x80 != 0 {
        skip(r, 3 << ((packed & 7) + 1)).map_err(bad)?;
    }
    let mut frames = Vec::new();
    // The graphic control extension applies to the next image only.
    let mut pending: Option<(u16, Disposal)> = None;
    let result = (|| -> io::Result<()> {
        loop {
            match byte(r)? {
                0x21 => {
                    let label = byte(r)?;
                    if label == 0xF9 {
                        let size = byte(r)?;
                        let p = byte(r)?;
                        let delay = le16(r)?;
                        skip(r, size.saturating_sub(3) as u64)?;
                        let disposal = match (p >> 2) & 7 {
                            2 => Disposal::Background,
                            3 => Disposal::Previous,
                            _ => Disposal::Keep,
                        };
                        pending = Some((delay, disposal));
                    }
                    skip_sub_blocks(r)?;
                }
                0x2C => {
                    let left = le16(r)? as u32;
                    let top = le16(r)? as u32;
                    let w = le16(r)? as u32;
                    let h = le16(r)? as u32;
                    let p = byte(r)?;
                    if p & 0x80 != 0 {
                        skip(r, 3 << ((p & 7) + 1))?;
                    }
                    byte(r)?; // LZW minimum code size
                    skip_sub_blocks(r)?;
                    let (delay, disposal) = pending.take().unwrap_or((0, Disposal::Keep));
                    frames.push(FrameInfo { left, top, width: w, height: h, delay_ms: delay_ms(delay), disposal });
                }
                0x3B => return Ok(()),
                _ => return Err(io::ErrorKind::InvalidData.into()),
            }
        }
    })();
    match result {
        Ok(()) => {}
        Err(_) if !frames.is_empty() => {}
        Err(e) => return Err(bad(e)),
    }
    if frames.is_empty() {
        return Err("the GIF has no frames".into());
    }
    Ok(GifInfo { width, height, frames })
}

/// Refuses a GIF with a frame larger than its canvas. A crafted file can pair a
/// small canvas with a 65535 x 65535 frame, which would need gigabytes to decode.
pub fn check_frames(info: &GifInfo) -> Result<(), String> {
    match info.frames.iter().position(|f| !frame_fits(f.width, f.height, info)) {
        Some(i) => {
            let f = &info.frames[i];
            Err(format!("frame {i} is {} x {}, larger than the {} x {} picture", f.width, f.height, info.width, info.height))
        }
        None => Ok(()),
    }
}

/// A frame of `width` x `height` pixels fits on the canvas.
pub fn frame_fits(width: u32, height: u32, info: &GifInfo) -> bool {
    width <= info.width && height <= info.height
}

/// The canvas a GIF's frames are drawn on, in premultiplied BGRA. GIF alpha is
/// all-or-nothing, so a frame pixel either replaces the canvas pixel or not.
pub struct Composer {
    width: u32,
    height: u32,
    canvas: Vec<u8>,
    /// The previous frame and, for `Disposal::Previous`, what was under it.
    last: Option<(FrameInfo, Option<Vec<u8>>)>,
}

impl Composer {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height, canvas: vec![0; (width * height * 4) as usize], last: None }
    }

    /// Back to an empty canvas (the start of the next loop).
    pub fn reset(&mut self) {
        self.canvas.iter_mut().for_each(|b| *b = 0);
        self.last = None;
    }

    pub fn canvas(&self) -> &[u8] {
        &self.canvas
    }

    /// The part of `f` that lies on the canvas: (x0, y0, x1, y1).
    fn clip(&self, f: &FrameInfo) -> (usize, usize, usize, usize) {
        let x1 = (f.left + f.width).min(self.width);
        let y1 = (f.top + f.height).min(self.height);
        (f.left.min(x1) as usize, f.top.min(y1) as usize, x1 as usize, y1 as usize)
    }

    fn copy_rect(&self, f: &FrameInfo) -> Vec<u8> {
        let (x0, y0, x1, y1) = self.clip(f);
        let w = self.width as usize;
        (y0..y1).flat_map(|y| self.canvas[(y * w + x0) * 4..(y * w + x1) * 4].iter().copied()).collect()
    }

    fn put_rect(&mut self, f: &FrameInfo, saved: &[u8]) {
        let (x0, y0, x1, y1) = self.clip(f);
        let (w, row) = (self.width as usize, (x1 - x0) * 4);
        for (i, y) in (y0..y1).enumerate() {
            self.canvas[(y * w + x0) * 4..(y * w + x1) * 4].copy_from_slice(&saved[i * row..(i + 1) * row]);
        }
    }

    /// Draws frame `f` from `pixels` (BGRA, `f.width` x `f.height`, top row first),
    /// first undoing the previous frame as its disposal asks.
    pub fn draw(&mut self, f: &FrameInfo, pixels: &[u8]) {
        if let Some((prev, saved)) = self.last.take() {
            match (prev.disposal, saved) {
                (Disposal::Background, _) => {
                    let blank = vec![0u8; self.copy_rect(&prev).len()];
                    self.put_rect(&prev, &blank);
                }
                (Disposal::Previous, Some(saved)) => self.put_rect(&prev, &saved),
                _ => {}
            }
        }
        let saved = (f.disposal == Disposal::Previous).then(|| self.copy_rect(f));
        let (x0, y0, x1, y1) = self.clip(f);
        let (w, fw) = (self.width as usize, f.width as usize);
        if pixels.len() >= fw * f.height as usize * 4 {
            for y in y0..y1 {
                for x in x0..x1 {
                    let s = ((y - f.top as usize) * fw + (x - f.left as usize)) * 4;
                    let a = pixels[s + 3];
                    if a != 0 {
                        let d = (y * w + x) * 4;
                        let px = [pixels[s], pixels[s + 1], pixels[s + 2], a];
                        self.canvas[d..d + 4].copy_from_slice(&px);
                    }
                }
            }
        }
        self.last = Some((*f, saved));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media_info::fixtures::{GifFrame, gif_frames};

    fn frame(left: u32, top: u32, w: u32, h: u32, disposal: Disposal) -> FrameInfo {
        FrameInfo { left, top, width: w, height: h, delay_ms: 100, disposal }
    }

    #[test]
    fn frame_table_of_an_animated_gif() {
        let frames = [
            GifFrame::solid(0, 0, 4, 3, 1, 5, 0, None),
            GifFrame::solid(1, 1, 2, 2, 2, 0, 2, Some(0)),
            GifFrame::solid(0, 0, 4, 3, 3, 1, 3, None),
        ];
        let bytes = gif_frames(4, 3, &frames, "t");
        let info = read_info(&mut bytes.as_slice()).unwrap();
        assert_eq!((info.width, info.height), (4, 3));
        assert_eq!(info.frames.len(), 3);
        assert_eq!(info.frames[0], FrameInfo { left: 0, top: 0, width: 4, height: 3, delay_ms: 50, disposal: Disposal::Keep });
        assert_eq!(info.frames[1], FrameInfo { left: 1, top: 1, width: 2, height: 2, delay_ms: 100, disposal: Disposal::Background });
        assert_eq!(info.frames[2].disposal, Disposal::Previous);
        assert_eq!(info.frames[2].delay_ms, 100, "1 cs counts as 100 ms, like browsers");
    }

    #[test]
    fn a_gif_cut_short_keeps_its_first_frames() {
        let frames = [GifFrame::solid(0, 0, 2, 2, 1, 10, 0, None), GifFrame::solid(0, 0, 2, 2, 2, 10, 0, None)];
        let bytes = gif_frames(2, 2, &frames, "t");
        let info = read_info(&mut &bytes[..bytes.len() - 4]).unwrap();
        assert_eq!(info.frames.len(), 1);
        assert!(read_info(&mut &b"GIF89a"[..]).is_err());
        assert!(read_info(&mut &b"PNG..."[..]).is_err());
    }

    #[test]
    fn a_frame_larger_than_the_canvas_is_refused() {
        // A 100 x 100 canvas with a 65535 x 65535 frame.
        let bytes = crate::media_info::fixtures::gif_oversized_frame(100, 100, 65535, 65535);
        let info = read_info(&mut bytes.as_slice()).unwrap();
        assert_eq!((info.frames[0].width, info.frames[0].height), (65535, 65535));
        let err = check_frames(&info).unwrap_err();
        assert!(err.contains("65535 x 65535") && err.contains("100 x 100"), "{err}");
        // One dimension too large is enough.
        let wide = crate::media_info::fixtures::gif_oversized_frame(100, 100, 101, 1);
        assert!(check_frames(&read_info(&mut wide.as_slice()).unwrap()).is_err());
    }

    #[test]
    fn frames_that_fit_the_canvas_are_accepted() {
        let frames = [GifFrame::solid(0, 0, 4, 3, 1, 5, 0, None), GifFrame::solid(2, 1, 2, 2, 2, 5, 0, None)];
        let info = read_info(&mut gif_frames(4, 3, &frames, "t").as_slice()).unwrap();
        assert_eq!(check_frames(&info), Ok(()));
        // Offset frames that hang over the edge are clipped when drawn, not refused.
        let over = [GifFrame::solid(3, 2, 4, 3, 1, 5, 0, None)];
        assert_eq!(check_frames(&read_info(&mut gif_frames(4, 3, &over, "t").as_slice()).unwrap()), Ok(()));
        assert!(frame_fits(4, 3, &info) && !frame_fits(5, 3, &info) && !frame_fits(4, 4, &info));
    }

    #[test]
    fn the_wireframe_fixture_is_a_one_frame_gif() {
        let bytes = crate::media_info::fixtures::gif(400, 200, "x");
        let info = read_info(&mut bytes.as_slice()).unwrap();
        assert_eq!((info.width, info.height, info.frames.len()), (400, 200, 1));
    }

    const RED: [u8; 4] = [0, 0, 255, 255];
    const BLUE: [u8; 4] = [255, 0, 0, 255];
    const CLEAR: [u8; 4] = [0, 0, 0, 0];

    fn px(c: &Composer, x: usize, y: usize) -> [u8; 4] {
        let i = (y * c.width as usize + x) * 4;
        c.canvas()[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn frames_build_up_and_transparent_pixels_show_what_is_under() {
        let mut c = Composer::new(2, 1);
        c.draw(&frame(0, 0, 2, 1, Disposal::Keep), &[RED, RED].concat());
        c.draw(&frame(0, 0, 2, 1, Disposal::Keep), &[BLUE, CLEAR].concat());
        assert_eq!((px(&c, 0, 0), px(&c, 1, 0)), (BLUE, RED));
    }

    #[test]
    fn background_disposal_clears_the_frame_area() {
        let mut c = Composer::new(3, 1);
        c.draw(&frame(0, 0, 3, 1, Disposal::Keep), &[RED, RED, RED].concat());
        c.draw(&frame(1, 0, 1, 1, Disposal::Background), &BLUE);
        assert_eq!(px(&c, 1, 0), BLUE);
        c.draw(&frame(0, 0, 1, 1, Disposal::Keep), &CLEAR);
        assert_eq!([px(&c, 0, 0), px(&c, 1, 0), px(&c, 2, 0)], [RED, CLEAR, RED]);
    }

    #[test]
    fn previous_disposal_restores_what_was_there() {
        let mut c = Composer::new(2, 1);
        c.draw(&frame(0, 0, 2, 1, Disposal::Keep), &[RED, RED].concat());
        c.draw(&frame(0, 0, 1, 1, Disposal::Previous), &BLUE);
        assert_eq!(px(&c, 0, 0), BLUE);
        c.draw(&frame(1, 0, 1, 1, Disposal::Keep), &CLEAR);
        assert_eq!(px(&c, 0, 0), RED);
        c.reset();
        assert_eq!(px(&c, 0, 0), CLEAR);
    }

    #[test]
    fn frames_outside_the_canvas_are_clipped() {
        let mut c = Composer::new(2, 2);
        c.draw(&frame(1, 1, 3, 3, Disposal::Background), &[RED; 9].concat());
        assert_eq!([px(&c, 0, 0), px(&c, 1, 1)], [CLEAR, RED]);
        c.draw(&frame(5, 5, 2, 2, Disposal::Keep), &[BLUE; 4].concat());
        assert_eq!(px(&c, 1, 1), CLEAR, "the earlier frame was cleared; the far one draws nothing");
    }
}
