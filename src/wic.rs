//! Image decoding with the Windows Imaging Component (WIC). Used off the UI
//! thread; the calling thread must have COM initialised.

use std::path::Path;

use windows::Win32::Foundation::GENERIC_READ;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppBGRA, GUID_WICPixelFormat32bppPBGRA, IWICBitmapDecoder,
    IWICBitmapSource, IWICImagingFactory, IWICPalette, WICBitmapDitherTypeNone, WICBitmapInterpolationModeFant,
    WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize};
use windows::core::{HSTRING, Interface};

use crate::pixels::{self, Pixels};

/// Larger images are not decoded for the clipboard (the file itself is still offered).
pub const MAX_PIXELS: u64 = 64 * 1024 * 1024;

/// Decoded pixels: 32-bit BGRA, top row first.
pub struct Bgra {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// COM for the current worker thread, released on drop.
pub struct ComThread(bool);

impl ComThread {
    pub fn init() -> Self {
        // SAFETY: called on a worker thread that does not use COM otherwise; paired with Drop.
        let ok = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
        Self(ok)
    }
}

impl Drop for ComThread {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balances the successful CoInitializeEx in `init`.
            unsafe { CoUninitialize() };
        }
    }
}

/// The first frame of an image file (for a GIF, its first frame).
pub fn decode_first_frame(path: &Path) -> Result<Bgra, String> {
    let fail = |e: windows::core::Error| format!("{} could not be decoded: {}", path.display(), e.message());
    // SAFETY: COM is initialised on this thread (ComThread); every object is used while alive
    // and released by Drop; `pixels` is exactly stride x height bytes.
    unsafe {
        let factory: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).map_err(fail)?;
        let name = HSTRING::from(path.as_os_str());
        let decoder = factory.CreateDecoderFromFilename(&name, None, GENERIC_READ, WICDecodeMetadataCacheOnDemand).map_err(fail)?;
        let frame = decoder.GetFrame(0).map_err(fail)?;
        let converter = factory.CreateFormatConverter().map_err(fail)?;
        converter
            .Initialize(&frame, &GUID_WICPixelFormat32bppBGRA, WICBitmapDitherTypeNone, None::<&IWICPalette>, 0.0, WICBitmapPaletteTypeCustom)
            .map_err(fail)?;
        let (mut width, mut height) = (0u32, 0u32);
        converter.GetSize(&mut width, &mut height).map_err(fail)?;
        if width as u64 * height as u64 > MAX_PIXELS {
            return Err(format!("{width} x {height} is too large to put on the clipboard as a picture"));
        }
        let mut pixels = pixels::bgra_buffer(width, height, MAX_PIXELS)?;
        converter.CopyPixels(std::ptr::null(), width * 4, &mut pixels).map_err(fail)?;
        Ok(Bgra { width, height, pixels })
    }
}

/// Pictures larger than this are not previewed (a guard against absurd headers).
const MAX_PREVIEW_PIXELS: u64 = 1 << 30;

/// The WIC factory, kept by a worker thread for all its decoding.
pub struct Wic(IWICImagingFactory);

impl Wic {
    /// Needs COM on this thread (`ComThread`).
    pub fn new() -> Result<Self, String> {
        // SAFETY: plain COM creation; the factory is owned and released by Drop.
        unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
            .map(Wic)
            .map_err(|e| format!("Windows Imaging Component is not available: {}", e.message()))
    }

    /// A decoder over our own handle to the file. Rust opens files with
    /// FILE_SHARE_DELETE, so while a GIF animates the user can still rename it
    /// or move it to the Recycle Bin. The handle must outlive the decoder.
    fn open(&self, path: &Path) -> Result<(IWICBitmapDecoder, std::fs::File), String> {
        use std::os::windows::io::AsRawHandle;
        let file = std::fs::File::open(path).map_err(|e| format!("could not be opened: {e}"))?;
        // SAFETY: the handle is valid and open for reading while `file` lives;
        // callers keep `file` alive longer than the decoder.
        let decoder = unsafe { self.0.CreateDecoderFromFileHandle(file.as_raw_handle() as usize, std::ptr::null(), WICDecodeMetadataCacheOnDemand) }
            .map_err(|e| format!("could not be decoded: {}", e.message()))?;
        Ok((decoder, file))
    }

    /// Premultiplied BGRA of `source` (already at its final size). Sources of
    /// more than `max_pixels` are refused before anything is allocated.
    fn pbgra(&self, source: &IWICBitmapSource, max_pixels: u64) -> Result<Pixels, String> {
        let fail = |e: windows::core::Error| format!("could not be decoded: {}", e.message());
        // SAFETY: COM objects used while alive; `data` is exactly stride x height bytes.
        unsafe {
            let converter = self.0.CreateFormatConverter().map_err(fail)?;
            converter
                .Initialize(source, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None::<&IWICPalette>, 0.0, WICBitmapPaletteTypeCustom)
                .map_err(fail)?;
            let (mut w, mut h) = (0u32, 0u32);
            converter.GetSize(&mut w, &mut h).map_err(fail)?;
            let mut data = pixels::bgra_buffer(w, h, max_pixels)?;
            converter.CopyPixels(std::ptr::null(), w * 4, &mut data).map_err(fail)?;
            Ok(Pixels { width: w, height: h, data })
        }
    }

    /// A picture's first frame, shrunk to fit `max_w` x `max_h` (never enlarged).
    /// WIC's scaler lets JPEG decode at a reduced size, so large photos stay cheap.
    pub fn picture_thumb(&self, path: &Path, max_w: u32, max_h: u32) -> Result<Pixels, String> {
        let fail = |e: windows::core::Error| format!("could not be decoded: {}", e.message());
        let (decoder, _file) = self.open(path)?;
        // SAFETY: COM objects used while alive; out-pointers are locals.
        unsafe {
            let frame = decoder.GetFrame(0).map_err(fail)?;
            let (mut sw, mut sh) = (0u32, 0u32);
            frame.GetSize(&mut sw, &mut sh).map_err(fail)?;
            if sw as u64 * sh as u64 > MAX_PREVIEW_PIXELS {
                return Err(format!("{sw} x {sh} is too large to preview"));
            }
            let (tw, th) = pixels::fit(sw, sh, max_w, max_h);
            if tw == 0 {
                return Err("has no pixels".into());
            }
            let source: IWICBitmapSource = if (tw, th) == (sw, sh) {
                frame.cast().map_err(fail)?
            } else {
                let scaler = self.0.CreateBitmapScaler().map_err(fail)?;
                scaler.Initialize(&frame, tw, th, WICBitmapInterpolationModeFant).map_err(fail)?;
                scaler.cast().map_err(fail)?
            };
            self.pbgra(&source, tw as u64 * th as u64)
        }
    }

    /// A GIF opened for decoding its frames one at a time.
    pub fn gif(&self, path: &Path) -> Result<GifFrames<'_>, String> {
        let (decoder, file) = self.open(path)?;
        Ok(GifFrames { wic: self, decoder, _file: file })
    }
}

pub struct GifFrames<'a> {
    wic: &'a Wic,
    // Released before the file it reads (fields drop in this order).
    decoder: IWICBitmapDecoder,
    _file: std::fs::File,
}

impl GifFrames<'_> {
    /// Frame `index` alone (its own rectangle, not the canvas), premultiplied
    /// BGRA. A second guard after `gif::check_frames`: a frame WIC sizes larger
    /// than `canvas` (w, h) is refused before its pixels are allocated. (WIC
    /// may report an absurd frame smaller and fail while decoding instead.)
    pub fn frame(&self, index: u32, canvas: (u32, u32)) -> Result<Pixels, String> {
        let fail = |e: windows::core::Error| format!("frame {index}: {}", e.message());
        // SAFETY: the decoder is alive; GetFrame validates the index; out-pointers are locals.
        let frame = unsafe { self.decoder.GetFrame(index) }.map_err(fail)?;
        let (mut w, mut h) = (0u32, 0u32);
        // SAFETY: as above.
        unsafe { frame.GetSize(&mut w, &mut h) }.map_err(fail)?;
        if w > canvas.0 || h > canvas.1 {
            return Err(format!("frame {index} is {w} x {h}, larger than the {} x {} picture", canvas.0, canvas.1));
        }
        let source: IWICBitmapSource = frame.cast().map_err(|e| e.message())?;
        self.wic.pbgra(&source, canvas.0 as u64 * canvas.1 as u64)
    }
}
