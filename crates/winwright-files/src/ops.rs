//! Blocking implementations of every [`FileOperation`]. Each runs on a `spawn_blocking` thread.

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::fs::{self, Metadata};
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, ERROR_FILE_EXISTS, ERROR_NOT_SAME_DEVICE};
use windows::Win32::Storage::FileSystem::{
    CopyFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_READONLY,
    FILE_ATTRIBUTE_REPARSE_POINT, GetDriveTypeW, MOVE_FILE_FLAGS, MOVEFILE_COPY_ALLOWED,
    MOVEFILE_REPLACE_EXISTING, MoveFileExW,
};
use windows::core::PCWSTR;
use winwright_contracts::backend::OperationContext;
use winwright_contracts::system::{FileEntry, FileOperation, FileResult};
use winwright_contracts::{WinwrightError, WinwrightResult};

use crate::glob::Glob;
use crate::path::{
    Protected, display, is_within, key, missing, normalize, resolve_entry, resolve_existing,
    resolve_stored_entry, validate_name,
};
use crate::shell::{KNOWN_FOLDER_NAMES, folder_id, known_folder_path, recycle};
use crate::text;
use crate::{io_platform, platform, wide};

pub(crate) const MAX_LIST_ENTRIES: usize = 5_000;
pub(crate) const MAX_SEARCH_RESULTS: usize = 5_000;
pub(crate) const MAX_SEARCH_DEPTH: usize = 32;
const CANCEL_CHECK_INTERVAL: usize = 256;
/// `GetDriveTypeW` result for a fixed disk (the only drive type with a Recycle Bin by default).
const DRIVE_FIXED: u32 = 3;

pub(crate) fn run(
    op: FileOperation,
    protected: &Protected,
    ctx: &OperationContext,
) -> WinwrightResult<FileResult> {
    match op {
        FileOperation::List {
            path,
            include_hidden,
        } => list(&path, include_hidden, ctx),
        FileOperation::Metadata { path } => metadata(&path),
        FileOperation::Copy {
            from,
            to,
            overwrite,
        } => copy(&from, &to, overwrite, protected),
        FileOperation::Move {
            from,
            to,
            overwrite,
        } => move_entry(&from, &to, overwrite, protected),
        FileOperation::Rename { path, new_name } => rename(&path, &new_name, protected),
        FileOperation::Delete { path } => delete(&path, protected),
        FileOperation::CreateDirectory { path } => create_directory(&path, protected),
        FileOperation::Search {
            root,
            pattern,
            max_results,
        } => search(&root, &pattern, max_results, ctx),
        FileOperation::KnownFolder { name } => known_folder(&name),
        FileOperation::Read {
            path,
            offset,
            length,
        } => text::read(&path, offset, length),
        FileOperation::Write {
            path,
            content,
            mode,
        } => text::write(&path, &content, mode, protected),
        FileOperation::Edit {
            path,
            old,
            new,
            count,
        } => text::edit(&path, &old, &new, count, protected),
        FileOperation::Grep {
            root,
            pattern,
            glob,
            ignore_case,
            max_results,
        } => text::grep(
            &root,
            &pattern,
            glob.as_deref(),
            ignore_case,
            max_results,
            ctx,
        ),
    }
}

pub(crate) fn file_entry(path: &Path, meta: &Metadata) -> FileEntry {
    let attributes = meta.file_attributes();
    let is_dir = attributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0;
    let shown = display(path);
    let name = shown
        .file_name()
        .unwrap_or(shown.as_os_str())
        .to_string_lossy()
        .into_owned();
    FileEntry {
        name,
        is_dir,
        size: if is_dir { 0 } else { meta.len() },
        modified_ms: meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .and_then(|since| u64::try_from(since.as_millis()).ok()),
        hidden: attributes & FILE_ATTRIBUTE_HIDDEN.0 != 0,
        readonly: attributes & FILE_ATTRIBUTE_READONLY.0 != 0,
        path: shown,
    }
}

/// Folders first, then names case-insensitively (exact name breaks ties deterministically).
pub(crate) fn sort_entries(entries: &mut [FileEntry]) {
    entries.sort_by_cached_key(|e| (!e.is_dir, e.name.to_lowercase(), e.name.clone()));
}

pub(crate) fn is_reparse_point(meta: &Metadata) -> bool {
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
}

pub(crate) fn is_directory(meta: &Metadata) -> bool {
    meta.file_attributes() & FILE_ATTRIBUTE_DIRECTORY.0 != 0
}

fn list(path: &Path, include_hidden: bool, ctx: &OperationContext) -> WinwrightResult<FileResult> {
    let dir = resolve_existing(path)?;
    let meta = fs::metadata(&dir).map_err(|e| missing(&dir, &e))?;
    if !meta.is_dir() {
        return Err(WinwrightError::invalid(format!(
            "{} is not a folder",
            display(&dir).display()
        )));
    }
    let mut entries = Vec::new();
    let items = fs::read_dir(&dir).map_err(|e| io_platform("read_dir", &e))?;
    for (index, item) in items.enumerate() {
        if index.is_multiple_of(CANCEL_CHECK_INTERVAL) {
            ctx.check("file list")?;
        }
        let Ok(item) = item else { continue };
        let Ok(meta) = item.metadata() else { continue };
        let entry = file_entry(&item.path(), &meta);
        if entry.hidden && !include_hidden {
            continue;
        }
        entries.push(entry);
    }
    sort_entries(&mut entries);
    let truncated = entries.len() > MAX_LIST_ENTRIES;
    entries.truncate(MAX_LIST_ENTRIES);
    Ok(FileResult::Entries { entries, truncated })
}

fn metadata(path: &Path) -> WinwrightResult<FileResult> {
    let resolved = resolve_existing(path)?;
    let meta = fs::metadata(&resolved).map_err(|e| missing(&resolved, &e))?;
    Ok(FileResult::Entry {
        entry: file_entry(&resolved, &meta),
    })
}

/// An existing entry about to be mutated in place (moved, renamed, recycled): the entry itself
/// and, for links, their target must both be outside protected locations.
pub(crate) fn mutable_entry(
    path: &Path,
    protected: &Protected,
) -> WinwrightResult<(PathBuf, Metadata)> {
    let entry = resolve_stored_entry(path)?;
    protected.check(&entry)?;
    let meta = fs::symlink_metadata(&entry).map_err(|e| missing(&entry, &e))?;
    if is_reparse_point(&meta)
        && let Ok(target) = fs::canonicalize(&entry)
    {
        protected.check(&target)?;
    }
    Ok((entry, meta))
}

/// Where `to` puts an item named `name`: inside `to` when it is an existing folder, otherwise
/// `to` itself (whose parent must exist).
fn destination(to: &Path, name: &OsStr) -> WinwrightResult<PathBuf> {
    let normalized = normalize(to)?;
    if fs::metadata(&normalized).is_ok_and(|meta| meta.is_dir()) {
        return Ok(resolve_existing(&normalized)?.join(name));
    }
    resolve_entry(&normalized)
}

/// Checks an existing destination before it is replaced.
fn check_replace(dest: &Path, overwrite: bool, protected: &Protected) -> WinwrightResult<()> {
    let Ok(existing) = fs::symlink_metadata(dest) else {
        return Ok(());
    };
    if !overwrite {
        return Err(exists(dest));
    }
    if is_directory(&existing) {
        return Err(WinwrightError::invalid(format!(
            "{} is a folder and cannot be replaced",
            display(dest).display()
        )));
    }
    if is_reparse_point(&existing)
        && let Ok(target) = fs::canonicalize(dest)
    {
        protected.check(&target)?;
    }
    Ok(())
}

fn exists(dest: &Path) -> WinwrightError {
    WinwrightError::invalid(format!(
        "{} already exists; pass overwrite: true to replace it",
        display(dest).display()
    ))
}

fn file_error(operation: &str, dest: &Path, err: &windows::core::Error) -> WinwrightError {
    let code = err.code();
    if code == ERROR_FILE_EXISTS.to_hresult() || code == ERROR_ALREADY_EXISTS.to_hresult() {
        exists(dest)
    } else if code == ERROR_NOT_SAME_DEVICE.to_hresult() {
        WinwrightError::invalid("folders cannot be moved to a different drive")
    } else {
        platform(operation, err)
    }
}

fn copy(
    from: &Path,
    to: &Path,
    overwrite: bool,
    protected: &Protected,
) -> WinwrightResult<FileResult> {
    let source = resolve_existing(from)?;
    let meta = fs::metadata(&source).map_err(|e| missing(&source, &e))?;
    if meta.is_dir() {
        return Err(WinwrightError::invalid(
            "copying folders is not supported yet; copy the files inside it",
        ));
    }
    let name = source
        .file_name()
        .ok_or_else(|| WinwrightError::invalid("the source has no file name"))?;
    let dest = destination(to, name)?;
    protected.check(&dest)?;
    if fs::canonicalize(&dest).is_ok_and(|existing| key(&existing) == key(&source)) {
        return Err(WinwrightError::invalid(
            "source and destination are the same file",
        ));
    }
    check_replace(&dest, overwrite, protected)?;

    let (source_w, dest_w) = (wide(&source), wide(&dest));
    // SAFETY: both paths are NUL-terminated and outlive the call. With `!overwrite` the copy
    // fails atomically if the destination appeared after the checks above.
    unsafe {
        CopyFileW(
            PCWSTR(source_w.as_ptr()),
            PCWSTR(dest_w.as_ptr()),
            !overwrite,
        )
    }
    .map_err(|e| file_error("CopyFileW", &dest, &e))?;
    Ok(FileResult::Path {
        path: display(&dest),
    })
}

fn move_file(source: &Path, dest: &Path, flags: MOVE_FILE_FLAGS) -> WinwrightResult<()> {
    let (source_w, dest_w) = (wide(source), wide(dest));
    // SAFETY: both paths are NUL-terminated and outlive the call. Without
    // MOVEFILE_REPLACE_EXISTING the move fails atomically if the destination exists.
    unsafe { MoveFileExW(PCWSTR(source_w.as_ptr()), PCWSTR(dest_w.as_ptr()), flags) }
        .map_err(|e| file_error("MoveFileExW", dest, &e))
}

fn move_entry(
    from: &Path,
    to: &Path,
    overwrite: bool,
    protected: &Protected,
) -> WinwrightResult<FileResult> {
    let (source, meta) = mutable_entry(from, protected)?;
    let name = source
        .file_name()
        .ok_or_else(|| WinwrightError::invalid("a drive root cannot be moved"))?
        .to_owned();
    let dest = destination(to, &name)?;
    protected.check(&dest)?;
    let (source_key, dest_key) = (key(&source), key(&dest));
    if source_key == dest_key && source.file_name() == dest.file_name() {
        return Err(WinwrightError::invalid(
            "source and destination are the same",
        ));
    }
    if is_directory(&meta) && source_key != dest_key && is_within(&dest_key, &source_key) {
        return Err(WinwrightError::invalid(
            "a folder cannot be moved into itself",
        ));
    }
    // A case-only change (`a.txt` → `A.txt`) targets the same entry and is not an overwrite.
    if source_key != dest_key {
        check_replace(&dest, overwrite, protected)?;
    }
    let flags = if overwrite {
        MOVEFILE_COPY_ALLOWED | MOVEFILE_REPLACE_EXISTING
    } else {
        MOVEFILE_COPY_ALLOWED
    };
    move_file(&source, &dest, flags)?;
    Ok(FileResult::Path {
        path: display(&dest),
    })
}

fn rename(path: &Path, new_name: &str, protected: &Protected) -> WinwrightResult<FileResult> {
    validate_name(new_name)?;
    let (source, _) = mutable_entry(path, protected)?;
    let parent = source
        .parent()
        .ok_or_else(|| WinwrightError::invalid("a drive root cannot be renamed"))?;
    let dest = parent.join(new_name);
    protected.check(&dest)?;
    if source.file_name() == Some(OsStr::new(new_name)) {
        return Ok(FileResult::Path {
            path: display(&source),
        });
    }
    if key(&source) != key(&dest) && fs::symlink_metadata(&dest).is_ok() {
        return Err(WinwrightError::invalid(format!(
            "{} already exists; rename never replaces an existing item",
            display(&dest).display()
        )));
    }
    move_file(&source, &dest, MOVE_FILE_FLAGS(0))?;
    Ok(FileResult::Path {
        path: display(&dest),
    })
}

fn create_directory(path: &Path, protected: &Protected) -> WinwrightResult<FileResult> {
    let normalized = normalize(path)?;
    // Walk up to the deepest existing ancestor; everything below it will be created.
    let mut missing_names: Vec<OsString> = Vec::new();
    let mut cursor = normalized.as_path();
    let base = loop {
        match fs::metadata(cursor) {
            Ok(meta) if meta.is_dir() => break resolve_existing(cursor)?,
            Ok(_) => {
                return Err(WinwrightError::invalid(format!(
                    "{} is a file, not a folder",
                    display(cursor).display()
                )));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let (Some(parent), Some(name)) = (cursor.parent(), cursor.file_name()) else {
                    return Err(missing(cursor, &e));
                };
                missing_names.push(name.to_owned());
                cursor = parent;
            }
            Err(e) => return Err(missing(cursor, &e)),
        }
    };
    // Creating a folder that already exists changes nothing.
    let mut target = base;
    for name in missing_names.iter().rev() {
        target.push(name);
        protected.check(&target)?;
    }
    if !missing_names.is_empty() {
        fs::create_dir_all(&target).map_err(|e| io_platform("create_dir_all", &e))?;
    }
    Ok(FileResult::Path {
        path: display(&target),
    })
}

fn search(
    root: &Path,
    pattern: &str,
    max_results: usize,
    ctx: &OperationContext,
) -> WinwrightResult<FileResult> {
    if max_results == 0 {
        return Err(WinwrightError::invalid("maxResults must be at least 1"));
    }
    let limit = max_results.min(MAX_SEARCH_RESULTS);
    let glob = Glob::new(pattern)?;
    let root = resolve_existing(root)?;
    let mut results = Vec::new();
    let mut full = false;
    let cut = walk(root, ctx, |path, meta| {
        if glob.matches(&path.file_name().unwrap_or_default().to_string_lossy()) {
            if results.len() == limit {
                full = true;
                return Walk::Stop;
            }
            results.push(file_entry(path, meta));
        }
        Walk::Next
    })?;
    Ok(FileResult::Entries {
        entries: results,
        truncated: cut || full,
    })
}

/// What a walk does after visiting an entry.
pub(crate) enum Walk {
    Next,
    /// Do not look inside this entry.
    SkipFolder,
    Stop,
}

/// Visits every entry under the folder `root`, breadth first, at most [`MAX_SEARCH_DEPTH`]
/// folders deep. Links and junctions are never followed, so the walk cannot loop; unreadable
/// folders (access denied, vanished) are skipped. Returns whether the walk was cut short: by
/// [`Walk::Stop`], the deadline, or the depth limit.
pub(crate) fn walk(
    root: PathBuf,
    ctx: &OperationContext,
    mut visit: impl FnMut(&Path, &Metadata) -> Walk,
) -> WinwrightResult<bool> {
    if !fs::metadata(&root).is_ok_and(|meta| meta.is_dir()) {
        return Err(WinwrightError::invalid(format!(
            "{} is not a folder",
            display(&root).display()
        )));
    }
    let mut cut = false;
    let mut visited = 0usize;
    let mut queue = VecDeque::from([(root, 0usize)]);
    while let Some((dir, depth)) = queue.pop_front() {
        let Ok(items) = fs::read_dir(&dir) else {
            continue;
        };
        for item in items {
            visited += 1;
            if visited.is_multiple_of(CANCEL_CHECK_INTERVAL) {
                if ctx.cancel.is_cancelled() {
                    return Err(WinwrightError::Cancelled);
                }
                if ctx.remaining().is_zero() {
                    return Ok(true);
                }
            }
            let Ok(item) = item else { continue };
            let Ok(meta) = item.metadata() else { continue };
            let path = item.path();
            match visit(&path, &meta) {
                Walk::Stop => return Ok(true),
                Walk::SkipFolder => continue,
                Walk::Next => {}
            }
            if is_directory(&meta) && !is_reparse_point(&meta) {
                if depth < MAX_SEARCH_DEPTH {
                    queue.push_back((path, depth + 1));
                } else {
                    cut = true;
                }
            }
        }
    }
    Ok(cut)
}

fn known_folder(name: &str) -> WinwrightResult<FileResult> {
    let wanted = name.trim().to_ascii_lowercase();
    let path = if wanted == "temp" {
        std::env::temp_dir()
    } else {
        let id = folder_id(&wanted).ok_or_else(|| {
            WinwrightError::invalid(format!(
                "unknown known folder `{name}`; supported: {}",
                KNOWN_FOLDER_NAMES.join(", ")
            ))
        })?;
        known_folder_path(&id).ok_or_else(|| {
            WinwrightError::invalid(format!("the {name} folder is not available for this user"))
        })?
    };
    let canonical = fs::canonicalize(&path).unwrap_or(path);
    Ok(FileResult::Path {
        path: display(&canonical),
    })
}

fn delete(path: &Path, protected: &Protected) -> WinwrightResult<FileResult> {
    let (entry, _) = mutable_entry(path, protected)?;
    recycle_entry(&entry)?;
    Ok(FileResult::Done)
}

/// Moves `entry` to the Recycle Bin and checks it is gone. Never deletes permanently.
pub(crate) fn recycle_entry(entry: &Path) -> WinwrightResult<()> {
    ensure_recycle_bin(entry)?;
    let shown = display(entry);
    recycle(&shown)?;
    if fs::symlink_metadata(entry).is_ok() {
        return Err(WinwrightError::ActionOutcomeUnknown {
            operation: "recycle".to_owned(),
            reason: format!("{} still exists after recycling", shown.display()),
        });
    }
    Ok(())
}

/// Only fixed local drives have a Recycle Bin by default; elsewhere the shell would delete
/// permanently, so the delete is refused up front.
fn ensure_recycle_bin(entry: &Path) -> WinwrightResult<()> {
    let volume = key(entry).into_iter().next().unwrap_or_default();
    let refuse = || {
        Err(WinwrightError::ActionBlocked {
            reason: format!(
                "{} is on a network or removable drive without a Recycle Bin; Winwright never \
                 deletes permanently",
                display(entry).display()
            ),
        })
    };
    if !volume.ends_with(':') {
        return refuse();
    }
    let root = wide(format!("{}\\", volume.to_ascii_uppercase()));
    // SAFETY: `root` is a NUL-terminated drive root such as `C:\`.
    if unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) } != DRIVE_FIXED {
        return refuse();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, is_dir: bool) -> FileEntry {
        FileEntry {
            path: PathBuf::from(name),
            name: name.to_owned(),
            is_dir,
            size: 0,
            modified_ms: None,
            hidden: false,
            readonly: false,
        }
    }

    #[test]
    fn folders_sort_first_then_names_case_insensitively() {
        let mut entries = vec![
            entry("b.txt", false),
            entry("Zeta", true),
            entry("A.txt", false),
            entry("alpha", true),
            entry("a.txt", false),
            entry("10.txt", false),
        ];
        sort_entries(&mut entries);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            ["alpha", "Zeta", "10.txt", "A.txt", "a.txt", "b.txt"]
        );
    }

    #[test]
    fn unknown_known_folders_list_the_supported_names() {
        let err = known_folder("Startup").unwrap_err();
        assert_eq!(err.code(), winwright_contracts::ErrorCode::InvalidRequest);
        assert!(err.to_string().contains("Downloads"), "{err}");
    }

    #[test]
    fn case_only_moves_keep_the_requested_spelling() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
            r"..\..\target\winwright-files-recase-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        let dir = fs::canonicalize(&dir).unwrap();
        let result = (|| {
            fs::write(dir.join("recase.txt"), "x").unwrap();
            let protected = Protected::default();
            move_entry(
                &dir.join("recase.txt"),
                &dir.join("ReCase.txt"),
                false,
                &protected,
            )?;
            // A source spelled differently from the stored name still names the same entry.
            rename(&dir.join("RECASE.TXT"), "recase.TXT", &protected)
        })();
        let names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        let _ = fs::remove_dir_all(&dir);
        result.unwrap();
        assert_eq!(names, ["recase.TXT"]);
    }

    #[test]
    fn network_paths_have_no_recycle_bin() {
        let err = ensure_recycle_bin(Path::new(r"\\server\share\dir\file.txt")).unwrap_err();
        assert_eq!(err.code(), winwright_contracts::ErrorCode::ActionBlocked);
    }
}
