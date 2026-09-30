use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::element::ElementInfo;
use crate::window::WindowSelector;

/// What to snapshot. Serialized as `"active"`, `"all-windows"`, `{"window": {...}}`,
/// or `{"subtree": {"ref": "e12"}}`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SnapshotTarget {
    #[default]
    Active,
    AllWindows,
    Window(WindowSelector),
    Subtree {
        #[serde(rename = "ref")]
        reference: String,
    },
}

fn yes() -> bool {
    true
}
fn default_max_depth() -> u32 {
    12
}
fn default_max_nodes() -> u32 {
    500
}
fn default_max_list_items() -> u32 {
    20
}

/// Snapshot options (spec §8). Defaults describe the token-efficient view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SnapshotRequest {
    #[serde(default)]
    pub target: SnapshotTarget,
    #[serde(default = "yes")]
    pub interactive_only: bool,
    #[serde(default = "yes")]
    pub include_text: bool,
    #[serde(default)]
    pub include_bounds: bool,
    #[serde(default)]
    pub include_patterns: bool,
    #[serde(default = "default_max_depth")]
    pub max_depth: u32,
    #[serde(default = "default_max_nodes")]
    pub max_nodes: u32,
    #[serde(default)]
    pub include_offscreen: bool,
    /// Children shown per container before `children=N showing=M` truncation.
    #[serde(default = "default_max_list_items")]
    pub max_list_items: u32,
    /// Keep every node, including unnamed layout containers (debugging only).
    #[serde(default)]
    pub raw_debug: bool,
    /// Also return the structured node tree next to the compact text.
    #[serde(default)]
    pub structured: bool,
    /// Return only what changed since this session's previous snapshot of the same windows
    /// (spec §41). The full tree is sent when there is nothing to compare against.
    #[serde(default)]
    pub diff: bool,
}

impl Default for SnapshotRequest {
    fn default() -> Self {
        Self {
            target: SnapshotTarget::Active,
            interactive_only: true,
            include_text: true,
            include_bounds: false,
            include_patterns: false,
            max_depth: default_max_depth(),
            max_nodes: default_max_nodes(),
            include_offscreen: false,
            max_list_items: default_max_list_items(),
            raw_debug: false,
            structured: false,
            diff: false,
        }
    }
}

impl SnapshotRequest {
    pub const MAX_DEPTH_LIMIT: u32 = 64;
    pub const MAX_NODES_LIMIT: u32 = 5_000;

    pub fn validate(&self) -> Result<(), String> {
        if self.max_depth == 0 || self.max_depth > Self::MAX_DEPTH_LIMIT {
            return Err(format!("maxDepth must be 1..={}", Self::MAX_DEPTH_LIMIT));
        }
        if self.max_nodes == 0 || self.max_nodes > Self::MAX_NODES_LIMIT {
            return Err(format!("maxNodes must be 1..={}", Self::MAX_NODES_LIMIT));
        }
        if self.max_list_items == 0 {
            return Err("maxListItems must be at least 1".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WindowSummary {
    #[serde(rename = "ref")]
    pub reference: String,
    pub title: String,
    pub process: String,
}

/// A read-only grid/table cell folded into its row (`Type="File folder"`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CellValue {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotNode {
    #[serde(flatten)]
    pub element: ElementInfo,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cells: Vec<CellValue>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<SnapshotNode>,
    /// Total children when the list was truncated to `maxListItems`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_count: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DesktopSnapshot {
    pub session: String,
    pub generation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_window: Option<WindowSummary>,
    /// Compact text tree (spec §8 example format).
    pub tree: String,
    pub node_count: u32,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nodes: Option<Vec<SnapshotNode>>,
    /// `DIFF s_1 -> s_2` followed by `+`/`-`/`~` lines and focus moves; `tree` is then empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_request_uses_spec_defaults() {
        let req: SnapshotRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(req, SnapshotRequest::default());
        assert!(req.interactive_only && req.include_text);
        assert_eq!((req.max_depth, req.max_nodes), (12, 500));
        req.validate().unwrap();
    }

    #[test]
    fn target_wire_forms() {
        let req: SnapshotRequest = serde_json::from_str(r#"{"target":"active"}"#).unwrap();
        assert_eq!(req.target, SnapshotTarget::Active);
        let req: SnapshotRequest = serde_json::from_str(r#"{"target":"all-windows"}"#).unwrap();
        assert_eq!(req.target, SnapshotTarget::AllWindows);
        let req: SnapshotRequest =
            serde_json::from_str(r#"{"target":{"window":{"title":"Notepad"}}}"#).unwrap();
        assert_eq!(
            req.target,
            SnapshotTarget::Window(WindowSelector {
                title: Some("Notepad".into()),
                ..Default::default()
            })
        );
        let req: SnapshotRequest =
            serde_json::from_str(r#"{"target":{"subtree":{"ref":"e7"}}}"#).unwrap();
        assert_eq!(
            req.target,
            SnapshotTarget::Subtree {
                reference: "e7".into()
            }
        );
    }

    #[test]
    fn unknown_fields_and_bad_limits_are_rejected() {
        assert!(serde_json::from_str::<SnapshotRequest>(r#"{"maxNode": 5}"#).is_err());
        let req = SnapshotRequest {
            max_nodes: 0,
            ..Default::default()
        };
        assert!(req.validate().is_err());
    }
}
