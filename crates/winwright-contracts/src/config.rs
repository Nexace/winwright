//! Configuration shape (spec §46). Every section has conservative defaults.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct Config {
    pub server: ServerConfig,
    pub automation: AutomationConfig,
    pub capture: CaptureConfig,
    pub security: SecurityConfig,
    pub overlay: OverlayConfig,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct ServerConfig {
    pub mcp_stdio: bool,
    pub http: bool,
    pub http_host: String,
    pub http_port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            mcp_stdio: true,
            http: false,
            http_host: "127.0.0.1".into(),
            http_port: 32145,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct AutomationConfig {
    pub default_timeout_ms: u64,
    pub reference_ttl_seconds: u64,
    pub max_snapshot_nodes: u32,
}

impl Default for AutomationConfig {
    fn default() -> Self {
        Self {
            default_timeout_ms: 10_000,
            reference_ttl_seconds: 30,
            max_snapshot_nodes: 500,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct CaptureConfig {
    pub on_demand_only: bool,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            on_demand_only: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ConfirmationMode {
    /// Confirm every state-changing action.
    Strict,
    /// Confirm sensitive/destructive actions only (spec §24 defaults).
    #[default]
    Balanced,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct SecurityConfig {
    pub confirmation_mode: ConfirmationMode,
    pub block_password_read: bool,
    pub allow_shell: bool,
    pub allow_powershell: bool,
    pub allow_clipboard_secrets: bool,
    /// Local audit log of state-changing actions (never typed text or values).
    pub audit: bool,
    /// Seconds an unanswered confirmation dialog waits before denying.
    pub confirmation_timeout_seconds: u64,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            confirmation_mode: ConfirmationMode::Balanced,
            block_password_read: true,
            allow_shell: false,
            allow_powershell: false,
            allow_clipboard_secrets: false,
            audit: true,
            confirmation_timeout_seconds: 60,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct OverlayConfig {
    pub enabled: bool,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_example_parses() {
        let json = r#"{
          "server": {"mcpStdio": true, "http": true, "httpHost": "127.0.0.1", "httpPort": 32145},
          "automation": {"defaultTimeoutMs": 10000, "referenceTtlSeconds": 30, "maxSnapshotNodes": 500},
          "capture": {"onDemandOnly": true},
          "security": {"confirmationMode": "balanced", "blockPasswordRead": true, "allowShell": false},
          "overlay": {"enabled": true}
        }"#;
        let cfg: Config = serde_json::from_str(json).unwrap();
        assert!(cfg.server.http);
        assert_eq!(cfg.security, SecurityConfig::default());
    }

    #[test]
    fn partial_config_keeps_safe_defaults() {
        let cfg: Config =
            serde_json::from_str(r#"{"automation":{"defaultTimeoutMs":5000}}"#).unwrap();
        assert_eq!(cfg.automation.default_timeout_ms, 5000);
        assert_eq!(cfg.automation.reference_ttl_seconds, 30);
        assert!(!cfg.security.allow_shell && cfg.security.block_password_read);
        assert!(!cfg.server.http, "HTTP is opt-in");
    }

    #[test]
    fn typos_are_rejected() {
        assert!(serde_json::from_str::<Config>(r#"{"security":{"allowShel":true}}"#).is_err());
    }
}
