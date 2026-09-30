//! COM apartment lifetime and HRESULT mapping.

use std::marker::PhantomData;

use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use winwright_contracts::WinwrightError;

pub fn platform(operation: &str, err: &windows::core::Error) -> WinwrightError {
    WinwrightError::Platform {
        operation: operation.to_owned(),
        hresult: err.code().0,
    }
}

/// A capture API refused the target (protected, closed, or otherwise uncapturable).
pub fn refused(operation: &str, err: &windows::core::Error) -> WinwrightError {
    WinwrightError::CaptureFailed {
        reason: format!("{operation} failed with HRESULT {:#010x}", err.code().0),
    }
}

/// Balanced `CoInitializeEx` / `CoUninitialize`. Declare it before any COM object so it drops
/// last. Also initializes WinRT: an MTA COM thread is a WinRT multithreaded apartment.
pub struct ComApartment {
    owned: bool,
    /// Apartment membership is per thread: keep the guard on the thread that created it.
    _thread_bound: PhantomData<*const ()>,
}

impl ComApartment {
    fn new(owned: bool) -> Self {
        Self {
            owned,
            _thread_bound: PhantomData,
        }
    }

    /// Joins the MTA; fails if the thread already lives in an STA. For the dedicated worker.
    pub fn init_mta() -> Result<Self, WinwrightError> {
        // SAFETY: called at the start of the dedicated worker thread; S_FALSE (already
        // initialized) still requires a matching CoUninitialize, which Drop provides.
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .map_err(|e| platform("CoInitializeEx", &e))?;
        Ok(Self::new(true))
    }

    /// Joins the MTA, or reuses the apartment the calling thread already has.
    pub fn ensure() -> Result<Self, WinwrightError> {
        // SAFETY: plain apartment initialization for the calling thread; balanced in Drop only
        // when this call succeeded.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr == RPC_E_CHANGED_MODE {
            return Ok(Self::new(false));
        }
        hr.ok().map_err(|e| platform("CoInitializeEx", &e))?;
        Ok(Self::new(true))
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.owned {
            // SAFETY: balances the successful CoInitializeEx above on the same thread.
            unsafe { CoUninitialize() };
        }
    }
}
