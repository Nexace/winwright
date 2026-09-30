//! Locator data model (spec §65). Matching lives in `winwright-core`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum MatchMode {
    #[default]
    Exact,
    Contains,
    Regex,
}

fn default_visible_only() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ElementLocator {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub framework_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ancestor: Option<Box<ElementLocator>>,
    #[serde(default, rename = "match")]
    pub match_mode: MatchMode,
    #[serde(default)]
    pub case_sensitive: bool,
    #[serde(default = "default_visible_only")]
    pub visible_only: bool,
    /// Zero-based explicit positional fallback. Never applied implicitly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nth: Option<usize>,
}

impl Default for ElementLocator {
    fn default() -> Self {
        Self {
            role: None,
            name: None,
            text: None,
            automation_id: None,
            class_name: None,
            framework_id: None,
            label: None,
            ancestor: None,
            match_mode: MatchMode::Exact,
            case_sensitive: false,
            visible_only: true,
            nth: None,
        }
    }
}

impl ElementLocator {
    pub const MAX_ANCESTOR_DEPTH: usize = 8;
    pub const MAX_STRING_LEN: usize = 512;
    pub const MAX_NTH: usize = 1_000;

    fn strings(&self) -> impl Iterator<Item = &String> {
        [
            &self.role,
            &self.name,
            &self.text,
            &self.automation_id,
            &self.class_name,
            &self.framework_id,
            &self.label,
        ]
        .into_iter()
        .flatten()
    }

    fn has_predicate(&self) -> bool {
        self.strings().next().is_some() || self.ancestor.is_some()
    }

    /// Enforces bounds on nesting, string size, and positional index.
    pub fn validate(&self) -> Result<(), String> {
        let mut depth = 0;
        let mut current = Some(self);
        while let Some(loc) = current {
            if depth > Self::MAX_ANCESTOR_DEPTH {
                return Err(format!(
                    "ancestor nesting exceeds {}",
                    Self::MAX_ANCESTOR_DEPTH
                ));
            }
            if !loc.has_predicate() {
                return Err("locator needs at least one predicate".into());
            }
            if loc.strings().any(|s| s.len() > Self::MAX_STRING_LEN) {
                return Err(format!(
                    "locator strings are limited to {} bytes",
                    Self::MAX_STRING_LEN
                ));
            }
            if loc.nth.is_some_and(|n| n > Self::MAX_NTH) {
                return Err(format!("nth is limited to {}", Self::MAX_NTH));
            }
            depth += 1;
            current = loc.ancestor.as_deref();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_shape_and_defaults() {
        let loc: ElementLocator = serde_json::from_str(
            r#"{"role":"Button","name":"Save","match":"contains","ancestor":{"role":"Window","name":"Save As"}}"#,
        )
        .unwrap();
        assert_eq!(loc.match_mode, MatchMode::Contains);
        assert!(loc.visible_only && !loc.case_sensitive && loc.nth.is_none());
        assert_eq!(
            loc.ancestor.as_ref().unwrap().name.as_deref(),
            Some("Save As")
        );
        loc.validate().unwrap();
    }

    #[test]
    fn validation_bounds() {
        assert!(ElementLocator::default().validate().is_err());
        let mut deep = ElementLocator {
            role: Some("Button".into()),
            ..Default::default()
        };
        for _ in 0..=ElementLocator::MAX_ANCESTOR_DEPTH {
            deep = ElementLocator {
                role: Some("Pane".into()),
                ancestor: Some(Box::new(deep)),
                ..Default::default()
            };
        }
        assert!(deep.validate().unwrap_err().contains("nesting"));
        let long = ElementLocator {
            name: Some("x".repeat(513)),
            ..Default::default()
        };
        assert!(long.validate().is_err());
        assert!(serde_json::from_str::<ElementLocator>(r#"{"nmae":"x"}"#).is_err());
    }
}
