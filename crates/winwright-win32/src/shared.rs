//! Named objects shared by every Winwright process in this sign-in session: each AI app (and
//! each session of one) starts its own `winwright mcp`, but there is one emergency stop and one
//! tray icon for all of them.

use windows::Win32::Foundation::{
    CloseHandle, HANDLE, INVALID_HANDLE_VALUE, WAIT_ABANDONED, WAIT_OBJECT_0,
};
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_ALL_ACCESS, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
    PAGE_READWRITE, UnmapViewOfFile,
};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, ReleaseMutex, ResetEvent, SetEvent, WaitForSingleObject,
};
use windows::core::{PCWSTR, w};
use winwright_contracts::{WinwrightError, WinwrightResult};

/// Set while the person has stopped Winwright (hotkey or tray); every process follows it.
pub struct StopSignal(HANDLE);

// SAFETY: an event handle may be used from any thread.
unsafe impl Send for StopSignal {}
// SAFETY: as above; SetEvent/ResetEvent/WaitForSingleObject are thread-safe.
unsafe impl Sync for StopSignal {}

impl StopSignal {
    /// The shared signal, or one of this process's own when it cannot be opened (an elevated
    /// Winwright created it): then the stop reaches this process only.
    pub fn open() -> WinwrightResult<Self> {
        // SAFETY: a named manual-reset event, created unsignaled or opened if it exists.
        match unsafe { CreateEventW(None, true, false, w!("Local\\Winwright.EmergencyStop")) } {
            Ok(handle) => Ok(Self(handle)),
            Err(err) => {
                tracing::warn!(%err, "shared emergency stop unavailable; this process stops alone");
                // SAFETY: an unnamed manual-reset event, owned by this process alone.
                unsafe { CreateEventW(None, true, false, PCWSTR::null()) }
                    .map(Self)
                    .map_err(|e| WinwrightError::Platform {
                        operation: "CreateEventW".into(),
                        hresult: e.code().0,
                    })
            }
        }
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

/// What some Winwright process is doing right now (an action's summary, never typed text),
/// for the tray icon of the one that shows it. A small named shared memory block guarded by a
/// sequence number: a writer makes it odd while it writes, readers retry until it is even.
pub struct Activity {
    map: HANDLE,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
}

// SAFETY: the mapping is shared memory accessed only through atomics and volatile copies.
unsafe impl Send for Activity {}
// SAFETY: as above.
unsafe impl Sync for Activity {}

/// UTF-16 units kept; a tray tooltip holds 127.
const ACTIVITY_UNITS: usize = 120;
/// seq (u32), length (u32), tick (u64), text.
const ACTIVITY_SIZE: usize = 16 + ACTIVITY_UNITS * 2;
/// An activity older than this is over (its process ended mid-action).
const ACTIVITY_STALE_MS: u64 = 5_000;

impl Activity {
    pub fn open() -> Option<Self> {
        // SAFETY: a named, page-file backed mapping, created or opened; mapped right after.
        unsafe {
            let map = CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                None,
                PAGE_READWRITE,
                0,
                ACTIVITY_SIZE as u32,
                w!("Local\\Winwright.Activity"),
            )
            .ok()?;
            let view = MapViewOfFile(map, FILE_MAP_ALL_ACCESS, 0, 0, ACTIVITY_SIZE);
            if view.Value.is_null() {
                let _ = CloseHandle(map);
                return None;
            }
            Some(Self { map, view })
        }
    }

    fn seq(&self) -> &std::sync::atomic::AtomicU32 {
        // SAFETY: the view is page aligned and at least ACTIVITY_SIZE bytes for our lifetime.
        unsafe { &*(self.view.Value as *const std::sync::atomic::AtomicU32) }
    }

    fn base(&self) -> *mut u8 {
        self.view.Value.cast()
    }

    /// Posts `text` (empty: nothing going on). Skipped when another process is writing.
    pub fn post(&self, text: &str) {
        use std::sync::atomic::Ordering;
        let seq = self.seq();
        let current = seq.load(Ordering::Acquire);
        if current % 2 == 1
            || seq
                .compare_exchange(current, current + 1, Ordering::AcqRel, Ordering::Relaxed)
                .is_err()
        {
            return;
        }
        let units: Vec<u16> = text.encode_utf16().take(ACTIVITY_UNITS).collect();
        // SAFETY: GetTickCount64 reads a clock; every write stays inside the mapped view.
        unsafe {
            let tick = windows::Win32::System::SystemInformation::GetTickCount64();
            std::ptr::write_volatile(self.base().add(4).cast::<u32>(), units.len() as u32);
            std::ptr::write_volatile(self.base().add(8).cast::<u64>(), tick);
            for (i, unit) in units.iter().enumerate() {
                std::ptr::write_volatile(self.base().add(16 + i * 2).cast::<u16>(), *unit);
            }
        }
        seq.store(current + 2, Ordering::Release);
    }

    /// The latest activity if it is recent and not empty.
    pub fn current(&self) -> Option<String> {
        use std::sync::atomic::Ordering;
        for _ in 0..8 {
            let before = self.seq().load(Ordering::Acquire);
            if before % 2 == 1 {
                std::thread::yield_now();
                continue;
            }
            // SAFETY: reads stay inside the mapped view; a torn read is caught by `seq`.
            let (len, tick, units) = unsafe {
                let len = (std::ptr::read_volatile(self.base().add(4).cast::<u32>()) as usize)
                    .min(ACTIVITY_UNITS);
                let tick = std::ptr::read_volatile(self.base().add(8).cast::<u64>());
                let units: Vec<u16> = (0..len)
                    .map(|i| std::ptr::read_volatile(self.base().add(16 + i * 2).cast::<u16>()))
                    .collect();
                (len, tick, units)
            };
            if self.seq().load(Ordering::Acquire) != before {
                continue;
            }
            // SAFETY: reads a clock.
            let now = unsafe { windows::Win32::System::SystemInformation::GetTickCount64() };
            return (len > 0 && now.saturating_sub(tick) < ACTIVITY_STALE_MS)
                .then(|| String::from_utf16_lossy(&units));
        }
        None
    }
}

impl Drop for Activity {
    fn drop(&mut self) {
        // SAFETY: unmaps and closes our own view and handle once.
        unsafe {
            let _ = UnmapViewOfFile(self.view);
            let _ = CloseHandle(self.map);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_activity_posted_by_one_handle_is_read_by_another() {
        let (a, b) = (Activity::open().unwrap(), Activity::open().unwrap());
        a.post("Click Button \"Send\" in Discord.exe");
        assert_eq!(
            b.current().as_deref(),
            Some("Click Button \"Send\" in Discord.exe")
        );
        a.post("");
        assert_eq!(b.current(), None, "empty means nothing going on");
        a.post(&"x".repeat(500));
        assert_eq!(
            b.current().unwrap().len(),
            ACTIVITY_UNITS,
            "cut to fit a tooltip"
        );
        a.post("");
    }
}
