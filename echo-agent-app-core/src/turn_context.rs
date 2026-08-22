//! Per-turn EKO policy projected at every model boundary.
//!
//! User-authored text stays untouched in conversation history. Dynamic product
//! policy, including the active interaction mode, is carried in a replaceable
//! projection so it cannot become part of the user request or TaskRun goal.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, RwLock};

use echo_agent::compression::{ContextProjection, PreModelContextProjector, ProjectionContext};
use echo_agent::error::Result as AgentResult;
use echo_agent::llm::types::Message;
use futures::future::BoxFuture;

use crate::prepared_turn::InstructionAuthorship;
use crate::tasks::task_runtime::compact_context::{
    TaskRuntimeContextProjector, TaskRuntimeProjectionRegistry,
};
use crate::tasks::task_runtime::{InteractionMode, RunTurnOrigin};

/// Stable projection owner for the current turn's product contract.
pub const TURN_CONTRACT_MARKER: &str = "eko:turn-contract";

const LANGUAGE_PRIORITY_RULES: &str = r#"Response language priority:
1. An explicit output-language request wins.
2. Otherwise use the current user-authored request's natural language.
3. If that request has no clear language signal, use the most recent clear user-authored language available in conversation context, then the stable run goal or task brief.
Do not infer response language from system instructions, mode contracts, tool schemas, source code, terminal output, logs, plan metadata, dependency summaries, or previous assistant text. Preserve code, identifiers, paths, commands, protocol fields, exact required headings, and verbatim evidence."#;

#[derive(Clone, Copy)]
struct TurnContract {
    interaction_mode: InteractionMode,
    origin: RunTurnOrigin,
    authorship: InstructionAuthorship,
}

struct TurnRegistration {
    id: uuid::Uuid,
    contract: TurnContract,
}

/// Process-stable bridge from interactive turn ownership into model calls.
pub struct TurnPromptContextRegistry {
    registrations: RwLock<HashMap<String, TurnRegistration>>,
}

impl TurnPromptContextRegistry {
    pub fn new() -> Self {
        Self {
            registrations: RwLock::new(HashMap::new()),
        }
    }

    pub fn register(
        self: &Arc<Self>,
        turn_id: impl Into<String>,
        interaction_mode: InteractionMode,
        origin: RunTurnOrigin,
        authorship: InstructionAuthorship,
    ) -> TurnPromptContextRegistration {
        let turn_id = turn_id.into();
        let id = uuid::Uuid::new_v4();
        self.registrations
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .insert(
                turn_id.clone(),
                TurnRegistration {
                    id,
                    contract: TurnContract {
                        interaction_mode,
                        origin,
                        authorship,
                    },
                },
            );
        TurnPromptContextRegistration {
            registry: Arc::clone(self),
            turn_id,
            id,
        }
    }

    fn contract(&self, turn_id: &str) -> Option<TurnContract> {
        self.registrations
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .get(turn_id)
            .map(|registration| registration.contract)
    }

    pub fn contains(&self, turn_id: &str) -> bool {
        self.registrations
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .contains_key(turn_id)
    }
}

impl Default for TurnPromptContextRegistry {
    fn default() -> Self {
        Self::new()
    }
}

pub struct TurnPromptContextRegistration {
    registry: Arc<TurnPromptContextRegistry>,
    turn_id: String,
    id: uuid::Uuid,
}

impl Drop for TurnPromptContextRegistration {
    fn drop(&mut self) {
        let mut registrations = self
            .registry
            .registrations
            .write()
            .unwrap_or_else(|error| error.into_inner());
        if registrations
            .get(&self.turn_id)
            .is_some_and(|registration| registration.id == self.id)
        {
            registrations.remove(&self.turn_id);
        }
    }
}

static TURN_PROMPT_CONTEXT_REGISTRY: LazyLock<Arc<TurnPromptContextRegistry>> =
    LazyLock::new(|| Arc::new(TurnPromptContextRegistry::new()));

pub fn turn_prompt_context_registry() -> Arc<TurnPromptContextRegistry> {
    Arc::clone(&TURN_PROMPT_CONTEXT_REGISTRY)
}

/// EKO's application projector: TaskRuntime recovery plus active-turn policy.
pub struct EkoContextProjector {
    task_runtime: TaskRuntimeContextProjector,
    turns: Arc<TurnPromptContextRegistry>,
    awaiter: Option<(
        Arc<crate::tasks::task_runtime::command_cells::CommandCellRuntimeService>,
        crate::workspace::WorkspaceExecutionScope,
    )>,
}

impl EkoContextProjector {
    pub fn new(
        task_runtime_registry: Arc<TaskRuntimeProjectionRegistry>,
        turns: Arc<TurnPromptContextRegistry>,
    ) -> Self {
        Self {
            task_runtime: TaskRuntimeContextProjector::new(task_runtime_registry),
            turns,
            awaiter: None,
        }
    }

    pub fn with_awaiter_results(
        mut self,
        service: Arc<crate::tasks::task_runtime::command_cells::CommandCellRuntimeService>,
        execution_scope: crate::workspace::WorkspaceExecutionScope,
    ) -> Self {
        self.awaiter = Some((service, execution_scope));
        self
    }
}

impl PreModelContextProjector for EkoContextProjector {
    fn project<'a>(
        &'a self,
        context: &'a ProjectionContext,
    ) -> BoxFuture<'a, AgentResult<Vec<ContextProjection>>> {
        Box::pin(async move {
            let mut projections = self.task_runtime.project(context).await?;
            let contract = context
                .turn_id
                .as_deref()
                .and_then(|turn_id| self.turns.contract(turn_id));
            projections.push(ContextProjection {
                marker: TURN_CONTRACT_MARKER.to_string(),
                message: contract.map(render_turn_contract).map(Message::user),
            });
            let pending_awaiter = match (
                self.awaiter.as_ref(),
                context.conversation_id.as_deref(),
                context.turn_id.as_deref(),
            ) {
                (Some((service, scope)), Some(conversation_id), Some(turn_id)) => service
                    .project_pending_awaiter_results(scope.workspace_id(), conversation_id, turn_id)
                    .map_err(echo_agent::error::ReactError::Other)?,
                _ => None,
            };
            projections.push(ContextProjection {
                marker: "[eko_pending_awaiter_results]".to_string(),
                message: pending_awaiter.map(Message::user),
            });
            Ok(projections)
        })
    }
}

fn render_turn_contract(contract: TurnContract) -> String {
    let language_anchor = match contract.authorship {
        InstructionAuthorship::User => {
            "This is a user-authored turn. Treat the non-projection user message as the current language anchor."
        }
        InstructionAuthorship::Runtime => {
            "This is a runtime-authored continuation turn. Treat the stable TaskRun goal and the most recent clear user-authored request as the language anchors, not the continuation instruction."
        }
    };
    format!(
        "[eko_turn_contract]\nInteraction mode: {}\nTurn origin: {}\nInstruction author: {}\nMode behavior:\n{}\n\nResponse language:\n{language_anchor}\n{LANGUAGE_PRIORITY_RULES}\n[/eko_turn_contract]",
        contract.interaction_mode.label(),
        contract.origin.as_str(),
        contract.authorship.as_str(),
        contract.interaction_mode.prompt_hint(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn projection_context(turn_id: Option<&str>) -> ProjectionContext {
        ProjectionContext {
            iteration: 3,
            agent_name: "eko".to_string(),
            session_id: None,
            conversation_id: Some("conversation".to_string()),
            run_id: None,
            turn_id: turn_id.map(str::to_string),
        }
    }

    #[tokio::test]
    async fn turn_contract_is_scoped_and_removed_after_registration_drop() -> Result<(), String> {
        let task_registry = Arc::new(TaskRuntimeProjectionRegistry::new());
        let turn_registry = Arc::new(TurnPromptContextRegistry::new());
        let projector = EkoContextProjector::new(task_registry, Arc::clone(&turn_registry));
        let registration = turn_registry.register(
            "turn-a",
            InteractionMode::Auto,
            RunTurnOrigin::User,
            InstructionAuthorship::User,
        );

        let active = projector
            .project(&projection_context(Some("turn-a")))
            .await
            .map_err(|error| error.to_string())?;
        let contract = active
            .iter()
            .find(|projection| projection.marker == TURN_CONTRACT_MARKER)
            .and_then(|projection| projection.message.as_ref())
            .and_then(|message| message.content.as_text())
            .ok_or_else(|| "active turn contract missing".to_string())?;
        if !contract.contains("Interaction mode: Auto")
            || !contract.contains("Turn origin: user")
            || !contract.contains("non-projection user message")
        {
            return Err(format!("turn contract is incomplete: {contract}"));
        }

        drop(registration);
        let inactive = projector
            .project(&projection_context(Some("turn-a")))
            .await
            .map_err(|error| error.to_string())?;
        let stale = inactive
            .iter()
            .find(|projection| projection.marker == TURN_CONTRACT_MARKER)
            .and_then(|projection| projection.message.as_ref());
        if stale.is_some() || turn_registry.contains("turn-a") {
            return Err("turn contract leaked beyond its registration".to_string());
        }
        Ok(())
    }

    #[tokio::test]
    async fn continuation_uses_run_goal_as_language_anchor() -> Result<(), String> {
        let task_registry = Arc::new(TaskRuntimeProjectionRegistry::new());
        let turn_registry = Arc::new(TurnPromptContextRegistry::new());
        let projector = EkoContextProjector::new(task_registry, Arc::clone(&turn_registry));
        let _registration = turn_registry.register(
            "turn-c",
            InteractionMode::Task,
            RunTurnOrigin::Continuation,
            InstructionAuthorship::Runtime,
        );

        let projections = projector
            .project(&projection_context(Some("turn-c")))
            .await
            .map_err(|error| error.to_string())?;
        let contract = projections
            .iter()
            .find(|projection| projection.marker == TURN_CONTRACT_MARKER)
            .and_then(|projection| projection.message.as_ref())
            .and_then(|message| message.content.as_text())
            .ok_or_else(|| "continuation contract missing".to_string())?;
        if !contract.contains("Turn origin: continuation")
            || !contract.contains("stable TaskRun goal")
            || !contract.contains("not the continuation instruction")
        {
            return Err(format!(
                "continuation language anchor is incomplete: {contract}"
            ));
        }
        Ok(())
    }

    #[test]
    fn latest_registration_owns_cleanup_for_the_same_turn() -> Result<(), String> {
        let registry = Arc::new(TurnPromptContextRegistry::new());
        let old = registry.register(
            "turn",
            InteractionMode::Chat,
            RunTurnOrigin::User,
            InstructionAuthorship::User,
        );
        let current = registry.register(
            "turn",
            InteractionMode::Task,
            RunTurnOrigin::Resume,
            InstructionAuthorship::Runtime,
        );
        drop(old);
        let contract = registry
            .contract("turn")
            .ok_or_else(|| "current registration missing".to_string())?;
        if contract.interaction_mode != InteractionMode::Task
            || contract.origin != RunTurnOrigin::Resume
            || contract.authorship != InstructionAuthorship::Runtime
        {
            return Err("old registration removed the current turn".to_string());
        }
        drop(current);
        if registry.contains("turn") {
            return Err("current registration did not clean up".to_string());
        }
        Ok(())
    }
}
