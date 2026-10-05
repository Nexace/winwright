//! Start menu shortcuts: `app_launch("Discord")` finds the shortcut named Discord (see
//! `names`) and reads the program, arguments and folder it starts. The shortcut itself is never opened: the program it
//! names is launched and judged like any other, so one that hides a command line still asks.

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Storage::FileSystem::WIN32_FIND_DATAW;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoUninitialize, IPersistFile, STGM_READ,
};
use windows::Win32::UI::Shell::{CommandLineToArgvW, IShellLinkW, ShellLink};
use windows::core::{Interface, PCWSTR};
use winwright_contracts::{WinwrightError, WinwrightResult};

use crate::handle::wide;

/// What a shortcut starts.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Shortcut {
    pub(crate) target: PathBuf,
    pub(crate) args: Vec<String>,
    pub(crate) dir: Option<PathBuf>,
}

/// The Start menu's program folders: this user's first, then everyone's.
fn start_menus() -> Vec<PathBuf> {
    let programs = |base: &str| {
        std::env::var_os(base)
            .map(|b| PathBuf::from(b).join(r"Microsoft\Windows\Start Menu\Programs"))
    };
    [programs("APPDATA"), programs("ProgramData")]
        .into_iter()
        .flatten()
        .collect()
}

fn collect(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(t) if t.is_dir() && depth > 0 => collect(&path, depth - 1, out),
            Ok(t)
                if t.is_file()
                    && path
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("lnk")) =>
            {
                out.push(path);
            }
            _ => {}
        }
    }
}

/// Every Start menu shortcut, this user's first, with the name the Start menu shows.
pub(crate) fn links() -> Vec<(String, PathBuf)> {
    let mut found = Vec::new();
    for root in start_menus() {
        collect(&root, 3, &mut found);
    }
    found
        .into_iter()
        .map(|p| {
            let name = p
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            (name, p)
        })
        .collect()
}

/// Splits a shortcut's argument string the way the program will see it.
pub(crate) fn split_args(args: &str) -> Vec<String> {
    if args.trim().is_empty() {
        return Vec::new();
    }
    // CommandLineToArgvW treats its first word as the program name.
    let line = wide(format!("x {args}"));
    let mut count = 0;
    // SAFETY: `line` is NUL-terminated; the returned array is freed below.
    let argv = unsafe { CommandLineToArgvW(PCWSTR(line.as_ptr()), &mut count) };
    if argv.is_null() {
        return Vec::new();
    }
    let out = (1..count.max(0) as usize)
        // SAFETY: `argv` holds `count` valid NUL-terminated strings.
        .map(|i| unsafe { (*argv.add(i)).to_string() }.unwrap_or_default())
        .collect();
    // SAFETY: `argv` came from CommandLineToArgvW and is freed once.
    let _ = unsafe { LocalFree(Some(HLOCAL(argv.cast()))) };
    out
}

fn text(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// Reads a shortcut through the shell's own parser, on a thread of its own (COM apartment).
pub(crate) fn read(link: &Path) -> WinwrightResult<Shortcut> {
    let path = wide(link.as_os_str());
    let label = link.display().to_string();
    let read = std::thread::spawn(move || {
        // SAFETY: COM is initialised for this short-lived thread and released below.
        let init = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        let result = (|| -> windows::core::Result<(String, String, String)> {
            // SAFETY: plain COM calls on interfaces owned by this thread; buffers outlive them.
            unsafe {
                let shell_link: IShellLinkW =
                    CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
                shell_link
                    .cast::<IPersistFile>()?
                    .Load(PCWSTR(path.as_ptr()), STGM_READ)?;
                let mut target = vec![0u16; 1024];
                let mut find = WIN32_FIND_DATAW::default();
                shell_link.GetPath(&mut target, &mut find, 0)?;
                let mut args = vec![0u16; 4096];
                shell_link.GetArguments(&mut args)?;
                let mut dir = vec![0u16; 1024];
                shell_link.GetWorkingDirectory(&mut dir)?;
                Ok((text(&target), text(&args), text(&dir)))
            }
        })();
        if init.is_ok() {
            // SAFETY: balances the successful CoInitializeEx above.
            unsafe { CoUninitialize() };
        }
        result
    })
    .join()
    .map_err(|_| WinwrightError::invalid(format!("cannot read the shortcut {label}")))?
    .map_err(|e| {
        WinwrightError::invalid(format!("cannot read the shortcut {label}: {}", e.message()))
    })?;
    let (target, args, dir) = read;
    if target.is_empty() {
        return Err(WinwrightError::invalid(format!(
            "the shortcut {label} names no program (an installer-managed shortcut); launch the \
             program by its file name instead"
        )));
    }
    Ok(Shortcut {
        target: PathBuf::from(target),
        args: split_args(&args),
        dir: (!dir.is_empty())
            .then(|| PathBuf::from(dir))
            .filter(|d| d.is_dir()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_arguments_split_like_a_command_line() {
        assert_eq!(
            split_args("--processStart Discord.exe"),
            ["--processStart", "Discord.exe"]
        );
        assert_eq!(split_args(r#"-a "two words" c"#), ["-a", "two words", "c"]);
        assert!(split_args("  ").is_empty());
    }
}
