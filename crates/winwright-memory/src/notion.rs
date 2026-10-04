//! Copies a report to Notion as a page under one parent page, through Notion's REST API with
//! an "API token" connection (an internal integration). WinHTTP does the request, so no HTTP
//! or TLS crates come in. Errors say what the person should fix and never hold the token.

use std::ffi::c_void;

use windows::Win32::Networking::WinHttp::{
    INTERNET_DEFAULT_HTTPS_PORT, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE,
    WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle, WinHttpConnect,
    WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
    WinHttpSendRequest, WinHttpSetTimeouts,
};
use windows::core::{HSTRING, PCWSTR, w};

const VERSION: &str = "2026-03-11";

pub struct Target {
    token: String,
    /// The parent page id as a UUID.
    parent: String,
}

impl Target {
    pub fn from_env(var: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let token = var("WINWRIGHT_NOTION_TOKEN").or_else(|| var("JARVIS_NOTION_TOKEN"))?;
        let parent = var("WINWRIGHT_NOTION_PARENT").or_else(|| var("JARVIS_NOTION_PARENT"))?;
        Some(Self {
            token: token.trim().to_owned(),
            parent: page_id(&parent)?,
        })
    }
}

/// The page id in a Notion link or id (dashes optional), as a UUID.
pub fn page_id(text: &str) -> Option<String> {
    let path = text.trim().split(['?', '#']).next().unwrap_or_default();
    let bare: String = path.chars().filter(|c| *c != '-').collect();
    let tail: String = bare
        .chars()
        .rev()
        .take(32)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if tail.len() != 32 || !tail.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let hex = tail.to_ascii_lowercase();
    Some(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

/// A report as a page: its heading becomes the title, its front matter one italic line.
pub fn page(report: &str) -> (String, String) {
    let (front, body) = report
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n"))
        .unwrap_or(("", report));
    let field = |name: &str| {
        front
            .lines()
            .find_map(|l| l.strip_prefix(name)?.strip_prefix(':'))
            .map(str::trim)
            .unwrap_or_default()
    };
    let mut title = "Task".to_owned();
    let mut rest = Vec::new();
    for line in body.trim().lines() {
        match line.strip_prefix("# ") {
            Some(heading) if title == "Task" && rest.is_empty() => title = heading.trim().into(),
            _ => rest.push(line),
        }
    }
    let mut meta: Vec<&str> = [field("date"), field("outcome"), field("source")]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect();
    if field("outsideContent") == "true" {
        meta.push("read outside content");
    }
    let title: String = title.chars().take(200).collect();
    (
        title,
        format!("_{}_\n\n{}", meta.join(" · "), rest.join("\n").trim()),
    )
}

/// Creates the page; returns its URL.
pub fn create_page(target: &Target, report: &str) -> Result<String, String> {
    let (title, markdown) = page(report);
    let body = serde_json::json!({
        "parent": { "type": "page_id", "page_id": target.parent },
        "properties": { "title": { "title": [ { "type": "text", "text": { "content": title } } ] } },
        "markdown": markdown,
    })
    .to_string();
    let headers = format!(
        "Authorization: Bearer {}\r\nNotion-Version: {VERSION}\r\nContent-Type: application/json\r\n",
        target.token
    );
    let (status, response) = post("api.notion.com", "/v1/pages", &headers, body.as_bytes())?;
    if (200..300).contains(&status) {
        let url = serde_json::from_slice::<serde_json::Value>(&response)
            .ok()
            .and_then(|v| v.get("url")?.as_str().map(str::to_owned))
            .unwrap_or_default();
        return Ok(url);
    }
    Err(match status {
        401 => {
            "Notion answered 401: the token (WINWRIGHT_NOTION_TOKEN) is wrong or was revoked".into()
        }
        403 => "Notion answered 403: let the connection insert content (its capabilities)".into(),
        404 => "Notion answered 404: share the parent page with the connection (••• > Connections)"
            .into(),
        other => format!("Notion answered {other}"),
    })
}

/// Closes a WinHTTP handle when dropped.
struct Handle(*mut c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: a handle WinHTTP returned, closed once.
            let _ = unsafe { WinHttpCloseHandle(self.0) };
        }
    }
}

fn handle(raw: *mut c_void, what: &str) -> Result<Handle, String> {
    if raw.is_null() {
        Err(format!(
            "cannot reach Notion ({what}: {})",
            windows::core::Error::from_thread()
        ))
    } else {
        Ok(Handle(raw))
    }
}

/// One HTTPS POST: the status code and the response body.
fn post(host: &str, path: &str, headers: &str, body: &[u8]) -> Result<(u32, Vec<u8>), String> {
    let fail =
        |what: &str, err: windows::core::Error| format!("cannot reach Notion ({what}: {err})");
    let host = HSTRING::from(host);
    let path = HSTRING::from(path);
    let headers: Vec<u16> = headers.encode_utf16().collect();
    let length = u32::try_from(body.len()).map_err(|_| "report too large".to_owned())?;
    // SAFETY: plain WinHTTP calls in order; every handle is closed by its guard, every buffer
    // outlives the call that uses it, and lengths match the buffers passed.
    unsafe {
        let session = handle(
            WinHttpOpen(
                w!("Winwright"),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            ),
            "open",
        )?;
        WinHttpSetTimeouts(session.0, 10_000, 10_000, 15_000, 15_000)
            .map_err(|e| fail("timeouts", e))?;
        let connect = handle(
            WinHttpConnect(session.0, &host, INTERNET_DEFAULT_HTTPS_PORT, 0),
            "connect",
        )?;
        let request = handle(
            WinHttpOpenRequest(
                connect.0,
                w!("POST"),
                &path,
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                WINHTTP_FLAG_SECURE,
            ),
            "request",
        )?;
        WinHttpSendRequest(
            request.0,
            Some(&headers),
            Some(body.as_ptr().cast()),
            length,
            length,
            0,
        )
        .map_err(|e| fail("send", e))?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut()).map_err(|e| fail("response", e))?;
        let mut status = 0u32;
        let mut size = size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some((&raw mut status).cast()),
            &mut size,
            std::ptr::null_mut(),
        )
        .map_err(|e| fail("status", e))?;
        let mut response = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            let mut read = 0u32;
            WinHttpReadData(
                request.0,
                chunk.as_mut_ptr().cast(),
                chunk.len() as u32,
                &mut read,
            )
            .map_err(|e| fail("read", e))?;
            if read == 0 || response.len() > 1 << 20 {
                break;
            }
            response.extend_from_slice(&chunk[..read as usize]);
        }
        Ok((status, response))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d";
    const UUID: &str = "1a2b3c4d-5e6f-7a8b-9c0d-1e2f3a4b5c6d";

    #[test]
    fn page_ids_come_from_links_and_ids() {
        for text in [
            format!("https://www.notion.so/space/Winwright-reports-{ID}"),
            format!("https://app.notion.com/p/Cafe-{ID}?pvs=4"),
            ID.to_owned(),
            UUID.to_uppercase(),
        ] {
            assert_eq!(page_id(&text).as_deref(), Some(UUID), "{text}");
        }
        for text in ["", "https://www.notion.so/Winwright", "abc123"] {
            assert_eq!(page_id(text), None, "{text}");
        }
    }

    #[test]
    fn settings_come_from_either_name() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        assert!(Target::from_env(env(&[])).is_none());
        assert!(Target::from_env(env(&[("JARVIS_NOTION_TOKEN", "ntn_x")])).is_none());
        let t = Target::from_env(env(&[
            ("JARVIS_NOTION_TOKEN", " ntn_x "),
            ("WINWRIGHT_NOTION_PARENT", ID),
        ]))
        .unwrap();
        assert_eq!((t.token.as_str(), t.parent.as_str()), ("ntn_x", UUID));
    }

    #[test]
    fn a_report_becomes_a_titled_page() {
        let report = "---\ndate: 2026-10-04T08:48:19Z\noutcome: done\noutsideContent: true\nsource: codex\n---\n# Weather in Pune\n\n**Summary:** Sunny.\n";
        let (title, body) = page(report);
        assert_eq!(title, "Weather in Pune");
        assert!(
            body.starts_with("_2026-10-04T08:48:19Z · done · codex · read outside content_\n\n")
        );
        assert!(body.ends_with("**Summary:** Sunny."));
        assert!(!body.contains("# Weather"));
    }
}
