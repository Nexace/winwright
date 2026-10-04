//! The clock in the two forms reports need: UTC for the record, local time for file names.

use windows::Win32::System::SystemInformation::{GetLocalTime, GetSystemTime};

pub struct Now {
    /// `2026-10-04T08:48:19Z`.
    pub utc: String,
    /// `2026-10-04-141819` (local time, so file names read as the person's day).
    pub local_stamp: String,
}

pub fn now() -> Now {
    // SAFETY: both only fill and return a SYSTEMTIME.
    let (utc, local) = unsafe { (GetSystemTime(), GetLocalTime()) };
    Now {
        utc: format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            utc.wYear, utc.wMonth, utc.wDay, utc.wHour, utc.wMinute, utc.wSecond
        ),
        local_stamp: format!(
            "{:04}-{:02}-{:02}-{:02}{:02}{:02}",
            local.wYear, local.wMonth, local.wDay, local.wHour, local.wMinute, local.wSecond
        ),
    }
}
