use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::core::PWSTR;

/// Executable file name for `pid` (`notepad.exe`), or `None` if the process is gone or
/// protected. Uses the limited-information right so it works on most other-user processes.
pub fn process_name(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    // SAFETY: OpenProcess has no pointer arguments; the handle is closed below on every path.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: `buf` outlives the call and `len` holds its capacity in u16 units, as required.
    let result = unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    };
    // SAFETY: `handle` was returned by OpenProcess above and is not used afterwards.
    let _ = unsafe { CloseHandle(handle) };
    result.ok()?;
    let path = String::from_utf16_lossy(&buf[..len as usize]);
    path.rsplit('\\').next().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_process_name_resolves() {
        let name = process_name(std::process::id()).expect("own process is queryable");
        assert!(name.to_lowercase().ends_with(".exe"), "{name}");
    }

    #[test]
    fn pid_zero_is_none() {
        assert_eq!(process_name(0), None);
    }
}
