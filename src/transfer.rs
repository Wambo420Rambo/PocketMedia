//! Clipboard, simulated paste and drag-out for library files.

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::Graphics::Gdi::{BI_RGB, BITMAPINFOHEADER};
use windows::Win32::System::Com::IDataObject;
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::{CF_DIB, CF_HDROP, DROPEFFECT, DROPEFFECT_COPY};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT, VK_V,
};
use windows::Win32::UI::Shell::{
    BHID_DataObject, DROPFILES, IShellItem, SHCreateItemFromParsingName, SHDoDragDrop,
};
use windows::core::{PCWSTR, w};


pub fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn path_wide(path: &Path) -> Vec<u16> {
    to_wide(&path.to_string_lossy())
}

/// What goes on the clipboard for one library item. Built off the UI thread by
/// `prepare` (it reads and decodes files).
#[derive(Debug, Default)]
pub struct Payload {
    /// The original file (CF_HDROP): Explorer, chat and mail apps attach it as is,
    /// so GIFs stay animated and videos are not converted.
    pub file: PathBuf,
    /// "PNG": the original bytes of a PNG file.
    pub png: Option<Vec<u8>>,
    /// "JFIF": the original bytes of a JPEG file.
    pub jfif: Option<Vec<u8>>,
    /// "GIF": the original bytes of a GIF file (no static picture is offered, so
    /// an app never pastes a frozen frame instead of the animation).
    pub gif: Option<Vec<u8>>,
    /// CF_DIB: the decoded picture, for apps that only take bitmaps (Paint).
    pub dib: Option<Vec<u8>>,
}

/// Reads (and for pictures, decodes) a library file for the clipboard.
/// Worker thread only: COM must be initialised there.
pub fn prepare(file: &Path) -> Result<Payload, String> {
    if !file.is_file() {
        return Err(format!("{} is no longer in the Media folder", file.display()));
    }
    let ext = file.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let read = || std::fs::read(file).map_err(|e| format!("{} could not be read: {e}", file.display()));
    let decoded = || crate::wic::decode_first_frame(file).ok().map(|b| dib_from_bgra(&b));
    let mut p = Payload { file: file.to_path_buf(), ..Default::default() };
    match ext.as_str() {
        "png" => {
            p.png = Some(read()?);
            p.dib = decoded();
        }
        "jpg" | "jpeg" => {
            p.jfif = Some(read()?);
            p.dib = decoded();
        }
        "gif" => p.gif = Some(read()?),
        _ => {} // videos: the file only
    }
    Ok(p)
}

/// Replaces the clipboard with `p`: the file (CF_HDROP with Preferred DropEffect
/// = copy), plus the original bytes and a bitmap where they apply.
pub fn put_on_clipboard(owner: HWND, p: &Payload) -> Result<(), String> {
    // SAFETY: registering a format name has no preconditions.
    let reg = |name: windows::core::PCWSTR| unsafe { RegisterClipboardFormatW(name) };
    let (png_f, jfif_f, gif_f, effect_f) = (reg(w!("PNG")), reg(w!("JFIF")), reg(w!("GIF")), reg(w!("Preferred DropEffect")));
    if png_f == 0 || jfif_f == 0 || gif_f == 0 || effect_f == 0 {
        return Err("Windows refused to register the clipboard formats.".into());
    }
    let hdrop = hdrop_bytes(&p.file);
    let effect = DROPEFFECT_COPY.0.to_le_bytes();

    open_clipboard(owner)?;
    let result = (|| {
        // SAFETY: the clipboard is open by this thread.
        unsafe { EmptyClipboard() }.map_err(|e| format!("Could not empty the clipboard: {e}"))?;
        for (format, bytes) in [(png_f, &p.png), (jfif_f, &p.jfif), (gif_f, &p.gif), (CF_DIB.0 as u32, &p.dib)] {
            if let Some(b) = bytes {
                set_data(format, b)?;
            }
        }
        set_data(CF_HDROP.0 as u32, &hdrop)?;
        set_data(effect_f, &effect)?;
        Ok(())
    })();
    // SAFETY: closes the clipboard opened above.
    let _ = unsafe { CloseClipboard() };
    result
}

/// Another app can hold the clipboard open for a moment; retry briefly.
fn open_clipboard(owner: HWND) -> Result<(), String> {
    for attempt in 0..10 {
        if unsafe { OpenClipboard(Some(owner)) }.is_ok() {
            return Ok(());
        }
        if attempt < 9 {
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
    }
    Err("The clipboard is in use by another app. Try again.".into())
}

fn set_data(format: u32, bytes: &[u8]) -> Result<(), String> {
    unsafe {
        let mem: HGLOBAL = GlobalAlloc(GMEM_MOVEABLE, bytes.len())
            .map_err(|e| format!("Out of memory for clipboard data: {e}"))?;
        let ptr = GlobalLock(mem) as *mut u8;
        if ptr.is_null() {
            let _ = GlobalFree(Some(mem));
            return Err("Could not lock clipboard memory.".into());
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
        let _ = GlobalUnlock(mem);
        // On success the clipboard owns `mem`; on failure we still do.
        if let Err(e) = SetClipboardData(format, Some(HANDLE(mem.0))) {
            let _ = GlobalFree(Some(mem));
            return Err(format!("Could not set clipboard format {format}: {e}"));
        }
        Ok(())
    }
}

/// A 32-bit bottom-up CF_DIB from top-down BGRA pixels.
pub fn dib_from_bgra(b: &crate::wic::Bgra) -> Vec<u8> {
    let stride = b.width as usize * 4;
    let header = BITMAPINFOHEADER {
        biSize: size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: b.width as i32,
        biHeight: b.height as i32, // positive = bottom-up
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0,
        biSizeImage: b.pixels.len() as u32,
        ..Default::default()
    };
    let mut out = Vec::with_capacity(size_of::<BITMAPINFOHEADER>() + b.pixels.len());
    // SAFETY: BITMAPINFOHEADER is a plain C struct; viewing it as bytes is sound.
    out.extend_from_slice(unsafe { as_bytes(&header) });
    for row in b.pixels.chunks_exact(stride.max(1)).rev() {
        out.extend_from_slice(row);
    }
    out
}

fn hdrop_bytes(file: &Path) -> Vec<u8> {
    let header = DROPFILES {
        pFiles: size_of::<DROPFILES>() as u32,
        fWide: true.into(),
        ..Default::default()
    };
    let mut out = Vec::new();
    out.extend_from_slice(unsafe { as_bytes(&header) });
    for unit in path_wide(file) {
        out.extend_from_slice(&unit.to_le_bytes()); // includes the path's NUL
    }
    out.extend_from_slice(&0u16.to_le_bytes()); // list terminator
    out
}

unsafe fn as_bytes<T>(value: &T) -> &[u8] {
    unsafe { std::slice::from_raw_parts(value as *const T as *const u8, size_of::<T>()) }
}

/// True while Win, Alt, Shift or Ctrl is physically held. A held modifier would
/// turn our Ctrl+V into a different shortcut in the target.
pub fn modifiers_held() -> bool {
    [VK_LWIN, VK_RWIN, VK_MENU, VK_SHIFT, VK_CONTROL]
        .iter()
        .any(|vk| unsafe { GetAsyncKeyState(vk.0 as i32) } as u16 & 0x8000 != 0)
}

/// Sends Ctrl+V to whatever window has keyboard focus. The caller must have
/// verified that focus is on the intended target.
pub fn send_ctrl_v() -> Result<(), String> {
    fn key(vk: VIRTUAL_KEY, up: bool) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                    ..Default::default()
                },
            },
        }
    }
    let inputs = [key(VK_CONTROL, false), key(VK_V, false), key(VK_V, true), key(VK_CONTROL, true)];
    let sent = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
    if sent as usize == inputs.len() {
        Ok(())
    } else {
        Err(format!(
            "Windows accepted only {sent} of {} simulated key events (input may be blocked).",
            inputs.len()
        ))
    }
}

/// Starts a copy-only drag of the file using the shell's own data object, the same
/// one Explorer uses when you drag a file. Blocks until the drop or cancel.
pub fn drag_file_out(source: HWND, file: &Path) -> Result<DROPEFFECT, String> {
    let wide = path_wide(file);
    unsafe {
        let item: IShellItem = SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None)
            .map_err(|e| format!("Could not open the file for dragging: {e}"))?;
        let data: IDataObject = item
            .BindToHandler(None, &BHID_DataObject)
            .map_err(|e| format!("Could not create drag data: {e}"))?;
        // No custom IDropSource: the shell supplies its default one and drag image.
        SHDoDragDrop(Some(source), &data, None, DROPEFFECT_COPY)
            .map_err(|e| format!("Drag failed: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dib_rows_are_bottom_up_with_the_right_header() {
        // 1 x 2: a red top row, a blue bottom row (BGRA).
        let b = crate::wic::Bgra { width: 1, height: 2, pixels: vec![0, 0, 255, 255, 255, 0, 0, 255] };
        let dib = dib_from_bgra(&b);
        let h = size_of::<BITMAPINFOHEADER>();
        assert_eq!(i32::from_le_bytes(dib[4..8].try_into().unwrap()), 1);
        assert_eq!(i32::from_le_bytes(dib[8..12].try_into().unwrap()), 2, "positive height: bottom-up");
        assert_eq!(&dib[h..h + 4], &[255, 0, 0, 255], "the bottom (blue) row comes first");
        assert_eq!(&dib[h + 4..h + 8], &[0, 0, 255, 255]);
    }

    #[test]
    fn hdrop_lists_the_one_file() {
        let bytes = hdrop_bytes(Path::new("C:\\PocketMedia\\Media\\GIFs\\a.gif"));
        let offset = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
        let wide: Vec<u16> = bytes[offset..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let text = String::from_utf16_lossy(&wide);
        assert_eq!(text, "C:\\PocketMedia\\Media\\GIFs\\a.gif\0\0");
    }
}
