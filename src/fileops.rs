//! File operations that need Windows itself: a move that never replaces a file,
//! the Recycle Bin, and showing a file in File Explorer.

use std::cell::Cell;
use std::io;
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::E_ABORT;
use windows::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::Shell::{
    FOF_ALLOWUNDO, FOF_NO_UI, FOFX_RECYCLEONDELETE, FileOperation, IFileOperation, IFileOperationProgressSink,
    IFileOperationProgressSink_Impl, IShellItem, SHCreateItemFromParsingName, SHQUERYRBINFO, SHQueryRecycleBinW,
    ShellExecuteW, TSF_DELETE_RECYCLE_IF_POSSIBLE,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HRESULT, HSTRING, PCWSTR, Ref, implement, w};

/// Moves (renames) `from` to `to` on the same volume. Fails with
/// `AlreadyExists` if `to` exists: nothing is ever replaced. A change of letter
/// case only (`a.png` to `A.png`) is allowed, as in Explorer.
pub fn move_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    let (f, t) = (HSTRING::from(from.as_os_str()), HSTRING::from(to.as_os_str()));
    // SAFETY: both HSTRINGs are valid null-terminated paths for the duration of the call.
    // Without MOVEFILE_REPLACE_EXISTING the move fails if `to` exists.
    unsafe { MoveFileExW(&f, &t, MOVEFILE_WRITE_THROUGH) }.map_err(|e| io::Error::from_raw_os_error(e.code().0 & 0xFFFF))
}

/// Why a file was not moved to the Recycle Bin.
#[derive(Debug, PartialEq, Eq)]
pub enum RecycleError {
    /// Windows would have deleted it permanently (the drive has no Recycle Bin,
    /// the file is larger than the bin, or the bin is set to delete files right
    /// away), so PocketMedia refused. The file is untouched.
    WouldDelete,
    Failed(String),
}

/// Self-test only: the next delete is treated as one Windows would not recycle.
static REFUSE_NEXT: AtomicBool = AtomicBool::new(false);

pub fn refuse_next_for_test(on: bool) {
    REFUSE_NEXT.store(on, Ordering::SeqCst);
}

/// Whether a delete Windows announced with `flags` (TRANSFER_SOURCE_FLAGS) may
/// go ahead: only when Windows will put the item in the Recycle Bin.
pub fn may_delete(flags: u32, refuse_for_test: bool) -> bool {
    !refuse_for_test && flags & TSF_DELETE_RECYCLE_IF_POSSIBLE.0 as u32 != 0
}

/// What the progress sink saw.
#[derive(Default)]
struct Verdict {
    asked: Cell<bool>,
    refused: Cell<bool>,
}

/// Told about each delete before it happens; cancels any that would not recycle.
#[implement(IFileOperationProgressSink)]
struct RecycleOnly(Rc<Verdict>);

impl IFileOperationProgressSink_Impl for RecycleOnly_Impl {
    fn PreDeleteItem(&self, flags: u32, _item: Ref<IShellItem>) -> windows::core::Result<()> {
        self.0.asked.set(true);
        if may_delete(flags, REFUSE_NEXT.swap(false, Ordering::SeqCst)) {
            Ok(())
        } else {
            // An error here cancels this delete and everything after it.
            self.0.refused.set(true);
            Err(E_ABORT.into())
        }
    }
    fn StartOperations(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn FinishOperations(&self, _: HRESULT) -> windows::core::Result<()> {
        Ok(())
    }
    fn PreRenameItem(&self, _: u32, _: Ref<IShellItem>, _: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostRenameItem(&self, _: u32, _: Ref<IShellItem>, _: &PCWSTR, _: HRESULT, _: Ref<IShellItem>) -> windows::core::Result<()> {
        Ok(())
    }
    fn PreMoveItem(&self, _: u32, _: Ref<IShellItem>, _: Ref<IShellItem>, _: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostMoveItem(&self, _: u32, _: Ref<IShellItem>, _: Ref<IShellItem>, _: &PCWSTR, _: HRESULT, _: Ref<IShellItem>) -> windows::core::Result<()> {
        Ok(())
    }
    fn PreCopyItem(&self, _: u32, _: Ref<IShellItem>, _: Ref<IShellItem>, _: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostCopyItem(&self, _: u32, _: Ref<IShellItem>, _: Ref<IShellItem>, _: &PCWSTR, _: HRESULT, _: Ref<IShellItem>) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostDeleteItem(&self, _: u32, _: Ref<IShellItem>, _: HRESULT, _: Ref<IShellItem>) -> windows::core::Result<()> {
        Ok(())
    }
    fn PreNewItem(&self, _: u32, _: Ref<IShellItem>, _: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostNewItem(&self, _: u32, _: Ref<IShellItem>, _: &PCWSTR, _: &PCWSTR, _: u32, _: HRESULT, _: Ref<IShellItem>) -> windows::core::Result<()> {
        Ok(())
    }
    fn UpdateProgress(&self, _: u32, _: u32) -> windows::core::Result<()> {
        Ok(())
    }
    fn ResetTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn PauseTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn ResumeTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
}

/// Moves a file to the Recycle Bin, and never deletes it permanently: Windows
/// reports, item by item, whether it will recycle, and any delete it would
/// not recycle is cancelled. Success is reported only when Windows said it
/// would recycle the file and the file is gone. Call on the UI thread (COM is
/// initialised there as single-threaded, which IFileOperation needs).
pub fn recycle(path: &Path) -> Result<(), RecycleError> {
    if !path.exists() {
        return Err(RecycleError::Failed("the file is not in the Media folder".into()));
    }
    let verdict = Rc::new(Verdict::default());
    let sink: IFileOperationProgressSink = RecycleOnly(verdict.clone()).into();
    let fail = |e: windows::core::Error| RecycleError::Failed(e.message());
    // SAFETY: COM is initialised on this thread; every COM object is owned here and
    // released by Drop; the path HSTRING outlives the call that reads it.
    let performed = unsafe {
        let op: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_INPROC_SERVER).map_err(fail)?;
        op.SetOperationFlags(FOF_NO_UI | FOF_ALLOWUNDO | FOFX_RECYCLEONDELETE).map_err(fail)?;
        let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(path.as_os_str()), None).map_err(fail)?;
        op.DeleteItem(&item, &sink).map_err(fail)?;
        op.PerformOperations()
    };
    let gone = !path.exists();
    if verdict.refused.get() {
        return if gone {
            Err(RecycleError::Failed("Windows removed it although PocketMedia cancelled".into()))
        } else {
            Err(RecycleError::WouldDelete)
        };
    }
    if let Err(e) = performed {
        return Err(RecycleError::Failed(format!("Windows reported: {}", e.message())));
    }
    if !gone {
        return Err(RecycleError::Failed("the file is still there".into()));
    }
    if !verdict.asked.get() {
        return Err(RecycleError::Failed("Windows removed it without saying whether it went to the Recycle Bin".into()));
    }
    Ok(())
}

/// Number of items in the Recycle Bin of the drive holding `path` (self-test evidence).
pub fn recycle_bin_items(path: &Path) -> Option<i64> {
    let mut info = SHQUERYRBINFO { cbSize: size_of::<SHQUERYRBINFO>() as u32, ..Default::default() };
    // SAFETY: `info` is a local, sized struct; the path outlives the call.
    unsafe { SHQueryRecycleBinW(&HSTRING::from(path.as_os_str()), &mut info) }.ok()?;
    Some(info.i64NumItems)
}

/// Opens File Explorer with `path` selected.
pub fn show_in_explorer(path: &Path) -> Result<(), String> {
    let args = HSTRING::from(format!("/select,\"{}\"", path.display()));
    // SAFETY: static and local strings outlive the call; no owner window.
    let result = unsafe { ShellExecuteW(None, w!("open"), w!("explorer.exe"), &args, None, SW_SHOWNORMAL) };
    if result.0 as usize > 32 { Ok(()) } else { Err(format!("Windows returned error {}", result.0 as usize)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_deletes_windows_will_recycle_may_go_ahead() {
        // Without TSF_DELETE_RECYCLE_IF_POSSIBLE Windows would delete
        // the file permanently.
        let recycle = TSF_DELETE_RECYCLE_IF_POSSIBLE.0 as u32;
        assert!(may_delete(recycle, false));
        assert!(may_delete(recycle | 0x400, false));
        assert!(!may_delete(0, false));
        assert!(!may_delete(0x400, false));
        assert!(!may_delete(recycle, true), "the self-test switch refuses even a recyclable file");
    }
}
