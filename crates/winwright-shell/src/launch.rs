//! `launch`: URIs, folders, and documents open through `ShellExecuteExW("open")`; executables
//! start through `CreateProcessW` with an explicit image path and a quoted argument vector,
//! never through `cmd.exe`. No elevation verb exists anywhere in this module.
//!
//! What the shell opens is allowlisted: a few URI schemes and document, image and media
//! types. Anything else (custom protocol handlers, shortcuts, scripts, installers) is started
//! by naming the program, with the file as an argument, so policy sees the real program.
//! Executables only run from local disks.

use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_BAD_EXE_FORMAT, ERROR_CANCELLED, ERROR_ELEVATION_REQUIRED,
    ERROR_FILE_NOT_FOUND, ERROR_NO_ASSOCIATION, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS,
};
use windows::Win32::Storage::FileSystem::GetDriveTypeW;
use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
};
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW,
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

/// Files the shell opens with their default handler: documents, images, and media. Anything
/// else (shortcuts that point anywhere, scripts, installers, unknown types) would run code the
/// policy never saw; starting the program with the file as an argument is the route for those.
const DOCUMENT_EXTENSIONS: &[&str] = &[
    "avi", "bmp", "csv", "doc", "docx", "flac", "gif", "heic", "htm", "html", "ico", "jpeg", "jpg",
    "json", "log", "m4a", "md", "mkv", "mov", "mp3", "mp4", "odp", "ods", "odt", "ogg", "pdf",
    "png", "ppt", "pptx", "rtf", "svg", "tif", "tiff", "tsv", "txt", "wav", "webm", "webp", "wma",
    "wmv", "xls", "xlsx", "xml", "yaml", "yml", "zip",
];

/// URI schemes the shell opens. Custom protocol handlers are a long-running source of
/// remote-code-execution bugs, and `file:` would skip the file checks, so the rest are refused.
const ALLOWED_SCHEMES: &[&str] = &["http", "https", "mailto", "ms-settings", "shell"];

/// `GetDriveTypeW` result for a mapped network drive.
const DRIVE_REMOTE: u32 = 4;

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
    /// What will be started, in full, for the person approving it: the program path, or the
    /// URI, folder or file the shell opens.
    pub(crate) fn target_text(&self) -> String {
        match &self.target {
            Target::Process(path) => path.display().to_string(),
            Target::Shell(target) => target.to_string_lossy().into_owned(),
        }
    }

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
                    window: None,
                })
            }
            Target::Shell(target) => {
                let pid = shell_open(&target, self.working_dir.as_deref())?;
                Ok(LaunchResult {
                    process_id: pid,
                    method: "shell".to_owned(),
                    window: None,
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
        let lower = scheme.to_ascii_lowercase();
        // `shell:Downloads` names a folder; `shell:::{CLSID}` and `shell:Desktop\x.lnk` can
        // reach anything.
        let rest = &app[scheme.len() + 1..];
        let plain_folder = rest.chars().all(|c| c.is_ascii_alphanumeric() || c == ' ');
        if !ALLOWED_SCHEMES.contains(&lower.as_str()) || (lower == "shell" && !plain_folder) {
            return Err(WinwrightError::ActionBlocked {
                reason: format!(
                    "`{scheme}:` URIs are not opened (allowed: http, https, mailto, ms-settings, \
                     shell:<folder>); launch the app itself instead"
                ),
            });
        }
        return Ok(Target::Shell(OsString::from(app)));
    }

    if !app.contains(['\\', '/', ':']) {
        // Bare name: `notepad`, `notepad.exe`, `msedge`, or `notes.txt` inside `working_dir`.
        check_extension(app)?;
        if let Some(image) = search_executable(app).or_else(|| app_path(app)) {
            return classify_file(image);
        }
        if let Some(local) = working_dir.map(|dir| dir.join(app))
            && local.exists()
        {
            return classify_path(local, app);
        }
        // Never left to ShellExecute: its own search tries the current folder and `.bat`/`.lnk`.
        return Err(WinwrightError::invalid(format!(
            "`{app}` was not found in System32, the Windows folder, PATH, or App Paths; pass \
             its full path"
        )));
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
    let path = std::path::absolute(&path).unwrap_or(path);
    // Refused before touching the path: even a metadata read sends a server our credentials.
    if on_network(&path) && is_executable(app) {
        return Err(remote_executable(&path));
    }
    classify_path(path, app)
}

fn classify_path(path: PathBuf, app: &str) -> WinwrightResult<Target> {
    match std::fs::metadata(&path) {
        Ok(meta) if meta.is_dir() => Ok(Target::Shell(path.into_os_string())),
        Ok(_) => classify_file(path),
        Err(_)
            if path.extension().is_none()
                && !on_network(&path)
                && path.with_extension("exe").is_file() =>
        {
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
    if is_executable(&name) {
        if on_network(&path) {
            return Err(remote_executable(&path));
        }
        Ok(Target::Process(path))
    } else {
        Ok(Target::Shell(path.into_os_string()))
    }
}

fn is_executable(name: &str) -> bool {
    extension_of(name).is_some_and(|ext| EXECUTABLE_EXTENSIONS.contains(&ext.as_str()))
}

/// Executables, documents on the allowlist, and names without an extension (folders, or
/// programs found by name) pass.
fn check_extension(name: &str) -> WinwrightResult<()> {
    match extension_of(name) {
        Some(ext)
            if !EXECUTABLE_EXTENSIONS.contains(&ext.as_str())
                && !DOCUMENT_EXTENSIONS.contains(&ext.as_str()) =>
        {
            Err(WinwrightError::ActionBlocked {
                reason: format!(
                    "`.{ext}` files are not opened with their default handler (only documents, \
                     images and media are); start the program that should open it and pass \
                     the file as an argument"
                ),
            })
        }
        _ => Ok(()),
    }
}

/// On a share (`\\server\share`, `\\?\UNC\…`, a device path) or a mapped network drive.
pub(crate) fn on_network(path: &Path) -> bool {
    use std::path::{Component, Prefix};
    let Some(Component::Prefix(prefix)) = path.components().next() else {
        return false;
    };
    match prefix.kind() {
        Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
            let root = wide(format!("{}:\\", char::from(letter)));
            // SAFETY: `root` is a NUL-terminated drive root such as `Z:\`.
            unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) == DRIVE_REMOTE }
        }
        _ => true,
    }
}

pub(crate) fn remote_executable(path: &Path) -> WinwrightError {
    WinwrightError::ActionBlocked {
        reason: format!(
            "{} is on a network share or drive; Winwright only starts programs from local disks",
            path.display()
        ),
    }
}

/// The program registered for `name` under App Paths (`msedge`, `winword`), current user
/// first: the lookup `ShellExecute` makes, without its search of the current folder.
fn app_path(name: &str) -> Option<PathBuf> {
    let key = if extension_of(name).is_some() {
        name.to_owned()
    } else {
        format!("{name}.exe")
    };
    let subkey = wide(format!(
        r"Software\Microsoft\Windows\CurrentVersion\App Paths\{key}"
    ));
    [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE]
        .into_iter()
        .find_map(|root| {
            let mut buf = vec![0u16; 1024];
            let mut bytes = (buf.len() * 2) as u32;
            // SAFETY: `subkey` is NUL-terminated; `buf` is writable for `bytes` bytes. The
            // default value is read; REG_EXPAND_SZ values come back expanded.
            let status = unsafe {
                RegGetValueW(
                    root,
                    PCWSTR(subkey.as_ptr()),
                    PCWSTR::null(),
                    RRF_RT_REG_SZ,
                    None,
                    Some(buf.as_mut_ptr().cast()),
                    Some(&mut bytes),
                )
            };
            if status != ERROR_SUCCESS {
                return None;
            }
            let units = (bytes as usize / 2).min(buf.len());
            let text = String::from_utf16_lossy(&buf[..units]);
            let text = text.trim_end_matches('\0').trim().trim_matches('"');
            (!text.is_empty()).then(|| PathBuf::from(text))
        })
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
pub(crate) fn search_executable(name: &str) -> Option<PathBuf> {
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
    fn only_allowlisted_uri_schemes_open() {
        for uri in [
            "ms-settings:display",
            "HTTPS://example.com/a",
            "mailto:someone@example.com",
            "shell:Downloads",
            "shell:Common Startup",
        ] {
            assert_eq!(
                classify(uri, None).unwrap(),
                Target::Shell(uri.into()),
                "{uri}"
            );
        }
        for uri in [
            "file:///C:/Windows/System32/calc.exe",
            "ms-msdt:/id x",
            "SEARCH-MS:query=x",
            "zoommtg://join?x",
            "vscode://file/C:/x",
            "shell:::{2559a1f3-21d7-11d4-bdaf-00c04f60b9f0}",
            r"shell:Desktop\run.lnk",
            r"shell:AppsFolder\Microsoft.WindowsCalculator_8wekyb3d8bbwe!App",
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
    fn bare_names_resolve_through_app_paths_and_never_through_the_shell() {
        // Edge is not on PATH; its App Paths entry names its executable.
        match classify("msedge", None).unwrap() {
            Target::Process(path) => assert!(
                path.to_string_lossy()
                    .to_lowercase()
                    .ends_with("msedge.exe"),
                "{}",
                path.display()
            ),
            other => panic!("msedge classified as {other:?}"),
        }
        assert_eq!(
            classify("winwright-no-such-app-4711", None)
                .unwrap_err()
                .code(),
            ErrorCode::InvalidRequest
        );
    }

    #[test]
    fn executables_on_shares_are_refused_without_touching_them() {
        for app in [
            r"\\winwright-no-such-host\share\tool.exe",
            r"\\?\UNC\winwright-no-such-host\share\tool.EXE",
            "//winwright-no-such-host/share/tool.com",
            r"\\.\winwright-no-such-device\tool.exe",
        ] {
            let started = std::time::Instant::now();
            let err = classify(app, None).unwrap_err();
            assert_eq!(err.code(), ErrorCode::ActionBlocked, "{app}: {err}");
            // A lookup of the host would take seconds.
            assert!(
                started.elapsed() < std::time::Duration::from_millis(500),
                "{app}"
            );
        }
    }

    #[test]
    fn paths_classify_by_kind_and_extension() {
        let scratch = Scratch::new("classify");
        let doc = scratch.0.join("notes.txt");
        let script = scratch.0.join("run.bat");
        let installer = scratch.0.join("setup.MSI");
        let shortcut = scratch.0.join("app.lnk");
        let web_shortcut = scratch.0.join("site.url");
        let unknown = scratch.0.join("data.xyz");
        for file in [
            &doc,
            &script,
            &installer,
            &shortcut,
            &web_shortcut,
            &unknown,
        ] {
            std::fs::write(file, b"x").unwrap();
        }

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
        for blocked in [&script, &installer, &shortcut, &web_shortcut, &unknown] {
            let err = classify(blocked.to_str().unwrap(), None).unwrap_err();
            assert_eq!(
                err.code(),
                ErrorCode::ActionBlocked,
                "{}",
                blocked.display()
            );
        }
        assert_eq!(
            classify("app.lnk", Some(&scratch.0)).unwrap_err().code(),
            ErrorCode::ActionBlocked
        );
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
