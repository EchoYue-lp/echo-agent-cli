//! EKO's product-specific command policy over framework permission modes.

use echo_agent::tools::{CommandPolicy, CommandPolicyDecision};

/// EKO's local-assistant command policy.
///
/// Agent automation approval remains owned by `PermissionService`; this policy
/// keeps the framework shell classifier permissive for unknown local programs
/// while preserving its dangerous-command and approval classifications.
pub struct EkoCommandPolicy;

impl CommandPolicy for EkoCommandPolicy {
    fn evaluate(&self, command: &str) -> CommandPolicyDecision {
        echo_agent::tools::shell::StandardCommandPolicy::permissive()
            .with_sandbox(true)
            .evaluate(command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::tools::permission::PermissionMode;

    #[test]
    fn every_framework_mode_round_trips_without_loss() {
        let modes = [
            PermissionMode::Default,
            PermissionMode::Plan,
            PermissionMode::AcceptEdits,
            PermissionMode::BypassPermissions,
            PermissionMode::Auto,
            PermissionMode::Bubble,
            PermissionMode::DontAsk,
            PermissionMode::StrictConfirm,
        ];
        for mode in modes {
            assert_eq!(mode.id().parse::<PermissionMode>(), Ok(mode));
            assert_eq!(mode.to_string(), mode.id());
        }
    }

    #[test]
    fn unknown_mode_is_rejected() {
        assert!("surprise".parse::<PermissionMode>().is_err());
    }

    #[test]
    fn eko_command_policy_preserves_sandbox_shell_syntax() {
        assert_eq!(
            EkoCommandPolicy.evaluate("echo hello | cat"),
            CommandPolicyDecision::Safe
        );
    }
}
