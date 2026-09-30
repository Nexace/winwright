//! Fixture launch helpers for Winwright's live-desktop tests (spec §50, §52): build, start,
//! observe, and reliably kill the controlled fixture applications.
//!
//! Anything that calls [`FixtureProcess::launch`] touches the real desktop, so such tests
//! must be `#[ignore = "needs an interactive desktop"]` and run with
//! `-- --ignored --test-threads=1`.

use std::ffi::c_void;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
};
use windows::core::BOOL;

const LAUNCH_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The controlled fixture applications in `fixtures/`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Fixture {
    /// Standard Win32 controls with fixed control IDs (`fixtures/test-win32-app`).
    Win32,
    /// Custom-drawn window with no accessible children (`fixtures/test-custom-canvas`).
    Canvas,
}

impl Fixture {
    /// Cargo package (and binary) name.
    pub fn package(self) -> &'static str {
        match self {
            Fixture::Win32 => "winwright-fixture-win32",
            Fixture::Canvas => "winwright-fixture-canvas",
        }
    }

    /// The title the fixture uses when started without `--title`.
    pub fn default_title(self) -> &'static str {
        match self {
            Fixture::Win32 => "Winwright Fixture",
            Fixture::Canvas => "Winwright Canvas",
        }
    }

    /// Path of the fixture executable in the running test's target profile directory.
    ///
    /// The first call per process runs `cargo build -p <package>` into the same target
    /// directory and profile as the test, so the fixture is never stale. If that build fails
    /// (no cargo on PATH, or the binary is locked by a running instance) an existing binary
    /// is used as-is.
    pub fn executable(self) -> io::Result<PathBuf> {
        static BUILDS: [OnceLock<Result<PathBuf, String>>; 2] = [OnceLock::new(), OnceLock::new()];
        let slot = match self {
            Fixture::Win32 => &BUILDS[0],
            Fixture::Canvas => &BUILDS[1],
        };
        slot.get_or_init(|| build(self))
            .clone()
            .map_err(io::Error::other)
    }
}

/// A running fixture instance. Dropping it kills the process and waits for it to exit.
#[derive(Debug)]
pub struct FixtureProcess {
    pub pid: u32,
    /// The fixture's main top-level window.
    pub hwnd: u64,
    /// The unique title the fixture was started with.
    pub title: String,
    child: Child,
}

impl FixtureProcess {
    /// Builds the fixture if needed, starts it with a unique `--title` (for example
    /// `Winwright Fixture 3a0f0002`), and waits up to 10 s for its top-level window.
    ///
    /// Only the fixture's own launch behaviour affects focus; this function never
    /// activates or moves any window.
    pub fn launch(fixture: Fixture) -> io::Result<Self> {
        let exe = fixture.executable()?;
        let title = unique_title(fixture.default_title());
        let child = Command::new(&exe)
            .arg("--title")
            .arg(&title)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()?;
        // Owned from here on, so every early return below kills the child in `Drop`.
        let mut process = Self {
            pid: child.id(),
            hwnd: 0,
            title,
            child,
        };
        let deadline = Instant::now() + LAUNCH_TIMEOUT;
        loop {
            if let Some(hwnd) = find_window(process.pid, &process.title) {
                process.hwnd = hwnd.0 as usize as u64;
                return Ok(process);
            }
            if let Some(status) = process.child.try_wait()? {
                return Err(io::Error::other(format!(
                    "{} exited during startup: {status}",
                    exe.display()
                )));
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "no window titled {:?} from pid {} within {LAUNCH_TIMEOUT:?}",
                        process.title, process.pid
                    ),
                ));
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    /// The main window's current title (the canvas fixture reports every input here).
    pub fn window_title(&self) -> String {
        window_text(HWND(self.hwnd as usize as *mut c_void))
    }

    /// Polls the title until `pred` accepts it, returning that title, or `None` on timeout.
    pub fn wait_for_title(&self, pred: impl Fn(&str) -> bool, timeout: Duration) -> Option<String> {
        let deadline = Instant::now() + timeout;
        loop {
            let title = self.window_title();
            if pred(&title) {
                return Some(title);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }
}

impl Drop for FixtureProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `<prefix> <hex>`: the low half mixes pid and clock (distinct across processes), the high
/// half is a per-process counter (distinct within one).
fn unique_title(prefix: &str) -> String {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let salt = (std::process::id() ^ (nanos >> 8)) as u16;
    format!("{prefix} {salt:04x}{:04x}", count as u16)
}

fn build(fixture: Fixture) -> Result<PathBuf, String> {
    let test_exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let layout = TargetLayout::from_exe(&test_exe).ok_or_else(|| {
        format!(
            "cannot infer the cargo target dir from {}",
            test_exe.display()
        )
    })?;
    let exe = layout
        .profile_dir
        .join(format!("{}.exe", fixture.package()));
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut cargo = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    cargo
        .current_dir(workspace)
        .args(["build", "--quiet", "--package", fixture.package()])
        .arg("--target-dir")
        .arg(&layout.target_dir)
        .args(layout.cargo_args())
        .stdin(Stdio::null());
    match cargo.output() {
        Ok(out) if out.status.success() => {}
        failed if exe.is_file() => {
            let reason = match failed {
                Ok(out) => String::from_utf8_lossy(&out.stderr).trim().to_owned(),
                Err(err) => err.to_string(),
            };
            eprintln!(
                "warning: building {} failed ({reason}); using existing {}",
                fixture.package(),
                exe.display()
            );
        }
        Ok(out) => {
            return Err(format!(
                "cargo build -p {} failed: {}",
                fixture.package(),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Err(err) => return Err(format!("cannot run cargo: {err}")),
    }
    if exe.is_file() {
        Ok(exe)
    } else {
        Err(format!("{} is missing after the build", exe.display()))
    }
}

/// Where cargo put the running test: `<target>/[<triple>/]<profile>/{deps,examples}/x.exe`.
#[derive(Debug, PartialEq, Eq)]
struct TargetLayout {
    target_dir: PathBuf,
    triple: Option<String>,
    profile: String,
    profile_dir: PathBuf,
}

impl TargetLayout {
    fn from_exe(exe: &Path) -> Option<Self> {
        let mut profile_dir = exe.parent()?;
        if matches!(profile_dir.file_name()?.to_str()?, "deps" | "examples") {
            profile_dir = profile_dir.parent()?;
        }
        let profile = profile_dir.file_name()?.to_str()?.to_owned();
        let parent = profile_dir.parent()?;
        let parent_name = parent.file_name()?.to_str()?;
        let (target_dir, triple) = if parent_name.contains("-windows-") {
            (parent.parent()?, Some(parent_name.to_owned()))
        } else {
            (parent, None)
        };
        Some(Self {
            target_dir: target_dir.to_path_buf(),
            triple,
            profile,
            profile_dir: profile_dir.to_path_buf(),
        })
    }

    /// Profile and target flags that make `cargo build` write into `profile_dir`.
    fn cargo_args(&self) -> Vec<&str> {
        let mut args = match self.profile.as_str() {
            "debug" => vec![],
            "release" => vec!["--release"],
            other => vec!["--profile", other],
        };
        if let Some(triple) = &self.triple {
            args.extend(["--target", triple]);
        }
        args
    }
}

unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` is the `&mut Vec<HWND>` passed by `top_level_windows`, alive for the
    // duration of the synchronous EnumWindows call.
    let out = unsafe { &mut *(lparam.0 as *mut Vec<HWND>) };
    out.push(hwnd);
    BOOL::from(true)
}

fn top_level_windows() -> Vec<HWND> {
    let mut out: Vec<HWND> = Vec::with_capacity(256);
    // SAFETY: `collect` only writes to `out`, which outlives this synchronous call.
    let _ = unsafe { EnumWindows(Some(collect), LPARAM(&mut out as *mut Vec<HWND> as isize)) };
    out
}

fn find_window(pid: u32, title: &str) -> Option<HWND> {
    top_level_windows().into_iter().find(|&hwnd| {
        let mut owner = 0u32;
        // SAFETY: `owner` is a valid out pointer; stale handles leave it 0.
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut owner)) };
        // SAFETY: takes the HWND by value; stale handles report false.
        owner == pid && unsafe { IsWindowVisible(hwnd) }.as_bool() && window_text(hwnd) == title
    })
}

fn window_text(hwnd: HWND) -> String {
    // For another process's window this reads the cached caption and never sends
    // WM_GETTEXT, so a hung fixture cannot block the test.
    // SAFETY: takes the HWND by value; stale handles report 0.
    let len = unsafe { GetWindowTextLengthW(hwnd) };
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len as usize + 1];
    // SAFETY: `buf` is writable and the API writes at most `buf.len()` units.
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

#[cfg(test)]
mod tests {
    use windows::Win32::UI::WindowsAndMessaging::IsWindow;

    use super::*;

    #[test]
    fn layout_from_debug_deps() {
        let layout = TargetLayout::from_exe(Path::new(r"C:\w\target\debug\deps\t-1a2b.exe"));
        assert_eq!(
            layout,
            Some(TargetLayout {
                target_dir: PathBuf::from(r"C:\w\target"),
                triple: None,
                profile: "debug".into(),
                profile_dir: PathBuf::from(r"C:\w\target\debug"),
            })
        );
        assert!(layout.unwrap().cargo_args().is_empty());
    }

    #[test]
    fn layout_with_triple_and_release() {
        let layout = TargetLayout::from_exe(Path::new(
            r"C:\w\target\x86_64-pc-windows-msvc\release\deps\t.exe",
        ))
        .unwrap();
        assert_eq!(layout.target_dir, PathBuf::from(r"C:\w\target"));
        assert_eq!(
            layout.cargo_args(),
            ["--release", "--target", "x86_64-pc-windows-msvc"]
        );
    }

    #[test]
    fn layout_with_custom_profile_example() {
        let layout =
            TargetLayout::from_exe(Path::new(r"D:\out\ci-fast\examples\demo.exe")).unwrap();
        assert_eq!(layout.target_dir, PathBuf::from(r"D:\out"));
        assert_eq!(layout.profile_dir, PathBuf::from(r"D:\out\ci-fast"));
        assert_eq!(layout.cargo_args(), ["--profile", "ci-fast"]);
    }

    #[test]
    fn titles_are_prefixed_and_unique() {
        let a = unique_title("Winwright Fixture");
        let b = unique_title("Winwright Fixture");
        assert_ne!(a, b);
        let suffix = a.strip_prefix("Winwright Fixture ").unwrap();
        assert_eq!(suffix.len(), 8);
        assert!(suffix.chars().all(|c| c.is_ascii_hexdigit()));
    }

    fn window_gone(hwnd: u64) -> bool {
        let hwnd = HWND(hwnd as usize as *mut c_void);
        let deadline = Instant::now() + Duration::from_secs(2);
        // SAFETY: takes the HWND by value; destroyed windows report false.
        while unsafe { IsWindow(Some(hwnd)) }.as_bool() {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(POLL_INTERVAL);
        }
        true
    }

    fn launch_and_check(fixture: Fixture) {
        let process = FixtureProcess::launch(fixture).expect("launch fixture");
        assert!(
            process
                .title
                .starts_with(&format!("{} ", fixture.default_title()))
        );
        assert_ne!(process.hwnd, 0);
        assert_eq!(process.window_title(), process.title);
        let hwnd = process.hwnd;
        drop(process);
        assert!(window_gone(hwnd), "fixture window survived drop");
    }

    #[test]
    #[ignore = "needs an interactive desktop"]
    fn launches_win32_fixture_and_kills_it_on_drop() {
        launch_and_check(Fixture::Win32);
    }

    #[test]
    #[ignore = "needs an interactive desktop"]
    fn launches_canvas_fixture_and_kills_it_on_drop() {
        launch_and_check(Fixture::Canvas);
    }

    #[test]
    #[ignore = "needs an interactive desktop"]
    fn concurrent_instances_are_distinct() {
        let a = FixtureProcess::launch(Fixture::Win32).expect("launch first");
        let b = FixtureProcess::launch(Fixture::Win32).expect("launch second");
        assert_ne!(a.title, b.title);
        assert_ne!(a.hwnd, b.hwnd);
        assert_ne!(a.pid, b.pid);
    }

    #[test]
    #[ignore = "needs an interactive desktop"]
    fn wait_for_title_matches_or_times_out() {
        let canvas = FixtureProcess::launch(Fixture::Canvas).expect("launch canvas");
        let title = canvas.title.clone();
        assert_eq!(
            canvas.wait_for_title(|t| t == title, Duration::from_secs(1)),
            Some(title)
        );
        let started = Instant::now();
        let never = canvas.wait_for_title(|t| t.ends_with(" - never"), Duration::from_millis(200));
        assert_eq!(never, None);
        assert!(started.elapsed() >= Duration::from_millis(200));
    }
}
