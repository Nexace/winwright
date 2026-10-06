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

/// What the user is asked to approve. Shown verbatim in trusted local UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfirmationPrompt {
    /// e.g. `Click Button "Submit"`.
    pub summary: String,
    pub target: Option<TargetSummary>,
    /// Why confirmation is needed (from the policy verdict).
    pub reason: String,
    /// Unanswered prompts are denied after this long.
    pub timeout_ms: u64,
    /// The app an "allow for a while" answer would cover; `None` offers only "Allow once".
    pub grant: Option<String>,
}

/// How the person answered a confirmation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Approval {
    Denied,
    Once,
    /// This action, and for a while the same kind of action in the prompt's `grant` app.
    ForAWhile,
}

/// Trusted local approval (spec §66). Only the human at the machine can answer; a model can
/// never satisfy a confirmation, and implementations must default to "deny".
pub trait Confirmer: Send + Sync {
    fn confirm<'a>(&'a self, prompt: ConfirmationPrompt)
    -> crate::backend::BackendFuture<'a, bool>;

    /// Like `confirm`, and may answer `ForAWhile` when the prompt offers a grant.
    fn approve<'a>(
        &'a self,
        prompt: ConfirmationPrompt,
    ) -> crate::backend::BackendFuture<'a, Approval> {
        Box::pin(async move {
            Ok(if self.confirm(prompt).await? {
                Approval::Once
            } else {
                Approval::Denied
            })
        })
    }
}
