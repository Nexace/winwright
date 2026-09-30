//! Audited COM interop: apartment lifetime, VARIANT reading, SAFEARRAY conversion.
//! Nothing here escapes the worker thread.

use std::ffi::c_void;

use windows::Win32::System::Com::{
    COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize, SAFEARRAY,
};
use windows::Win32::System::Ole::{
    SafeArrayAccessData, SafeArrayDestroy, SafeArrayGetDim, SafeArrayGetLBound, SafeArrayGetUBound,
    SafeArrayUnaccessData,
};
use windows::Win32::System::Variant::{VARIANT, VT_BOOL, VT_BSTR, VT_I4, VT_R8, VariantClear};
use winwright_contracts::WinwrightError;

pub fn platform(operation: &str, err: &windows::core::Error) -> WinwrightError {
    WinwrightError::Platform {
        operation: operation.to_owned(),
        hresult: err.code().0,
    }
}

/// Balanced `CoInitializeEx(MTA)` / `CoUninitialize` for the worker thread.
/// Declare it before any COM object so it drops last.
pub struct ComApartment(());

impl ComApartment {
    pub fn init_mta() -> Result<Self, WinwrightError> {
        // SAFETY: called once at the start of the dedicated worker thread; S_FALSE (already
        // initialized) still requires a matching CoUninitialize, which Drop provides.
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .map_err(|e| platform("CoInitializeEx", &e))?;
        Ok(Self(()))
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        // SAFETY: balances the successful CoInitializeEx in `init_mta` on the same thread.
        unsafe { CoUninitialize() };
    }
}

/// Owned VARIANT that is cleared on drop. Accessors only read the matching `vt` arm, so the
/// UIA "not supported" sentinel (VT_UNKNOWN) and VT_EMPTY both read as `None`.
pub struct OwnedVariant(VARIANT);

impl OwnedVariant {
    pub fn new(v: VARIANT) -> Self {
        Self(v)
    }

    fn vt(&self) -> u16 {
        // SAFETY: every VARIANT, including a zeroed one, has a valid `vt` discriminant.
        unsafe { self.0.Anonymous.Anonymous.vt.0 }
    }

    pub fn as_bool(&self) -> Option<bool> {
        // SAFETY: `boolVal` is the active union arm when vt == VT_BOOL.
        (self.vt() == VT_BOOL.0)
            .then(|| unsafe { self.0.Anonymous.Anonymous.Anonymous.boolVal }.as_bool())
    }

    pub fn as_f64(&self) -> Option<f64> {
        // SAFETY: `dblVal` is the active union arm when vt == VT_R8.
        (self.vt() == VT_R8.0).then(|| unsafe { self.0.Anonymous.Anonymous.Anonymous.dblVal })
    }

    pub fn as_i32(&self) -> Option<i32> {
        // SAFETY: `lVal` is the active union arm when vt == VT_I4.
        (self.vt() == VT_I4.0).then(|| unsafe { self.0.Anonymous.Anonymous.Anonymous.lVal })
    }

    pub fn as_string(&self) -> Option<String> {
        // SAFETY: `bstrVal` is the active union arm when vt == VT_BSTR; we only borrow it and
        // VariantClear frees it in Drop.
        (self.vt() == VT_BSTR.0)
            .then(|| unsafe { self.0.Anonymous.Anonymous.Anonymous.bstrVal.to_string() })
    }
}

impl Drop for OwnedVariant {
    fn drop(&mut self) {
        // SAFETY: the VARIANT was produced by UIA and is owned solely by this wrapper.
        let _ = unsafe { VariantClear(&mut self.0) };
    }
}

/// Converts and destroys a one-dimensional `VT_I4` SAFEARRAY returned by `GetRuntimeId`.
///
/// # Safety
/// `psa` must be null or a SAFEARRAY of i32 owned by the caller; it is destroyed here.
pub unsafe fn take_i32_safearray(psa: *mut SAFEARRAY) -> Vec<i32> {
    if psa.is_null() {
        return Vec::new();
    }
    let mut out = Vec::new();
    // SAFETY: `psa` is a valid owned SAFEARRAY per the contract. We access its data between
    // SafeArrayAccessData/Unaccess and destroy it exactly once.
    unsafe {
        if SafeArrayGetDim(psa) == 1
            && let (Ok(lo), Ok(hi)) = (SafeArrayGetLBound(psa, 1), SafeArrayGetUBound(psa, 1))
            && hi >= lo
        {
            let mut data: *mut c_void = std::ptr::null_mut();
            if SafeArrayAccessData(psa, &mut data).is_ok() && !data.is_null() {
                let len = (hi - lo + 1) as usize;
                out.extend_from_slice(std::slice::from_raw_parts(data as *const i32, len));
                let _ = SafeArrayUnaccessData(psa);
            }
        }
        let _ = SafeArrayDestroy(psa);
    }
    out
}
