//! Notification-area icon, its right-click menu and balloon messages.

use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use std::cell::RefCell;

use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, HMENU, MF_SEPARATOR, MF_STRING, PostMessageW,
    SetForegroundWindow, SetMenuDefaultItem, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    TrackPopupMenu, WM_NULL,
};
use windows::core::{PCWSTR, w};

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

thread_local! {
    /// The icon in the notification area, while PocketMedia shows one.
    static ICON: RefCell<Option<TrayIcon>> = const { RefCell::new(None) };
}

/// The notification-area icon, with one owner: dropping it removes the icon.
struct TrayIcon(HWND);

impl Drop for TrayIcon {
    fn drop(&mut self) {
        let data = base_data(self.0);
        // SAFETY: `data` is a local naming this icon; deleting a missing icon only fails.
        let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
    }
}

/// Shows the icon (start-up, and again after Explorer restarts). Any icon shown
/// before is removed first.
pub fn add(hwnd: HWND) {
    ICON.with(|i| drop(i.borrow_mut().take()));
    let mut data = base_data(hwnd);
    data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    data.uCallbackMessage = WM_TRAY;
    data.hIcon = crate::winutil::app_icon(true);
    copy_into(&mut data.szTip, DEFAULT_TIP);
    // May fail if Explorer is not running yet; TaskbarCreated will retry.
    // SAFETY: `data` is a local with cbSize set.
    let _ = unsafe { Shell_NotifyIconW(NIM_ADD, &data) };
    ICON.with(|i| *i.borrow_mut() = Some(TrayIcon(hwnd)));
}

/// Exit: removes the icon.
pub fn remove() {
    ICON.with(|i| drop(i.borrow_mut().take()));
}

pub fn set_tip(hwnd: HWND, tip: &str) {
    let mut data = base_data(hwnd);
    data.uFlags = NIF_TIP;
    copy_into(&mut data.szTip, tip);
    // SAFETY: `data` is a local with cbSize set.
    let _ = unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
}


/// A popup menu with one owner: dropping it destroys the menu.
struct Menu(HMENU);

impl Menu {
    fn new() -> Option<Self> {
        // SAFETY: no arguments; the handle is owned by the returned value.
        unsafe { CreatePopupMenu() }.ok().map(Menu)
    }

    fn item(&self, id: usize, text: PCWSTR) {
        // SAFETY: our menu; `text` is a static wide string.
        let _ = unsafe { AppendMenuW(self.0, MF_STRING, id, text) };
    }

    fn separator(&self) {
        // SAFETY: our menu; a separator has no text.
        let _ = unsafe { AppendMenuW(self.0, MF_SEPARATOR, 0, None) };
    }
}

impl Drop for Menu {
    fn drop(&mut self) {
        // SAFETY: this value is the menu's only owner, so it is destroyed once.
        let _ = unsafe { DestroyMenu(self.0) };
    }
}

pub fn show_menu(hwnd: HWND) -> MenuChoice {
    let Some(menu) = Menu::new() else {
        return MenuChoice::None;
    };
    menu.item(CMD_POPUP, w!("Quick popup"));
    menu.item(CMD_MANAGER, w!("Open manager"));
    menu.item(CMD_SETTINGS, w!("Settings"));
    menu.separator();
    menu.item(CMD_EXIT, w!("Exit"));
    // SAFETY: our menu; CMD_POPUP is one of its items.
    let _ = unsafe { SetMenuDefaultItem(menu.0, CMD_POPUP as u32, 0) };

    let mut pt = POINT::default();
    // SAFETY: out-pointer to a local.
    let _ = unsafe { GetCursorPos(&mut pt) };
    // Required so the menu closes when the user clicks elsewhere.
    // SAFETY: `hwnd` is our own main window.
    let foreground = unsafe { SetForegroundWindow(hwnd) }.as_bool();
    crate::selftest::note(&format!("tray menu: SetForegroundWindow -> {foreground}"));
    // SAFETY: our menu and window; the menu outlives the call (it is modal).
    let cmd = unsafe { TrackPopupMenu(menu.0, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, pt.x, pt.y, None, hwnd, None) };
    crate::selftest::note(&format!("tray menu: TrackPopupMenu returned {}", cmd.0));
    // SAFETY: posting a no-op message to our own window.
    let _ = unsafe { PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0)) };
    match cmd.0 as usize {
        CMD_POPUP => MenuChoice::QuickPopup,
        CMD_MANAGER => MenuChoice::Manager,
        CMD_SETTINGS => MenuChoice::Settings,
        CMD_EXIT => MenuChoice::Exit,
        _ => MenuChoice::None,
    }
}
