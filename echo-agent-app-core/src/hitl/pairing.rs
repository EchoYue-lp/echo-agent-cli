//! EKO delivery identity; framework request IDs remain reusable correlations.

use echo_agent::human_loop::{HumanLoopKind, HumanLoopRequest, HumanLoopResponse};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Read-only projection of an existing invocation owner. Providers retain a
/// Weak owner in the closure; this carries no independently mutable scope.
pub type HitlScopeResolver =
    Arc<dyn Fn() -> Result<HitlRequestScope, echo_agent::error::ReactError> + Send + Sync>;

/// The original application owner scope, carried verbatim to reply adapters.
/// Absent coordinates remain absent for standalone providers; real surfaces
/// supply their original runtime/session coordinates rather than inventing them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct HitlRequestScope {
    pub workspace_id: Option<String>,
    pub host_generation: Option<String>,
    pub conversation_id: Option<String>,
    pub root_turn_id: Option<String>,
    pub turn_id: Option<String>,
    pub run_id: Option<String>,
    pub task_id: Option<String>,
    pub channel_id: Option<String>,
    pub channel_conversation_id: Option<String>,
    pub sender_id: Option<String>,
    pub sender_incarnation: Option<String>,
}

impl HitlRequestScope {
    pub fn for_foreground_observer(
        &self,
        observer: &crate::foreground_turn::WeakForegroundTurnControl,
        surface: crate::foreground_turn::ForegroundTurnSurface,
    ) -> Result<Self, echo_agent::error::ReactError> {
        let control = observer.upgrade().ok_or_else(|| {
            echo_agent::error::ReactError::Other(
                "HITL original foreground owner is unavailable".to_string(),
            )
        })?;
        self.for_foreground_turn(&control, surface)
    }

    pub fn for_foreground_turn(
        &self,
        foreground: &crate::foreground_turn::ForegroundTurnControl,
        surface: crate::foreground_turn::ForegroundTurnSurface,
    ) -> Result<Self, echo_agent::error::ReactError> {
        let missing = || {
            echo_agent::error::ReactError::Other(
                "HITL original foreground owner is unavailable".to_string(),
            )
        };
        let workspace = self.workspace_id.as_deref().ok_or_else(missing)?;
        let conversation = self.conversation_id.as_deref().ok_or_else(missing)?;
        let root = self.root_turn_id.as_deref().ok_or_else(missing)?;
        let snapshot = foreground
            .snapshots_for_conversation_scoped(workspace, conversation)
            .map_err(|error| echo_agent::error::ReactError::Other(error.to_string()))?
            .into_iter()
            .find(|snapshot| {
                snapshot.surface == surface
                    && snapshot.root_turn_id == root
                    && !snapshot.cancellation_requested
            })
            .ok_or_else(missing)?;
        let mut scope = self.clone();
        scope.turn_id = Some(snapshot.active_turn_id);
        if scope.run_id.is_none() {
            scope.run_id = snapshot.run_id;
        }
        Ok(scope)
    }

    /// Fill only missing runtime coordinates from the original scoped turn.
    /// A provider's explicit browser/session/turn binding is never rewritten.
    pub fn with_current_runtime(mut self) -> Self {
        if let Some(resources) = crate::chat_resources::current_chat_resources() {
            self.workspace_id
                .get_or_insert_with(|| resources.execution_scope.workspace_id().to_string());
            if let Some(receipt) = resources.workspace_io_receipt.as_ref() {
                self.host_generation
                    .get_or_insert_with(|| receipt.host_generation().to_string());
            }
            if self.conversation_id.is_none() {
                self.conversation_id = resources.conv_id.clone();
            }
            self.root_turn_id
                .get_or_insert_with(|| resources.root_message_id.clone());
            self.turn_id
                .get_or_insert_with(|| resources.root_message_id.clone());
        }
        self
    }

    /// A scoped provider must not consume a pair belonging to another owner.
    pub fn accepts(&self, actual: &Self) -> bool {
        let coordinates = [
            (&self.workspace_id, &actual.workspace_id),
            (&self.host_generation, &actual.host_generation),
            (&self.conversation_id, &actual.conversation_id),
            (&self.root_turn_id, &actual.root_turn_id),
            (&self.turn_id, &actual.turn_id),
            (&self.run_id, &actual.run_id),
            (&self.task_id, &actual.task_id),
            (&self.channel_id, &actual.channel_id),
            (
                &self.channel_conversation_id,
                &actual.channel_conversation_id,
            ),
            (&self.sender_id, &actual.sender_id),
            (&self.sender_incarnation, &actual.sender_incarnation),
        ];
        coordinates
            .into_iter()
            .all(|(expected, actual)| expected.is_none() || expected == actual)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum HitlRequestKind {
    Approval,
    Input,
    Selection,
}

impl From<&HumanLoopKind> for HitlRequestKind {
    fn from(kind: &HumanLoopKind) -> Self {
        match kind {
            HumanLoopKind::Approval => Self::Approval,
            HumanLoopKind::Input => Self::Input,
            HumanLoopKind::Selection => Self::Selection,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct HitlRequestIdentity {
    pub request_id: String,
    pub response_target: String,
    pub scope: HitlRequestScope,
    pub kind: HitlRequestKind,
}

#[derive(Debug, Clone)]
pub struct PairedHumanLoopRequest {
    pub request: HumanLoopRequest,
    pub identity: HitlRequestIdentity,
}

impl PairedHumanLoopRequest {
    pub fn validate(&self) -> Result<(), echo_agent::error::ReactError> {
        if self.identity.response_target.trim().is_empty()
            || self.identity.request_id.trim().is_empty()
            || self.request.request_id.as_deref() != Some(self.identity.request_id.as_str())
            || self.identity.kind != HitlRequestKind::from(&self.request.kind)
        {
            return Err(echo_agent::error::ReactError::Other(
                "Invalid paired EKO HITL identity".to_string(),
            ));
        }
        Ok(())
    }
}

/// Call once inside each actual request future, or once before dispatcher
/// cloning. The paired entry never generates or repairs a response target.
pub fn pair_request(
    mut request: HumanLoopRequest,
    mut scope: HitlRequestScope,
) -> Result<PairedHumanLoopRequest, echo_agent::error::ReactError> {
    let request_id = request
        .request_id
        .get_or_insert_with(|| uuid::Uuid::new_v4().to_string())
        .clone();
    if scope.task_id.is_none() {
        scope.task_id = request.task_id.clone();
    }
    let paired = PairedHumanLoopRequest {
        identity: HitlRequestIdentity {
            request_id,
            response_target: uuid::Uuid::new_v4().to_string(),
            scope,
            kind: HitlRequestKind::from(&request.kind),
        },
        request,
    };
    paired.validate()?;
    Ok(paired)
}

/// EKO adapter entry, deliberately separate from the unchanged framework trait.
pub trait PairedHumanLoopProvider: Send + Sync {
    fn request_paired(
        &self,
        request: PairedHumanLoopRequest,
    ) -> BoxFuture<'_, Result<HumanLoopResponse, echo_agent::error::ReactError>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reused_request_id_gets_a_new_target_without_changing_request_context()
    -> Result<(), echo_agent::error::ReactError> {
        let mut request = HumanLoopRequest::input("中文\n🙂");
        request.request_id = Some("reusable".to_string());
        request.session_id = Some("original-browser-session".to_string());
        request.context = Some(serde_json::json!({"original": true}));
        let a = pair_request(request.clone(), HitlRequestScope::default())?;
        let b = pair_request(request.clone(), HitlRequestScope::default())?;
        assert_eq!(a.identity.request_id, "reusable");
        assert_ne!(a.identity.response_target, b.identity.response_target);
        assert_eq!(a.request.context, request.context);
        assert_eq!(a.request.session_id, request.session_id);
        let mut invalid = a;
        invalid.identity.response_target.clear();
        assert!(invalid.validate().is_err());
        Ok(())
    }
}
