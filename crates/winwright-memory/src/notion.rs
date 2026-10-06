//! Copies a report to Notion as a page under one parent page, through Notion's REST API with
//! an "API token" connection (an internal integration), sent through WinHTTP (`crate::http`).
//! Errors say what the person should fix and never hold the token.

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
    let (status, response) = crate::http::request(
        "POST",
        "api.notion.com",
        "/v1/pages",
        &headers,
        body.as_bytes(),
    )?;
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
