//! Image decoding with the Windows Imaging Component (WIC). Used off the UI
//! thread; the calling thread must have COM initialised.

use std::path::Path;

use windows::Win32::Foundation::GENERIC_READ;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppBGRA, IWICImagingFactory, IWICPalette,
    WICBitmapDitherTypeNone, WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize};
use windows::core::HSTRING;

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
        let stride = width * 4;
        let mut pixels = vec![0u8; (stride * height) as usize];
        converter.CopyPixels(std::ptr::null(), stride, &mut pixels).map_err(fail)?;
        Ok(Bgra { width, height, pixels })
    }
}
