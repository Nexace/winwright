//! Process listing from a Toolhelp snapshot, enriched with session, image path, and integrity
//! level where the limited-information access right allows.

use windows::Win32::Foundation::HANDLE;
use windows::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, IsValidSid,
    TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TokenIntegrityLevel,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::{
    OpenProcess, OpenProcessToken, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};
use windows::core::PWSTR;
use winwright_contracts::WinwrightResult;
use winwright_contracts::system::ProcessInfo;

use crate::handle::OwnedHandle;
use crate::platform;

/// Reported when `ProcessIdToSessionId` is denied: never equal to a real interactive session.
pub const UNKNOWN_SESSION: u32 = u32::MAX;

/// Long-path capacity for `QueryFullProcessImageNameW`, in UTF-16 units.
const IMAGE_PATH_CAPACITY: usize = 32_768;

pub(crate) fn list() -> WinwrightResult<Vec<ProcessInfo>> {
    // SAFETY: no pointer arguments; the snapshot handle is owned and closed by OwnedHandle.
    let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
        .map_err(|e| platform("CreateToolhelp32Snapshot", &e))?;
    let snapshot = OwnedHandle(raw);
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut image_buf = vec![0u16; IMAGE_PATH_CAPACITY];
    let mut out = Vec::with_capacity(256);

    // SAFETY: `snapshot` is a live Toolhelp snapshot and `entry.dwSize` is initialized.
    unsafe { Process32FirstW(snapshot.0, &mut entry) }
        .map_err(|e| platform("Process32FirstW", &e))?;
    loop {
        out.push(describe(&entry, &mut image_buf));
        // SAFETY: as above; ERROR_NO_MORE_FILES ends the walk.
        if unsafe { Process32NextW(snapshot.0, &mut entry) }.is_err() {
            break;
        }
    }
    Ok(out)
}

fn describe(entry: &PROCESSENTRY32W, image_buf: &mut [u16]) -> ProcessInfo {
    let pid = entry.th32ProcessID;
    let name_len = entry
        .szExeFile
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(entry.szExeFile.len());
    let (path, integrity) = match open_limited(pid) {
        Some(process) => (
            image_path(&process, image_buf),
            integrity_rid(process.0).map(|rid| integrity_name(rid).to_owned()),
        ),
        None => (None, None),
    };
    ProcessInfo {
        process_id: pid,
        parent_process_id: entry.th32ParentProcessID,
        name: String::from_utf16_lossy(&entry.szExeFile[..name_len]),
        path,
        session_id: session_of(pid),
        integrity,
    }
}

fn open_limited(pid: u32) -> Option<OwnedHandle> {
    if pid == 0 {
        return None;
    }
    // SAFETY: no pointer arguments; the handle is owned and closed by OwnedHandle.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    OwnedHandle::new(handle)
}

fn session_of(pid: u32) -> u32 {
    let mut session = 0u32;
    // SAFETY: `session` is a valid out pointer for the duration of the call.
    match unsafe { ProcessIdToSessionId(pid, &mut session) } {
        Ok(()) => session,
        Err(_) => UNKNOWN_SESSION,
    }
}

fn image_path(process: &OwnedHandle, buf: &mut [u16]) -> Option<String> {
    let mut len = buf.len() as u32;
    // SAFETY: `buf` is writable for `len` UTF-16 units and outlives the call; `process` is
    // live with PROCESS_QUERY_LIMITED_INFORMATION.
    unsafe {
        QueryFullProcessImageNameW(
            process.0,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    }
    .ok()?;
    Some(String::from_utf16_lossy(&buf[..len as usize]))
}

/// Mandatory-label RID of the process token (`SECURITY_MANDATORY_*_RID`), if queryable.
fn integrity_rid(process: HANDLE) -> Option<u32> {
    let mut raw = HANDLE::default();
    // SAFETY: `process` is live with PROCESS_QUERY_LIMITED_INFORMATION, which TOKEN_QUERY
    // needs; `raw` is a valid out pointer and is owned by OwnedHandle afterwards.
    unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut raw) }.ok()?;
    let token = OwnedHandle::new(raw)?;

    let mut len = 0u32;
    // SAFETY: size query with no buffer; it fails with ERROR_INSUFFICIENT_BUFFER and sets `len`.
    let _ = unsafe { GetTokenInformation(token.0, TokenIntegrityLevel, None, 0, &mut len) };
    if (len as usize) < size_of::<TOKEN_MANDATORY_LABEL>() {
        return None;
    }
    // u64 storage keeps the pointer-bearing TOKEN_MANDATORY_LABEL correctly aligned.
    let mut buf = vec![0u64; (len as usize).div_ceil(size_of::<u64>())];
    // SAFETY: `buf` is writable for at least `len` bytes and outlives the call.
    unsafe {
        GetTokenInformation(
            token.0,
            TokenIntegrityLevel,
            Some(buf.as_mut_ptr().cast()),
            len,
            &mut len,
        )
    }
    .ok()?;
    // SAFETY: the call succeeded, so `buf` starts with an initialized, aligned
    // TOKEN_MANDATORY_LABEL whose SID points into `buf`, which is still alive.
    let sid = unsafe { (*buf.as_ptr().cast::<TOKEN_MANDATORY_LABEL>()).Label.Sid };
    // SAFETY: `sid` points into `buf`; IsValidSid only reads it.
    if !unsafe { IsValidSid(sid) }.as_bool() {
        return None;
    }
    // SAFETY: `sid` is a valid SID, so its sub-authority count is readable.
    let count = unsafe { *GetSidSubAuthorityCount(sid) };
    let last = u32::from(count.checked_sub(1)?);
    // SAFETY: `last` is below the SID's sub-authority count.
    Some(unsafe { *GetSidSubAuthority(sid, last) })
}

/// Maps a mandatory-label RID onto the contract's integrity names.
pub(crate) fn integrity_name(rid: u32) -> &'static str {
    match rid {
        0..0x2000 => "low",
        0x2000..0x3000 => "medium",
        0x3000..0x4000 => "high",
        _ => "system",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integrity_levels_map_by_rid_band() {
        assert_eq!(integrity_name(0x0000), "low"); // untrusted
        assert_eq!(integrity_name(0x1000), "low");
        assert_eq!(integrity_name(0x2000), "medium");
        assert_eq!(integrity_name(0x2100), "medium"); // medium-plus
        assert_eq!(integrity_name(0x3000), "high");
        assert_eq!(integrity_name(0x4000), "system");
        assert_eq!(integrity_name(0x5000), "system"); // protected process
    }

    #[test]
    fn own_process_is_listed_with_details() {
        let own = std::process::id();
        let list = list().unwrap();
        let me = list.iter().find(|p| p.process_id == own).expect("own pid");
        assert!(me.name.to_lowercase().ends_with(".exe"), "{}", me.name);
        assert!(me.path.as_deref().is_some_and(|p| p.ends_with(&me.name)));
        assert!(matches!(me.integrity.as_deref(), Some("medium" | "high")));
        assert_ne!(me.session_id, UNKNOWN_SESSION);
        assert!(list.iter().any(|p| p.process_id == 4), "System process");
    }
}
