//! Dropping files or a folder onto PocketMedia ("dropping files or a
//! folder" is an entry point of the one import pipeline; the Add media box and
//! the Manager's hint invite it). The quick popup and the Manager each register
//! an OLE drop target. A drop is read here, then handed to the window by a posted
//! message, so the window works on it after Windows' drag code has returned.

use std::path::PathBuf;
use std::sync::Mutex;

use windows::Win32::Foundation::{HWND, LPARAM, POINTL, WPARAM};
use windows::Win32::System::Com::{DVASPECT_CONTENT, FORMATETC, IDataObject, TYMED_HGLOBAL};
use windows::Win32::System::Ole::{
    CF_HDROP, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE, IDropTarget, IDropTarget_Impl, RegisterDragDrop, ReleaseStgMedium,
    RevokeDragDrop,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows::core::{Ref, implement};

use crate::import::Source;

/// Posted to the window that received a drop; the paths wait in `PENDING`.
pub const WM_DROPPED: u32 = WM_APP + 5;

/// The last drop, until its window takes it.
/// The window is kept as a plain number: HWND is not `Send`.
static PENDING: Mutex<Option<(usize, Vec<PathBuf>)>> = Mutex::new(None);

/// Whether the window can take a drop right now (for example, not while an import runs).
pub type Accepts = fn() -> bool;

/// The drop target of one window. Released by `Registration`.
#[implement(IDropTarget)]
struct Target {
    hwnd: usize,
    accepts: Accepts,
    /// What the drag over us offers: files we can take.
    files: std::cell::Cell<bool>,
}

impl Target {
    fn effect(&self) -> DROPEFFECT {
        if self.files.get() && !crate::paste::dragging_out() && (self.accepts)() { DROPEFFECT_COPY } else { DROPEFFECT_NONE }
    }
}

impl IDropTarget_Impl for Target_Impl {
    fn DragEnter(&self, data: Ref<IDataObject>, _keys: MODIFIERKEYS_FLAGS, _pt: &POINTL, effect: *mut DROPEFFECT) -> windows::core::Result<()> {
        let offers = data.ok().is_ok_and(|d| {
            // SAFETY: `HDROP_FORMAT` is a valid FORMATETC; the call only reads it.
            unsafe { d.QueryGetData(&HDROP_FORMAT) }.is_ok()
        });
        self.files.set(offers);
        set_effect(effect, self.effect());
        Ok(())
    }
    fn DragOver(&self, _keys: MODIFIERKEYS_FLAGS, _pt: &POINTL, effect: *mut DROPEFFECT) -> windows::core::Result<()> {
        set_effect(effect, self.effect());
        Ok(())
    }
    fn DragLeave(&self) -> windows::core::Result<()> {
        self.files.set(false);
        Ok(())
    }
    fn Drop(&self, data: Ref<IDataObject>, _keys: MODIFIERKEYS_FLAGS, _pt: &POINTL, effect: *mut DROPEFFECT) -> windows::core::Result<()> {
        let take = self.effect() == DROPEFFECT_COPY;
        self.files.set(false);
        let paths = if take { data.ok().map(paths_of).unwrap_or_default() } else { Vec::new() };
        if paths.is_empty() {
            set_effect(effect, DROPEFFECT_NONE);
            return Ok(());
        }
        // Copy only: the files stay where they are (the import copies them).
        set_effect(effect, DROPEFFECT_COPY);
        if let Ok(mut p) = PENDING.lock() {
            *p = Some((self.hwnd, paths));
        }
        // SAFETY: posting to our own window; a closed window only makes it fail.
        let _ = unsafe { PostMessageW(Some(HWND(self.hwnd as *mut _)), WM_DROPPED, WPARAM(0), LPARAM(0)) };
        Ok(())
    }
}

const HDROP_FORMAT: FORMATETC =
    FORMATETC { cfFormat: CF_HDROP.0, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 };

fn set_effect(effect: *mut DROPEFFECT, value: DROPEFFECT) {
    if !effect.is_null() {
        // SAFETY: OLE passes a valid pointer to write the effect to.
        unsafe { *effect = value };
    }
}

/// The files and folders a drag offers (CF_HDROP).
fn paths_of(data: &IDataObject) -> Vec<PathBuf> {
    // SAFETY: a valid FORMATETC; the returned medium is released below.
    let Ok(mut medium) = (unsafe { data.GetData(&HDROP_FORMAT) }) else { return Vec::new() };
    // SAFETY: TYMED_HGLOBAL was asked for, so `hGlobal` is the member that is set.
    let drop = HDROP(unsafe { medium.u.hGlobal }.0);
    let mut out = Vec::new();
    // SAFETY: `drop` is the HDROP inside the medium, alive until ReleaseStgMedium.
    let count = unsafe { DragQueryFileW(drop, u32::MAX, None) };
    for i in 0..count {
        // SAFETY: as above; the first call asks for the length, the second fills `buf`.
        let len = unsafe { DragQueryFileW(drop, i, None) } as usize;
        let mut buf = vec![0u16; len + 1];
        // SAFETY: `buf` has room for the path and its terminating zero.
        let n = unsafe { DragQueryFileW(drop, i, Some(&mut buf)) } as usize;
        out.push(PathBuf::from(String::from_utf16_lossy(&buf[..n.min(len)])));
    }
    // SAFETY: the medium came from GetData and is released once.
    unsafe { ReleaseStgMedium(&mut medium) };
    out
}

/// What a drop starts: files, or one folder with all its subfolders. A drop of
/// several folders, or of folders and files together, is refused with a message.
pub fn source_for(paths: Vec<PathBuf>) -> Result<Source, String> {
    let folders = paths.iter().filter(|p| p.is_dir()).count();
    match (paths.len(), folders) {
        (0, _) => Err("Nothing was dropped.".into()),
        (1, 1) => Ok(Source::Folder(paths.into_iter().next().unwrap_or_default())),
        (_, 0) => Ok(Source::Files(paths)),
        _ => Err("Drop files, or one folder at a time.".into()),
    }
}

/// Takes the drop waiting for `hwnd` (on WM_DROPPED).
pub fn take(hwnd: HWND) -> Option<Vec<PathBuf>> {
    let mut p = PENDING.lock().ok()?;
    if p.as_ref().is_some_and(|(h, _)| *h == hwnd.0 as usize) { p.take().map(|(_, paths)| paths) } else { None }
}

/// A window's drop target; revoked when dropped (the window is being destroyed).
pub struct Registration(HWND);

impl Registration {
    /// Makes `hwnd` accept drops of files and folders.
    pub fn new(hwnd: HWND, accepts: Accepts) -> Result<Self, String> {
        let target: IDropTarget = Target { hwnd: hwnd.0 as usize, accepts, files: std::cell::Cell::new(false) }.into();
        // SAFETY: our own window on this (OLE-initialised) thread; OLE keeps its own
        // reference to `target` until RevokeDragDrop.
        unsafe { RegisterDragDrop(hwnd, &target) }.map_err(|e| format!("Dropping files will not work: {e}"))?;
        Ok(Self(hwnd))
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        // SAFETY: revokes the registration made in `new` for this window.
        let _ = unsafe { RevokeDragDrop(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drop_is_files_or_one_folder() {
        let root = std::env::temp_dir().join(format!("pm-drop-{}", std::process::id()));
        let (a, b, f, g) = (root.join("a.png"), root.join("b.gif"), root.join("folder"), root.join("other"));
        std::fs::create_dir_all(&f).unwrap();
        std::fs::create_dir_all(&g).unwrap();
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();
        assert!(matches!(source_for(vec![a.clone(), b.clone()]), Ok(Source::Files(v)) if v == vec![a.clone(), b.clone()]));
        assert!(matches!(source_for(vec![f.clone()]), Ok(Source::Folder(p)) if p == f));
        assert!(source_for(vec![f.clone(), g.clone()]).is_err(), "two folders");
        assert!(source_for(vec![a.clone(), f.clone()]).is_err(), "a file and a folder");
        assert!(source_for(Vec::new()).is_err());
        // A file that vanished is still offered: the import lists it as unreadable.
        assert!(matches!(source_for(vec![root.join("gone.png")]), Ok(Source::Files(_))));
        let _ = std::fs::remove_dir_all(&root);
    }
}
