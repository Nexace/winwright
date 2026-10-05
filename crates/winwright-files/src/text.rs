//! Text files: reading by lines, writing, exact edits, and content search. A file that is
//! replaced or edited goes to the Recycle Bin first, so every change can be undone there.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use winwright_contracts::backend::OperationContext;
use winwright_contracts::system::{FileResult, TextMatch, WriteMode};
use winwright_contracts::{WinwrightError, WinwrightResult};
use winwright_security::is_secret_path;

use crate::glob::Glob;
use crate::io_platform;
use crate::ops::{
    MAX_SEARCH_RESULTS, Walk, is_directory, is_reparse_point, mutable_entry, recycle_entry, walk,
};
use crate::path::{Protected, display, missing, resolve_existing, resolve_stored_entry};

/// Larger files are not read whole: `grep` skips them, `read` and `edit` refuse them.
pub(crate) const MAX_TEXT_FILE_BYTES: u64 = 10 * 1024 * 1024;
/// Most one `write` may put in a file.
const MAX_WRITE_BYTES: usize = 5 * 1024 * 1024;
/// A found line is cut to this many characters.
const MAX_MATCH_CHARS: usize = 300;
/// A NUL byte this early marks a file as binary.
const BINARY_SNIFF_BYTES: usize = 8 * 1024;

/// How a text file is stored, so an edit keeps it that way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Encoding {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
}

/// A text file's content and encoding; `None` for binary content.
pub(crate) fn decode(bytes: &[u8]) -> Option<(String, Encoding)> {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return utf8(rest).map(|t| (t, Encoding::Utf8Bom));
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return utf16(rest, u16::from_le_bytes).map(|t| (t, Encoding::Utf16Le));
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return utf16(rest, u16::from_be_bytes).map(|t| (t, Encoding::Utf16Be));
    }
    utf8(bytes).map(|t| (t, Encoding::Utf8))
}

fn utf8(bytes: &[u8]) -> Option<String> {
    if bytes[..bytes.len().min(BINARY_SNIFF_BYTES)].contains(&0) {
        return None;
    }
    Some(String::from_utf8_lossy(bytes).into_owned())
}

fn utf16(bytes: &[u8], unit: fn([u8; 2]) -> u16) -> Option<String> {
    let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|&c| unit(c)).collect();
    let text = String::from_utf16_lossy(&units);
    (!text.contains('\0')).then_some(text)
}

/// `text` as stored in `encoding`, without a byte-order mark.
fn encode_body(text: &str, encoding: Encoding) -> Vec<u8> {
    match encoding {
        Encoding::Utf8 | Encoding::Utf8Bom => text.as_bytes().to_vec(),
        Encoding::Utf16Le => text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        Encoding::Utf16Be => text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
    }
}

/// `text` as a whole file in `encoding`, byte-order mark included.
pub(crate) fn encode(text: &str, encoding: Encoding) -> Vec<u8> {
    let bom: &[u8] = match encoding {
        Encoding::Utf8 => &[],
        Encoding::Utf8Bom => &[0xEF, 0xBB, 0xBF],
        Encoding::Utf16Le => &[0xFF, 0xFE],
        Encoding::Utf16Be => &[0xFE, 0xFF],
    };
    [bom, &encode_body(text, encoding)].concat()
}

/// Lines `offset`.. of `text` (a negative offset counts from the end), at most `length` of
/// them, joined by `\n`; with the first line's index, the line count, and whether lines
/// follow the ones returned.
pub(crate) fn lines(text: &str, offset: i64, length: usize) -> (String, usize, usize, bool) {
    let all: Vec<&str> = text.lines().collect();
    let total = all.len();
    let first = if offset < 0 {
        total.saturating_sub(usize::try_from(offset.unsigned_abs()).unwrap_or(usize::MAX))
    } else {
        usize::try_from(offset).unwrap_or(usize::MAX).min(total)
    };
    let end = first.saturating_add(length).min(total);
    (all[first..end].join("\n"), first, total, end < total)
}

/// `text` with `old` replaced by `new`, which must occur exactly `count` times. Line breaks in
/// `old` and `new` follow the file's, so text copied from a read matches a `\r\n` file.
pub(crate) fn replace_exact(
    text: &str,
    old: &str,
    new: &str,
    count: usize,
) -> WinwrightResult<String> {
    if old.is_empty() {
        return Err(WinwrightError::invalid(
            "`old` is empty: give the text to replace",
        ));
    }
    let crlf = text.contains("\r\n");
    let breaks = |s: &str| {
        if crlf {
            s.replace("\r\n", "\n").replace('\n', "\r\n")
        } else {
            s.to_owned()
        }
    };
    let (old, new) = (breaks(old), breaks(new));
    let found = text.matches(old.as_str()).count();
    if found != count {
        let hint = if found == 0 {
            "copy it exactly from a fresh read"
        } else {
            "add surrounding lines to make it unique, or set count"
        };
        return Err(WinwrightError::invalid(format!(
            "`old` occurs {found} times, not {count}: {hint}"
        )));
    }
    Ok(text.replace(old.as_str(), &new))
}

/// A text file's resolved path, content, and encoding.
fn load(file: &Path) -> WinwrightResult<(String, Encoding)> {
    let meta = fs::metadata(file).map_err(|e| missing(file, &e))?;
    if meta.is_dir() {
        return Err(WinwrightError::invalid(format!(
            "{} is a folder; list it instead",
            display(file).display()
        )));
    }
    if meta.len() > MAX_TEXT_FILE_BYTES {
        return Err(WinwrightError::invalid(format!(
            "{} is {} MB; text files up to 10 MB can be read and edited (grep searches the rest)",
            display(file).display(),
            meta.len() / (1024 * 1024)
        )));
    }
    let bytes = fs::read(file).map_err(|e| io_platform("read file", &e))?;
    decode(&bytes).ok_or_else(|| {
        WinwrightError::invalid(format!("{} is not a text file", display(file).display()))
    })
}

/// The person is asked before secrets are read or written, judging the path as requested. One
/// that reaches a secret only through a link or junction is refused, so the question is asked
/// about the real path.
pub(crate) fn no_hidden_secret(requested: &Path, resolved: &Path) -> WinwrightResult<()> {
    if is_secret_path(resolved) && !is_secret_path(requested) {
        return Err(WinwrightError::ActionBlocked {
            reason: format!(
                "{} leads to {}, which holds secrets: use that path",
                requested.display(),
                display(resolved).display()
            ),
        });
    }
    Ok(())
}

pub(crate) fn read(path: &Path, offset: i64, length: usize) -> WinwrightResult<FileResult> {
    if length == 0 {
        return Err(WinwrightError::invalid("length must be at least 1"));
    }
    let file = resolve_existing(path)?;
    no_hidden_secret(path, &file)?;
    let (text, _) = load(&file)?;
    let (text, first_line, total_lines, truncated) = lines(&text, offset, length);
    Ok(FileResult::Text {
        path: display(&file),
        text,
        first_line,
        total_lines,
        truncated,
    })
}

pub(crate) fn write(
    path: &Path,
    content: &str,
    mode: WriteMode,
    protected: &Protected,
) -> WinwrightResult<FileResult> {
    if content.len() > MAX_WRITE_BYTES {
        return Err(WinwrightError::invalid(
            "content is limited to 5 MB per write; append the rest",
        ));
    }
    let dest = resolve_stored_entry(path)?;
    protected.check(&dest)?;
    no_hidden_secret(path, &dest)?;
    let existing = fs::symlink_metadata(&dest).ok();
    if let Some(meta) = &existing {
        if is_directory(meta) {
            return Err(WinwrightError::invalid(format!(
                "{} is a folder",
                display(&dest).display()
            )));
        }
        if is_reparse_point(meta) {
            return Err(WinwrightError::invalid(format!(
                "{} is a link; write to the file it points to",
                display(&dest).display()
            )));
        }
    }
    match (mode, existing.is_some()) {
        (WriteMode::Create, true) => {
            return Err(WinwrightError::invalid(format!(
                "{} already exists; use mode overwrite or append",
                display(&dest).display()
            )));
        }
        (WriteMode::Overwrite, true) => replace(&dest, content.as_bytes())?,
        (WriteMode::Append, true) => append(&dest, content)?,
        (_, false) => create_new(&dest, content.as_bytes())?,
    }
    Ok(FileResult::Path {
        path: display(&dest),
    })
}

pub(crate) fn edit(
    path: &Path,
    old: &str,
    new: &str,
    count: usize,
    protected: &Protected,
) -> WinwrightResult<FileResult> {
    let (file, meta) = mutable_entry(path, protected)?;
    no_hidden_secret(path, &file)?;
    if is_reparse_point(&meta) {
        return Err(WinwrightError::invalid(format!(
            "{} is a link; edit the file it points to",
            display(&file).display()
        )));
    }
    let (text, encoding) = load(&file)?;
    // Text that does not turn back into the same bytes (invalid UTF-8, a legacy code page) would
    // be changed outside the edit too.
    let original = fs::read(&file).map_err(|e| missing(&file, &e))?;
    if encode(&text, encoding) != original {
        return Err(WinwrightError::invalid(format!(
            "{} is not valid UTF-8 or UTF-16 text; editing it would change other bytes too",
            display(&file).display()
        )));
    }
    let edited = replace_exact(&text, old, new, count)?;
    replace(&file, &encode(&edited, encoding))?;
    Ok(FileResult::Path {
        path: display(&file),
    })
}

/// Writes `bytes` to a file that must not exist yet.
fn create_new(dest: &Path, bytes: &[u8]) -> WinwrightResult<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dest)
        .map_err(|e| io_platform("create file", &e))?;
    file.write_all(bytes)
        .map_err(|e| io_platform("write file", &e))
}

/// Replaces an existing file: the new content is written beside it first, the old file goes
/// to the Recycle Bin, then the new one takes its name. A failure part-way leaves both
/// versions findable and says where.
fn replace(dest: &Path, bytes: &[u8]) -> WinwrightResult<()> {
    let name = dest
        .file_name()
        .ok_or_else(|| WinwrightError::invalid("the file has no name"))?;
    let mut temp_name = name.to_owned();
    temp_name.push(format!(".winwright-{}.tmp", std::process::id()));
    let temp: PathBuf = dest.with_file_name(temp_name);
    create_new(&temp, bytes)?;
    if let Err(err) = recycle_entry(dest) {
        let _ = fs::remove_file(&temp);
        return Err(err);
    }
    fs::rename(&temp, dest).map_err(|e| WinwrightError::ActionOutcomeUnknown {
        operation: "replace file".to_owned(),
        reason: format!(
            "the old {} is in the Recycle Bin and the new content in {}: {e}",
            display(dest).display(),
            display(&temp).display()
        ),
    })
}

/// Appends `content` in the file's own encoding (UTF-16 files stay UTF-16).
fn append(dest: &Path, content: &str) -> WinwrightResult<()> {
    let mut head = [0u8; 3];
    let read = fs::File::open(dest)
        .and_then(|mut f| f.read(&mut head))
        .map_err(|e| io_platform("read file", &e))?;
    let encoding = match &head[..read] {
        [0xFF, 0xFE, ..] => Encoding::Utf16Le,
        [0xFE, 0xFF, ..] => Encoding::Utf16Be,
        _ => Encoding::Utf8,
    };
    let mut file = OpenOptions::new()
        .append(true)
        .open(dest)
        .map_err(|e| io_platform("open file", &e))?;
    file.write_all(&encode_body(content, encoding))
        .map_err(|e| io_platform("write file", &e))
}

/// Lines matching `pattern` in the text files under `root`. Secret files and folders below the
/// root are skipped (their lines would reach the model); a secret root was confirmed by the
/// person.
pub(crate) fn grep(
    root: &Path,
    pattern: &str,
    glob: Option<&str>,
    ignore_case: bool,
    max_results: usize,
    ctx: &OperationContext,
) -> WinwrightResult<FileResult> {
    if max_results == 0 {
        return Err(WinwrightError::invalid("maxResults must be at least 1"));
    }
    let limit = max_results.min(MAX_SEARCH_RESULTS);
    let regex = regex::RegexBuilder::new(pattern)
        .case_insensitive(ignore_case)
        .size_limit(1 << 20)
        .build()
        .map_err(|e| WinwrightError::invalid(format!("pattern: {e}")))?;
    let glob = glob.map(Glob::new).transpose()?;
    let requested = root;
    let root = resolve_existing(root)?;
    no_hidden_secret(requested, &root)?;
    let secret_root = is_secret_path(&root);
    let mut matches = Vec::new();
    let mut full = false;
    let cut = walk(root, ctx, |path, meta| {
        if !secret_root && is_secret_path(path) {
            return Walk::SkipFolder;
        }
        // A file link is read through to its target, past the secret and size checks.
        if is_directory(meta)
            || is_reparse_point(meta)
            || meta.len() > MAX_TEXT_FILE_BYTES
            || glob.as_ref().is_some_and(|g| {
                !g.matches(&path.file_name().unwrap_or_default().to_string_lossy())
            })
        {
            return Walk::Next;
        }
        let Some((text, _)) = fs::read(path).ok().and_then(|b| decode(&b)) else {
            return Walk::Next;
        };
        for (index, line) in text.lines().enumerate() {
            if !regex.is_match(line) {
                continue;
            }
            if matches.len() == limit {
                full = true;
                return Walk::Stop;
            }
            matches.push(TextMatch {
                path: display(path),
                line: index + 1,
                text: line.chars().take(MAX_MATCH_CHARS).collect(),
            });
        }
        Walk::Next
    })?;
    Ok(FileResult::Matches {
        matches,
        truncated: cut || full,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_encodings_round_trip_and_binary_is_refused() {
        for encoding in [
            Encoding::Utf8,
            Encoding::Utf8Bom,
            Encoding::Utf16Le,
            Encoding::Utf16Be,
        ] {
            let bytes = encode("héllo\r\nwörld", encoding);
            assert_eq!(
                decode(&bytes),
                Some(("héllo\r\nwörld".to_owned(), encoding)),
                "{encoding:?}"
            );
        }
        assert_eq!(decode(b"MZ\x90\x00\x03"), None);
        assert_eq!(decode(b""), Some((String::new(), Encoding::Utf8)));
    }

    #[test]
    fn lines_by_offset_and_from_the_end() {
        let text = "a\r\nb\nc\nd\n";
        assert_eq!(lines(text, 0, 2), ("a\nb".to_owned(), 0, 4, true));
        assert_eq!(lines(text, 2, 10), ("c\nd".to_owned(), 2, 4, false));
        assert_eq!(lines(text, -1, 10), ("d".to_owned(), 3, 4, false));
        assert_eq!(lines(text, -10, 1), ("a".to_owned(), 0, 4, true));
        assert_eq!(lines(text, 99, 1), (String::new(), 4, 4, false));
    }

    #[test]
    fn only_text_that_round_trips_may_be_edited() {
        // Windows-1252 "café": the é byte is not UTF-8, so a rewrite would change it.
        let legacy = b"caf\xe9 au lait";
        let (text, encoding) = decode(legacy).unwrap();
        assert_ne!(encode(&text, encoding), legacy.to_vec());
        let utf8 = "caf\u{e9}".as_bytes();
        let (text, encoding) = decode(utf8).unwrap();
        assert_eq!(encode(&text, encoding), utf8.to_vec());
    }

    #[test]
    fn edits_replace_exactly_the_expected_occurrences() {
        assert_eq!(
            replace_exact("let a = 1;\nlet b = 1;", "a = 1", "a = 2", 1).unwrap(),
            "let a = 2;\nlet b = 1;"
        );
        let err = replace_exact("x x", "x", "y", 1).unwrap_err();
        assert!(err.to_string().contains("occurs 2 times"), "{err}");
        let err = replace_exact("abc", "zzz", "y", 1).unwrap_err();
        assert!(err.to_string().contains("occurs 0 times"), "{err}");
        assert_eq!(replace_exact("x x", "x", "y", 2).unwrap(), "y y");
        assert!(replace_exact("abc", "", "y", 1).is_err());
        // `\n` in the request matches a file that uses `\r\n`, and new lines follow it.
        assert_eq!(
            replace_exact("one\r\ntwo\r\n", "one\ntwo", "1\n2", 1).unwrap(),
            "1\r\n2\r\n"
        );
    }

    /// A scratch folder under `target`, removed by the caller.
    fn scratch(tag: &str) -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
            r"..\..\target\winwright-files-{tag}-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::canonicalize(&dir).unwrap()
    }

    fn read_all(file: &Path) -> WinwrightResult<String> {
        match read(file, 0, 100)? {
            FileResult::Text { text, .. } => Ok(text),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn files_are_created_and_appended() {
        let dir = scratch("write");
        let file = dir.join("notes.txt");
        let protected = Protected::default();
        let result = (|| {
            write(&file, "one\n", WriteMode::Create, &protected)?;
            let again = write(&file, "x", WriteMode::Create, &protected).unwrap_err();
            assert!(again.to_string().contains("already exists"), "{again}");
            write(&file, "two\n", WriteMode::Append, &protected)?;
            read_all(&file)
        })();
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(result.unwrap(), "one\ntwo");
    }

    /// Opt-in: replacing and editing put the old version in the real Recycle Bin.
    /// `cargo test -p winwright-files -- --ignored replaced`
    #[test]
    #[ignore = "puts two small test files in the Recycle Bin"]
    fn files_are_replaced_and_edited_through_the_recycle_bin() {
        let dir = scratch("replace");
        let file = dir.join("winwright-replace-test.txt");
        let protected = Protected::default();
        let result = (|| {
            write(&file, "one\r\ntwo\r\n", WriteMode::Create, &protected)?;
            edit(&file, "two", "2", 1, &protected)?;
            let edited = read_all(&file)?;
            write(&file, "new", WriteMode::Overwrite, &protected)?;
            Ok::<_, WinwrightError>((edited, read_all(&file)?))
        })();
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        let _ = fs::remove_dir_all(&dir);
        let (edited, replaced) = result.unwrap();
        assert_eq!(edited, "one\n2");
        assert_eq!(replaced, "new");
        assert_eq!(
            leftovers,
            ["winwright-replace-test.txt"],
            "no temp file left"
        );
    }
}
