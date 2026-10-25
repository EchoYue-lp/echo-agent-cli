//! Shared EKO text adapters; all persistence and execution stay with the original services.
pub mod agent_router;
pub mod analysis;
pub mod research;
pub mod task_run_control;

pub fn extension_receipt_terminal(
    receipt: &crate::extension_commands::ExtensionCommandReceipt,
) -> crate::chat_driver::TurnOutcome {
    use crate::extension_commands::ExtensionCommandStatus;

    match receipt.status() {
        ExtensionCommandStatus::Settled => crate::chat_driver::TurnOutcome::Completed,
        ExtensionCommandStatus::Committed => {
            crate::chat_driver::TurnOutcome::Failed(echo_agent::error::AgentFailure::message(
                "extension_committed",
                receipt.meta().error.clone().unwrap_or_else(|| {
                    "Extension durable state is committed; runtime settlement is pending"
                        .to_string()
                }),
            ))
        }
        ExtensionCommandStatus::Degraded => {
            crate::chat_driver::TurnOutcome::Failed(echo_agent::error::AgentFailure::message(
                "extension_degraded",
                receipt
                    .meta()
                    .error
                    .clone()
                    .unwrap_or_else(|| "Extension settlement is degraded".to_string()),
            ))
        }
        ExtensionCommandStatus::Failed => {
            crate::chat_driver::TurnOutcome::Failed(echo_agent::error::AgentFailure::message(
                "extension_failed",
                receipt
                    .meta()
                    .error
                    .clone()
                    .unwrap_or_else(|| "Extension command failed".to_string()),
            ))
        }
    }
}
