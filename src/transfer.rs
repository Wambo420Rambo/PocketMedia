//! Clipboard, simulated paste and drag-out for the test PNG.

use std::path::Path;

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

use crate::test_png::{self, TestImage};

pub fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn path_wide(path: &Path) -> Vec<u16> {
    to_wide(&path.to_string_lossy())
}

/// Replaces the clipboard with the test PNG as: "PNG" (encoded file bytes, read by
/// browsers/Electron/Office), CF_DIB (classic bitmap apps), CF_HDROP (the file
/// itself, for Explorer and file-attach targets) and Preferred DropEffect = copy.
pub fn put_on_clipboard(owner: HWND, png: &[u8], image: &TestImage, file: &Path) -> Result<(), String> {
    let png_format = unsafe { RegisterClipboardFormatW(w!("PNG")) };
    let effect_format = unsafe { RegisterClipboardFormatW(w!("Preferred DropEffect")) };
    if png_format == 0 || effect_format == 0 {
        return Err("Windows refused to register the PNG clipboard formats.".into());
    }

    let dib = dib_bytes(image);
    let hdrop = hdrop_bytes(file);
    let effect = DROPEFFECT_COPY.0.to_le_bytes();

    open_clipboard(owner)?;
    let result = (|| {
        unsafe { EmptyClipboard() }.map_err(|e| format!("Could not empty the clipboard: {e}"))?;
        set_data(png_format, png)?;
        set_data(CF_DIB.0 as u32, &dib)?;
        set_data(CF_HDROP.0 as u32, &hdrop)?;
        set_data(effect_format, &effect)?;
        Ok(())
    })();
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

fn dib_bytes(image: &TestImage) -> Vec<u8> {
    let pixels = image.bgra_bottom_up();
    let header = BITMAPINFOHEADER {
        biSize: size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: test_png::WIDTH as i32,
        biHeight: test_png::HEIGHT as i32, // positive = bottom-up
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0,
        biSizeImage: pixels.len() as u32,
        ..Default::default()
    };
    let mut out = Vec::with_capacity(size_of::<BITMAPINFOHEADER>() + pixels.len());
    out.extend_from_slice(unsafe { as_bytes(&header) });
    out.extend_from_slice(&pixels);
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
            .map_err(|e| format!("Could not open the test file for dragging: {e}"))?;
        let data: IDataObject = item
            .BindToHandler(None, &BHID_DataObject)
            .map_err(|e| format!("Could not create drag data: {e}"))?;
        // No custom IDropSource: the shell supplies its default one and drag image.
        SHDoDragDrop(Some(source), &data, None, DROPEFFECT_COPY)
            .map_err(|e| format!("Drag failed: {e}"))
    }
}
