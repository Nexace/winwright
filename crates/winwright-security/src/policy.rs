use winwright_contracts::config::{ConfirmationMode, SecurityConfig};
use winwright_contracts::security::{
    ActionRisk, Capability, PermissionDecision, PolicyVerdict, ProposedAction,
};

/// Evaluates a [`ProposedAction`] against the user's configuration.
///
/// Model-supplied flags never reach this function as approval: `Confirm` can only be
/// satisfied by trusted local UI or an explicit user-controlled CLI flow.
#[derive(Clone, Debug, Default)]
pub struct Policy {
    config: SecurityConfig,
}

fn verdict(decision: PermissionDecision, reason: impl Into<String>) -> PolicyVerdict {
    PolicyVerdict {
        decision,
        reason: reason.into(),
    }
}

impl Policy {
    pub fn new(config: SecurityConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &SecurityConfig {
        &self.config
    }

    pub fn evaluate(&self, action: &ProposedAction) -> PolicyVerdict {
        use PermissionDecision::{Allow, Confirm, Deny};

        // Capabilities that are blocked or gated regardless of the target.
        match action.capability {
            Capability::ReadSensitive if self.config.block_password_read => {
                return verdict(Deny, "reading sensitive fields is blocked");
            }
            Capability::Elevated => {
                return verdict(
                    Deny,
                    "elevation requires the separately launched elevated helper",
                );
            }
            Capability::ClipboardSecret if !self.config.allow_clipboard_secrets => {
                return verdict(Deny, "placing secrets on the clipboard is disabled");
            }
            Capability::Shell if !self.config.allow_shell => {
                return verdict(Deny, "shell execution is disabled (security.allowShell)");
            }
            Capability::PowerShell if !self.config.allow_powershell => {
                return verdict(
                    Deny,
                    "PowerShell execution is disabled (security.allowPowershell)",
                );
            }
            Capability::Shell | Capability::PowerShell => {
                return verdict(Confirm, "shell execution always needs user confirmation");
            }
            Capability::FileDelete | Capability::ProcessTerminate => {
                return verdict(Confirm, "destructive operation needs user confirmation");
            }
            _ => {}
        }

        match action.risk {
            ActionRisk::ReadOnly => verdict(Allow, "read-only"),
            ActionRisk::Normal => match self.config.confirmation_mode {
                ConfirmationMode::Balanced | ConfirmationMode::Relaxed => {
                    verdict(Allow, "ordinary interaction")
                }
                ConfirmationMode::Strict => {
                    verdict(Confirm, "strict mode confirms every state change")
                }
            },
            ActionRisk::Sensitive => match self.config.confirmation_mode {
                ConfirmationMode::Relaxed => verdict(Allow, "relaxed mode lets sends through"),
                ConfirmationMode::Balanced | ConfirmationMode::Strict => {
                    verdict(Confirm, "action may send, submit, or move things")
                }
            },
            ActionRisk::Destructive => {
                verdict(Confirm, "action may delete, spend, or change security")
            }
            ActionRisk::Privileged => verdict(Deny, "privileged actions are blocked by default"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(capability: Capability, risk: ActionRisk) -> ProposedAction {
        ProposedAction {
            tool: "test".into(),
            capability,
            risk,
            target: None,
        }
    }

    fn decide(policy: &Policy, capability: Capability, risk: ActionRisk) -> PermissionDecision {
        policy.evaluate(&action(capability, risk)).decision
    }

    #[test]
    fn defaults_are_conservative() {
        use ActionRisk::*;
        use PermissionDecision::*;
        let p = Policy::default();
        assert_eq!(decide(&p, Capability::Observe, ReadOnly), Allow);
        assert_eq!(decide(&p, Capability::Interact, Normal), Allow);
        assert_eq!(decide(&p, Capability::Interact, Sensitive), Confirm);
        assert_eq!(decide(&p, Capability::Interact, Destructive), Confirm);
        assert_eq!(decide(&p, Capability::Interact, Privileged), Deny);
        assert_eq!(decide(&p, Capability::ReadSensitive, ReadOnly), Deny);
        assert_eq!(decide(&p, Capability::Shell, ReadOnly), Deny);
        assert_eq!(decide(&p, Capability::PowerShell, ReadOnly), Deny);
        assert_eq!(decide(&p, Capability::ClipboardSecret, Normal), Deny);
        assert_eq!(decide(&p, Capability::Elevated, Normal), Deny);
        assert_eq!(decide(&p, Capability::FileDelete, Normal), Confirm);
        assert_eq!(decide(&p, Capability::ProcessTerminate, Normal), Confirm);
    }

    #[test]
    fn enabling_shell_still_requires_confirmation() {
        let p = Policy::new(SecurityConfig {
            allow_shell: true,
            ..Default::default()
        });
        assert_eq!(
            decide(&p, Capability::Shell, ActionRisk::ReadOnly),
            PermissionDecision::Confirm
        );
    }

    #[test]
    fn strict_mode_confirms_normal_interaction() {
        let p = Policy::new(SecurityConfig {
            confirmation_mode: ConfirmationMode::Strict,
            ..Default::default()
        });
        assert_eq!(
            decide(&p, Capability::Interact, ActionRisk::Normal),
            PermissionDecision::Confirm
        );
        assert_eq!(
            decide(&p, Capability::Observe, ActionRisk::ReadOnly),
            PermissionDecision::Allow
        );
    }

    #[test]
    fn relaxed_mode_asks_only_before_what_cannot_be_undone() {
        use ActionRisk::*;
        use PermissionDecision::*;
        let p = Policy::new(SecurityConfig {
            confirmation_mode: ConfirmationMode::Relaxed,
            ..Default::default()
        });
        assert_eq!(decide(&p, Capability::Interact, Normal), Allow);
        assert_eq!(decide(&p, Capability::Interact, Sensitive), Allow);
        assert_eq!(decide(&p, Capability::FileWrite, Sensitive), Allow);
        assert_eq!(decide(&p, Capability::Interact, Destructive), Confirm);
        assert_eq!(decide(&p, Capability::FileDelete, Normal), Confirm);
        assert_eq!(decide(&p, Capability::ProcessTerminate, Normal), Confirm);
        assert_eq!(decide(&p, Capability::Interact, Privileged), Deny);
        assert_eq!(decide(&p, Capability::Shell, Normal), Deny);
        assert_eq!(decide(&p, Capability::PowerShell, Normal), Deny);
        assert_eq!(decide(&p, Capability::ReadSensitive, ReadOnly), Deny);
        let shell = Policy::new(SecurityConfig {
            confirmation_mode: ConfirmationMode::Relaxed,
            allow_shell: true,
            allow_powershell: true,
            ..Default::default()
        });
        assert_eq!(decide(&shell, Capability::Shell, Sensitive), Confirm);
        assert_eq!(decide(&shell, Capability::PowerShell, Sensitive), Confirm);
    }

    #[test]
    fn elevation_cannot_be_configured_on() {
        let p = Policy::new(SecurityConfig {
            allow_shell: true,
            allow_powershell: true,
            allow_clipboard_secrets: true,
            block_password_read: false,
            ..Default::default()
        });
        assert_eq!(
            decide(&p, Capability::Elevated, ActionRisk::Normal),
            PermissionDecision::Deny
        );
    }
}
