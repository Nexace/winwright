//! Security DTOs (spec §24, §66). Evaluation lives in `winwright-security`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum ActionRisk {
    ReadOnly,
    Normal,
    Sensitive,
    Destructive,
    Privileged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum PermissionDecision {
    Allow,
    Confirm,
    Deny,
}

/// Coarse capability a tool call needs. Risky ones are default-deny.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Capability {
    Observe,
    Capture,
    Interact,
    PhysicalInput,
    WindowControl,
    ProcessLaunch,
    ProcessTerminate,
    FileRead,
    FileWrite,
    FileDelete,
    Shell,
    PowerShell,
    ClipboardSecret,
    ReadSensitive,
    Elevated,
}

/// Target description used for policy and audit. Never holds field values.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TargetSummary {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Produced by every MCP/API/CLI/workflow call before execution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProposedAction {
    pub tool: String,
    pub capability: Capability,
    pub risk: ActionRisk,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<TargetSummary>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PolicyVerdict {
    pub decision: PermissionDecision,
    pub reason: String,
}
