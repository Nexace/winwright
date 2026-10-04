//! Shell interop: known-folder resolution and Recycle-Bin-only deletion through
//! `IFileOperation`. A progress sink vetoes every item the shell would delete permanently.

use std::cell::Cell;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance, CoInitializeEx,
    CoTaskMemFree, CoUninitialize,
};
use windows::Win32::UI::Shell::{
    COPYENGINE_E_RECYCLE_BIN_NOT_FOUND, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI,
    FOF_SILENT, FOFX_RECYCLEONDELETE, FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads,
    FOLDERID_LocalAppData, FOLDERID_Music, FOLDERID_Pictures, FOLDERID_Profile,
    FOLDERID_ProgramData, FOLDERID_ProgramFiles, FOLDERID_ProgramFilesX64,
    FOLDERID_ProgramFilesX86, FOLDERID_RoamingAppData, FOLDERID_Startup, FOLDERID_Videos,
    FOLDERID_Windows, FileOperation, IFileOperation, IFileOperationProgressSink,
    IFileOperationProgressSink_Impl, IShellItem, KF_FLAG_DEFAULT, SHCreateItemFromParsingName,
    SHGetKnownFolderPath, TSF_DELETE_RECYCLE_IF_POSSIBLE,
};
use windows::core::{ComObject, GUID, HRESULT, PCWSTR, Ref, implement};
use winwright_contracts::{WinwrightError, WinwrightResult};

use crate::path::Protected;
use crate::{platform, wide};

/// Names accepted by the `knownFolder` operation (case-insensitive).
pub(crate) const KNOWN_FOLDER_NAMES: &[&str] = &[
    "Desktop",
    "Documents",
    "Downloads",
    "Pictures",
    "Music",
    "Videos",
    "Profile",
    "Home",
    "LocalAppData",
    "RoamingAppData",
    "AppData",
    "Temp",
];

/// KNOWNFOLDERID for a lower-cased name from [`KNOWN_FOLDER_NAMES`] (except `temp`).
pub(crate) fn folder_id(name: &str) -> Option<GUID> {
    Some(match name {
        "desktop" => FOLDERID_Desktop,
        "documents" => FOLDERID_Documents,
        "downloads" => FOLDERID_Downloads,
        "pictures" => FOLDERID_Pictures,
        "music" => FOLDERID_Music,
        "videos" => FOLDERID_Videos,
        "profile" | "home" => FOLDERID_Profile,
        "localappdata" => FOLDERID_LocalAppData,
        "roamingappdata" | "appdata" => FOLDERID_RoamingAppData,
        _ => return None,
    })
}

pub(crate) fn known_folder_path(id: &GUID) -> Option<PathBuf> {
    // SAFETY: `id` points to a valid KNOWNFOLDERID for the duration of the call.
    let raw = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) }.ok()?;
    // SAFETY: on success `raw` is a NUL-terminated string that stays valid until freed below.
    let path = PathBuf::from(OsString::from_wide(unsafe { raw.as_wide() }));
    // SAFETY: the shell allocated `raw` with CoTaskMemAlloc; it is not used after this.
    unsafe { CoTaskMemFree(Some(raw.0.cast_const().cast())) };
    Some(path)
}

/// The protected set for this user and machine, from known folders plus environment variables
/// (whichever resolve; duplicates collapse after canonicalization).
pub(crate) fn system_protected() -> Protected {
    let mut trees: Vec<PathBuf> = [
        FOLDERID_Windows,
        FOLDERID_ProgramFiles,
        FOLDERID_ProgramFilesX86,
        FOLDERID_ProgramFilesX64,
        FOLDERID_ProgramData,
        FOLDERID_Startup,
    ]
    .iter()
    .filter_map(known_folder_path)
    .collect();
    trees.extend(
        [
            "WINDIR",
            "SystemRoot",
            "ProgramFiles",
            "ProgramFiles(x86)",
            "ProgramW6432",
            "ProgramData",
        ]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from),
    );
    // Winwright's own config, audit log, and program folder (a DLL dropped next to the exe
    // loads into it): changing them changes the rules.
    trees.extend(
        [FOLDERID_RoamingAppData, FOLDERID_LocalAppData]
            .iter()
            .filter_map(known_folder_path)
            .chain(
                ["APPDATA", "LOCALAPPDATA"]
                    .into_iter()
                    .filter_map(std::env::var_os)
                    .map(PathBuf::from),
            )
            .map(|dir| dir.join("winwright")),
    );
    trees.extend(
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf)),
    );
    let profile = known_folder_path(&FOLDERID_Profile)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from));
    // Task memory: a report rewritten through the file tools could steer later conversations.
    trees.extend(profile.iter().map(|home| home.join(".winwright")));
    trees.extend(std::env::var_os("WINWRIGHT_REPORTS_DIR").map(PathBuf::from));
    Protected::new(trees, profile)
}

/// Balanced single-threaded apartment for the current blocking-pool thread.
struct Apartment;

impl Apartment {
    fn enter() -> WinwrightResult<Self> {
        // SAFETY: plain apartment initialization for this thread; Drop balances it.
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }
            .ok()
            .map_err(|e| platform("CoInitializeEx", &e))?;
        Ok(Self)
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: balances the successful CoInitializeEx in `enter` on the same thread.
        unsafe { CoUninitialize() };
    }
}

/// Vetoes permanent deletion: the shell clears `TSF_DELETE_RECYCLE_IF_POSSIBLE` for an item
/// it cannot recycle (too large, Recycle Bin disabled, no bin on the volume), and failing
/// `PreDeleteItem` skips that item. `PostDeleteItem` without a Recycle Bin item is recorded.
#[implement(IFileOperationProgressSink)]
#[derive(Default)]
struct RecycleGuard {
    refused: Cell<u32>,
    unrecycled: Cell<u32>,
}

impl IFileOperationProgressSink_Impl for RecycleGuard_Impl {
    fn StartOperations(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn FinishOperations(&self, _result: HRESULT) -> windows::core::Result<()> {
        Ok(())
    }
    fn PreRenameItem(
        &self,
        _flags: u32,
        _item: Ref<IShellItem>,
        _new_name: &PCWSTR,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostRenameItem(
        &self,
        _flags: u32,
        _item: Ref<IShellItem>,
        _new_name: &PCWSTR,
        _result: HRESULT,
        _created: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn PreMoveItem(
        &self,
        _flags: u32,
        _item: Ref<IShellItem>,
        _destination: Ref<IShellItem>,
        _new_name: &PCWSTR,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostMoveItem(
        &self,
        _flags: u32,
        _item: Ref<IShellItem>,
        _destination: Ref<IShellItem>,
        _new_name: &PCWSTR,
        _result: HRESULT,
        _created: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn PreCopyItem(
        &self,
        _flags: u32,
        _item: Ref<IShellItem>,
        _destination: Ref<IShellItem>,
        _new_name: &PCWSTR,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostCopyItem(
        &self,
        _flags: u32,
        _item: Ref<IShellItem>,
        _destination: Ref<IShellItem>,
        _new_name: &PCWSTR,
        _result: HRESULT,
        _created: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn PreDeleteItem(&self, flags: u32, _item: Ref<IShellItem>) -> windows::core::Result<()> {
        if flags & TSF_DELETE_RECYCLE_IF_POSSIBLE.0 as u32 == 0 {
            self.refused.set(self.refused.get() + 1);
            return Err(COPYENGINE_E_RECYCLE_BIN_NOT_FOUND.into());
        }
        Ok(())
    }
    fn PostDeleteItem(
        &self,
        _flags: u32,
        _item: Ref<IShellItem>,
        result: HRESULT,
        recycled: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        if result.is_ok() && recycled.is_null() {
            self.unrecycled.set(self.unrecycled.get() + 1);
        }
        Ok(())
    }
    fn PreNewItem(
        &self,
        _flags: u32,
        _destination: Ref<IShellItem>,
        _new_name: &PCWSTR,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostNewItem(
        &self,
        _flags: u32,
        _destination: Ref<IShellItem>,
        _new_name: &PCWSTR,
        _template: &PCWSTR,
        _attributes: u32,
        _result: HRESULT,
        _created: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn UpdateProgress(&self, _total: u32, _done: u32) -> windows::core::Result<()> {
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

/// Moves `path` (display form, no `\\?\`) to the Recycle Bin. Never deletes permanently: items
/// the shell cannot recycle are refused with `ActionBlocked`. Blocking; initializes an STA.
pub(crate) fn recycle(path: &Path) -> WinwrightResult<()> {
    let _apartment = Apartment::enter()?;
    let shown = path.display();
    let wide_path = wide(path);
    // SAFETY: `wide_path` is NUL-terminated and outlives the call; no bind context is passed.
    let item: IShellItem = unsafe { SHCreateItemFromParsingName(PCWSTR(wide_path.as_ptr()), None) }
        .map_err(|e| platform("SHCreateItemFromParsingName", &e))?;
    // SAFETY: activates the shell's FileOperation class on this thread's apartment.
    let operation: IFileOperation = unsafe { CoCreateInstance(&FileOperation, None, CLSCTX_ALL) }
        .map_err(|e| platform("CoCreateInstance(FileOperation)", &e))?;
    let flags =
        FOFX_RECYCLEONDELETE | FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI;
    // SAFETY: `operation` is a live IFileOperation owned by this thread.
    unsafe { operation.SetOperationFlags(flags) }
        .map_err(|e| platform("IFileOperation::SetOperationFlags", &e))?;

    let guard = ComObject::new(RecycleGuard::default());
    // SAFETY: the sink is a live COM object that outlives the operation; it is unadvised below.
    let cookie = unsafe { operation.Advise(guard.as_interface::<IFileOperationProgressSink>()) }
        .map_err(|e| platform("IFileOperation::Advise", &e))?;
    // SAFETY: `item` is a live shell item; no per-item sink is passed.
    let queued = unsafe { operation.DeleteItem(&item, None) };
    let performed = match queued {
        // SAFETY: runs the queued delete synchronously on this thread.
        Ok(()) => unsafe { operation.PerformOperations() },
        Err(e) => Err(e),
    };
    // SAFETY: `cookie` was returned by Advise on this same operation.
    let _ = unsafe { operation.Unadvise(cookie) };
    // SAFETY: queries the finished operation; an error is treated as "aborted".
    let aborted = unsafe { operation.GetAnyOperationsAborted() }.map_or(true, |b| b.as_bool());

    if guard.refused.get() > 0 {
        return Err(WinwrightError::ActionBlocked {
            reason: format!(
                "{shown} cannot be moved to the Recycle Bin (too large, or the Recycle Bin is \
                 disabled for this drive); Winwright never deletes permanently"
            ),
        });
    }
    if guard.unrecycled.get() > 0 {
        return Err(WinwrightError::ActionOutcomeUnknown {
            operation: "delete".to_owned(),
            reason: format!("the shell removed {shown} without reporting a Recycle Bin item"),
        });
    }
    performed.map_err(|e| platform("IFileOperation::PerformOperations", &e))?;
    if aborted {
        return Err(WinwrightError::ActionBlocked {
            reason: format!("moving {shown} to the Recycle Bin was aborted"),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn winwrights_own_folders_are_protected() {
        let protected = system_protected();
        let local = known_folder_path(&FOLDERID_LocalAppData).unwrap();
        let roaming = known_folder_path(&FOLDERID_RoamingAppData).unwrap();
        let exe_dir = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        for path in [
            local.join("winwright").join("audit.jsonl"),
            roaming.join("winwright").join("config.json"),
            exe_dir.join("version.dll"),
            known_folder_path(&FOLDERID_Profile)
                .unwrap()
                .join(".winwright")
                .join("reports")
                .join("2026-10-04-021326-task.md"),
            // Containing them is protected too.
            roaming.clone(),
        ] {
            assert!(protected.check(&path).is_err(), "{}", path.display());
        }
        assert!(
            protected
                .check(&std::env::temp_dir().join("winwright-scratch.txt"))
                .is_ok()
        );
    }
}
