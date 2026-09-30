//! Integration tests. Every write happens inside a fresh folder under %TEMP% that the test
//! creates and removes; protected-location tests are refused before anything touches disk.

use std::fs::{self, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use winwright_contracts::backend::OperationContext;
use winwright_contracts::ids::SessionId;
use winwright_contracts::system::{FileEntry, FileOperation, FileResult, FileService};
use winwright_contracts::{ErrorCode, WinwrightResult};
use winwright_files::LocalFiles;

const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;

/// A unique folder under %TEMP%, removed (with its contents) on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "winwright-files-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(fs::canonicalize(&dir).unwrap())
    }

    /// Plain (non-verbatim) path inside the scratch folder.
    fn path(&self, relative: &str) -> PathBuf {
        let root = self.0.to_str().unwrap().trim_start_matches(r"\\?\");
        Path::new(root).join(relative)
    }

    fn write(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.path(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, contents).unwrap();
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Test files may have been made read-only.
        for entry in fs::read_dir(&self.0).into_iter().flatten().flatten() {
            if let Ok(meta) = entry.metadata() {
                let mut perms = meta.permissions();
                #[allow(clippy::permissions_set_readonly_false)]
                perms.set_readonly(false);
                let _ = fs::set_permissions(entry.path(), perms);
            }
        }
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn ctx() -> OperationContext {
    OperationContext::new(
        SessionId::parse("files-tests").unwrap(),
        Duration::from_secs(30),
        CancellationToken::new(),
    )
}

async fn run(files: &LocalFiles, op: FileOperation) -> WinwrightResult<FileResult> {
    files.execute(op, &ctx()).await
}

fn entries(result: FileResult) -> (Vec<FileEntry>, bool) {
    match result {
        FileResult::Entries { entries, truncated } => (entries, truncated),
        other => panic!("expected entries, got {other:?}"),
    }
}

fn path_of(result: FileResult) -> PathBuf {
    match result {
        FileResult::Path { path } => path,
        other => panic!("expected a path, got {other:?}"),
    }
}

fn same(a: &Path, b: &Path) -> bool {
    a.to_string_lossy()
        .eq_ignore_ascii_case(&b.to_string_lossy())
}

#[tokio::test]
async fn list_sorts_folders_first_and_hides_hidden_entries() {
    let files = LocalFiles::new();
    let scratch = Scratch::new();
    scratch.write("b.txt", "b");
    scratch.write("A.txt", "a");
    fs::create_dir(scratch.path("zdir")).unwrap();
    fs::create_dir(scratch.path("Adir")).unwrap();
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .attributes(FILE_ATTRIBUTE_HIDDEN)
        .open(scratch.path("secret.txt"))
        .unwrap();

    let op = |include_hidden| FileOperation::List {
        path: scratch.path(""),
        include_hidden,
    };
    let (visible, truncated) = entries(run(&files, op(false)).await.unwrap());
    let names: Vec<&str> = visible.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["Adir", "zdir", "A.txt", "b.txt"]);
    assert!(!truncated);
    assert!(visible[0].is_dir && !visible[2].is_dir);
    assert!(
        same(&visible[2].path, &scratch.path("A.txt")),
        "{:?}",
        visible[2].path
    );
    assert!(!visible[2].path.to_string_lossy().starts_with(r"\\?\"));

    let (all, _) = entries(run(&files, op(true)).await.unwrap());
    let hidden = all
        .iter()
        .find(|e| e.name == "secret.txt")
        .expect("hidden entry");
    assert!(hidden.hidden);
}

#[tokio::test]
async fn metadata_reports_size_time_and_readonly() {
    let files = LocalFiles::new();
    let scratch = Scratch::new();
    let file = scratch.write("data.bin", "12345");
    let mut perms = fs::metadata(&file).unwrap().permissions();
    perms.set_readonly(true);
    fs::set_permissions(&file, perms).unwrap();

    let entry = match run(&files, FileOperation::Metadata { path: file.clone() })
        .await
        .unwrap()
    {
        FileResult::Entry { entry } => entry,
        other => panic!("{other:?}"),
    };
    assert_eq!(entry.name, "data.bin");
    assert_eq!(entry.size, 5);
    assert!(!entry.is_dir);
    assert!(entry.readonly);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let modified = entry.modified_ms.expect("modified time");
    assert!(modified <= now_ms + 5_000 && modified + 600_000 > now_ms);
}

#[tokio::test]
async fn copy_refuses_overwrite_unless_asked() {
    let files = LocalFiles::new();
    let scratch = Scratch::new();
    let source = scratch.write("source.txt", "new");
    let dest = scratch.write("dest.txt", "old");

    let copy = |overwrite| FileOperation::Copy {
        from: source.clone(),
        to: dest.clone(),
        overwrite,
    };
    let err = run(&files, copy(false)).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::InvalidRequest);
    assert_eq!(fs::read_to_string(&dest).unwrap(), "old");

    let out = path_of(run(&files, copy(true)).await.unwrap());
    assert!(same(&out, &dest));
    assert_eq!(fs::read_to_string(&dest).unwrap(), "new");
    assert_eq!(fs::read_to_string(&source).unwrap(), "new");

    let fresh = path_of(
        run(
            &files,
            FileOperation::Copy {
                from: source.clone(),
                to: scratch.path("fresh.txt"),
                overwrite: false,
            },
        )
        .await
        .unwrap(),
    );
    assert_eq!(fs::read_to_string(fresh).unwrap(), "new");
}

#[tokio::test]
async fn copy_into_a_folder_keeps_the_name_and_folders_are_not_copied() {
    let files = LocalFiles::new();
    let scratch = Scratch::new();
    let source = scratch.write("report.txt", "r");
    fs::create_dir(scratch.path("archive")).unwrap();

    let out = path_of(
        run(
            &files,
            FileOperation::Copy {
                from: source,
                to: scratch.path("archive"),
                overwrite: false,
            },
        )
        .await
        .unwrap(),
    );
    assert!(same(&out, &scratch.path(r"archive\report.txt")));
    assert!(scratch.path(r"archive\report.txt").is_file());

    let err = run(
        &files,
        FileOperation::Copy {
            from: scratch.path("archive"),
            to: scratch.path("archive2"),
            overwrite: false,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), ErrorCode::InvalidRequest);
}

#[tokio::test]
async fn move_refuses_overwrite_unless_asked() {
    let files = LocalFiles::new();
    let scratch = Scratch::new();
    let source = scratch.write("a.txt", "a");
    let blocker = scratch.write("b.txt", "b");

    let err = run(
        &files,
        FileOperation::Move {
            from: source.clone(),
            to: blocker.clone(),
            overwrite: false,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), ErrorCode::InvalidRequest);
    assert!(source.exists());
    assert_eq!(fs::read_to_string(&blocker).unwrap(), "b");

    run(
        &files,
        FileOperation::Move {
            from: source.clone(),
            to: blocker.clone(),
            overwrite: true,
        },
    )
    .await
    .unwrap();
    assert!(!source.exists());
    assert_eq!(fs::read_to_string(&blocker).unwrap(), "a");

    fs::create_dir(scratch.path("sub")).unwrap();
    let moved = path_of(
        run(
            &files,
            FileOperation::Move {
                from: blocker.clone(),
                to: scratch.path("sub"),
                overwrite: false,
            },
        )
        .await
        .unwrap(),
    );
    assert!(same(&moved, &scratch.path(r"sub\b.txt")));
    assert!(!blocker.exists());

    let err = run(
        &files,
        FileOperation::Move {
            from: scratch.path("sub"),
            to: scratch.path(r"sub\inner"),
            overwrite: false,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), ErrorCode::InvalidRequest);
}

#[tokio::test]
async fn rename_validates_names_and_never_overwrites() {
    let files = LocalFiles::new();
    let scratch = Scratch::new();
    let file = scratch.write("draft.txt", "d");
    scratch.write("taken.txt", "t");

    let rename = |path: &Path, new_name: &str| FileOperation::Rename {
        path: path.to_path_buf(),
        new_name: new_name.to_owned(),
    };
    for bad in [
        r"..\escape.txt",
        "sub/x.txt",
        "CON",
        "x.txt:stream",
        "",
        "..",
        "end.",
    ] {
        let err = run(&files, rename(&file, bad)).await.unwrap_err();
        assert_eq!(err.code(), ErrorCode::InvalidRequest, "{bad:?}");
    }
    let err = run(&files, rename(&file, "taken.txt")).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::InvalidRequest);
    assert!(file.exists());

    let renamed = path_of(run(&files, rename(&file, "final.txt")).await.unwrap());
    assert!(same(&renamed, &scratch.path("final.txt")));
    assert!(!file.exists());

    let recased = path_of(
        run(&files, rename(&scratch.path("final.txt"), "FINAL.txt"))
            .await
            .unwrap(),
    );
    assert!(recased.ends_with("FINAL.txt"));
    let listed = fs::read_dir(&scratch.0)
        .unwrap()
        .flatten()
        .any(|e| e.file_name() == "FINAL.txt");
    assert!(listed, "case-only rename applied");
}

#[tokio::test]
async fn create_directory_is_recursive_and_idempotent() {
    let files = LocalFiles::new();
    let scratch = Scratch::new();
    let target = scratch.path(r"one\two\three");
    let op = || FileOperation::CreateDirectory {
        path: target.clone(),
    };
    let created = path_of(run(&files, op()).await.unwrap());
    assert!(same(&created, &target));
    assert!(target.is_dir());
    assert!(same(&path_of(run(&files, op()).await.unwrap()), &target));

    scratch.write("file.txt", "x");
    let err = run(
        &files,
        FileOperation::CreateDirectory {
            path: scratch.path(r"file.txt\sub"),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), ErrorCode::InvalidRequest);
}

#[tokio::test]
async fn search_matches_names_case_insensitively_and_caps_results() {
    let files = LocalFiles::new();
    let scratch = Scratch::new();
    scratch.write("top.TXT", "");
    scratch.write(r"a\middle.txt", "");
    scratch.write(r"a\b\c\deep.txt", "");
    scratch.write(r"a\b\image.png", "");

    let search = |pattern: &str, max_results| FileOperation::Search {
        root: scratch.path(""),
        pattern: pattern.to_owned(),
        max_results,
    };
    let (found, truncated) = entries(run(&files, search("*.txt", 200)).await.unwrap());
    let mut names: Vec<&str> = found.iter().map(|e| e.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["deep.txt", "middle.txt", "top.TXT"]);
    assert!(!truncated);
    // Breadth-first: shallower matches come first.
    assert_eq!(found[0].name, "top.TXT");

    let (capped, truncated) = entries(run(&files, search("*.txt", 2)).await.unwrap());
    assert_eq!(capped.len(), 2);
    assert!(truncated);

    let (pngs, _) = entries(run(&files, search("IMAGE.???", 200)).await.unwrap());
    assert_eq!(pngs.len(), 1);

    for bad in [search(r"a\*.txt", 10), search("*.txt", 0)] {
        let err = run(&files, bad).await.unwrap_err();
        assert_eq!(err.code(), ErrorCode::InvalidRequest);
    }
}

#[tokio::test]
async fn known_folders_resolve() {
    let files = LocalFiles::new();
    for name in [
        "Desktop",
        "documents",
        "DOWNLOADS",
        "Profile",
        "Home",
        "AppData",
        "Temp",
    ] {
        let path = path_of(
            run(
                &files,
                FileOperation::KnownFolder {
                    name: name.to_owned(),
                },
            )
            .await
            .unwrap(),
        );
        assert!(path.is_absolute(), "{name}: {}", path.display());
        assert!(!path.to_string_lossy().starts_with(r"\\?\"), "{name}");
    }
    let err = run(
        &files,
        FileOperation::KnownFolder {
            name: "Nowhere".to_owned(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), ErrorCode::InvalidRequest);
    assert!(err.to_string().contains("Documents"));
}

#[tokio::test]
async fn protected_locations_are_refused_without_touching_disk() {
    let files = LocalFiles::new();
    let scratch = Scratch::new();
    let source = scratch.write("payload.txt", "p");
    let windows_dir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".to_owned());
    let program_files =
        std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".to_owned());
    let into_windows = Path::new(&windows_dir).join("winwright-should-not-exist.txt");
    let into_program_files = Path::new(&program_files).join("winwright-should-not-exist.txt");
    let windows_subdir = Path::new(&windows_dir).join("winwright-should-not-exist-dir");
    let drive_root_dir = PathBuf::from(format!(
        "{}\\winwright-should-not-exist-dir",
        &windows_dir[..2]
    ));

    let refusals = [
        FileOperation::Copy {
            from: source.clone(),
            to: into_windows.clone(),
            overwrite: false,
        },
        FileOperation::Move {
            from: source.clone(),
            to: into_program_files.clone(),
            overwrite: true,
        },
        FileOperation::CreateDirectory {
            path: windows_subdir.join("nested"),
        },
        FileOperation::CreateDirectory {
            path: drive_root_dir.clone(),
        },
        FileOperation::Delete {
            path: into_windows.clone(),
        },
        FileOperation::Rename {
            path: into_windows.clone(),
            new_name: "renamed.txt".to_owned(),
        },
    ];
    for op in refusals {
        let label = format!("{op:?}");
        let err = run(&files, op).await.unwrap_err();
        assert_eq!(err.code(), ErrorCode::ActionBlocked, "{label}: {err}");
    }
    assert!(!into_windows.exists());
    assert!(!into_program_files.exists());
    assert!(!windows_subdir.exists());
    assert!(!drive_root_dir.exists());
    assert!(source.exists(), "refused move left the source in place");
    assert!(
        files
            .protected_locations()
            .iter()
            .any(|p| same(p, Path::new(&windows_dir)))
    );

    // Reads are allowed anywhere readable.
    run(
        &files,
        FileOperation::Metadata {
            path: PathBuf::from(&windows_dir),
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn malformed_paths_are_invalid() {
    let files = LocalFiles::new();
    let scratch = Scratch::new();
    let file = scratch.write("f.txt", "x");
    let bad = [
        PathBuf::new(),
        PathBuf::from(r"relative\f.txt"),
        PathBuf::from(r"\\.\PhysicalDrive0"),
        PathBuf::from(r"\\?\GLOBALROOT\Device\HarddiskVolume1"),
        PathBuf::from(format!("{}:stream", file.display())),
        scratch.path("CON"),
        scratch.path(r"..\..\..\..\..\..\..\..\..\..\..\..\x"),
    ];
    for path in bad {
        let label = path.display().to_string();
        let err = run(&files, FileOperation::Metadata { path })
            .await
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::InvalidRequest, "{label}: {err}");
    }
    let err = run(
        &files,
        FileOperation::Metadata {
            path: scratch.path("missing.txt"),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), ErrorCode::InvalidRequest);
}

#[tokio::test]
#[ignore = "needs an interactive desktop"]
async fn delete_moves_to_the_recycle_bin() {
    let files = LocalFiles::new();
    let scratch = Scratch::new();
    let file = scratch.write("recycle-me.txt", "bye");
    let result = run(&files, FileOperation::Delete { path: file.clone() })
        .await
        .unwrap();
    assert_eq!(result, FileResult::Done);
    assert!(!file.exists());
}
