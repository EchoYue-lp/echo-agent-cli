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
    /// Resolve location identity while the original PlanTask revision is
    /// prepared. Embeddings without project environments preserve None.
    async fn bind_project(
        &self,
        _target: &TaskExecutionTarget,
    ) -> Result<Option<super::types::ProjectExecutionBinding>, String> {
        Ok(None)
    }

    /// Read references from that same target host while its original pool
    /// lease is held. The resolver must compare the frozen location first.
    async fn project_environment(
        &self,
        _target: &TaskExecutionTarget,
    ) -> Result<Option<super::store::ProjectExecutionEnvironment>, String> {
        Ok(None)
    }

    async fn acquire(
        &self,
        leader: &AgentAddress,
        target: &TaskExecutionTarget,
    ) -> Result<crate::agent_pool::AgentPoolExecutionLease, String>;
}
