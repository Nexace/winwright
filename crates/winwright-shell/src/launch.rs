//! `launch`: URIs, folders, and documents open through `ShellExecuteExW("open")`; executables
//! start through `CreateProcessW` with an explicit image path and a quoted argument vector,
//! never through `cmd.exe`. No elevation verb exists anywhere in this module.

use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_BAD_EXE_FORMAT, ERROR_CANCELLED, ERROR_ELEVATION_REQUIRED,
    ERROR_FILE_NOT_FOUND, ERROR_NO_ASSOCIATION, ERROR_PATH_NOT_FOUND,
};
use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
};
use windows::Win32::System::SystemInformation::{GetSystemDirectoryW, GetWindowsDirectoryW};
use windows::Win32::System::Threading::{
    CREATE_NEW_CONSOLE, CreateProcessW, GetProcessId, PROCESS_INFORMATION, STARTUPINFOW,
};
use windows::Win32::UI::Shell::{
    SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    ShellExecuteExW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{PCWSTR, PWSTR};
use winwright_contracts::system::{LaunchRequest, LaunchResult};
use winwright_contracts::{WinwrightError, WinwrightResult};

use crate::handle::{OwnedHandle, wide};
use crate::{platform, reject_nul};

/// Image types `CreateProcessW` runs directly.
const EXECUTABLE_EXTENSIONS: &[&str] = &["exe", "com"];

/// Files that run code through a script host, `cmd.exe`, or an installer when "opened".
/// Launching them would bypass the argument-vector guarantee; `exec` with an explicit
/// interpreter (behind policy) is the supported route.
const SCRIPT_EXTENSIONS: &[&str] = &[
    "appinstaller",
    "application",
    "appref-ms",
    "appx",
    "appxbundle",
    "bat",
    "chm",
    "cmd",
    "cpl",
    "diagcab",
    "hta",
    "jar",
    "js",
    "jse",
    "msi",
    "msix",
    "msixbundle",
    "msp",
    "pif",
    "ps1",
    "psc1",
    "psm1",
    "py",
    "pyw",
    "pyz",
    "pyzw",
    "reg",
    "scr",
    "settingcontent-ms",
    "vb",
    "vbe",
    "vbs",
    "ws",
    "wsf",
    "wsh",
];

/// URI schemes that execute local content or have a history of remote-code-execution abuse.
/// `file:` is refused so paths always go through the file classification above.
const BLOCKED_SCHEMES: &[&str] = &[
    "file",
    "hcp",
    "its",
    "javascript",
    "mk",
    "ms-appinstaller",
    "ms-its",
    "ms-msdt",
    "ms-officecmd",
    "search",
    "search-ms",
    "vbscript",
];

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Target {
    /// `ShellExecuteExW("open")`: a URI, folder, document, or a name registered under App Paths.
    Shell(OsString),
    /// `CreateProcessW` with this image path.
    Process(PathBuf),
}

/// A validated launch: nothing has touched the desktop yet.
#[derive(Debug)]
pub(crate) struct Plan {
    pub(crate) target: Target,
    args: Vec<String>,
    working_dir: Option<PathBuf>,
}

impl Plan {
    pub(crate) fn kind(&self) -> &'static str {
        match self.target {
            Target::Shell(_) => "shell",
            Target::Process(_) => "process",
        }
    }

    /// Log-safe label: a URI scheme or a file name, never a full URI or path (either can carry
    /// tokens or user names).
    pub(crate) fn log_label(&self) -> String {
        match &self.target {
            Target::Process(path) => file_name(path),
            Target::Shell(target) => {
                let text = target.to_string_lossy();
                match uri_scheme(&text) {
                    Some(scheme) => format!("{scheme}:"),
                    None => file_name(Path::new(&*text)),
                }
            }
        }
    }

    /// Blocking: runs on a `spawn_blocking` thread.
    pub(crate) fn run(self) -> WinwrightResult<LaunchResult> {
        match self.target {
            Target::Process(image) => {
                let pid = create_process(&image, &self.args, self.working_dir.as_deref())?;
                Ok(LaunchResult {
                    process_id: Some(pid),
                    method: "process".to_owned(),
                })
            }
            Target::Shell(target) => {
                let pid = shell_open(&target, self.working_dir.as_deref())?;
                Ok(LaunchResult {
                    process_id: pid,
                    method: "shell".to_owned(),
                })
            }
        }
    }
}

/// Validates the request and decides how it would be launched, without launching anything.
pub(crate) fn plan(request: &LaunchRequest) -> WinwrightResult<Plan> {
    let app = request.app.trim();
    if app.is_empty() {
        return Err(WinwrightError::invalid("app is empty"));
    }
    reject_nul("app", app)?;
    for arg in &request.args {
        reject_nul("launch arguments", arg)?;
    }
    if let Some(dir) = &request.working_dir {
        if dir.as_os_str().encode_wide().any(|unit| unit == 0) {
            return Err(WinwrightError::invalid(
                "workingDir must not contain NUL characters",
            ));
        }
        if !dir.is_absolute() || !dir.is_dir() {
            return Err(WinwrightError::invalid(format!(
                "workingDir {} is not an existing absolute folder",
                dir.display()
            )));
        }
    }
    if requests_elevation(app, &request.args) {
        return Err(WinwrightError::ActionBlocked {
            reason: "Winwright never requests elevation (runas); start elevated tools yourself"
                .to_owned(),
        });
    }
    let target = classify(app, request.working_dir.as_deref())?;
    if matches!(target, Target::Shell(_)) && !request.args.is_empty() {
        return Err(WinwrightError::invalid(format!(
            "arguments can only be passed to an executable; `{app}` opens with its default handler"
        )));
    }
    Ok(Plan {
        target,
        args: request.args.clone(),
        working_dir: request.working_dir.clone(),
    })
}

/// `scheme` of `scheme:rest` when it is a URI: at least two characters (so `C:\…` is a drive,
/// not a scheme), starting with a letter, then letters, digits, `+`, `-`, or `.`.
pub(crate) fn uri_scheme(app: &str) -> Option<&str> {
    let (scheme, _) = app.split_once(':')?;
    let mut chars = scheme.chars();
    let first = chars.next()?;
    let valid = scheme.len() >= 2
        && first.is_ascii_alphabetic()
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    valid.then_some(scheme)
}

pub(crate) fn classify(app: &str, working_dir: Option<&Path>) -> WinwrightResult<Target> {
    if let Some(scheme) = uri_scheme(app) {
        if BLOCKED_SCHEMES.contains(&scheme.to_ascii_lowercase().as_str()) {
            return Err(WinwrightError::ActionBlocked {
                reason: format!("`{scheme}:` URIs are not launched; pass a folder or file path"),
            });
        }
        return Ok(Target::Shell(OsString::from(app)));
    }

    if !app.contains(['\\', '/', ':']) {
        // Bare name: `notepad`, `notepad.exe`, `calc`, or `notes.txt` inside `working_dir`.
        check_extension(app)?;
        if let Some(image) = search_executable(app) {
            return classify_file(image);
        }
        if let Some(local) = working_dir.map(|dir| dir.join(app))
            && local.exists()
        {
            return classify_path(local, app);
        }
        // Not found: let the shell resolve App Paths (`msedge`, `winword`).
        return Ok(Target::Shell(OsString::from(app)));
    }

    // Anything after the drive (`C:`) or verbatim-drive (`\\?\C:`) prefix that contains a
    // colon names an alternate data stream.
    let after_prefix = app.strip_prefix(r"\\?\").unwrap_or(app);
    if after_prefix.get(2..).is_some_and(|rest| rest.contains(':')) {
        return Err(WinwrightError::invalid(
            "alternate data streams (`file:stream`) cannot be launched",
        ));
    }

    let mut path = PathBuf::from(app);
    if path.is_relative()
        && let Some(dir) = working_dir
    {
        path = dir.join(path);
    }
    classify_path(path, app)
}

fn classify_path(path: PathBuf, app: &str) -> WinwrightResult<Target> {
    match std::fs::metadata(&path) {
        Ok(meta) if meta.is_dir() => Ok(Target::Shell(path.into_os_string())),
        Ok(_) => classify_file(path),
        Err(_) if path.extension().is_none() && path.with_extension("exe").is_file() => {
            classify_file(path.with_extension("exe"))
        }
        Err(_) => Err(WinwrightError::invalid(format!("`{app}` does not exist"))),
    }
}

fn classify_file(path: PathBuf) -> WinwrightResult<Target> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    check_extension(&name)?;
    match extension_of(&name) {
        Some(ext) if EXECUTABLE_EXTENSIONS.contains(&ext.as_str()) => Ok(Target::Process(path)),
        _ => Ok(Target::Shell(path.into_os_string())),
    }
}

fn check_extension(name: &str) -> WinwrightResult<()> {
    match extension_of(name) {
        Some(ext) if SCRIPT_EXTENSIONS.contains(&ext.as_str()) => {
            Err(WinwrightError::ActionBlocked {
                reason: format!(
                    "`.{ext}` files run through a script host, cmd.exe, or an installer; \
                     use exec with an explicit interpreter instead"
                ),
            })
        }
        _ => Ok(()),
    }
}

/// Lower-case extension as Windows resolves it: trailing dots and spaces are ignored, so
/// `run.bat.` is a `.bat`.
pub(crate) fn extension_of(name: &str) -> Option<String> {
    let trimmed = name.trim_end_matches(['.', ' ']);
    let (_, ext) = trimmed.rsplit_once('.')?;
    (!ext.is_empty()).then(|| ext.to_ascii_lowercase())
}

/// `runas.exe` itself, or an argument that asks a launcher for the `runas` verb
/// (`Start-Process -Verb RunAs`).
pub(crate) fn requests_elevation(app: &str, args: &[String]) -> bool {
    let name = app.rsplit(['\\', '/']).next().unwrap_or(app);
    let stem = name
        .trim_end_matches(['.', ' '])
        .split('.')
        .next()
        .unwrap_or(name);
    let runas_token = |value: &str| {
        value
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|token| token.eq_ignore_ascii_case("runas"))
    };
    stem.eq_ignore_ascii_case("runas") || args.iter().any(|arg| runas_token(arg))
}

/// System32, the Windows directory, then absolute `PATH` entries. The current directory is
/// deliberately not searched (binary planting).
fn search_executable(name: &str) -> Option<PathBuf> {
    let file = if Path::new(name).extension().is_some() {
        name.to_owned()
    } else {
        format!("{name}.exe")
    };
    let mut dirs: Vec<PathBuf> = [system_directory(), windows_directory()]
        .into_iter()
        .flatten()
        .collect();
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path).filter(|dir| dir.is_absolute()));
    }
    dirs.into_iter()
        .map(|dir| dir.join(&file))
        .find(|candidate| candidate.is_file())
}

fn system_directory() -> Option<PathBuf> {
    let mut buf = [0u16; 260];
    // SAFETY: `buf` is a writable buffer; the API writes at most its length.
    let len = unsafe { GetSystemDirectoryW(Some(&mut buf)) } as usize;
    (len > 0 && len < buf.len()).then(|| PathBuf::from(OsString::from_wide(&buf[..len])))
}

fn windows_directory() -> Option<PathBuf> {
    let mut buf = [0u16; 260];
    // SAFETY: `buf` is a writable buffer; the API writes at most its length.
    let len = unsafe { GetWindowsDirectoryW(Some(&mut buf)) } as usize;
    (len > 0 && len < buf.len()).then(|| PathBuf::from(OsString::from_wide(&buf[..len])))
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// Quotes one argument so `CommandLineToArgvW` / the MSVC CRT parse it back verbatim.
/// Backslashes are literal unless they precede a quote; those runs are doubled.
pub(crate) fn quote_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\x0b', '"']) {
        return arg.to_owned();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

/// NUL-terminated, writable command line: the quoted image path, then each quoted argument.
pub(crate) fn command_line(image: &Path, args: &[String]) -> Vec<u16> {
    let mut line: Vec<u16> = Vec::new();
    line.push(u16::from(b'"'));
    line.extend(image.as_os_str().encode_wide());
    line.push(u16::from(b'"'));
    for arg in args {
        line.push(u16::from(b' '));
        line.extend(quote_arg(arg).encode_utf16());
    }
    line.push(0);
    line
}

fn create_process(
    image: &Path,
    args: &[String],
    working_dir: Option<&Path>,
) -> WinwrightResult<u32> {
    let application = wide(image);
    let mut line = command_line(image, args);
    let directory = working_dir.map(wide);
    let startup = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut info = PROCESS_INFORMATION::default();
    // SAFETY: every string is NUL-terminated and outlives the call; `line` is the writable
    // buffer CreateProcessW requires; `startup` and `info` are valid for the whole call.
    // No handles are inherited, so the app never sees Winwright's pipes; console programs get
    // their own console instead of sharing (and corrupting) Winwright's stdio.
    unsafe {
        CreateProcessW(
            PCWSTR(application.as_ptr()),
            Some(PWSTR(line.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_NEW_CONSOLE,
            None,
            directory
                .as_ref()
                .map_or(PCWSTR::null(), |dir| PCWSTR(dir.as_ptr())),
            &startup,
            &mut info,
        )
    }
    .map_err(|e| create_process_error(image, &e))?;
    let _thread = OwnedHandle::new(info.hThread);
    let _process = OwnedHandle::new(info.hProcess);
    Ok(info.dwProcessId)
}

fn create_process_error(image: &Path, err: &windows::core::Error) -> WinwrightError {
    let name = file_name(image);
    let code = err.code();
    if code == ERROR_ELEVATION_REQUIRED.to_hresult() {
        WinwrightError::ActionBlocked {
            reason: format!("{name} requires elevation; Winwright never requests elevation"),
        }
    } else if code == ERROR_FILE_NOT_FOUND.to_hresult() || code == ERROR_PATH_NOT_FOUND.to_hresult()
    {
        WinwrightError::invalid(format!("{name} was not found"))
    } else if code == ERROR_BAD_EXE_FORMAT.to_hresult() {
        WinwrightError::invalid(format!("{name} is not a valid Windows executable"))
    } else {
        platform("CreateProcessW", err)
    }
}

/// Balanced single-threaded apartment for shell execution on a blocking-pool thread.
struct StaGuard(bool);

impl StaGuard {
    fn enter() -> Self {
        // SAFETY: plain apartment initialization for the current thread; Drop balances it only
        // when it succeeded (S_OK or S_FALSE). A thread already in the MTA keeps its apartment.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        Self(hr.is_ok())
    }
}

impl Drop for StaGuard {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balances the successful CoInitializeEx in `enter` on the same thread.
            unsafe { CoUninitialize() };
        }
    }
}

fn shell_open(target: &OsStr, working_dir: Option<&Path>) -> WinwrightResult<Option<u32>> {
    let _apartment = StaGuard::enter();
    let verb = wide("open");
    let file = wide(target);
    let directory = working_dir.map(wide);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI | SEE_MASK_NOASYNC,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpDirectory: directory
            .as_ref()
            .map_or(PCWSTR::null(), |dir| PCWSTR(dir.as_ptr())),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: `info` is zero-initialized with cbSize set; the verb, file, and directory strings
    // are NUL-terminated and outlive the call; lpParameters stays null.
    unsafe { ShellExecuteExW(&mut info) }.map_err(|e| shell_error(&e))?;
    let Some(process) = OwnedHandle::new(info.hProcess) else {
        return Ok(None);
    };
    // SAFETY: `process` is the live handle ShellExecuteExW returned for SEE_MASK_NOCLOSEPROCESS;
    // it is closed when `process` drops.
    let pid = unsafe { GetProcessId(process.0) };
    Ok((pid != 0).then_some(pid))
}

fn shell_error(err: &windows::core::Error) -> WinwrightError {
    let code = err.code();
    if code == ERROR_FILE_NOT_FOUND.to_hresult() || code == ERROR_PATH_NOT_FOUND.to_hresult() {
        WinwrightError::invalid("the app, file, or URI handler was not found")
    } else if code == ERROR_NO_ASSOCIATION.to_hresult() {
        WinwrightError::invalid("no application is associated with this file type or URI scheme")
    } else if code == ERROR_CANCELLED.to_hresult() || code == ERROR_ACCESS_DENIED.to_hresult() {
        WinwrightError::ActionBlocked {
            reason: "the launch was refused or cancelled (for example, a declined UAC prompt)"
                .to_owned(),
        }
    } else {
        platform("ShellExecuteExW", err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::UI::Shell::CommandLineToArgvW;
    use winwright_contracts::ErrorCode;

    fn request(app: &str, args: &[&str]) -> LaunchRequest {
        LaunchRequest {
            app: app.to_owned(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            working_dir: None,
        }
    }

    fn code(result: WinwrightResult<Plan>) -> ErrorCode {
        result.expect_err("plan must be refused").code()
    }

    /// Unique scratch folder under %TEMP%, removed on drop.
    struct Scratch(PathBuf);
    impl Scratch {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir = std::env::temp_dir().join(format!(
                "winwright-shell-{tag}-{}-{nanos}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn uri_detection_distinguishes_schemes_from_drives() {
        assert_eq!(uri_scheme("ms-settings:display"), Some("ms-settings"));
        assert_eq!(uri_scheme("https://example.com/a:b"), Some("https"));
        assert_eq!(uri_scheme("mailto:someone@example.com"), Some("mailto"));
        assert_eq!(uri_scheme("shell:Downloads"), Some("shell"));
        assert_eq!(uri_scheme(r"C:\Windows\notepad.exe"), None);
        assert_eq!(uri_scheme("c:relative"), None);
        assert_eq!(uri_scheme(r"\\?\C:\Windows"), None);
        assert_eq!(uri_scheme(r"\\server\share"), None);
        assert_eq!(uri_scheme("notepad.exe"), None);
        assert_eq!(uri_scheme("1password:open"), None);
        assert_eq!(uri_scheme("bad scheme:x"), None);
    }

    #[test]
    fn uris_open_through_the_shell_and_dangerous_schemes_are_blocked() {
        assert_eq!(
            classify("ms-settings:display", None).unwrap(),
            Target::Shell("ms-settings:display".into())
        );
        for uri in [
            "file:///C:/Windows/System32/calc.exe",
            "ms-msdt:/id x",
            "SEARCH-MS:query=x",
        ] {
            let err = classify(uri, None).unwrap_err();
            assert_eq!(err.code(), ErrorCode::ActionBlocked, "{uri}");
        }
    }

    #[test]
    fn bare_names_resolve_to_system_executables() {
        let system = system_directory().unwrap();
        for name in ["notepad", "notepad.exe", "NOTEPAD"] {
            match classify(name, None).unwrap() {
                Target::Process(path) => {
                    assert!(path.starts_with(&system), "{name} -> {}", path.display());
                    assert!(
                        path.to_string_lossy()
                            .to_lowercase()
                            .ends_with("notepad.exe"),
                        "{}",
                        path.display()
                    );
                }
                other => panic!("{name} classified as {other:?}"),
            }
        }
    }

    #[test]
    fn unknown_bare_names_fall_back_to_app_paths() {
        assert_eq!(
            classify("winwright-no-such-app-4711", None).unwrap(),
            Target::Shell("winwright-no-such-app-4711".into())
        );
    }

    #[test]
    fn paths_classify_by_kind_and_extension() {
        let scratch = Scratch::new("classify");
        let doc = scratch.0.join("notes.txt");
        let script = scratch.0.join("run.bat");
        let installer = scratch.0.join("setup.MSI");
        std::fs::write(&doc, b"hi").unwrap();
        std::fs::write(&script, b"@echo off").unwrap();
        std::fs::write(&installer, b"x").unwrap();

        assert_eq!(
            classify(scratch.0.to_str().unwrap(), None).unwrap(),
            Target::Shell(scratch.0.clone().into_os_string())
        );
        assert_eq!(
            classify(doc.to_str().unwrap(), None).unwrap(),
            Target::Shell(doc.clone().into_os_string())
        );
        assert_eq!(
            classify("notes.txt", Some(&scratch.0)).unwrap(),
            Target::Shell(doc.clone().into_os_string())
        );
        for blocked in [&script, &installer] {
            let err = classify(blocked.to_str().unwrap(), None).unwrap_err();
            assert_eq!(err.code(), ErrorCode::ActionBlocked);
        }
        assert_eq!(
            classify("run.cmd", None).unwrap_err().code(),
            ErrorCode::ActionBlocked
        );

        let system = system_directory().unwrap();
        let without_ext = system.join("notepad");
        assert_eq!(
            classify(without_ext.to_str().unwrap(), None).unwrap(),
            Target::Process(system.join("notepad.exe"))
        );
        let missing = scratch.0.join("missing.exe");
        assert_eq!(
            classify(missing.to_str().unwrap(), None)
                .unwrap_err()
                .code(),
            ErrorCode::InvalidRequest
        );
        let stream = format!("{}:hidden.exe", doc.display());
        assert_eq!(
            classify(&stream, None).unwrap_err().code(),
            ErrorCode::InvalidRequest
        );
    }

    #[test]
    fn extensions_ignore_trailing_dots_and_spaces() {
        assert_eq!(extension_of("run.bat. . "), Some("bat".to_owned()));
        assert_eq!(extension_of("App.EXE"), Some("exe".to_owned()));
        assert_eq!(extension_of("README"), None);
        assert_eq!(extension_of("archive."), None);
    }

    #[test]
    fn elevation_is_refused() {
        assert_eq!(code(plan(&request("runas", &[]))), ErrorCode::ActionBlocked);
        assert_eq!(
            code(plan(&request(
                r"C:\Windows\System32\RUNAS.EXE",
                &["/user:admin", "cmd"]
            ))),
            ErrorCode::ActionBlocked
        );
        assert_eq!(
            code(plan(&request(
                "powershell",
                &["Start-Process", "cmd", "-Verb", "RunAs"]
            ))),
            ErrorCode::ActionBlocked
        );
        assert!(!requests_elevation("notepad", &["runascii.txt".to_owned()]));
    }

    #[test]
    fn script_hosts_reached_by_file_association_are_blocked() {
        for name in [
            "tool.py",
            "tool.PYW",
            "app.jar",
            "help.chm",
            "fix.diagcab",
            "x.vb",
            "x.ws",
        ] {
            let err = classify(name, None).unwrap_err();
            assert_eq!(err.code(), ErrorCode::ActionBlocked, "{name}: {err}");
        }
        for uri in [
            "mk:@MSITStore:C:\\x.chm::/a.htm",
            "ms-its:x.chm::/a.htm",
            "hcp://x",
        ] {
            let err = classify(uri, None).unwrap_err();
            assert_eq!(err.code(), ErrorCode::ActionBlocked, "{uri}: {err}");
        }
    }

    #[test]
    fn nul_empty_and_bad_working_dirs_are_invalid() {
        assert_eq!(code(plan(&request("", &[]))), ErrorCode::InvalidRequest);
        assert_eq!(code(plan(&request("   ", &[]))), ErrorCode::InvalidRequest);
        assert_eq!(
            code(plan(&request("notepad\0calc", &[]))),
            ErrorCode::InvalidRequest
        );
        assert_eq!(
            code(plan(&request("notepad", &["a\0b"]))),
            ErrorCode::InvalidRequest
        );
        let mut req = request("notepad", &[]);
        req.working_dir = Some(PathBuf::from(r"C:\winwright\definitely\missing"));
        assert_eq!(code(plan(&req)), ErrorCode::InvalidRequest);
        req.working_dir = Some(PathBuf::from("relative"));
        assert_eq!(code(plan(&req)), ErrorCode::InvalidRequest);
    }

    #[test]
    fn shell_targets_take_no_arguments() {
        assert_eq!(
            code(plan(&request("ms-settings:display", &["--x"]))),
            ErrorCode::InvalidRequest
        );
        let ok = plan(&request("notepad", &["C:\\notes file.txt"])).unwrap();
        assert_eq!(ok.kind(), "process");
        assert_eq!(ok.log_label().to_lowercase(), "notepad.exe");
        let uri = plan(&request("https://example.com/?token=secret", &[])).unwrap();
        assert_eq!(uri.log_label(), "https:");
    }

    fn parse_command_line(line: &[u16]) -> Vec<String> {
        let mut argc = 0i32;
        // SAFETY: `line` is NUL-terminated; the returned block is freed with LocalFree below
        // after its `argc` entries are copied out.
        unsafe {
            let argv = CommandLineToArgvW(PCWSTR(line.as_ptr()), &mut argc);
            assert!(!argv.is_null());
            let parsed = (0..argc as usize)
                .map(|i| (*argv.add(i)).to_string().unwrap())
                .collect();
            let _ = LocalFree(Some(HLOCAL(argv.cast())));
            parsed
        }
    }

    #[test]
    fn quoting_round_trips_through_command_line_to_argv() {
        let args: Vec<String> = [
            "plain",
            "",
            "two words",
            "tab\there",
            r#"say "hi""#,
            r"C:\path\to\dir\",
            r"C:\path with space\",
            r#"back\\"quote"#,
            r"\\server\share",
            "\"",
            "trailing\\\\",
            "unicode é ✓",
            "a&b|c>d^e%PATH%",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        let image = Path::new(r"C:\Program Files\App\app.exe");
        let parsed = parse_command_line(&command_line(image, &args));
        assert_eq!(parsed[0], image.to_str().unwrap());
        assert_eq!(parsed[1..], args[..]);
    }

    #[test]
    fn simple_arguments_are_not_quoted() {
        assert_eq!(quote_arg("--flag=value"), "--flag=value");
        assert_eq!(quote_arg(r"C:\dir\file.txt"), r"C:\dir\file.txt");
        assert_eq!(quote_arg(""), r#""""#);
        assert_eq!(quote_arg(r"a b\"), r#""a b\\""#);
    }
}
