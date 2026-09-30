use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::geometry::PhysicalRect;

fn is_false(b: &bool) -> bool {
    !*b
}

/// A top-level window. `hwnd` is an opaque number for correlation, not a capability:
/// every use is revalidated against process identity inside the platform layer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WindowInfo {
    pub hwnd: u64,
    pub title: String,
    pub class_name: String,
    pub process_id: u32,
    /// Executable file name (`notepad.exe`); empty when the process cannot be queried.
    pub process_name: String,
    pub bounds: PhysicalRect,
    #[serde(default, skip_serializing_if = "is_false")]
    pub minimized: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub maximized: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub foreground: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub topmost: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_hwnd: Option<u64>,
}

/// How a caller names a window. All supplied fields must match.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WindowSelector {
    /// Case-insensitive substring of the title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Case-insensitive executable name, with or without `.exe`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hwnd: Option<u64>,
}

impl WindowSelector {
    pub fn is_empty(&self) -> bool {
        self.title.is_none() && self.process.is_none() && self.hwnd.is_none()
    }

    pub fn matches(&self, w: &WindowInfo) -> bool {
        if self.hwnd.is_some_and(|h| h != w.hwnd) {
            return false;
        }
        if let Some(title) = &self.title
            && !w.title.to_lowercase().contains(&title.to_lowercase())
        {
            return false;
        }
        if let Some(process) = &self.process {
            let want = process.to_lowercase();
            let want = want.strip_suffix(".exe").unwrap_or(&want);
            let have = w.process_name.to_lowercase();
            let have = have.strip_suffix(".exe").unwrap_or(&have);
            if want != have {
                return false;
            }
        }
        true
    }

    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(t) = &self.title {
            parts.push(format!("title~{t:?}"));
        }
        if let Some(p) = &self.process {
            parts.push(format!("process={p}"));
        }
        if let Some(h) = self.hwnd {
            parts.push(format!("hwnd={h:#x}"));
        }
        if parts.is_empty() {
            "any window".into()
        } else {
            parts.join(" ")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notepad() -> WindowInfo {
        WindowInfo {
            hwnd: 0x1234,
            title: "Untitled - Notepad".into(),
            class_name: "Notepad".into(),
            process_id: 42,
            process_name: "Notepad.exe".into(),
            bounds: PhysicalRect::new(0, 0, 800, 600),
            minimized: false,
            maximized: false,
            foreground: true,
            topmost: false,
            owner_hwnd: None,
        }
    }

    #[test]
    fn selector_matching() {
        let w = notepad();
        let sel = |title: Option<&str>, process: Option<&str>, hwnd: Option<u64>| WindowSelector {
            title: title.map(Into::into),
            process: process.map(Into::into),
            hwnd,
        };
        assert!(sel(Some("notepad"), None, None).matches(&w));
        assert!(sel(None, Some("notepad"), None).matches(&w));
        assert!(sel(None, Some("NOTEPAD.EXE"), Some(0x1234)).matches(&w));
        assert!(!sel(None, Some("note"), None).matches(&w));
        assert!(!sel(Some("Paint"), None, None).matches(&w));
        assert!(!sel(None, None, Some(1)).matches(&w));
    }

    #[test]
    fn false_flags_are_omitted() {
        let json = serde_json::to_value(notepad()).unwrap();
        assert_eq!(json["foreground"], true);
        assert!(json.get("minimized").is_none());
        assert!(json.get("ownerHwnd").is_none());
    }
}
