//! Notification-area icon, its right-click menu and balloon messages.

use std::cell::Cell;

use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
    DeleteObject, HBITMAP,
};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconIndirect, CreatePopupMenu, DestroyIcon, DestroyMenu, GetCursorPos,
    GetSystemMetrics, HICON, ICONINFO, MF_SEPARATOR, MF_STRING, PostMessageW, SM_CXSMICON,
    SetForegroundWindow, SetMenuDefaultItem, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    TrackPopupMenu, WM_NULL,
};
use windows::core::w;

use crate::WM_TRAY;

const ICON_ID: u32 = 1;
const CMD_OPEN: usize = 1;
const CMD_EXIT: usize = 2;
const DEFAULT_TIP: &str = "PocketMedia";

thread_local! {
    static ICON: Cell<HICON> = Cell::new(HICON::default());
}

pub enum MenuChoice {
    Open,
    Exit,
    None,
}

fn base_data(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: ICON_ID,
        ..Default::default()
    }
}

fn copy_into(dst: &mut [u16], text: &str) {
    let max = dst.len() - 1;
    for (slot, unit) in dst.iter_mut().zip(text.encode_utf16().take(max)) {
        *slot = unit;
    }
}

pub fn add(hwnd: HWND) {
    let icon = ICON.with(|c| {
        if c.get().is_invalid() {
            c.set(make_icon());
        }
        c.get()
    });
    let mut data = base_data(hwnd);
    data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    data.uCallbackMessage = WM_TRAY;
    data.hIcon = icon;
    copy_into(&mut data.szTip, DEFAULT_TIP);
    // May fail if Explorer is not running yet; TaskbarCreated will retry.
    unsafe {
        let _ = Shell_NotifyIconW(NIM_ADD, &data);
    }
}

pub fn remove(hwnd: HWND) {
    let data = base_data(hwnd);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &data);
    }
    ICON.with(|c| {
        let icon = c.replace(HICON::default());
        if !icon.is_invalid() {
            unsafe {
                let _ = DestroyIcon(icon);
            }
        }
    });
}

pub fn set_tip(hwnd: HWND, tip: &str) {
    let mut data = base_data(hwnd);
    data.uFlags = NIF_TIP;
    copy_into(&mut data.szTip, tip);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
    }
}


pub fn show_menu(hwnd: HWND) -> MenuChoice {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return MenuChoice::None;
        };
        let _ = AppendMenuW(menu, MF_STRING, CMD_OPEN, w!("Open"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, CMD_EXIT, w!("Exit"));
        let _ = SetMenuDefaultItem(menu, CMD_OPEN as u32, 0);

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // Required so the menu closes when the user clicks elsewhere.
        let foreground = SetForegroundWindow(hwnd).as_bool();
        crate::selftest::note(&format!("tray menu: SetForegroundWindow -> {foreground}"));
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
            pt.x,
            pt.y,
            None,
            hwnd,
            None,
        );
        crate::selftest::note(&format!("tray menu: TrackPopupMenu returned {}", cmd.0));
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        match cmd.0 as usize {
            CMD_OPEN => MenuChoice::Open,
            CMD_EXIT => MenuChoice::Exit,
            _ => MenuChoice::None,
        }
    }
}

/// Small lavender "media frame" icon drawn in code (no resource compiler needed).
fn make_icon() -> HICON {
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16);
    unsafe {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                biHeight: -size, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let Ok(color) = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0) else {
            return HICON::default();
        };
        let pixels = std::slice::from_raw_parts_mut(bits as *mut u32, (size * size) as usize);
        let s = size as f32;
        let radius = s * 0.22;
        for y in 0..size {
            for x in 0..size {
                let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                // Rounded-square coverage (1px soft edge).
                let dx = (radius - fx).max(fx - (s - radius)).max(0.0);
                let dy = (radius - fy).max(fy - (s - radius)).max(0.0);
                let dist = (dx * dx + dy * dy).sqrt();
                let alpha = (radius - dist + 0.5).clamp(0.0, 1.0);
                let inner = fx > s * 0.28 && fx < s * 0.72 && fy > s * 0.30 && fy < s * 0.70;
                let (r, g, b) = if inner { (0x1B, 0x1F, 0x27) } else { (0xB0, 0x8C, 0xFF) };
                let a = (alpha * 255.0) as u32;
                // Premultiplied BGRA.
                let pm = |c: u32| c * a / 255;
                pixels[(y * size + x) as usize] = (a << 24) | (pm(r) << 16) | (pm(g) << 8) | pm(b);
            }
        }
        // All-zero AND mask: the alpha channel of the colour bitmap decides transparency.
        let mask_bits = vec![0u8; (((size + 15) / 16) * 2 * size) as usize];
        let mask: HBITMAP = CreateBitmap(size, size, 1, 1, Some(mask_bits.as_ptr() as *const _));
        let icon_info = ICONINFO {
            fIcon: true.into(),
            hbmMask: mask,
            hbmColor: color,
            ..Default::default()
        };
        let icon = CreateIconIndirect(&icon_info).unwrap_or_default();
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        icon
    }
}
