use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use windows::Win32::Foundation::{CloseHandle, HANDLE};

/// Owned kernel handle, closed exactly once on drop.
pub(crate) struct OwnedHandle(pub(crate) HANDLE);

impl OwnedHandle {
    /// Wraps a handle returned by a successful Win32 call; null/invalid values yield `None`.
    pub(crate) fn new(handle: HANDLE) -> Option<Self> {
        (!handle.is_invalid()).then_some(Self(handle))
    }
}

// SAFETY: kernel handles are process-wide values, valid on any thread; the wrapper only closes
// the handle (once, on drop) and hands it to thread-safe Win32 calls.
unsafe impl Send for OwnedHandle {}
// SAFETY: as above; shared references only pass the handle value to thread-safe Win32 calls.
unsafe impl Sync for OwnedHandle {}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: the handle came from a successful Win32 call, is owned solely by this
        // wrapper, and is not used after this point.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// NUL-terminated UTF-16 copy of `value` for `PCWSTR` parameters.
pub(crate) fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value
        .as_ref()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}
