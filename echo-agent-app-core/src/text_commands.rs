//! Shared EKO text adapters; all persistence and execution stay with the original services.
pub mod agent_router;
pub mod analysis;
pub mod research;
pub mod task_run_control;

/// Native management-command knowledge retained before legacy text rendering.
/// Result values only: persistence and control dedupe remain in their original owners.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ManagementCommandError {
    #[error("{0}")]
    InvalidInput(String),
    #[error("{0}")]
    ReadFailed(String),
    #[error("{0}")]
    NotAdmitted(String),
    #[error("{0}")]
    Unknown(String),
}

impl ManagementCommandError {
    pub(crate) fn native(error: impl std::fmt::Display, may_mutate: bool) -> Self {
        if may_mutate {
            Self::Unknown(error.to_string())
        } else {
            Self::ReadFailed(error.to_string())
        }
    }

    pub(crate) fn product_io(
        error: crate::product_data_io::ProductDataIoError,
        may_mutate: bool,
    ) -> Self {
        use crate::product_data_io::ProductDataIoError;
        match error {
            error @ ProductDataIoError::Admission { .. } => Self::NotAdmitted(error.to_string()),
            // OwnerClosed is emitted by the result receiver AFTER admission;
            // it is not evidence that the physical mutation never ran.
            error @ (ProductDataIoError::Join { .. } | ProductDataIoError::OwnerClosed { .. }) => {
                Self::native(error, may_mutate)
            }
        }
    }

    pub(crate) fn analysis_control(error: crate::product_data_io::AnalysisRunControlError) -> Self {
        use crate::product_data_io::AnalysisRunControlError;
        match error {
            error @ (AnalysisRunControlError::SupervisorClosed
            | AnalysisRunControlError::AlreadyRunning { .. }
            | AnalysisRunControlError::Busy { .. }) => Self::NotAdmitted(error.to_string()),
            error @ (AnalysisRunControlError::NotFound { .. }
            | AnalysisRunControlError::ReceiptMismatch { .. }) => {
                Self::InvalidInput(error.to_string())
            }
            error => Self::Unknown(error.to_string()),
        }
    }

    pub(crate) fn context(self, prefix: &str) -> Self {
        match self {
            Self::InvalidInput(message) => Self::InvalidInput(format!("{prefix}{message}")),
            Self::ReadFailed(message) => Self::ReadFailed(format!("{prefix}{message}")),
            Self::NotAdmitted(message) => Self::NotAdmitted(format!("{prefix}{message}")),
            Self::Unknown(message) => Self::Unknown(format!("{prefix}{message}")),
        }
    }
}

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
