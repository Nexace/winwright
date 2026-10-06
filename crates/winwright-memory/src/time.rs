//! The clock in the two forms reports need: UTC for the record, local time for file names.

use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows::Win32::System::SystemInformation::{GetLocalTime, GetSystemTime};
use windows::Win32::System::Time::{
    FileTimeToSystemTime, SystemTimeToFileTime, SystemTimeToTzSpecificLocalTime,
};

/// 1601-01-01 to 1970-01-01 in FILETIME's 100 ns ticks.
const UNIX_EPOCH_TICKS: u64 = 116_444_736_000_000_000;

/// Unix epoch milliseconds moved into this PC's time zone, with daylight saving as it was on
/// that day; unchanged when Windows cannot convert them.
pub fn local_ms(utc_ms: u64) -> u64 {
    let ticks = utc_ms
        .saturating_mul(10_000)
        .saturating_add(UNIX_EPOCH_TICKS);
    let utc_file = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    let (mut utc, mut local, mut local_file) = (
        SYSTEMTIME::default(),
        SYSTEMTIME::default(),
        FILETIME::default(),
    );
    // SAFETY: each call reads and fills the local structs passed, which outlive it.
    let converted = unsafe {
        FileTimeToSystemTime(&utc_file, &mut utc).is_ok()
            && SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).is_ok()
            && SystemTimeToFileTime(&local, &mut local_file).is_ok()
    };
    if !converted {
        return utc_ms;
    }
    let ticks = (u64::from(local_file.dwHighDateTime) << 32) | u64::from(local_file.dwLowDateTime);
    ticks.saturating_sub(UNIX_EPOCH_TICKS) / 10_000
}

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

#[cfg(test)]
mod tests {
    #[test]
    fn local_time_is_within_a_day_of_utc() {
        let utc = 1_791_244_800_000;
        let local = super::local_ms(utc);
        assert!(local.abs_diff(utc) <= 14 * 3_600_000, "{local} vs {utc}");
        assert_eq!(local % 60_000, 0, "zones move whole minutes");
    }
}
