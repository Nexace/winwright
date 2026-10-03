//! Path validation, canonical resolution, and the protected-location invariant.
//!
//! Every path is absolute, lexically normalized, and free of device namespaces, reserved device
//! names, alternate data streams, and names Win32 would silently rewrite (trailing dots or
//! spaces). Existing paths are then canonicalized, so links and 8.3 names cannot dodge the
//! protected-location check.

use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::OsStringExt;
use std::path::{Component, Path, PathBuf, Prefix};

use windows::Win32::Storage::FileSystem::{FindClose, FindFirstFileW, WIN32_FIND_DATAW};
use windows::core::PCWSTR;
use winwright_contracts::{WinwrightError, WinwrightResult};

use crate::io_platform;

/// Characters Win32 rejects in names, besides separators, `:` and control characters.
const FORBIDDEN_CHARS: [char; 6] = ['<', '>', '"', '|', '?', '*'];
const MAX_NAME_UNITS: usize = 255;

/// Validates one file or folder name (a path component or a rename target).
pub(crate) fn validate_name(name: &str) -> WinwrightResult<()> {
    let invalid = |why: &str| Err(WinwrightError::invalid(format!("name `{name}` {why}")));
    if name.is_empty() {
        return Err(WinwrightError::invalid("name is empty"));
    }
    if name == "." || name == ".." {
        return invalid("is not a file name");
    }
    if name.contains(['\\', '/']) {
        return invalid("must be a plain name without folder separators");
    }
    if name.contains(':') {
        return invalid("names an alternate data stream (`name:stream`), which is not allowed");
    }
    if name.chars().any(char::is_control) {
        return invalid("contains control or NUL characters");
    }
    if name.contains(FORBIDDEN_CHARS) {
        return invalid("contains a character Windows forbids (< > \" | ? *)");
    }
    if name.ends_with(['.', ' ']) {
        return invalid("ends with a dot or space, which Windows would silently strip");
    }
    if is_reserved_device_name(name) {
        return invalid("is a reserved device name");
    }
    if name.encode_utf16().count() > MAX_NAME_UNITS {
        return invalid("is longer than 255 characters");
    }
    Ok(())
}

/// `CON`, `NUL`, `COM1`, `LPT¹`, … with or without an extension (`nul.txt`).
pub(crate) fn is_reserved_device_name(name: &str) -> bool {
    let base = name
        .split('.')
        .next()
        .unwrap_or(name)
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    if matches!(
        base.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" | "CLOCK$"
    ) {
        return true;
    }
    let mut chars = base.chars();
    let stem: String = chars.by_ref().take(3).collect();
    let rest: Vec<char> = chars.collect();
    (stem == "COM" || stem == "LPT") && matches!(rest.as_slice(), ['0'..='9' | '¹' | '²' | '³'])
}

fn not_absolute(path: &Path) -> WinwrightError {
    WinwrightError::invalid(format!(
        "{} is not an absolute path; use a full path such as C:\\Users\\me\\Documents\\file.txt \
         (the knownFolder operation resolves Desktop, Documents, Downloads, …)",
        path.display()
    ))
}

/// Lexically validates and normalizes an absolute path (`.` removed, `..` applied) without
/// touching the disk.
pub(crate) fn normalize(path: &Path) -> WinwrightResult<PathBuf> {
    let raw = path.as_os_str();
    if raw.is_empty() {
        return Err(WinwrightError::invalid("path is empty"));
    }
    if raw.as_encoded_bytes().contains(&0) {
        return Err(WinwrightError::invalid(
            "path must not contain NUL characters",
        ));
    }
    if raw.as_encoded_bytes().starts_with(br"\??\") {
        return Err(namespace_path(path));
    }

    let mut components = path.components().peekable();
    let mut out = PathBuf::new();
    match components.next() {
        Some(Component::Prefix(prefix)) => {
            let unc = match prefix.kind() {
                Prefix::Disk(_) | Prefix::VerbatimDisk(_) => false,
                Prefix::UNC(server, _) | Prefix::VerbatimUNC(server, _)
                    if !matches!(server.to_str(), Some("." | "?")) =>
                {
                    true
                }
                _ => return Err(namespace_path(path)),
            };
            out.push(prefix.as_os_str());
            if components.next_if_eq(&Component::RootDir).is_none() && !unc {
                // `C:folder` is relative to the drive's current directory.
                return Err(not_absolute(path));
            }
            out.push(Component::RootDir.as_os_str());
        }
        _ => return Err(not_absolute(path)),
    }

    let mut depth = 0usize;
    for component in components {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if depth == 0 {
                    return Err(WinwrightError::invalid(format!(
                        "{} climbs above its root with `..`",
                        path.display()
                    )));
                }
                out.pop();
                depth -= 1;
            }
            Component::Normal(name) => {
                validate_name(&name.to_string_lossy())?;
                out.push(name);
                depth += 1;
            }
            Component::Prefix(_) | Component::RootDir => return Err(not_absolute(path)),
        }
    }
    Ok(out)
}

fn namespace_path(path: &Path) -> WinwrightError {
    WinwrightError::invalid(format!(
        "{} is a device or namespace path (\\\\.\\, \\\\?\\GLOBALROOT, \\\\?\\Volume…), which is not allowed",
        path.display()
    ))
}

/// An existing path with every link followed, in canonical form.
pub(crate) fn resolve_existing(path: &Path) -> WinwrightResult<PathBuf> {
    let normalized = normalize(path)?;
    let canonical = std::fs::canonicalize(&normalized).map_err(|e| missing(&normalized, &e))?;
    // A link may point somewhere this module refuses, e.g. a device path.
    normalize(&canonical)?;
    Ok(canonical)
}

/// The directory entry `path` names, without following a final link: the canonical parent
/// joined with the entry name. Roots resolve to themselves. The entry need not exist.
pub(crate) fn resolve_entry(path: &Path) -> WinwrightResult<PathBuf> {
    let normalized = normalize(path)?;
    let (Some(parent), Some(name)) = (normalized.parent(), normalized.file_name()) else {
        return Ok(normalized);
    };
    let parent = std::fs::canonicalize(parent).map_err(|e| missing(parent, &e))?;
    normalize(&parent)?;
    Ok(parent.join(name))
}

/// [`resolve_entry`] for an entry about to be mutated in place, under the name its directory
/// stores: an 8.3 alias (`C:\Users\JOHNSM~1`) would otherwise key differently from the
/// protected long name. Missing entries and roots resolve as [`resolve_entry`] does.
pub(crate) fn resolve_stored_entry(path: &Path) -> WinwrightResult<PathBuf> {
    let entry = resolve_entry(path)?;
    Ok(match (entry.parent(), stored_name(&entry)) {
        (Some(parent), Some(name)) => parent.join(name),
        _ => entry,
    })
}

/// The name the directory stores for an existing entry, which the caller may have spelled as
/// its 8.3 short alias. Does not follow a final link. `None` when the entry does not exist.
fn stored_name(entry: &Path) -> Option<OsString> {
    let wide = crate::wide(entry);
    let mut data = WIN32_FIND_DATAW::default();
    // SAFETY: `wide` is NUL-terminated and free of wildcards (`validate_name` refuses `*`, `?`,
    // `<`, `>`, and `"`); `data` is a valid out pointer for the duration of the call.
    let handle = unsafe { FindFirstFileW(PCWSTR(wide.as_ptr()), &mut data) }.ok()?;
    // SAFETY: `handle` is the search handle opened above; it is closed exactly once.
    let _ = unsafe { FindClose(handle) };
    let len = data
        .cFileName
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(data.cFileName.len());
    (len > 0).then(|| OsString::from_wide(&data.cFileName[..len]))
}

pub(crate) fn missing(path: &Path, err: &std::io::Error) -> WinwrightError {
    if err.kind() == std::io::ErrorKind::NotFound {
        WinwrightError::invalid(format!("{} does not exist", display(path).display()))
    } else {
        io_platform("resolve path", err)
    }
}

/// User-facing form: `\\?\C:\x` → `C:\x`, `\\?\UNC\srv\share` → `\\srv\share`.
pub(crate) fn display(path: &Path) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path.to_path_buf();
    };
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\")
        && rest.as_bytes().get(1) == Some(&b':')
    {
        return PathBuf::from(rest);
    }
    path.to_path_buf()
}

/// Case-insensitive comparison key: the volume (`c:` or `\\srv\share`) then each name,
/// lower-cased. Verbatim and plain spellings of the same path produce the same key.
pub(crate) fn key(path: &Path) -> Vec<String> {
    let lower = |s: &OsStr| s.to_string_lossy().to_lowercase();
    let mut key = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => key.push(match prefix.kind() {
                Prefix::Disk(d) | Prefix::VerbatimDisk(d) => {
                    format!("{}:", char::from(d).to_ascii_lowercase())
                }
                Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
                    format!(r"\\{}\{}", lower(server), lower(share))
                }
                Prefix::Verbatim(other) | Prefix::DeviceNS(other) => {
                    format!(r"\\?\{}", lower(other))
                }
            }),
            Component::Normal(name) => key.push(lower(name)),
            Component::ParentDir => {
                key.pop();
            }
            Component::RootDir | Component::CurDir => {}
        }
    }
    key
}

/// `path` equals `base` or lies below it.
pub(crate) fn is_within(path: &[String], base: &[String]) -> bool {
    path.len() >= base.len() && path[..base.len()] == *base
}

/// Locations no write, move, rename, delete, or create may touch (a hard invariant, applied
/// before any permission policy):
/// - drive / share roots and the items directly inside them;
/// - anything at or below a protected tree (Windows, Program Files, ProgramData, Startup);
/// - anything that contains a protected tree or the profile (`C:\Users`);
/// - the user profile folder and its top-level items (`AppData`, `Desktop` itself, …).
///
/// Anything deeper inside the profile (`Desktop\report.docx`, `Downloads\x`) is allowed.
#[derive(Debug, Default)]
pub(crate) struct Protected {
    trees: Vec<(Vec<String>, PathBuf)>,
    profile: Option<(Vec<String>, PathBuf)>,
}

impl Protected {
    pub(crate) fn new(trees: impl IntoIterator<Item = PathBuf>, profile: Option<PathBuf>) -> Self {
        let resolve = |path: PathBuf| {
            let canonical = std::fs::canonicalize(&path).unwrap_or(path);
            (key(&canonical), display(&canonical))
        };
        let mut out = Self::default();
        for (tree_key, shown) in trees.into_iter().filter(|p| p.is_absolute()).map(resolve) {
            if !tree_key.is_empty() && !out.trees.iter().any(|(k, _)| *k == tree_key) {
                out.trees.push((tree_key, shown));
            }
        }
        out.profile = profile.filter(|p| p.is_absolute()).map(resolve);
        out
    }

    pub(crate) fn locations(&self) -> Vec<PathBuf> {
        self.trees
            .iter()
            .chain(&self.profile)
            .map(|(_, shown)| shown.clone())
            .collect()
    }

    /// `ActionBlocked` when mutating `target` (canonical or normalized) would touch a
    /// protected location.
    pub(crate) fn check(&self, target: &Path) -> WinwrightResult<()> {
        let target_key = key(target);
        let shown = display(target);
        let blocked = |why: String| {
            Err(WinwrightError::ActionBlocked {
                reason: format!("{} {why}", shown.display()),
            })
        };
        if target_key.len() <= 2 {
            return blocked(
                "is a drive root or directly inside one; those locations are protected".to_owned(),
            );
        }
        for (tree, tree_shown) in &self.trees {
            if is_within(&target_key, tree) {
                return blocked(format!(
                    "is inside the protected folder {}",
                    tree_shown.display()
                ));
            }
            if is_within(tree, &target_key) {
                return blocked(format!(
                    "contains the protected folder {}",
                    tree_shown.display()
                ));
            }
        }
        if let Some((profile, profile_shown)) = &self.profile {
            if is_within(profile, &target_key) {
                return blocked(format!(
                    "is or contains the user profile {}",
                    profile_shown.display()
                ));
            }
            if is_within(&target_key, profile) && target_key.len() == profile.len() + 1 {
                return blocked(format!(
                    "is a top-level item of the user profile {}; work inside Desktop, \
                     Documents, Downloads, … instead",
                    profile_shown.display()
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winwright_contracts::ErrorCode;

    fn norm(path: &str) -> WinwrightResult<PathBuf> {
        normalize(Path::new(path))
    }

    fn invalid(path: &str) {
        let err = norm(path).expect_err(path);
        assert_eq!(err.code(), ErrorCode::InvalidRequest, "{path}: {err}");
    }

    #[test]
    fn absolute_paths_normalize_lexically() {
        assert_eq!(
            norm(r"C:\Users\me\.\Documents\..\Desktop\a.txt").unwrap(),
            PathBuf::from(r"C:\Users\me\Desktop\a.txt")
        );
        assert_eq!(norm("C:/Users/me").unwrap(), PathBuf::from(r"C:\Users\me"));
        assert_eq!(norm(r"C:\").unwrap(), PathBuf::from(r"C:\"));
        assert_eq!(
            norm(r"\\?\C:\Users\me").unwrap(),
            PathBuf::from(r"\\?\C:\Users\me")
        );
        assert_eq!(
            norm(r"\\server\share\dir").unwrap(),
            PathBuf::from(r"\\server\share\dir")
        );
        assert!(norm(r"\\?\UNC\server\share\dir").is_ok());
    }

    #[test]
    fn relative_and_escaping_paths_are_rejected() {
        for path in [
            "",
            "file.txt",
            r"dir\file.txt",
            r"..\x",
            r"C:relative",
            r"\rooted",
            r"C:\..\x",
            r"C:\a\..\..\b",
        ] {
            invalid(path);
        }
    }

    #[test]
    fn device_and_namespace_paths_are_rejected() {
        for path in [
            r"\\.\PhysicalDrive0",
            r"\\.\C:\x",
            "//./pipe/x",
            r"\\?\GLOBALROOT\Device\HarddiskVolume1\x",
            r"\\?\Volume{01234567-89ab-cdef-0123-456789abcdef}\x",
            r"\??\C:\x",
            "//?/C:/x",
            r"\\?\pipe\x",
        ] {
            invalid(path);
        }
    }

    #[test]
    fn reserved_names_streams_and_bad_characters_are_rejected() {
        for path in [
            r"C:\Users\me\CON",
            r"C:\Users\me\nul.txt",
            r"C:\Users\me\Com1",
            r"C:\Users\me\LPT9.log",
            r"C:\Users\me\COM¹",
            r"C:\Users\me\conin$",
            r"C:\Users\me\file.txt:secret",
            r"C:\Users\me\file.txt::$DATA",
            r"C:\Users\me\trailing.",
            r"C:\Users\me\trailing ",
            r"C:\Users\me\a*b",
            r"C:\Users\me\a?b",
            "C:\\Users\\me\\a\u{1}b",
            "C:\\Users\\me\\a\0b",
        ] {
            invalid(path);
        }
        // Lookalikes that are ordinary names.
        for path in [
            r"C:\Users\me\console.txt",
            r"C:\Users\me\COM10",
            r"C:\Users\me\nully",
        ] {
            assert!(norm(path).is_ok(), "{path}");
        }
    }

    #[test]
    fn rename_targets_must_be_plain_names() {
        assert!(validate_name("report (final).docx").is_ok());
        assert!(validate_name(".gitignore").is_ok());
        for name in [
            "", ".", "..", r"a\b", "a/b", "x:y", "CON", "aux.txt", "end.", "end ", "a|b",
        ] {
            assert!(validate_name(name).is_err(), "{name:?}");
        }
        assert!(validate_name(&"n".repeat(256)).is_err());
    }

    #[test]
    fn display_strips_verbatim_prefixes() {
        assert_eq!(
            display(Path::new(r"\\?\C:\Users\me")),
            PathBuf::from(r"C:\Users\me")
        );
        assert_eq!(
            display(Path::new(r"\\?\UNC\srv\share\x")),
            PathBuf::from(r"\\srv\share\x")
        );
        assert_eq!(display(Path::new(r"C:\x")), PathBuf::from(r"C:\x"));
    }

    #[test]
    fn keys_ignore_case_and_verbatim_spelling() {
        assert_eq!(
            key(Path::new(r"\\?\C:\Users\Me")),
            key(Path::new(r"c:\users\me"))
        );
        assert_eq!(
            key(Path::new(r"\\?\UNC\Srv\Share\X")),
            key(Path::new(r"\\srv\share\x"))
        );
        assert_eq!(key(Path::new(r"C:\")), vec!["c:".to_owned()]);
    }

    fn fake() -> Protected {
        // Paths below Z: do not exist, so they are keyed as written.
        Protected::new(
            [
                PathBuf::from(r"Z:\Windows"),
                PathBuf::from(r"Z:\Program Files"),
                PathBuf::from(r"Z:\ProgramData"),
                PathBuf::from(
                    r"Z:\Users\me\AppData\Roaming\Microsoft\Windows\Start Menu\Programs\Startup",
                ),
            ],
            Some(PathBuf::from(r"Z:\Users\me")),
        )
    }

    fn blocked(protected: &Protected, path: &str) {
        let err = protected.check(Path::new(path)).expect_err(path);
        assert_eq!(err.code(), ErrorCode::ActionBlocked, "{path}");
    }

    #[test]
    fn protected_trees_roots_and_profile_top_level_are_blocked() {
        let protected = fake();
        for path in [
            r"Z:\",
            r"Z:\new-folder",
            r"Z:\file.txt",
            r"Z:\Windows",
            r"Z:\WINDOWS\System32\drivers\etc\hosts",
            r"\\?\Z:\windows\x.dll",
            r"Z:\Program Files\App\app.exe",
            r"Z:\ProgramData\Vendor\cfg.ini",
            r"Z:\Users",
            r"Z:\Users\me",
            r"Z:\Users\me\Desktop",
            r"Z:\Users\me\AppData",
            r"Z:\Users\me\NTUSER.DAT",
            r"Z:\Users\me\.ssh",
            r"Z:\Users\me\AppData\Roaming\Microsoft\Windows\Start Menu\Programs\Startup\x.lnk",
            r"Z:\Users\me\AppData\Roaming\Microsoft\Windows\Start Menu\Programs",
            r"\\srv\share",
            r"\\srv\share\top",
        ] {
            blocked(&protected, path);
        }
    }

    /// A fresh folder under the workspace `target\` directory, removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
                r"..\..\target\winwright-files-{tag}-{}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(std::fs::canonicalize(&dir).unwrap())
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn short_path(path: &Path) -> Option<PathBuf> {
        use windows::Win32::Storage::FileSystem::GetShortPathNameW;

        let wide = crate::wide(path);
        let mut buf = vec![0u16; 1024];
        // SAFETY: `wide` is NUL-terminated; the API writes at most `buf.len()` units.
        let len = unsafe { GetShortPathNameW(PCWSTR(wide.as_ptr()), Some(&mut buf)) } as usize;
        (len > 0 && len < buf.len()).then(|| PathBuf::from(OsString::from_wide(&buf[..len])))
    }

    #[test]
    fn short_name_aliases_cannot_dodge_the_protected_check() {
        let scratch = Scratch::new("short-names");
        let profile = scratch.0.join("Long Profile Name");
        std::fs::create_dir(&profile).unwrap();
        let Some(short) = short_path(&profile).filter(|s| s.file_name() != profile.file_name())
        else {
            eprintln!("8.3 names are disabled on this volume; nothing to check");
            return;
        };
        // `C:\Users\LONGPR~1` must be keyed (and refused) like `C:\Users\Long Profile Name`.
        let entry = resolve_stored_entry(&display(&short)).unwrap();
        assert_eq!(key(&entry), key(&profile), "{}", entry.display());
        let protected = Protected::new([], Some(profile.clone()));
        let err = protected.check(&entry).unwrap_err();
        assert_eq!(err.code(), ErrorCode::ActionBlocked);
        // Entries that do not exist yet keep the name as written.
        let fresh = profile.join("not-created-yet.txt");
        assert_eq!(key(&resolve_stored_entry(&fresh).unwrap()), key(&fresh));
    }

    #[test]
    fn ordinary_user_locations_are_allowed() {
        let protected = fake();
        for path in [
            r"Z:\Users\me\Desktop\report.docx",
            r"Z:\Users\me\Documents\Projects\new",
            r"Z:\Users\me\Downloads\x.zip",
            r"Z:\Users\me\AppData\Local\Temp\winwright\x",
            r"Z:\data\photos\a.jpg",
            r"Z:\Windows-old-backup\x\y",
            r"\\srv\share\team\notes.txt",
            r"Z:\Users\other\Documents\x",
        ] {
            assert!(protected.check(Path::new(path)).is_ok(), "{path}");
        }
    }
}
