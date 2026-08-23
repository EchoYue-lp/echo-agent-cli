//! Lossless EKO permission-mode wire adapter.

use echo_agent::tools::permission::PermissionMode;
use echo_agent::tools::{CommandPolicy, CommandPolicyDecision};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionModeDto {
    Default,
    Plan,
    AutoEdit,
    FullAuto,
    Auto,
    Bubble,
    DontAsk,
    Strict,
}

impl PermissionModeDto {
    pub const fn id(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Plan => "plan",
            Self::AutoEdit => "auto-edit",
            Self::FullAuto => "full-auto",
            Self::Auto => "auto",
            Self::Bubble => "bubble",
            Self::DontAsk => "dont-ask",
            Self::Strict => "strict",
        }
    }
}

impl TryFrom<&str> for PermissionModeDto {
    type Error = String;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value.trim().to_ascii_lowercase().as_str() {
            "default" | "ask" => Ok(Self::Default),
            "plan" => Ok(Self::Plan),
            "auto-edit" | "autoedit" | "accept-edits" | "acceptedits" => Ok(Self::AutoEdit),
            "full-auto" | "fullauto" | "bypass" | "bypass-permissions" | "bypasspermissions" => {
                Ok(Self::FullAuto)
            }
            "auto" => Ok(Self::Auto),
            "bubble" => Ok(Self::Bubble),
            "dont-ask" | "dontask" => Ok(Self::DontAsk),
            "strict" | "strict-confirm" | "strict-confirmation" => Ok(Self::Strict),
            other => Err(format!(
                "invalid permission mode '{other}'; expected default, plan, auto-edit, full-auto, auto, bubble, dont-ask, or strict"
            )),
        }
    }
}

impl From<PermissionModeDto> for PermissionMode {
    fn from(value: PermissionModeDto) -> Self {
        match value {
            PermissionModeDto::Default => Self::Default,
            PermissionModeDto::Plan => Self::Plan,
            PermissionModeDto::AutoEdit => Self::AcceptEdits,
            PermissionModeDto::FullAuto => Self::BypassPermissions,
            PermissionModeDto::Auto => Self::Auto,
            PermissionModeDto::Bubble => Self::Bubble,
            PermissionModeDto::DontAsk => Self::DontAsk,
            PermissionModeDto::Strict => Self::StrictConfirm,
        }
    }
}

impl From<PermissionMode> for PermissionModeDto {
    fn from(value: PermissionMode) -> Self {
        match value {
            PermissionMode::Default => Self::Default,
            PermissionMode::Plan => Self::Plan,
            PermissionMode::AcceptEdits => Self::AutoEdit,
            PermissionMode::BypassPermissions => Self::FullAuto,
            PermissionMode::Auto => Self::Auto,
            PermissionMode::Bubble => Self::Bubble,
            PermissionMode::DontAsk => Self::DontAsk,
            PermissionMode::StrictConfirm => Self::Strict,
        }
    }
}

pub fn parse_permission_mode(value: &str) -> Result<PermissionMode, String> {
    PermissionModeDto::try_from(value).map(Into::into)
}

pub fn permission_mode_id(value: PermissionMode) -> &'static str {
    PermissionModeDto::from(value).id()
}

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
            let dto = PermissionModeDto::from(mode);
            assert_eq!(PermissionMode::from(dto), mode);
            assert_eq!(parse_permission_mode(dto.id()), Ok(mode));
        }
    }

    #[test]
    fn unknown_mode_is_rejected() {
        assert!(parse_permission_mode("surprise").is_err());
    }

    #[test]
    fn eko_command_policy_preserves_sandbox_shell_syntax() {
        assert_eq!(
            EkoCommandPolicy.evaluate("echo hello | cat"),
            CommandPolicyDecision::Safe
        );
    }
}
