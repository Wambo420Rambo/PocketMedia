//! Video frames through Media Foundation's Source Reader: a poster frame for
//! the thumbnail, and the frames of the hover preview. Only the video stream is
//! selected, so no audio is ever decoded or heard. Used off the UI thread, with
//! COM initialised (`wic::ComThread`).

use std::path::Path;

use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer, IMFMediaBuffer, IMFSourceReader, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE,
    MF_MT_MINIMUM_DISPLAY_APERTURE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_MT_VIDEO_ROTATION, MF_PD_DURATION,
    MF_SOURCE_READER_ALL_STREAMS, MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, MF_SOURCE_READER_FIRST_VIDEO_STREAM,
    MF_SOURCE_READER_MEDIASOURCE, MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED, MF_SOURCE_READERF_ENDOFSTREAM,
    MF_SOURCE_READERF_ERROR, MF_VERSION, MFCreateAttributes, MFCreateMediaType, MFCreateSourceReaderFromURL,
    MFMediaType_Video, MFSTARTUP_NOSOCKET, MFShutdown, MFStartup, MFVideoFormat_RGB32,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Variant::VT_I8;
use windows::core::{GUID, HSTRING, Interface};

use crate::pixels::{self, Pixels, Rotation, Source};

const VIDEO: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
/// 100-nanosecond units per second (Media Foundation time).
pub const SECOND: i64 = 10_000_000;

/// Media Foundation for as long as a reader lives (MFStartup is counted).
struct Mf;

impl Mf {
    fn start() -> Result<Self, String> {
        // SAFETY: plain call; balanced by MFShutdown in Drop.
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET) }
            .map(|()| Mf)
            .map_err(|e| format!("Media Foundation is not available: {}", e.message()))
    }
}

impl Drop for Mf {
    fn drop(&mut self) {
        // SAFETY: balances the successful MFStartup in `start`.
        let _ = unsafe { MFShutdown() };
    }
}

/// Where the picture is inside the decoded frame, and how it is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Shape {
    width: u32,
    height: u32,
    /// Visible area: x, y, width, height (decoders pad, e.g. 1080 to 1088).
    crop: (u32, u32, u32, u32),
    /// Bytes per row; negative when the frame is stored bottom-up.
    stride: i32,
}

pub struct Reader {
    // Field order matters: the reader is released before Media Foundation shuts down.
    reader: IMFSourceReader,
    shape: Shape,
    rotation: Rotation,
    /// Pixel aspect ratio (numerator, denominator).
    par: (u32, u32),
    _mf: Mf,
}

fn fail(what: &'static str) -> impl Fn(windows::core::Error) -> String {
    move |e| format!("{what}: {}", e.message())
}

impl Reader {
    /// Opens the file's first video stream, decoding to 32-bit RGB.
    pub fn open(path: &Path) -> Result<Self, String> {
        let mf = Mf::start()?;
        // SAFETY: COM objects are owned locals; out-pointers are locals; the
        // HSTRING outlives the call.
        unsafe {
            let mut attrs = None;
            MFCreateAttributes(&mut attrs, 1).map_err(fail("could not prepare the video reader"))?;
            let attrs = attrs.ok_or("could not prepare the video reader")?;
            attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1).map_err(fail("could not prepare the video reader"))?;
            let reader = MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), &attrs)
                .map_err(fail("this video can't be opened"))?;
            reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false).map_err(fail("this video can't be opened"))?;
            reader.SetStreamSelection(VIDEO, true).map_err(fail("this file has no video PocketMedia can show"))?;
            let native = reader.GetNativeMediaType(VIDEO, 0).map_err(fail("this file has no video PocketMedia can show"))?;
            let rotation = Rotation::from_degrees(native.GetUINT32(&MF_MT_VIDEO_ROTATION).unwrap_or(0));
            let par = native.GetUINT64(&MF_MT_PIXEL_ASPECT_RATIO).map(split).unwrap_or((1, 1));
            let out = MFCreateMediaType().map_err(fail("could not prepare the video reader"))?;
            out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(fail("could not prepare the video reader"))?;
            out.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32).map_err(fail("could not prepare the video reader"))?;
            reader.SetCurrentMediaType(VIDEO, None, &out).map_err(fail("this video's format can't be previewed"))?;
            let shape = current_shape(&reader)?;
            let par = if par.0 == 0 || par.1 == 0 { (1, 1) } else { par };
            Ok(Reader { reader, shape, rotation, par, _mf: mf })
        }
    }

    /// The size the video is shown at: visible area, pixel aspect and rotation applied.
    pub fn display_size(&self) -> (u32, u32) {
        let (_, _, w, h) = self.shape.crop;
        let w = (w as u64 * self.par.0 as u64 / self.par.1 as u64).max(1) as u32;
        if self.rotation.swaps() { (h, w) } else { (w, h) }
    }

    /// Length in 100 ns units, if the file says.
    pub fn duration(&self) -> Option<i64> {
        // SAFETY: plain call; the PROPVARIANT holds a number (nothing to free).
        let pv = unsafe { self.reader.GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION) }.ok()?;
        // SAFETY: MF_PD_DURATION is a VT_UI8; reading the u64 member.
        let d = unsafe { pv.Anonymous.Anonymous.Anonymous.uhVal };
        (d > 0).then_some(d as i64)
    }

    /// Jumps to `position` (100 ns units); decoding resumes at the keyframe before it.
    pub fn seek(&self, position: i64) -> Result<(), String> {
        let mut pv = PROPVARIANT::default();
        // SAFETY: writing the VT_I8 member of a zeroed PROPVARIANT; it owns nothing.
        unsafe {
            (*pv.Anonymous.Anonymous).vt = VT_I8;
            (*pv.Anonymous.Anonymous).Anonymous.hVal = position;
            self.reader.SetCurrentPosition(&GUID::zeroed(), &pv).map_err(fail("could not seek in the video"))
        }
    }

    /// The next frame, shrunk to fit `max_w` x `max_h`, with its time; None at the end.
    pub fn next_frame(&mut self, max_w: u32, max_h: u32) -> Result<Option<(i64, Pixels)>, String> {
        loop {
            let (mut flags, mut time, mut sample) = (0u32, 0i64, None);
            // SAFETY: out-pointers are locals; the reader is alive.
            unsafe { self.reader.ReadSample(VIDEO, 0, None, Some(&mut flags), Some(&mut time), Some(&mut sample)) }
                .map_err(fail("the video could not be decoded"))?;
            if flags & MF_SOURCE_READERF_ERROR.0 as u32 != 0 {
                return Err("the video could not be decoded".into());
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                return Ok(None);
            }
            if flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 as u32 != 0 {
                self.shape = current_shape(&self.reader)?;
            }
            let Some(sample) = sample else { continue }; // a gap in the stream
            // SAFETY: the sample is alive; the buffer is released by Drop.
            let buffer = unsafe { sample.ConvertToContiguousBuffer() }.map_err(fail("the video could not be decoded"))?;
            return self.convert(&buffer, max_w, max_h).map(|p| Some((time, p)));
        }
    }

    fn convert(&self, buffer: &IMFMediaBuffer, max_w: u32, max_h: u32) -> Result<Pixels, String> {
        let s = self.shape;
        let (tw, th) = pixels::fit(self.display_size().0, self.display_size().1, max_w, max_h);
        let (sw, sh) = if self.rotation.swaps() { (th, tw) } else { (tw, th) };
        let locked = Locked::new(buffer, s)?;
        let (stride, bottom_up) = (locked.pitch.unsigned_abs() as usize, locked.pitch < 0);
        let (x, y, w, h) = s.crop;
        // The first byte of the visible area in memory order.
        let first_row = if bottom_up { s.height - y - h } else { y } as usize;
        let start = first_row * stride + x as usize * 4;
        let data = locked.bytes().get(start..).ok_or("the video frame is smaller than it says")?;
        let src = Source { data, width: w, height: h, stride, opaque: true, bottom_up };
        let scaled = pixels::scale(&src, sw.max(1), sh.max(1)).ok_or("the video frame is smaller than it says")?;
        Ok(pixels::rotate(scaled, self.rotation))
    }
}

fn split(v: u64) -> (u32, u32) {
    ((v >> 32) as u32, v as u32)
}

/// Frame size, stride and visible area of the reader's current output type.
fn current_shape(reader: &IMFSourceReader) -> Result<Shape, String> {
    // SAFETY: the reader is alive; out-buffers are locals.
    unsafe {
        let t = reader.GetCurrentMediaType(VIDEO).map_err(fail("the video could not be decoded"))?;
        let (width, height) = t.GetUINT64(&MF_MT_FRAME_SIZE).map(split).map_err(fail("the video has no frame size"))?;
        if width == 0 || height == 0 {
            return Err("the video has no frame size".into());
        }
        let stride = t.GetUINT32(&MF_MT_DEFAULT_STRIDE).map(|s| s as i32).unwrap_or(width as i32 * 4);
        // MFVideoArea: OffsetX {fract u16, value i16}, OffsetY {..}, Area {cx i32, cy i32}.
        let mut area = [0u8; 16];
        let crop = match t.GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, &mut area, None) {
            Ok(()) => {
                let x = i16::from_le_bytes([area[2], area[3]]).max(0) as u32;
                let y = i16::from_le_bytes([area[6], area[7]]).max(0) as u32;
                let w = i32::from_le_bytes([area[8], area[9], area[10], area[11]]).max(0) as u32;
                let h = i32::from_le_bytes([area[12], area[13], area[14], area[15]]).max(0) as u32;
                clamp_crop((x, y, w, h), width, height)
            }
            Err(_) => (0, 0, width, height),
        };
        Ok(Shape { width, height, crop, stride })
    }
}

/// A visible area that lies inside the frame and is not empty.
fn clamp_crop((x, y, w, h): (u32, u32, u32, u32), width: u32, height: u32) -> (u32, u32, u32, u32) {
    if w == 0 || h == 0 || x >= width || y >= height {
        return (0, 0, width, height);
    }
    (x, y, w.min(width - x), h.min(height - y))
}

/// A media buffer locked for reading, unlocked on drop. Prefers the 2-D lock,
/// which gives this buffer's real row pitch.
struct Locked<'a> {
    buffer: &'a IMFMediaBuffer,
    two_d: Option<IMF2DBuffer>,
    base: *const u8,
    len: usize,
    pitch: i32,
}

impl<'a> Locked<'a> {
    fn new(buffer: &'a IMFMediaBuffer, s: Shape) -> Result<Self, String> {
        let rows = s.height as usize;
        if let Ok(two_d) = buffer.cast::<IMF2DBuffer>() {
            let (mut scan0, mut pitch) = (std::ptr::null_mut(), 0i32);
            // SAFETY: out-pointers are locals; unlocked in Drop.
            if unsafe { two_d.Lock2D(&mut scan0, &mut pitch) }.is_ok() && !scan0.is_null() && pitch != 0 {
                let abs = pitch.unsigned_abs() as usize;
                // With a negative pitch the top row is the last one in memory.
                let base = if pitch < 0 {
                    // SAFETY: Lock2D guarantees `rows` rows of `abs` bytes ending at scan0's row.
                    unsafe { scan0.sub(abs * (rows - 1)) }
                } else {
                    scan0
                };
                return Ok(Locked { buffer, two_d: Some(two_d), base, len: abs * rows, pitch });
            }
        }
        let (mut ptr, mut current) = (std::ptr::null_mut(), 0u32);
        // SAFETY: out-pointers are locals; unlocked in Drop.
        unsafe { buffer.Lock(&mut ptr, None, Some(&mut current)) }.map_err(fail("the video frame could not be read"))?;
        if ptr.is_null() {
            return Err("the video frame could not be read".into());
        }
        Ok(Locked { buffer, two_d: None, base: ptr, len: current as usize, pitch: s.stride })
    }

    fn bytes(&self) -> &[u8] {
        // SAFETY: the buffer stays locked while `self` lives; `len` bytes from `base`
        // are readable per the lock call.
        unsafe { std::slice::from_raw_parts(self.base, self.len) }
    }
}

impl Drop for Locked<'_> {
    fn drop(&mut self) {
        // SAFETY: balances the successful lock in `new`.
        unsafe {
            match &self.two_d {
                Some(b) => {
                    let _ = b.Unlock2D();
                }
                None => {
                    let _ = self.buffer.Unlock();
                }
            }
        }
    }
}

/// The poster for a video thumbnail: a frame one second in (past fade-ins) when
/// the video is longer than two seconds, else the first frame.
pub fn poster(path: &Path, max_w: u32, max_h: u32) -> Result<Pixels, String> {
    let mut r = Reader::open(path)?;
    if r.duration().is_some_and(|d| d > 2 * SECOND) {
        r.seek(SECOND)?;
    }
    match r.next_frame(max_w, max_h)? {
        Some((_, p)) => Ok(p),
        None => Err("the video has no frames".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_visible_area_stays_inside_the_frame() {
        assert_eq!(clamp_crop((0, 0, 1920, 1080), 1920, 1088), (0, 0, 1920, 1080));
        assert_eq!(clamp_crop((0, 0, 0, 0), 640, 360), (0, 0, 640, 360));
        assert_eq!(clamp_crop((10, 10, 9999, 9999), 640, 360), (10, 10, 630, 350));
        assert_eq!(clamp_crop((700, 0, 10, 10), 640, 360), (0, 0, 640, 360));
        assert_eq!(split(0x0000_0780_0000_0438), (1920, 1080));
    }
}
