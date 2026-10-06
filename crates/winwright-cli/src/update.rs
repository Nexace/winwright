//! Optional update check (`updates.check`, off by default): while this process leads, it asks
//! GitHub at most once a day for the latest release and, when that is newer than this build,
//! says so in a notification and offers a tray menu item that opens the release page. Failures
//! stay silent (no network, or a private repository answering 404).

use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};

const DAY_MS: u64 = 86_400_000;
/// How often the config is read again, so turning the check on applies within minutes.
const POLL: Duration = Duration::from_secs(600);

/// A release newer than this build.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub tag: String,
    /// Its page on github.com.
    pub url: String,
}

impl Release {
    /// `v0.3.0`.
    pub fn label(&self) -> String {
        format!("v{}", self.tag.trim_start_matches('v'))
    }
}

/// What the last check found, kept between processes (any of them may lead next).
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct State {
    checked_ms: u64,
    latest: Option<Release>,
}

static AVAILABLE: Mutex<Option<Release>> = Mutex::new(None);

/// The newer release the tray menu offers, if any.
pub fn available() -> Option<Release> {
    AVAILABLE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// Sets what the menu offers; true when that changed.
fn set_available(release: Option<Release>) -> bool {
    let mut slot = AVAILABLE.lock().unwrap_or_else(PoisonError::into_inner);
    let changed = *slot != release;
    *slot = release;
    changed
}

/// `/repos/<owner>/<repo>/releases/latest` for a GitHub repository URL.
fn latest_path(repository: &str) -> Option<String> {
    let rest = repository
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .strip_prefix("https://github.com/")?;
    let mut parts = rest.split('/');
    let (owner, repo) = (parts.next()?, parts.next()?);
    (parts.next().is_none() && !owner.is_empty() && !repo.is_empty())
        .then(|| format!("/repos/{owner}/{repo}/releases/latest"))
}

/// `v1.2.3` or `1.2.3` as numbers; `None` for anything else, pre-releases included.
fn version(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.trim().trim_start_matches('v').split('.');
    let v = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(v)
}

fn newer(tag: &str, current: &str) -> bool {
    matches!((version(tag), version(current)), (Some(a), Some(b)) if a > b)
}

/// The release a `releases/latest` answer names, when it is newer than `current` and its page
/// is a plain github.com link (it is opened in the browser).
fn release_of(body: &[u8], current: &str) -> Option<Release> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    let tag = v.get("tag_name")?.as_str()?;
    let url = v.get("html_url")?.as_str()?;
    let plain = url.starts_with("https://github.com/") && url.chars().all(|c| c.is_ascii_graphic());
    (plain && newer(tag, current)).then(|| Release {
        tag: tag.to_owned(),
        url: url.to_owned(),
    })
}

fn due(checked_ms: u64, now_ms: u64) -> bool {
    checked_ms > now_ms || now_ms - checked_ms >= DAY_MS
}

fn state_file() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(|d| PathBuf::from(d).join("winwright").join("update-check.json"))
}

fn read_state(file: &std::path::Path) -> State {
    std::fs::read(file)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn write_state(file: &std::path::Path, state: &State) {
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(err) = std::fs::write(file, serde_json::to_vec(state).unwrap_or_default()) {
        tracing::debug!(%err, "update check state not saved");
    }
}

/// Asks GitHub; `Ok(None)` when there is nothing newer (or no public release to see).
fn fetch(path: &str, current: &str) -> Result<Option<Release>, String> {
    let (status, body) = winwright_memory::http::request(
        "GET",
        "api.github.com",
        path,
        "Accept: application/vnd.github+json\r\nX-GitHub-Api-Version: 2022-11-28\r\n",
        &[],
    )?;
    Ok((status == 200)
        .then(|| release_of(&body, current))
        .flatten())
}

/// Starts the check on a thread of its own for the rest of this process. `enabled` reads
/// `updates.check` from the config file each time; `found` hears of each newly found release;
/// `refresh` redraws the tray menu when what it offers changes.
pub fn start(
    enabled: impl Fn() -> bool + Send + 'static,
    found: impl Fn(&Release) + Send + 'static,
    refresh: impl Fn() + Send + 'static,
) {
    let current = env!("CARGO_PKG_VERSION");
    let (Some(path), Some(file)) = (latest_path(env!("CARGO_PKG_REPOSITORY")), state_file()) else {
        return;
    };
    let spawned = std::thread::Builder::new()
        .name("winwright-update-check".into())
        .spawn(move || {
            loop {
                let mut offer = None;
                if enabled() {
                    let mut state = read_state(&file);
                    let now = winwright_core::audit::now_ms();
                    if due(state.checked_ms, now) {
                        // Recorded before asking, so a failing network is asked once a day too.
                        state.checked_ms = now;
                        match fetch(&path, current) {
                            Ok(release) => {
                                if let Some(r) = release
                                    .as_ref()
                                    .filter(|r| state.latest.as_ref() != Some(*r))
                                {
                                    found(r);
                                }
                                state.latest = release;
                            }
                            Err(err) => tracing::debug!(%err, "update check failed"),
                        }
                        write_state(&file, &state);
                    }
                    offer = state.latest.filter(|r| newer(&r.tag, current));
                }
                if set_available(offer) {
                    refresh();
                }
                std::thread::sleep(POLL);
            }
        });
    if let Err(err) = spawned {
        tracing::debug!(%err, "update check not started");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_api_path_comes_from_the_repository_url() {
        assert_eq!(
            latest_path("https://github.com/Nexace/winwright").as_deref(),
            Some("/repos/Nexace/winwright/releases/latest")
        );
        assert_eq!(
            latest_path("https://github.com/Nexace/winwright.git/").as_deref(),
            Some("/repos/Nexace/winwright/releases/latest")
        );
        assert!(latest_path(env!("CARGO_PKG_REPOSITORY")).is_some());
        for url in [
            "",
            "https://gitlab.com/a/b",
            "https://github.com/a",
            "https://github.com/a/b/c",
        ] {
            assert_eq!(latest_path(url), None, "{url}");
        }
    }

    #[test]
    fn only_newer_plain_releases_count() {
        assert!(newer("v0.3.0", "0.2.0") && newer("0.2.10", "0.2.9") && newer("v1.0.0", "0.9.9"));
        assert!(!newer("v0.2.0", "0.2.0") && !newer("v0.1.9", "0.2.0"));
        assert!(
            !newer("v0.3.0-beta.1", "0.2.0"),
            "pre-releases are not offered"
        );
        assert!(!newer("latest", "0.2.0"));
        let body = |tag: &str, url: &str| format!(r#"{{"tag_name":"{tag}","html_url":"{url}"}}"#);
        let page = "https://github.com/Nexace/winwright/releases/tag/v0.3.0";
        assert_eq!(
            release_of(body("v0.3.0", page).as_bytes(), "0.2.0"),
            Some(Release {
                tag: "v0.3.0".into(),
                url: page.into()
            })
        );
        assert_eq!(release_of(body("v0.2.0", page).as_bytes(), "0.2.0"), None);
        for url in [
            "https://evil.example/x",
            "https://github.com/a b",
            "file:///C:/x",
        ] {
            assert_eq!(
                release_of(body("v9.0.0", url).as_bytes(), "0.2.0"),
                None,
                "{url}"
            );
        }
        assert_eq!(release_of(b"not json", "0.2.0"), None);
        assert_eq!(
            Release {
                tag: "0.3.0".into(),
                url: page.into()
            }
            .label(),
            "v0.3.0"
        );
    }

    #[test]
    fn checks_are_at_most_daily() {
        let now = 10 * DAY_MS;
        assert!(due(0, now), "never checked");
        assert!(!due(now - DAY_MS + 1, now));
        assert!(due(now - DAY_MS, now));
        assert!(
            due(now + 5_000, now),
            "a clock set back does not stop checks for good"
        );
    }

    #[test]
    fn state_survives_a_round_trip_and_bad_files() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-scratch")
            .join(format!("cli-update-{}", std::process::id()));
        let file = dir.join("update-check.json");
        assert_eq!(read_state(&file), State::default());
        let state = State {
            checked_ms: 42,
            latest: Some(Release {
                tag: "v0.3.0".into(),
                url: "https://github.com/x/y/releases/tag/v0.3.0".into(),
            }),
        };
        write_state(&file, &state);
        assert_eq!(read_state(&file), state);
        std::fs::write(&file, "garbage").unwrap();
        assert_eq!(read_state(&file), State::default());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
