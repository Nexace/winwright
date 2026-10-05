//! Named objects shared by every Winwright process in this sign-in session: each AI app (and
//! each session of one) starts its own `winwright mcp`, but there is one emergency stop and one
//! tray icon for all of them.

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, ReleaseMutex, ResetEvent, SetEvent, WaitForSingleObject,
};
use windows::core::w;
use winwright_contracts::{WinwrightError, WinwrightResult};

/// Set while the person has stopped Winwright (hotkey or tray); every process follows it.
pub struct StopSignal(HANDLE);

// SAFETY: an event handle may be used from any thread.
unsafe impl Send for StopSignal {}
// SAFETY: as above; SetEvent/ResetEvent/WaitForSingleObject are thread-safe.
unsafe impl Sync for StopSignal {}

impl StopSignal {
    pub fn open() -> WinwrightResult<Self> {
        // SAFETY: a named manual-reset event, created unsignaled or opened if it exists.
        unsafe { CreateEventW(None, true, false, w!("Local\\Winwright.EmergencyStop")) }
            .map(Self)
            .map_err(|e| WinwrightError::Platform {
                operation: "CreateEventW".into(),
                hresult: e.code().0,
            })
    }

    pub fn stop(&self) {
        // SAFETY: a live event handle.
        let _ = unsafe { SetEvent(self.0) };
    }

    pub fn resume(&self) {
        // SAFETY: a live event handle.
        let _ = unsafe { ResetEvent(self.0) };
    }

    pub fn is_stopped(&self) -> bool {
        // SAFETY: a zero timeout only polls the event.
        let wait = unsafe { WaitForSingleObject(self.0, 0) };
        wait == WAIT_OBJECT_0
    }
}

impl Drop for StopSignal {
    fn drop(&mut self) {
        // SAFETY: closes our own handle once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// Held by the one process that shows the tray icon and owns the hotkey. Ownership belongs to
/// the thread that took it: keep it on that thread. When the process ends, the next one to
/// try takes it.
pub struct LeaderLock(HANDLE);

impl LeaderLock {
    pub fn try_acquire() -> Option<Self> {
        // SAFETY: a named mutex, opened if it exists.
        let handle = unsafe { CreateMutexW(None, false, w!("Local\\Winwright.Leader")) }.ok()?;
        // SAFETY: a zero timeout only tries; an abandoned mutex (its owner died) is ours.
        let wait = unsafe { WaitForSingleObject(handle, 0) };
        if wait == WAIT_OBJECT_0 || wait == WAIT_ABANDONED {
            Some(Self(handle))
        } else {
            // SAFETY: our handle, closed once.
            let _ = unsafe { CloseHandle(handle) };
            None
        }
    }
}

impl Drop for LeaderLock {
    fn drop(&mut self) {
        // SAFETY: owned by this thread (see the type's doc); closed once.
        unsafe {
            let _ = ReleaseMutex(self.0);
            let _ = CloseHandle(self.0);
        }
    }
}
