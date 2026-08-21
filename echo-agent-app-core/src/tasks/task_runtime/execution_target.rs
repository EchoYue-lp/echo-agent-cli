//! Thin application adapter for resolving a frozen PlanTask target.
//!
//! The framework DAG remains unaware of workspaces and Agent groups. EKO
//! resolves one task to an existing conversation-scoped Agent lease, then the
//! normal dispatcher persists the result in the leader TaskRun.

use async_trait::async_trait;

use super::types::TaskExecutionTarget;
use crate::agent_router::AgentAddress;

#[async_trait]
pub trait TaskExecutionTargetResolver: Send + Sync {
    async fn acquire(
        &self,
        leader: &AgentAddress,
        target: &TaskExecutionTarget,
    ) -> Result<crate::agent_pool::AgentPoolExecutionLease, String>;
}
