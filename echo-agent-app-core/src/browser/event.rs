use serde::{Deserialize, Serialize};

use super::session::{BrowserBackend, BrowserObservation, BrowserSession, BrowserTab};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserFrame {
    pub data_url: String,
    pub mime_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BrowserEvent {
    SessionStarted {
        session: BrowserSession,
    },
    TabOpened {
        session_id: String,
        tab: BrowserTab,
    },
    NavigationStarted {
        session_id: String,
        tab_id: String,
        url: String,
    },
    NavigationCompleted {
        session_id: String,
        tab_id: String,
        url: String,
    },
    Snapshot {
        observation: BrowserObservation,
    },
    Screenshot {
        observation: BrowserObservation,
        frame: Option<BrowserFrame>,
    },
    Diagnostic {
        category: String,
        observation: BrowserObservation,
    },
    BackendChanged {
        session_id: String,
        backend: BrowserBackend,
    },
    ConfirmationRequested {
        session_id: String,
        tab_id: String,
        risk: String,
        summary: String,
    },
    ConfirmationResolved {
        session_id: String,
        tab_id: String,
        approved: bool,
    },
    ActionStarted {
        session_id: String,
        tab_id: String,
        action: String,
        run_id: Option<String>,
        turn_id: Option<String>,
        execution_id: Option<String>,
    },
    ActionCompleted {
        session_id: String,
        tab_id: String,
        action: String,
        run_id: Option<String>,
        turn_id: Option<String>,
        execution_id: Option<String>,
    },
    ActionFailed {
        session_id: String,
        tab_id: String,
        action: String,
        run_id: Option<String>,
        turn_id: Option<String>,
        execution_id: Option<String>,
        error: String,
    },
    SessionClosed {
        session_id: String,
    },
}

impl BrowserEvent {
    pub fn name(&self) -> &'static str {
        match self {
            Self::SessionStarted { .. } => "browser_session_started",
            Self::TabOpened { .. } => "browser_tab_opened",
            Self::NavigationStarted { .. } => "browser_navigation_started",
            Self::NavigationCompleted { .. } => "browser_navigation_completed",
            Self::Snapshot { .. } => "browser_snapshot",
            Self::Screenshot { .. } => "browser_screenshot",
            Self::Diagnostic { .. } => "browser_diagnostic",
            Self::BackendChanged { .. } => "browser_backend_changed",
            Self::ConfirmationRequested { .. } => "browser_confirmation_requested",
            Self::ConfirmationResolved { .. } => "browser_confirmation_resolved",
            Self::ActionStarted { .. } => "browser_action_started",
            Self::ActionCompleted { .. } => "browser_action_completed",
            Self::ActionFailed { .. } => "browser_action_failed",
            Self::SessionClosed { .. } => "browser_session_closed",
        }
    }
}
