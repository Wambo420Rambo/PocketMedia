//! The Windows file and folder pickers (IFileOpenDialog) for Add media.
//!
//! Under the self-test the pickers are not shown (a modal dialog cannot be
//! driven safely); the test sets the files and folder they would return.

use std::path::PathBuf;
use std::sync::Mutex;

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FOS_ALLOWMULTISELECT, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_NOCHANGEDIR, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS,
    FileOpenDialog, IFileOpenDialog, IShellItem, SIGDN_FILESYSPATH,
};
use windows::core::w;

use crate::import::Source;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    Files,
    Folder,
}

/// Self-test only: what the pickers return (files, folder).
static TEST_PICK: Mutex<Option<(Vec<PathBuf>, PathBuf)>> = Mutex::new(None);

pub fn set_test_pick(files: Vec<PathBuf>, folder: PathBuf) {
    if let Ok(mut t) = TEST_PICK.lock() {
        *t = Some((files, folder));
    }
}

/// Shows the picker. None when the user cancelled (or it could not be shown).
pub fn pick(owner: HWND, kind: Pick) -> Option<Source> {
    if crate::app(|a| a.quiet.get()) {
        let t = TEST_PICK.lock().ok()?.clone()?;
        return Some(match kind {
            Pick::Files => Source::Files(t.0),
            Pick::Folder => Source::Folder(t.1),
        });
    }
    show(owner, kind)
}

fn path_of(item: &IShellItem) -> Option<PathBuf> {
    // SAFETY: item is a live shell item; the returned string is freed below.
    let p = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }.ok()?;
    // SAFETY: GetDisplayName returned a valid null-terminated string.
    let s = unsafe { p.to_string() }.ok();
    // SAFETY: the string was allocated by the shell with CoTaskMemAlloc and is freed once.
    unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
    s.map(PathBuf::from)
}

fn show(owner: HWND, kind: Pick) -> Option<Source> {
    // SAFETY: COM is initialised on this (UI) thread by OleInitialize.
    let dialog: IFileOpenDialog = unsafe { CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) }.ok()?;
    // SAFETY: dialog is a live COM object for every call below; strings are static.
    unsafe {
        let mut options = dialog.GetOptions().ok()?;
        options |= FOS_FORCEFILESYSTEM | FOS_NOCHANGEDIR | FOS_PATHMUSTEXIST;
        match kind {
            Pick::Files => {
                options |= FOS_ALLOWMULTISELECT | FOS_FILEMUSTEXIST;
                let filters = [COMDLG_FILTERSPEC { pszName: w!("Pictures, GIFs and videos"), pszSpec: w!("*.jpg;*.jpeg;*.png;*.gif;*.mp4") }];
                dialog.SetFileTypes(&filters).ok()?;
                dialog.SetTitle(w!("Choose files to add")).ok()?;
            }
            Pick::Folder => {
                options |= FOS_PICKFOLDERS;
                dialog.SetTitle(w!("Choose a folder to import")).ok()?;
            }
        }
        dialog.SetOptions(options).ok()?;
        // Err when the user cancels.
        dialog.Show(Some(owner)).ok()?;
        match kind {
            Pick::Files => {
                let items = dialog.GetResults().ok()?;
                let count = items.GetCount().ok()?;
                let files = (0..count).filter_map(|i| items.GetItemAt(i).ok()).filter_map(|item| path_of(&item)).collect();
                Some(Source::Files(files))
            }
            Pick::Folder => path_of(&dialog.GetResult().ok()?).map(Source::Folder),
        }
    }
}
