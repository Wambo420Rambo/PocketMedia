//! Notification-area icon, its right-click menu and balloon messages.

use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, MF_SEPARATOR, MF_STRING, PostMessageW,
    SetForegroundWindow, SetMenuDefaultItem, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    TrackPopupMenu, WM_NULL,
};
use windows::core::w;

use crate::WM_TRAY;

const ICON_ID: u32 = 1;
const CMD_POPUP: usize = 1;
const CMD_EXIT: usize = 2;
const CMD_MANAGER: usize = 3;
const CMD_SETTINGS: usize = 4;
const DEFAULT_TIP: &str = "PocketMedia";

pub enum MenuChoice {
    QuickPopup,
    Manager,
    Settings,
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
    let mut data = base_data(hwnd);
    data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    data.uCallbackMessage = WM_TRAY;
    data.hIcon = crate::winutil::app_icon(true);
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
        let _ = AppendMenuW(menu, MF_STRING, CMD_POPUP, w!("Quick popup"));
        let _ = AppendMenuW(menu, MF_STRING, CMD_MANAGER, w!("Open manager"));
        let _ = AppendMenuW(menu, MF_STRING, CMD_SETTINGS, w!("Settings"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, CMD_EXIT, w!("Exit"));
        let _ = SetMenuDefaultItem(menu, CMD_POPUP as u32, 0);

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
            CMD_POPUP => MenuChoice::QuickPopup,
            CMD_MANAGER => MenuChoice::Manager,
            CMD_SETTINGS => MenuChoice::Settings,
            CMD_EXIT => MenuChoice::Exit,
            _ => MenuChoice::None,
        }
    }
}
