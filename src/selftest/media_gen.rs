//! Self-test only: real media files made with Windows' own encoders, so the
//! previews are tested on files WIC and Media Foundation really decode: JPEG
//! pictures (WIC) and H.264 MP4 videos (Media Foundation's sink writer).
//! The calling thread must have COM initialised.

use std::path::Path;

use windows::Win32::Foundation::GENERIC_WRITE;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatJpeg, GUID_WICPixelFormat24bppBGR, IWICImagingFactory,
    WICBitmapEncoderNoCache,
};
use windows::Win32::Media::MediaFoundation::{
    IMFAttributes, IMFByteStream, MF_MT_AVG_BITRATE, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE,
    MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_VERSION, MFCreateMediaType,
    MFCreateMemoryBuffer, MFCreateSample, MFCreateSinkWriterFromURL, MFMediaType_Video, MFSTARTUP_NOSOCKET,
    MFShutdown, MFStartup, MFVideoFormat_H264, MFVideoFormat_RGB32, MFVideoInterlace_Progressive,
};
use windows::Win32::System::Com::StructuredStorage::IPropertyBag2;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::core::HSTRING;

fn err(what: &str) -> impl Fn(windows::core::Error) -> String + '_ {
    move |e| format!("{what}: {}", e.message())
}

/// A `w` x `h` JPEG: a gradient tinted by `seed`, with a dark square that
/// covers the middle. The square's offset also depends on `seed`, so every seed
/// makes a different file (the import skips identical files).
pub fn jpeg(path: &Path, w: u32, h: u32, seed: u32) -> Result<(), String> {
    let (cols, rows) = ((w / 6).max(1), (h / 6).max(1));
    let (ox, oy) = (seed % cols, seed / cols % rows);
    let mut bgr = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            let (x0, y0) = (w / 3 + ox, h / 3 + oy);
            let dark = x > x0 && x < x0 + w / 3 && y > y0 && y < y0 + h / 3;
            let (r, g, b) = ((x * 255 / w.max(1)) as u8, (y * 255 / h.max(1)) as u8, (seed * 47 % 256) as u8);
            bgr.extend(if dark { [b / 4, g / 4, r / 4] } else { [b, g, r] });
        }
    }
    // SAFETY: COM objects are owned locals used while alive; out-pointers are locals;
    // `bgr` is exactly h rows of w * 3 bytes.
    unsafe {
        let f: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).map_err(err("WIC"))?;
        let stream = f.CreateStream().map_err(err("stream"))?;
        stream.InitializeFromFilename(&HSTRING::from(path.as_os_str()), GENERIC_WRITE.0).map_err(err("create file"))?;
        let encoder = f.CreateEncoder(&GUID_ContainerFormatJpeg, std::ptr::null()).map_err(err("JPEG encoder"))?;
        encoder.Initialize(&stream, WICBitmapEncoderNoCache).map_err(err("JPEG encoder"))?;
        let (mut frame, mut props) = (None, None::<IPropertyBag2>);
        encoder.CreateNewFrame(&mut frame, &mut props).map_err(err("JPEG frame"))?;
        let frame = frame.ok_or("no JPEG frame")?;
        frame.Initialize(props.as_ref()).map_err(err("JPEG frame"))?;
        frame.SetSize(w, h).map_err(err("JPEG size"))?;
        let mut format = GUID_WICPixelFormat24bppBGR;
        frame.SetPixelFormat(&mut format).map_err(err("JPEG format"))?;
        if format != GUID_WICPixelFormat24bppBGR {
            return Err("the JPEG encoder wants another pixel format".into());
        }
        frame.WritePixels(h, w * 3, &bgr).map_err(err("JPEG pixels"))?;
        frame.Commit().map_err(err("JPEG frame"))?;
        encoder.Commit().map_err(err("JPEG file"))?;
    }
    Ok(())
}

struct Mf;

impl Mf {
    fn start() -> Result<Self, String> {
        // SAFETY: plain call; balanced in Drop.
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET) }.map(|()| Mf).map_err(err("Media Foundation"))
    }
}

impl Drop for Mf {
    fn drop(&mut self) {
        // SAFETY: balances the successful MFStartup.
        let _ = unsafe { MFShutdown() };
    }
}

/// An H.264 MP4 of `frames` frames at `fps`: the colour shifts every frame and
/// a white square moves across, so every frame differs.
pub fn mp4(path: &Path, w: u32, h: u32, frames: u32, fps: u32, seed: u32) -> Result<(), String> {
    let _mf = Mf::start()?;
    let size = (w as u64) << 32 | h as u64;
    let rate = (fps as u64) << 32 | 1;
    let duration = 10_000_000i64 / fps as i64;
    // SAFETY: COM objects are owned locals; buffers are locked only while written,
    // within their length.
    unsafe {
        let writer = MFCreateSinkWriterFromURL(&HSTRING::from(path.as_os_str()), None::<&IMFByteStream>, None::<&IMFAttributes>)
            .map_err(err("MP4 writer"))?;
        let out = MFCreateMediaType().map_err(err("media type"))?;
        out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(err("media type"))?;
        out.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264).map_err(err("media type"))?;
        out.SetUINT32(&MF_MT_AVG_BITRATE, 400_000).map_err(err("media type"))?;
        out.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).map_err(err("media type"))?;
        out.SetUINT64(&MF_MT_FRAME_SIZE, size).map_err(err("media type"))?;
        out.SetUINT64(&MF_MT_FRAME_RATE, rate).map_err(err("media type"))?;
        out.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, 1 << 32 | 1).map_err(err("media type"))?;
        let stream = writer.AddStream(&out).map_err(err("H.264 encoder"))?;
        let input = MFCreateMediaType().map_err(err("media type"))?;
        input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(err("media type"))?;
        input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32).map_err(err("media type"))?;
        input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).map_err(err("media type"))?;
        input.SetUINT32(&MF_MT_DEFAULT_STRIDE, w * 4).map_err(err("media type"))?;
        input.SetUINT64(&MF_MT_FRAME_SIZE, size).map_err(err("media type"))?;
        input.SetUINT64(&MF_MT_FRAME_RATE, rate).map_err(err("media type"))?;
        input.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, 1 << 32 | 1).map_err(err("media type"))?;
        writer.SetInputMediaType(stream, &input, None::<&IMFAttributes>).map_err(err("H.264 encoder input"))?;
        writer.BeginWriting().map_err(err("MP4 writer"))?;
        let bytes = w * h * 4;
        let side = (h / 4).max(2);
        for i in 0..frames {
            let buffer = MFCreateMemoryBuffer(bytes).map_err(err("frame buffer"))?;
            let mut ptr = std::ptr::null_mut();
            buffer.Lock(&mut ptr, None, None).map_err(err("frame buffer"))?;
            let data = std::slice::from_raw_parts_mut(ptr, bytes as usize);
            let (sx, sy) = ((w - side) * i / frames.max(1), (h - side) / 2);
            let tint = ((seed * 61 + i * 255 / frames.max(1)) % 256) as u8;
            for y in 0..h {
                for x in 0..w {
                    let at = ((y * w + x) * 4) as usize;
                    let square = x >= sx && x < sx + side && y >= sy && y < sy + side;
                    let px = if square { [255, 255, 255, 255] } else { [tint, 255 - tint, (x * 255 / w) as u8, 255] };
                    data[at..at + 4].copy_from_slice(&px);
                }
            }
            buffer.Unlock().map_err(err("frame buffer"))?;
            buffer.SetCurrentLength(bytes).map_err(err("frame buffer"))?;
            let sample = MFCreateSample().map_err(err("frame"))?;
            sample.AddBuffer(&buffer).map_err(err("frame"))?;
            sample.SetSampleTime(i as i64 * duration).map_err(err("frame"))?;
            sample.SetSampleDuration(duration).map_err(err("frame"))?;
            writer.WriteSample(stream, &sample).map_err(err("H.264 encoder"))?;
        }
        writer.Finalize().map_err(err("MP4 file"))?;
    }
    Ok(())
}
