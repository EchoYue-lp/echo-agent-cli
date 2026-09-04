#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::agent::Agent;

    type TestResult<T = ()> = Result<T, String>;

    struct MemoryReleaseProbe {
        pool: Arc<AgentPool>,
        released_after_pool: Arc<std::sync::atomic::AtomicBool>,
    }

    impl crate::tasks::task_runtime::store::RunDriverExecutionReceipt for MemoryReleaseProbe {
        fn release(self: Box<Self>) -> futures::future::BoxFuture<'static, ()> {
            Box::pin(async move {
                let pool_is_idle = self.pool.active_execution_count() == 0;
                self.released_after_pool
                    .store(pool_is_idle, Ordering::SeqCst);
            })
        }
    }

    #[test]
    fn test_pool_config_default() {
        let config = PoolConfig::default();
        assert_eq!(config.max_agents, 10);
        assert_eq!(config.idle_timeout, Duration::from_secs(1800));
        assert!(config.enable_background_agent);
    }

    #[test]
    fn test_pool_config_custom() {
        let config = PoolConfig {
            max_agents: 5,
            idle_timeout: Duration::from_secs(60),
            enable_background_agent: false,
        };
        assert_eq!(config.max_agents, 5);
        assert!(!config.enable_background_agent);
    }

    #[tokio::test]
    async fn test_pool_exposes_max_agents() -> TestResult {
        let pool = create_test_pool(4, false).await?;
        assert_eq!(pool.max_agents(), 4);
        Ok(())
    }

    #[tokio::test]
    async fn test_pool_background_task_concurrency_is_conservative() -> TestResult {
        let small = create_test_pool(1, false).await?;
        assert_eq!(small.background_task_concurrency(), 1);

        let medium = create_test_pool(3, false).await?;
        assert_eq!(medium.background_task_concurrency(), 2);

        let large = create_test_pool(10, false).await?;
        assert_eq!(large.background_task_concurrency(), 4);
        assert_eq!(large.foreground_agent_reserve(), 1);
        assert_eq!(large.composite_parallelism(), 3);
        Ok(())
    }

    #[test]
    fn test_pool_error_display() {
        let err = PoolError::PoolFull { max: 5 };
        assert!(err.to_string().contains("5"));
        assert!(err.to_string().contains("full"));

        let err = PoolError::AgentCreation("test error".to_string());
        assert!(err.to_string().contains("test error"));
    }

    #[test]
    fn test_pool_error_is_std_error() {
        // Verify PoolError implements std::error::Error
        let err: Box<dyn std::error::Error> = Box::new(PoolError::PoolFull { max: 3 });
        assert!(err.to_string().contains("3"));
    }

    #[tokio::test]
    async fn test_pooled_agent_timestamps() -> TestResult {
        let pool = create_test_pool(2, false).await?;
        let lease = pool
            .acquire("test-conv")
            .await
            .map_err(|error| error.to_string())?;
        drop(lease);
        let agents = pool.agents.read().await;
        let pa = agents
            .get("test-conv")
            .ok_or_else(|| "pooled agent was not retained".to_string())?;
        assert!(pa.created_at.elapsed().as_millis() < 100);
        assert!(pa.last_used.elapsed().as_millis() < 100);
        Ok(())
    }

    #[tokio::test]
    async fn test_pool_acquire_creates_agent() -> TestResult {
        let pool = create_test_pool(3, false).await?;
        assert_eq!(pool.pool_size().await, 0);

        let handle = pool.acquire("conv-1").await;
        assert!(handle.is_ok());
        assert_eq!(pool.pool_size().await, 1);
        Ok(())
    }

    #[tokio::test]
    async fn test_pool_acquire_reuses_existing() -> TestResult {
        let pool = create_test_pool(3, false).await?;

        let _h1 = pool.acquire("conv-1").await.map_err(|e| e.to_string())?;
        let _h2 = pool.acquire("conv-1").await.map_err(|e| e.to_string())?;

        // Same conversation_id should return the same agent (pool size stays 1)
        assert_eq!(pool.pool_size().await, 1);
        Ok(())
    }

    #[tokio::test]
    async fn test_pool_acquire_different_ids() -> TestResult {
        let pool = create_test_pool(5, false).await?;

        let _h1 = pool.acquire("conv-1").await.map_err(|e| e.to_string())?;
        let _h2 = pool.acquire("conv-2").await.map_err(|e| e.to_string())?;
        let _h3 = pool.acquire("conv-3").await.map_err(|e| e.to_string())?;

        assert_eq!(pool.pool_size().await, 3);
        Ok(())
    }

    #[tokio::test]
    async fn process_agent_execution_is_bounded_across_workspace_pools() -> TestResult {
        let governor = Arc::new(AgentExecutionGovernor::new(PROCESS_AGENT_EXECUTION_LIMIT));
        let mut pools = Vec::new();
        for _ in 0..3 {
            let mut pool = create_test_pool(10, false).await?;
            pool.process_agent_execution = governor.clone();
            pools.push(Arc::new(pool));
        }
        let mut leases = Vec::new();
        for index in 0..PROCESS_AGENT_EXECUTION_LIMIT {
            let pool = pools
                .get(index % pools.len())
                .ok_or_else(|| "workspace pool is missing".to_string())?;
            leases.push(
                pool.acquire(&format!("workspace-conversation-{index}"))
                    .await
                    .map_err(|error| error.to_string())?,
            );
        }
        assert_eq!(
            governor.snapshot(),
            AgentExecutionResourceSnapshot {
                active: PROCESS_AGENT_EXECUTION_LIMIT,
                limit: PROCESS_AGENT_EXECUTION_LIMIT,
            }
        );

        let waiting_pool = pools
            .first()
            .cloned()
            .ok_or_else(|| "waiting workspace pool is missing".to_string())?;
        assert!(matches!(
            waiting_pool.acquire("workspace-conversation-waiting").await,
            Err(PoolError::ExecutionLeaseCapacity)
        ));
        leases.pop();
        let admitted = waiting_pool
            .acquire("workspace-conversation-waiting")
            .await
            .map_err(|error| error.to_string())?;
        drop(admitted);
        drop(leases);
        assert!(governor.snapshot().active <= governor.snapshot().limit);
        Ok(())
    }

    #[tokio::test]
    async fn test_exact_execution_retirement_removes_agent() -> TestResult {
        let pool = create_test_pool(5, false).await?;

        let execution = pool.acquire("conv-1").await.map_err(|e| e.to_string())?;
        assert_eq!(pool.pool_size().await, 1);

        assert!(
            pool.retire_execution("conv-1", execution)
                .await
                .map_err(|error| error.to_string())?
        );
        assert_eq!(pool.pool_size().await, 0);
        Ok(())
    }

    #[tokio::test]
    async fn exact_execution_retirement_rejects_wrong_key() -> TestResult {
        let pool = create_test_pool(5, false).await?;
        let execution = pool.acquire("owned").await.map_err(|e| e.to_string())?;
        assert!(matches!(
            pool.retire_execution("other", execution).await,
            Err(PoolError::ExecutionLeaseMismatch)
        ));
        assert_eq!(pool.pool_size().await, 1);
        Ok(())
    }

    #[tokio::test]
    async fn exact_execution_retirement_waits_for_overlapping_same_key_receipt() -> TestResult {
        let pool = create_test_pool(5, false).await?;
        let first = pool.acquire("shared").await.map_err(|e| e.to_string())?;
        let second = pool.acquire("shared").await.map_err(|e| e.to_string())?;

        assert!(
            !pool
                .retire_execution("shared", first)
                .await
                .map_err(|error| error.to_string())?
        );
        assert_eq!(pool.pool_size().await, 1);
        assert!(
            pool.retire_execution("shared", second)
                .await
                .map_err(|error| error.to_string())?
        );
        assert_eq!(pool.pool_size().await, 0);
        Ok(())
    }

    #[tokio::test]
    async fn awaited_conversation_retirement_blocks_aba_and_replaces_exact_generation() -> TestResult
    {
        let pool = Arc::new(create_test_pool(5, false).await?);
        let old_execution = pool.acquire("shared").await.map_err(|e| e.to_string())?;
        let old_agent = old_execution.agent();
        let retirement_pool = Arc::clone(&pool);
        let mut retirement =
            tokio::spawn(
                async move { retirement_pool.retire_conversation_and_wait("shared").await },
            );
        tokio::time::timeout(Duration::from_secs(2), async {
            while !pool.conversation_retiring_for_test("shared") {
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "conversation retirement admission did not close".to_string())?;
        assert!(matches!(
            pool.acquire("shared").await,
            Err(PoolError::ConversationRetirementPending { .. })
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut retirement)
                .await
                .is_err(),
            "retirement completed before the old execution receipt settled"
        );

        drop(old_execution);
        assert!(
            retirement
                .await
                .map_err(|error| error.to_string())?
                .map_err(|error| error.to_string())?
        );
        assert!(!pool.conversation_retiring_for_test("shared"));
        let replacement = pool.acquire("shared").await.map_err(|e| e.to_string())?;
        assert!(!Arc::ptr_eq(old_agent.inner(), replacement.agent().inner()));
        drop(replacement);
        Ok(())
    }

    #[tokio::test]
    async fn cancelled_conversation_retirement_reopens_without_claiming_settlement() -> TestResult {
        let pool = Arc::new(create_test_pool(5, false).await?);
        let old_execution = pool.acquire("shared").await.map_err(|e| e.to_string())?;
        let retirement_pool = Arc::clone(&pool);
        let retirement =
            tokio::spawn(
                async move { retirement_pool.retire_conversation_and_wait("shared").await },
            );
        tokio::time::timeout(Duration::from_secs(2), async {
            while !pool.conversation_retiring_for_test("shared") {
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "conversation retirement admission did not close".to_string())?;
        retirement.abort();
        let _join = retirement.await;
        assert!(!pool.conversation_retiring_for_test("shared"));
        let overlapping = pool.acquire("shared").await.map_err(|e| e.to_string())?;
        drop(overlapping);
        drop(old_execution);
        Ok(())
    }

    #[tokio::test]
    async fn drained_retirement_keeps_admission_closed_until_aggregate_commit() -> TestResult {
        let pool = create_test_pool(5, false).await?;
        let cached = pool
            .acquire("shared")
            .await
            .map_err(|error| error.to_string())?;
        drop(cached);
        let retirement = pool
            .begin_conversation_retirement("shared")
            .map_err(|error| error.to_string())?;
        assert!(
            pool.drain_conversation_retirement(&retirement)
                .await
                .map_err(|error| error.to_string())?
        );
        assert!(matches!(
            pool.acquire("shared").await,
            Err(PoolError::ConversationRetirementPending { .. })
        ));
        drop(retirement);
        let replacement = pool
            .acquire("shared")
            .await
            .map_err(|error| error.to_string())?;
        drop(replacement);
        Ok(())
    }

    #[tokio::test]
    async fn retirement_receipt_cannot_complete_against_another_pool() -> TestResult {
        let pool_a = create_test_pool(5, false).await?;
        let pool_b = create_test_pool(5, false).await?;
        let cached_b = pool_b.acquire("shared").await.map_err(|e| e.to_string())?;
        drop(cached_b);

        let retirement = pool_a
            .begin_conversation_retirement("shared")
            .map_err(|error| error.to_string())?;
        let result = pool_b.complete_conversation_retirement(retirement).await;
        assert!(matches!(result, Err(PoolError::RetirementReceiptMismatch)));
        assert_eq!(pool_b.pool_size().await, 1);
        assert!(!pool_a.conversation_retiring_for_test("shared"));
        let still_cached = pool_b.acquire("shared").await.map_err(|e| e.to_string())?;
        drop(still_cached);
        Ok(())
    }

    #[tokio::test]
    async fn runtime_checkpoint_restores_same_incarnation_but_not_rotated_key() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let state_store = Arc::new(
            echo_agent::state::FileRuntimeStateStore::new(temp.path())
                .map_err(|error| error.to_string())?,
        );
        let pool = create_test_pool(5, false).await?;
        pool.apply_state_store(state_store).await;

        let first = pool
            .acquire("runtime-incarnation-a")
            .await
            .map_err(|error| error.to_string())?;
        let first_agent = first.agent();
        first_agent
            .read_async(|agent| {
                Box::pin(async move {
                    agent
                        .load_messages(vec![echo_agent::llm::types::Message::user(
                            "incarnation-a history".to_string(),
                        )])
                        .await;
                    agent.force_checkpoint().await
                })
            })
            .await
            .map_err(|error| error.to_string())?;
        drop(first);
        pool.retire_conversation_and_wait("runtime-incarnation-a")
            .await
            .map_err(|error| error.to_string())?;

        let same_incarnation = pool
            .acquire("runtime-incarnation-a")
            .await
            .map_err(|error| error.to_string())?;
        let restored = same_incarnation
            .agent()
            .read_async(|agent| Box::pin(async move { agent.resume_from_state_store().await }))
            .await
            .map_err(|error| error.to_string())?;
        if restored.is_none() {
            return Err("same incarnation did not restore its checkpoint".into());
        }
        drop(same_incarnation);

        let rotated = pool
            .acquire("runtime-incarnation-b")
            .await
            .map_err(|error| error.to_string())?;
        let rotated_agent = rotated.agent();
        let rotated_restore = rotated_agent
            .read_async(|agent| Box::pin(async move { agent.resume_from_state_store().await }))
            .await
            .map_err(|error| error.to_string())?;
        let rotated_messages = rotated_agent
            .read_async(|agent| Box::pin(async move { agent.get_messages().await }))
            .await;
        if rotated_restore.is_some()
            || rotated_messages.iter().any(|message| {
                message
                    .text_content()
                    .is_some_and(|text| text.contains("incarnation-a history"))
            })
        {
            return Err("rotated incarnation restored the previous model context".into());
        }
        drop(rotated);
        Ok(())
    }

    #[tokio::test]
    async fn test_pool_lease_existing_returns_none_for_unknown() -> TestResult {
        let pool = create_test_pool(5, false).await?;
        assert!(
            pool.lease_existing("unknown")
                .await
                .map_err(|error| error.to_string())?
                .is_none()
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_pool_lease_existing_returns_some_for_known() -> TestResult {
        let pool = create_test_pool(5, false).await?;
        let _h = pool.acquire("conv-1").await.map_err(|e| e.to_string())?;
        assert!(
            pool.lease_existing("conv-1")
                .await
                .map_err(|error| error.to_string())?
                .is_some()
        );
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_admission_linearizes_with_pool_lock_before_lease_publication() -> TestResult {
        let pool = Arc::new(create_test_pool(5, false).await?);
        let initial = pool.acquire("reserved").await.map_err(|e| e.to_string())?;
        drop(initial);

        let agents = pool.agents.write().await;
        let handle = agents
            .get("reserved")
            .map(|pooled| pooled.handle.clone())
            .ok_or_else(|| "reserved pooled Agent is missing".to_string())?;
        let accepted = pool
            .admission
            .issue_process_scoped("reserved", handle.clone(), &pool.process_agent_execution)
            .map_err(|error| error.to_string())?;
        pool.begin_shutdown();
        assert!(matches!(
            pool.admission
                .issue_process_scoped("reserved", handle, &pool.process_agent_execution,),
            Err(PoolError::ShuttingDown)
        ));
        drop(agents);

        let shutdown_pool = Arc::clone(&pool);
        let shutdown = tokio::spawn(async move { shutdown_pool.shutdown().await });
        tokio::task::yield_now().await;
        assert!(!shutdown.is_finished());
        drop(accepted);
        tokio::time::timeout(std::time::Duration::from_secs(1), shutdown)
            .await
            .map_err(|_| "pool shutdown did not wait for the accepted reservation".to_string())?
            .map_err(|error| error.to_string())??;
        Ok(())
    }

    #[tokio::test]
    async fn task_execute_resolves_the_current_conversation_agent() -> TestResult {
        let pool = Arc::new(create_test_pool(5, false).await?);
        let pooled = pool
            .acquire("conv-1")
            .await
            .map_err(|error| error.to_string())?;
        let fallback = create_test_agent_handle()?;
        let store = Arc::new(
            crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory()
                .map_err(|error| error.to_string())?,
        );
        let tool = crate::tasks::task_runtime::ExecuteTaskTool::new(store, fallback.clone())
            .with_agent_pool(Arc::downgrade(&pool));

        let resolved = tool
            .execution_agent_for_test(Some("conv-1".to_string()))
            .await
            .map_err(|error| error.to_string())?;
        let pooled_agent = pooled.agent();
        assert!(Arc::ptr_eq(resolved.inner(), pooled_agent.inner()));

        let unresolved = tool
            .execution_agent_for_test(Some("missing".to_string()))
            .await
            .map_err(|error| error.to_string())?;
        assert!(Arc::ptr_eq(unresolved.inner(), fallback.inner()));
        Ok(())
    }

    #[tokio::test]
    async fn test_pool_rejects_overflow_while_all_receipts_are_active() -> TestResult {
        let pool = create_test_pool(2, false).await?;

        let _h1 = pool.acquire("conv-1").await.map_err(|e| e.to_string())?;
        let _h2 = pool.acquire("conv-2").await.map_err(|e| e.to_string())?;
        assert_eq!(pool.pool_size().await, 2);

        assert!(matches!(
            pool.acquire("conv-3").await,
            Err(PoolError::PoolFull { max: 2 })
        ));
        assert_eq!(pool.pool_size().await, 2);
        Ok(())
    }

    #[tokio::test]
    async fn test_pool_evicts_idle_after_execution_receipts_drop() -> TestResult {
        let pool = create_test_pool(2, false).await?;
        let h1 = pool.acquire("conv-1").await.map_err(|e| e.to_string())?;
        let h2 = pool.acquire("conv-2").await.map_err(|e| e.to_string())?;
        drop(h1);
        drop(h2);

        let _h3 = pool.acquire("conv-3").await.map_err(|e| e.to_string())?;
        assert_eq!(pool.pool_size().await, 2);
        Ok(())
    }

    #[tokio::test]
    async fn continuation_capacity_is_bounded_without_consuming_conversation_slots() -> TestResult {
        let pool = create_test_pool(2, false).await?;

        let _continuation_one = pool
            .acquire("__continuation__:run-1")
            .await
            .map_err(|error| error.to_string())?;
        let _continuation_two = pool
            .acquire("__continuation__:run-2")
            .await
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            pool.acquire("__continuation__:run-3").await,
            Err(PoolError::PoolFull { max: 2 })
        ));

        let _conversation_one = pool
            .acquire("conv-1")
            .await
            .map_err(|error| error.to_string())?;
        let _conversation_two = pool
            .acquire("conv-2")
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(pool.pool_size().await, 4);
        assert!(matches!(
            pool.acquire("conv-3").await,
            Err(PoolError::PoolFull { max: 2 })
        ));
        Ok(())
    }

    #[tokio::test]
    async fn continuation_capacity_reuses_an_idle_slot() -> TestResult {
        let pool = create_test_pool(2, false).await?;
        let first = pool
            .acquire("__continuation__:run-1")
            .await
            .map_err(|error| error.to_string())?;
        let second = pool
            .acquire("__continuation__:run-2")
            .await
            .map_err(|error| error.to_string())?;
        drop(first);
        drop(second);

        let _replacement = pool
            .acquire("__continuation__:run-3")
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(pool.pool_size().await, 2);
        Ok(())
    }

    #[tokio::test]
    async fn test_pool_background_agent_precreated() -> TestResult {
        // Background agent pre-creation only happens in from_runtime().
        // With manual construction, no background agent exists until acquired.
        let pool = create_test_pool(5, true).await?;
        // Manually created pool has no pre-created agents
        assert_eq!(pool.pool_size().await, 0);
        // But background_agent() returns None since __background__ wasn't pre-created
        assert!(pool.background_agent().await.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn test_pool_background_agent_not_created_when_disabled() -> TestResult {
        let pool = create_test_pool(5, false).await?;
        assert!(pool.background_agent().await.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn test_pool_background_agent_acquire_on_demand() -> TestResult {
        let pool = create_test_pool(5, false).await?;
        // Can acquire __background__ on demand even without pre-creation
        let bg = pool.acquire("__background__").await;
        assert!(bg.is_ok());
        assert!(pool.background_agent().await.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn test_shared_resources_extraction() -> TestResult {
        let agent = create_test_agent()?;
        let handle = AgentHandle::new(agent);
        let shared = SharedResources::extract_from(&handle, None).await;

        // ToolManager should be extracted
        assert!(shared.tool_manager.is_some());
        // HookRegistry should be extracted
        assert!(shared.hook_registry.is_some());
        // TokenTracker should be extracted
        assert!(shared.token_tracker.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn test_shared_resources_arc_sharing() -> TestResult {
        let agent = create_test_agent()?;
        let handle = AgentHandle::new(agent);
        let shared = SharedResources::extract_from(&handle, None).await;

        // Verify Arc reference counts indicate sharing
        let tm = shared
            .tool_manager
            .as_ref()
            .ok_or_else(|| "tool manager should be extracted".to_string())?;
        // At least 2 references: one in original agent, one in shared
        assert!(
            Arc::strong_count(tm) >= 2,
            "ToolManager Arc should be shared (count={})",
            Arc::strong_count(tm)
        );

        let tt = shared
            .token_tracker
            .as_ref()
            .ok_or_else(|| "token tracker should be extracted".to_string())?;
        assert!(
            Arc::strong_count(tt) >= 2,
            "TokenUsageTracker Arc should be shared (count={})",
            Arc::strong_count(tt)
        );
        Ok(())
    }

    // ── Helpers ──────────────────────────────────────────────────────

    fn create_test_agent() -> TestResult<echo_agent::agent::ReactAgent> {
        use echo_agent::agent::ReactAgentBuilder;
        use echo_agent::testing::MockLlmClient;

        let mock_llm = Arc::new(MockLlmClient::new().with_model_name("test-model"));

        ReactAgentBuilder::new()
            .model("test-model")
            .llm_client(mock_llm)
            .build()
            .map_err(|error| error.to_string())
    }

    fn create_test_agent_handle() -> TestResult<AgentHandle> {
        create_test_agent().map(AgentHandle::new)
    }

    async fn create_test_pool(max_agents: usize, enable_bg: bool) -> TestResult<AgentPool> {
        let agent = create_test_agent()?;
        let handle = AgentHandle::new(agent);
        Ok(AgentPool::new_for_test(handle, None, None, max_agents, enable_bg).await)
    }

    async fn create_test_pool_with_review_integration() -> TestResult<AgentPool> {
        let agent = create_test_agent()?;
        let handle = AgentHandle::new(agent);
        let store = Arc::new(echo_agent::memory::InMemoryStore::new())
            as Arc<dyn echo_agent::memory::Store>;
        let echo_agent_dir = std::env::temp_dir()
            .join(format!("echo-agent-pool-test-{}", uuid::Uuid::new_v4()))
            .join(".echo-agent");
        let review_integration = Arc::new(crate::evolution::ReviewIntegration::new(
            echo_agent::evolution::ReviewConfig::default(),
            echo_agent_dir,
            store.clone(),
        ));
        Ok(AgentPool::new_for_test(handle, Some(review_integration), Some(store), 3, false).await)
    }

    async fn expect_subagent_bus_probe(
        receiver: &mut tokio::sync::broadcast::Receiver<Arc<echo_agent::subagent::SubagentEvent>>,
        expected: &str,
    ) -> TestResult {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let event = tokio::time::timeout_at(deadline, receiver.recv())
                .await
                .map_err(|_| format!("timed out waiting for Subagent bus probe '{expected}'"))?
                .map_err(|error| format!("Subagent bus probe receive failed: {error}"))?;
            if let echo_agent::subagent::SubagentEvent::Registered { name } = event.as_ref()
                && name == expected
            {
                return Ok(());
            }
        }
    }

    #[tokio::test]
    async fn primary_existing_and_future_pool_agents_share_subagent_event_bus() -> TestResult {
        let pool = create_test_pool(3, false).await?;
        let primary = pool
            .primary_agent()
            .await
            .map_err(|error| error.to_string())?;
        let mut receiver = primary
            .read(|agent| agent.subagent_registry().event_bus().subscribe())
            .await;

        for (conversation_id, probe) in [
            ("subagent-bus-existing", "existing-pool-agent"),
            ("subagent-bus-future", "future-pool-agent"),
        ] {
            let lease = pool
                .acquire(conversation_id)
                .await
                .map_err(|error| error.to_string())?;
            let event_bus = lease
                .agent()
                .read(|agent| agent.subagent_registry().event_bus().clone())
                .await;
            event_bus.emit(echo_agent::subagent::SubagentEvent::Registered {
                name: probe.to_string(),
            });
            expect_subagent_bus_probe(&mut receiver, probe).await?;
            drop(lease);
        }
        Ok(())
    }

    #[tokio::test]
    async fn workspace_primary_and_conversation_agent_share_seed_subagent_event_bus() -> TestResult
    {
        let seed = Arc::new(create_test_pool(3, false).await?);
        let seed_primary = seed
            .primary_agent()
            .await
            .map_err(|error| error.to_string())?;
        let mut receiver = seed_primary
            .read(|agent| agent.subagent_registry().event_bus().subscribe())
            .await;
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("shared-subagent-bus-workspace");
        std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        let now = chrono::Utc::now();
        let workspace = crate::workspace::Workspace {
            id: crate::workspace::WorkspaceId::from_name("shared-subagent-bus"),
            name: "Shared Subagent bus".to_string(),
            root,
            project_root: None,
            kind: WorkspaceKind::General,
            metadata: crate::workspace::WorkspaceMetadata::default(),
            product_data_generation: String::new(),
            created_at: now,
            last_active: now,
        };
        let registry = crate::workspace::runtime::WorkspaceRuntimeRegistry::new();
        let host = registry
            .get_or_open(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let runtime = host
            .get_or_open_execution(&seed)
            .await
            .map_err(|error| error.to_string())?;

        let workspace_primary_bus = runtime
            .primary_agent()
            .read(|agent| agent.subagent_registry().event_bus().clone())
            .await;
        workspace_primary_bus.emit(echo_agent::subagent::SubagentEvent::Registered {
            name: "workspace-primary".to_string(),
        });
        expect_subagent_bus_probe(&mut receiver, "workspace-primary").await?;

        let conversation = runtime
            .pool()
            .acquire("workspace-conversation")
            .await
            .map_err(|error| error.to_string())?;
        let conversation_bus = conversation
            .agent()
            .read(|agent| agent.subagent_registry().event_bus().clone())
            .await;
        conversation_bus.emit(echo_agent::subagent::SubagentEvent::Registered {
            name: "workspace-conversation".to_string(),
        });
        expect_subagent_bus_probe(&mut receiver, "workspace-conversation").await?;
        drop(conversation);
        Ok(())
    }

    #[tokio::test]
    async fn runtime_state_store_rebind_reaches_existing_and_future_agents() -> TestResult {
        let pool = create_test_pool(4, false).await?;
        let existing_lease = pool
            .acquire("existing-state-binding")
            .await
            .map_err(|error| error.to_string())?;
        let existing = existing_lease.agent();
        drop(existing_lease);
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store: Arc<dyn echo_agent::state::RuntimeStateStore> = Arc::new(
            echo_agent::state::FileRuntimeStateStore::new(temp.path())
                .map_err(|error| error.to_string())?,
        );

        pool.apply_state_store(store.clone()).await;
        let existing_store = existing
            .read(|agent| agent.state_store().clone())
            .await
            .ok_or_else(|| "existing agent has no runtime state store".to_string())?;
        assert!(Arc::ptr_eq(&existing_store, &store));

        let future_lease = pool
            .acquire("future-state-binding")
            .await
            .map_err(|error| error.to_string())?;
        let future_store = future_lease
            .agent()
            .read(|agent| agent.state_store().clone())
            .await
            .ok_or_else(|| "future agent has no runtime state store".to_string())?;
        assert!(Arc::ptr_eq(&future_store, &store));
        Ok(())
    }

    #[tokio::test]
    async fn future_agent_uses_committed_local_config_without_api_key() -> TestResult {
        let pool = create_test_pool(4, false).await?;
        let runtime = ModelRuntimeConfig {
            id: "local:model".to_string(),
            display_name: "Local model".to_string(),
            provider: "local".to_string(),
            model: "model".to_string(),
            api_protocol: echo_agent::llm::LlmApiProtocol::ChatCompletions,
            input_modalities: echo_agent::llm::ModelInputModality::text_only(),
            auth_token: None,
            auth_source: "none".to_string(),
            base_url: Some("http://127.0.0.1:11434/v1/chat/completions".to_string()),
            api_key_env: None,
            requires_api_key: false,
            temperature: None,
            max_tokens: None,
            context_window: None,
            thinking_profile: echo_agent::llm::ThinkingProfile::unknown(),
        };

        let prepared = infra::prepare_runtime_llm(&runtime)?;
        let mut candidate = pool.app_config.read().await.clone();
        candidate.model_providers.insert(
            runtime.provider.clone(),
            crate::config::ModelProviderConfig {
                base_url: runtime.base_url.clone(),
                ..Default::default()
            },
        );
        candidate
            .configured_models
            .push(crate::config::ConfiguredModel {
                id: runtime.id.clone(),
                display_name: runtime.display_name.clone(),
                provider: runtime.provider.clone(),
                model: runtime.model.clone(),
                api_protocol: runtime.api_protocol,
                ..Default::default()
            });
        crate::model_config::set_default_model(&mut candidate, &runtime.id)?;
        pool.prepare_model_publication(candidate, runtime, prepared)
            .await?
            .commit()
            .await;
        let lease = pool
            .acquire("future-local")
            .await
            .map_err(|error| error.to_string())?;
        let handle = lease.agent();
        let applied = handle
            .read(|agent| agent.llm_config().cloned())
            .await
            .ok_or_else(|| "future agent has no LLM config".to_string())?;
        assert!(applied.api_key.is_empty());
        assert_eq!(
            applied.base_url,
            "http://127.0.0.1:11434/v1/chat/completions"
        );
        Ok(())
    }

    #[tokio::test]
    async fn future_pool_agent_uses_the_session_model_selection() -> TestResult {
        use echo_agent::agent::Agent;

        let agent = create_test_agent_handle()?;
        let mut config = EkoConfig {
            configured_models: vec![
                crate::config::ConfiguredModel {
                    id: "local:a".to_string(),
                    display_name: "A".to_string(),
                    provider: "local".to_string(),
                    model: "a".to_string(),
                    context_window: Some(100_000),
                    ..crate::config::ConfiguredModel::default()
                },
                crate::config::ConfiguredModel {
                    id: "local:b".to_string(),
                    display_name: "B".to_string(),
                    provider: "local".to_string(),
                    model: "b".to_string(),
                    context_window: Some(200_000),
                    ..crate::config::ConfiguredModel::default()
                },
            ],
            ..EkoConfig::default()
        };
        config.model.default_model_id = Some("local:a".to_string());
        config.model_providers.insert(
            "local".to_string(),
            crate::config::ModelProviderConfig {
                auth_token: None,
                base_url: Some("http://127.0.0.1:11434/v1/chat/completions".to_string()),
                ..Default::default()
            },
        );
        let selected = crate::model_config::resolve_runtime_model(&config, Some("local:b"))
            .map_err(|error| error.to_string())?;
        let session = crate::model_config::session_config_for_runtime(&config, &selected)?;
        let pool = AgentPool::new_for_test_with_config(&agent, None, None, 3, false, session).await;

        let lease = pool
            .acquire("future-session-selection")
            .await
            .map_err(|error| error.to_string())?;
        let handle = lease.agent();
        let (model, token_limit) = handle
            .read(|pooled| {
                (
                    pooled.model_name().to_string(),
                    pooled.config().get_token_limit(),
                )
            })
            .await;

        assert_eq!(model, "b");
        assert_eq!(token_limit, 200_000);
        Ok(())
    }

    #[tokio::test]
    async fn workspace_transition_gate_rejects_acquire_until_publication_finishes() -> TestResult {
        let pool = create_test_pool(4, false).await?;
        let mut transition = pool
            .preflight_workspace_transition()
            .await
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            pool.acquire("blocked-before-commit").await,
            Err(PoolError::WorkspaceTransition)
        ));
        transition.commit().await;
        assert!(matches!(
            pool.acquire("blocked-after-commit").await,
            Err(PoolError::WorkspaceTransition)
        ));
        drop(transition);

        let published = pool
            .acquire("published")
            .await
            .map_err(|error| error.to_string())?;
        drop(published);
        Ok(())
    }

    #[tokio::test]
    async fn failed_pool_preflight_reopens_admission() -> TestResult {
        let pool = create_test_pool(4, false).await?;
        let active_lease = pool
            .acquire("active")
            .await
            .map_err(|error| error.to_string())?;
        let active = active_lease.agent();
        drop(active_lease);
        let execution = active.read(|agent| agent.execution_mutex().clone()).await;
        let execution_guard = execution.lock().await;
        let error = pool
            .preflight_workspace_transition()
            .await
            .err()
            .ok_or_else(|| "busy pool transition unexpectedly succeeded".to_string())?;
        assert!(error.to_string().contains("executing"));
        drop(execution_guard);

        let reopened = pool
            .acquire("reopened")
            .await
            .map_err(|error| error.to_string())?;
        drop(reopened);
        Ok(())
    }

    #[tokio::test]
    async fn issued_lease_blocks_transition_even_before_execution_mutex_is_locked() -> TestResult {
        let pool = create_test_pool(4, false).await?;
        let issued = pool
            .acquire("issued-before-execution")
            .await
            .map_err(|error| error.to_string())?;
        let mut transition = Box::pin(pool.preflight_workspace_transition());

        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut transition)
                .await
                .is_err(),
            "transition must wait for the issued execution receipt"
        );
        assert!(matches!(
            pool.acquire("blocked-while-draining").await,
            Err(PoolError::WorkspaceTransition)
        ));

        drop(issued);
        let mut transition = tokio::time::timeout(Duration::from_secs(1), transition)
            .await
            .map_err(|_| "transition did not observe the released lease".to_string())?
            .map_err(|error| error.to_string())?;
        transition.commit().await;
        drop(transition);

        let new_generation = pool
            .acquire("new-generation")
            .await
            .map_err(|error| error.to_string())?;
        drop(new_generation);
        Ok(())
    }

    #[tokio::test]
    async fn cancelled_preflight_wait_reopens_pool_admission() -> TestResult {
        let pool = create_test_pool(4, false).await?;
        let issued = pool
            .acquire("issued-before-cancel")
            .await
            .map_err(|error| error.to_string())?;

        assert!(
            tokio::time::timeout(
                Duration::from_millis(20),
                pool.preflight_workspace_transition(),
            )
            .await
            .is_err(),
            "preflight should still be draining the issued lease"
        );
        let admitted_after_cancel = pool
            .acquire("admitted-after-cancel")
            .await
            .map_err(|error| error.to_string())?;

        drop(admitted_after_cancel);
        drop(issued);
        Ok(())
    }

    #[tokio::test]
    async fn workspace_routing_applies_to_existing_and_future_pool_agents() -> Result<(), String> {
        let pool = create_test_pool(4, false).await?;
        let existing = pool
            .acquire("existing")
            .await
            .map_err(|error| error.to_string())?;

        pool.apply_workspace_routing(WorkspaceKind::DataAnalysis { datasets: vec![] })
            .await;
        let existing_context = existing.agent().read(|agent| agent.context().clone()).await;
        assert!(
            existing_context
                .lock()
                .await
                .has_projection("eko:workspace-profile")
        );

        let future = pool
            .acquire("future")
            .await
            .map_err(|error| error.to_string())?;
        let future_context = future.agent().read(|agent| agent.context().clone()).await;
        assert!(
            future_context
                .lock()
                .await
                .has_projection("eko:workspace-profile")
        );
        Ok(())
    }

    #[tokio::test]
    async fn working_dir_applies_to_existing_and_future_pool_agents() -> Result<(), String> {
        let pool = create_test_pool(4, false).await?;
        let existing = pool
            .acquire("existing-working-dir")
            .await
            .map_err(|error| error.to_string())?;
        let root = std::env::temp_dir().join("eko-pool-working-dir");

        pool.apply_working_dir(Some(root.clone())).await;
        assert_eq!(
            existing.agent().read(|agent| agent.working_dir()).await,
            Some(root.clone())
        );

        let future = pool
            .acquire("future-working-dir")
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            future.agent().read(|agent| agent.working_dir()).await,
            Some(root)
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_pool_agent_installs_layered_memory_runtime() -> TestResult {
        let pool = create_test_pool_with_review_integration().await?;
        let handle = pool
            .acquire("conv-memory")
            .await
            .map_err(|error| error.to_string())?;

        let has_layer_manager = handle
            .agent()
            .read(|agent| agent.has_memory_layer_manager())
            .await;
        assert!(
            has_layer_manager,
            "pooled agents must install MemoryLayerManager so TriggerDetector writes real memory"
        );
        Ok(())
    }

    #[tokio::test]
    async fn primary_existing_and_future_agents_share_exact_scoped_memory_arc() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store_a = Arc::new(echo_agent::memory::InMemoryStore::new())
            as Arc<dyn echo_agent::memory::Store>;
        let integration_a = Arc::new(crate::evolution::ReviewIntegration::new_scoped(
            echo_agent::evolution::ReviewConfig::default(),
            temp.path().join("workspace-a/.eko"),
            store_a.clone(),
            "workspace-a".to_string(),
            "generation-a".to_string(),
        ));
        let manager_a = integration_a
            .lease_generation()
            .map_err(|error| error.to_string())?
            .layer_manager()
            .map_err(|error| error.to_string())?;
        let primary = create_test_agent_handle()?;
        let primary_manager = manager_a.clone();
        primary
            .write(|agent| agent.install_memory_layer_manager(primary_manager))
            .await;
        let pool = AgentPool::new_for_test(
            primary.clone(),
            Some(integration_a.clone()),
            Some(store_a),
            3,
            false,
        )
        .await;

        let existing = pool
            .acquire("existing-memory-generation")
            .await
            .map_err(|error| error.to_string())?;
        let existing_manager = existing
            .agent()
            .read(|agent| agent.memory_layer_manager().cloned())
            .await
            .ok_or_else(|| "existing pooled Agent has no memory manager".to_string())?;
        drop(existing);
        let future = pool
            .acquire("future-memory-generation")
            .await
            .map_err(|error| error.to_string())?;
        let future_manager = future
            .agent()
            .read(|agent| agent.memory_layer_manager().cloned())
            .await
            .ok_or_else(|| "future pooled Agent has no memory manager".to_string())?;
        let installed_primary = primary
            .read(|agent| agent.memory_layer_manager().cloned())
            .await
            .ok_or_else(|| "primary Agent has no memory manager".to_string())?;

        assert!(Arc::ptr_eq(&manager_a, &installed_primary));
        assert!(Arc::ptr_eq(&manager_a, &existing_manager));
        assert!(Arc::ptr_eq(&manager_a, &future_manager));

        let integration_b = crate::evolution::ReviewIntegration::new_scoped(
            echo_agent::evolution::ReviewConfig::default(),
            temp.path().join("workspace-b/.eko"),
            Arc::new(echo_agent::memory::InMemoryStore::new()),
            "workspace-b".to_string(),
            "generation-b".to_string(),
        );
        let manager_b = integration_b
            .lease_generation()
            .map_err(|error| error.to_string())?
            .layer_manager()
            .map_err(|error| error.to_string())?;
        assert!(!Arc::ptr_eq(&manager_a, &manager_b));
        assert!(!Arc::ptr_eq(
            &integration_a.hot_memory_projection_source(),
            &integration_b.hot_memory_projection_source(),
        ));
        Ok(())
    }

    #[tokio::test]
    async fn test_task_subagents_are_isolated_and_have_memory_runtime() -> TestResult {
        let pool = create_test_pool_with_review_integration().await?;

        let task_a = pool
            .acquire("__task__:task-a")
            .await
            .map_err(|error| error.to_string())?;
        let task_b = pool
            .acquire("__task__:task-b")
            .await
            .map_err(|error| error.to_string())?;

        assert!(
            !Arc::ptr_eq(task_a.agent().inner(), task_b.agent().inner()),
            "parallel background tasks must use distinct subagent instances"
        );

        let task_a_has_memory = task_a
            .agent()
            .read(|agent| agent.has_memory_layer_manager())
            .await;
        let task_b_has_memory = task_b
            .agent()
            .read(|agent| agent.has_memory_layer_manager())
            .await;
        assert!(task_a_has_memory);
        assert!(task_b_has_memory);
        Ok(())
    }

    #[tokio::test]
    async fn test_released_task_subagent_frees_pool_capacity() -> TestResult {
        let pool = create_test_pool(1, false).await?;

        let task_a = pool
            .acquire("__task__:task-a")
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(pool.pool_size().await, 1);

        assert!(
            pool.retire_execution("__task__:task-a", task_a)
                .await
                .map_err(|error| error.to_string())?
        );
        assert_eq!(pool.pool_size().await, 0);

        let task_b = pool.acquire("__task__:task-b").await;
        assert!(
            task_b.is_ok(),
            "released task subagent should free capacity for a later task"
        );
        assert_eq!(pool.pool_size().await, 1);
        Ok(())
    }

    #[tokio::test]
    async fn test_permission_mode_applies_to_existing_and_future_pool_agents() -> TestResult {
        let pool = create_test_pool(3, false).await?;
        let first = pool
            .acquire("conv-a")
            .await
            .map_err(|error| error.to_string())?;

        pool.apply_permission_mode(PermissionMode::BypassPermissions)
            .await;

        let first_mode = first
            .agent()
            .read(|agent| agent.get_permission_mode())
            .await;
        assert_eq!(first_mode, PermissionMode::BypassPermissions);

        let second = pool
            .acquire("conv-b")
            .await
            .map_err(|error| error.to_string())?;
        let second_mode = second
            .agent()
            .read(|agent| agent.get_permission_mode())
            .await;
        assert_eq!(second_mode, PermissionMode::BypassPermissions);
        Ok(())
    }

    #[tokio::test]
    async fn thinking_applies_to_primary_existing_future_and_inherited_subagents() -> TestResult {
        let pool = create_test_pool(3, false).await?;
        let existing = pool
            .acquire("thinking-existing")
            .await
            .map_err(|error| error.to_string())?;
        let thinking = Some(echo_agent::llm::ThinkingConfig::Level(
            echo_agent::llm::ThinkingLevel::High,
        ));

        pool.apply_thinking(thinking.clone()).await;

        for handle in [
            pool.primary_agent()
                .await
                .map_err(|error| error.to_string())?,
            existing.agent(),
        ] {
            assert_eq!(handle.read(|agent| agent.thinking().cloned()).await, thinking);
        }
        let future = pool
            .acquire("thinking-future")
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            future.agent().read(|agent| agent.thinking().cloned()).await,
            thinking
        );
        let agents = pool.agents.read().await;
        for key in ["thinking-existing", "thinking-future"] {
            let inherited = agents
                .get(key)
                .and_then(|pooled| {
                    pooled
                        .model_consumers
                        .inherited_handle_for_test("reviewer")
                })
                .ok_or_else(|| format!("{key} has no inherited Subagent"))?;
            assert_eq!(inherited.read(|agent| agent.thinking().cloned()).await, thinking);
        }
        Ok(())
    }

    #[tokio::test]
    async fn thinking_and_iteration_publication_do_not_wait_for_busy_agents() -> TestResult {
        let pool = create_test_pool(3, false).await?;
        let existing = pool
            .acquire("busy-config")
            .await
            .map_err(|error| error.to_string())?;
        let handle = existing.agent();
        let busy = handle.inner().read().await;

        tokio::time::timeout(
            Duration::from_secs(1),
            pool.apply_thinking(Some(echo_agent::llm::ThinkingConfig::Level(
                echo_agent::llm::ThinkingLevel::Low,
            ))),
        )
        .await
        .map_err(|_| "thinking publication waited for a busy agent".to_string())?;
        tokio::time::timeout(Duration::from_secs(1), pool.apply_max_iterations(0))
            .await
            .map_err(|_| "max_iterations publication waited for a busy agent".to_string())?;
        drop(busy);

        let refreshed = pool
            .acquire("busy-config")
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            refreshed
                .agent()
                .read(|agent| agent.thinking().cloned())
                .await,
            Some(echo_agent::llm::ThinkingConfig::Level(
                echo_agent::llm::ThinkingLevel::Low
            ))
        );
        assert_eq!(
            refreshed
                .agent()
                .read(|agent| agent.config().get_max_iterations())
                .await,
            usize::MAX
        );
        Ok(())
    }

    #[tokio::test]
    async fn max_iterations_applies_to_primary_existing_and_future_agents() -> TestResult {
        let pool = create_test_pool(3, false).await?;
        let existing = pool
            .acquire("iterations-existing")
            .await
            .map_err(|error| error.to_string())?;

        pool.apply_max_iterations(37).await;

        for handle in [
            pool.primary_agent()
                .await
                .map_err(|error| error.to_string())?,
            existing.agent(),
        ] {
            assert_eq!(
                handle
                    .read(|agent| agent.config().get_max_iterations())
                    .await,
                37
            );
        }
        let future = pool
            .acquire("iterations-future")
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            future
                .agent()
                .read(|agent| agent.config().get_max_iterations())
                .await,
            37
        );
        Ok(())
    }

    #[tokio::test]
    async fn tool_control_generation_reaches_primary_existing_and_future_agents() -> TestResult {
        let pool = create_test_pool(3, false).await?;
        let existing = pool
            .acquire("tool-control-existing")
            .await
            .map_err(|error| error.to_string())?;
        let receipt = pool
            .tool_control()
            .set_enabled("shell", false)
            .map_err(|error| error.to_string())?;
        assert_eq!(receipt.revision, 1);
        pool.publish_tool_control_generation()
            .await
            .map_err(|error| error.to_string())?;

        for handle in [
            pool.primary_agent()
                .await
                .map_err(|error| error.to_string())?,
            existing.agent(),
        ] {
            assert!(
                crate::tool_control::snapshot_disabled_tools(&handle)
                    .await
                    .contains("shell")
            );
        }

        let future = pool
            .acquire("tool-control-future")
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            crate::tool_control::snapshot_disabled_tools(&future.agent())
                .await
                .contains("shell")
        );
        let agents = pool.agents.read().await;
        for pooled in agents.values() {
            assert!(
                pooled
                    .model_consumers
                    .tool_control_is_projected_for_test("shell")
                    .await
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn delayed_tool_control_publisher_cannot_overwrite_a_newer_generation() -> TestResult {
        let pool = Arc::new(create_test_pool(3, false).await?);
        let existing = pool
            .acquire("tool-control-race")
            .await
            .map_err(|error| error.to_string())?;
        let agents_guard = pool.agents.write().await;
        pool.tool_control()
            .set_enabled("shell", false)
            .map_err(|error| error.to_string())?;
        let delayed_pool = Arc::clone(&pool);
        let delayed =
            tokio::spawn(async move { delayed_pool.publish_tool_control_generation().await });
        tokio::task::yield_now().await;
        let latest = pool
            .tool_control()
            .set_enabled("read_file", false)
            .map_err(|error| error.to_string())?;
        assert_eq!(latest.revision, 2);
        drop(agents_guard);
        delayed
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        pool.publish_tool_control_generation()
            .await
            .map_err(|error| error.to_string())?;

        for handle in [
            pool.primary_agent()
                .await
                .map_err(|error| error.to_string())?,
            existing.agent(),
        ] {
            let disabled = crate::tool_control::snapshot_disabled_tools(&handle).await;
            assert!(disabled.contains("shell"));
            assert!(disabled.contains("read_file"));
        }
        Ok(())
    }

    #[tokio::test]
    async fn system_prompt_applies_to_primary_existing_and_future_pool_agents() -> TestResult {
        let pool = create_test_pool(3, false).await?;
        let first = pool
            .acquire("conv-a")
            .await
            .map_err(|error| error.to_string())?;
        pool.apply_system_prompt("Shared EKO prompt".to_string())
            .await;

        assert!(
            pool.primary_agent()
                .await
                .map_err(|error| error.to_string())?
                .read(|agent| agent.system_prompt().starts_with("Shared EKO prompt"))
                .await
        );
        assert!(
            first
                .agent()
                .read(|agent| agent.system_prompt().starts_with("Shared EKO prompt"))
                .await
        );
        let future = pool
            .acquire("conv-b")
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            future
                .agent()
                .read(|agent| agent.system_prompt().starts_with("Shared EKO prompt"))
                .await
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_permission_mode_does_not_wait_for_busy_pool_agent() -> TestResult {
        let pool = create_test_pool(3, false).await?;
        let first = pool
            .acquire("conv-a")
            .await
            .map_err(|error| error.to_string())?;
        let first_handle = first.agent();
        let busy_guard = first_handle.inner().read().await;

        tokio::time::timeout(
            Duration::from_secs(1),
            pool.apply_permission_mode(PermissionMode::BypassPermissions),
        )
        .await
        .map_err(|_| "permission update waited for a busy agent".to_string())?;
        drop(busy_guard);

        let refreshed = pool
            .acquire("conv-a")
            .await
            .map_err(|error| error.to_string())?;
        let refreshed_mode = refreshed
            .agent()
            .read(|agent| agent.get_permission_mode())
            .await;
        assert_eq!(refreshed_mode, PermissionMode::BypassPermissions);
        Ok(())
    }

    #[tokio::test]
    async fn cleanup_monitor_start_is_idempotent_and_shutdown_awaits_exit() -> TestResult {
        let pool = Arc::new(create_test_pool(2, false).await?);
        pool.spawn_cleanup_monitor().await;
        let first_id = pool
            .cleanup_handle
            .lock()
            .map_err(|_| "cleanup monitor handle is unavailable".to_string())?
            .as_ref()
            .map(tokio::task::JoinHandle::id);

        pool.spawn_cleanup_monitor().await;
        let second_id = pool
            .cleanup_handle
            .lock()
            .map_err(|_| "cleanup monitor handle is unavailable".to_string())?
            .as_ref()
            .map(tokio::task::JoinHandle::id);
        assert!(first_id.is_some());
        assert_eq!(second_id, first_id);

        pool.shutdown().await?;
        assert!(pool.cleanup_cancel.is_cancelled());
        assert!(
            pool.cleanup_handle
                .lock()
                .map_err(|_| "cleanup monitor handle is unavailable".to_string())?
                .is_none()
        );
        Ok(())
    }

    #[tokio::test]
    async fn supervised_release_keeps_overlapping_same_key_agent_until_last_receipt() -> TestResult
    {
        let pool = Arc::new(create_test_pool(2, false).await?);
        let first = pool
            .acquire("shared-run")
            .await
            .map_err(|error| error.to_string())?;
        let first_receipt = pool.retain_for_supervised_run("shared-run".to_string(), first);
        let second = pool
            .acquire("shared-run")
            .await
            .map_err(|error| error.to_string())?;
        let second_agent = second.agent();
        let second_receipt = pool.retain_for_supervised_run("shared-run".to_string(), second);

        crate::tasks::task_runtime::store::RunDriverExecutionReceipt::release(Box::new(
            first_receipt,
        ))
        .await;
        assert_eq!(pool.pool_size().await, 1);

        let third = pool
            .acquire("shared-run")
            .await
            .map_err(|error| error.to_string())?;
        assert!(Arc::ptr_eq(second_agent.inner(), third.agent().inner()));
        drop(third);

        crate::tasks::task_runtime::store::RunDriverExecutionReceipt::release(Box::new(
            second_receipt,
        ))
        .await;
        assert_eq!(pool.pool_size().await, 0);
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_waits_for_active_receipts_and_rejects_later_acquire() -> TestResult {
        let pool = Arc::new(create_test_pool(2, false).await?);
        let active = pool
            .acquire("active-during-shutdown")
            .await
            .map_err(|error| error.to_string())?;
        let shutdown_pool = Arc::clone(&pool);
        let shutdown = tokio::spawn(async move { shutdown_pool.shutdown().await });

        while !pool.shutting_down.load(Ordering::Acquire) {
            if shutdown.is_finished() {
                return Err("pool shutdown finished while an execution receipt was active".into());
            }
            tokio::task::yield_now().await;
        }
        assert!(!shutdown.is_finished());
        assert!(matches!(
            pool.acquire("after-shutdown").await,
            Err(PoolError::ShuttingDown)
        ));

        drop(active);
        shutdown
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert_eq!(pool.pool_size().await, 0);
        Ok(())
    }

    #[tokio::test]
    async fn permanent_terminal_debt_reports_abandonment_and_unblocks_pool_shutdown() -> TestResult
    {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("tasks");
        let blocked_root = temp.path().join("tasks-blocked");
        let store = Arc::new(
            crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory_with_shadow_root(
                root.clone(),
            )
            .map_err(|error| error.to_string())?,
        );
        let canonical_root = std::fs::canonicalize(temp.path())
            .map_err(|error| error.to_string())?
            .join("tasks");
        store
            .create_run(
                "permanent-terminal-debt",
                "workspace-a",
                "conversation",
                "message",
                crate::tasks::task_runtime::DomainProfile::General,
                "preserve non-terminal disk truth",
                "",
                crate::tasks::task_runtime::AttendedMode::Unattended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run(
                "permanent-terminal-debt",
                crate::tasks::task_runtime::TaskRunStatus::Running,
            )
            .map_err(|error| error.to_string())?;

        let pool = Arc::new(create_test_pool(2, false).await?);
        let pool_execution = pool
            .acquire("permanent-terminal-debt")
            .await
            .map_err(|error| error.to_string())?;
        let pool_for_driver = Arc::clone(&pool);
        let released_after_pool = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let release_probe = Arc::clone(&released_after_pool);
        let admission = store
            .reserve_run_driver_admission(
                "permanent-terminal-debt".to_string(),
                echo_agent::agent::CancellationToken::new(),
            )
            .map_err(|error| error.to_string())?;
        let generation_lease = store
            .lease_active_workspace_generation()
            .map_err(|error| error.to_string())?;
        let waiter = store
            .spawn_run_driver(
                admission,
                generation_lease,
                move |mut receipt_owner| async move {
                    receipt_owner.retain(MemoryReleaseProbe {
                        pool: Arc::clone(&pool_for_driver),
                        released_after_pool: release_probe,
                    });
                    receipt_owner.retain(pool_for_driver.retain_for_supervised_run(
                        "permanent-terminal-debt".to_string(),
                        pool_execution,
                    ));
                    std::fs::rename(&root, &blocked_root)
                        .map_err(|error| format!("block task root: {error}"))?;
                    std::fs::write(&root, b"block directory recreation")
                        .map_err(|error| format!("replace task root: {error}"))?;
                    Err::<(), String>("injected permanent driver failure".to_string())
                },
            )
            .map_err(|error| error.to_string())?;
        let driver_error = waiter
            .await
            .map_err(|error| error.to_string())?
            .err()
            .ok_or_else(|| "permanent driver failure was not reported".to_string())?;
        assert!(driver_error.contains("terminal settlement failed"));

        let shutdown_error =
            tokio::time::timeout(Duration::from_secs(2), store.shutdown_run_drivers())
                .await
                .map_err(|_| "TaskRun driver shutdown timed out on permanent debt".to_string())?
                .err()
                .ok_or_else(|| "permanent settlement debt was not reported".to_string())?;
        assert_eq!(shutdown_error.abandoned_settlements.len(), 1);
        let diagnostic = shutdown_error
            .abandoned_settlements
            .first()
            .ok_or_else(|| "abandoned settlement diagnostic is missing".to_string())?;
        let driver_token = diagnostic
            .driver_token
            .ok_or_else(|| "abandoned settlement driver token is missing".to_string())?;
        assert_eq!(diagnostic.run_id, "permanent-terminal-debt");
        assert_eq!(diagnostic.root, canonical_root);
        assert!(!diagnostic.error.is_empty());
        let shutdown_text = shutdown_error.to_string();
        assert!(shutdown_text.contains("run=permanent-terminal-debt"));
        assert!(shutdown_text.contains(&format!("driver_token={driver_token}")));
        assert!(shutdown_text.contains(&diagnostic.root.display().to_string()));
        assert_eq!(store.active_run_driver_count()?, 0);
        assert_eq!(store.active_run_driver_receipt_count()?, 0);
        assert!(released_after_pool.load(Ordering::SeqCst));
        let transition = store
            .begin_workspace_transition()
            .await
            .map_err(|error| error.to_string())?;
        drop(transition);

        tokio::time::timeout(Duration::from_secs(2), pool.shutdown())
            .await
            .map_err(|_| "AgentPool shutdown remained blocked by abandoned debt".to_string())??;
        std::fs::remove_file(temp.path().join("tasks")).map_err(|error| error.to_string())?;
        std::fs::rename(temp.path().join("tasks-blocked"), temp.path().join("tasks"))
            .map_err(|error| error.to_string())?;
        let run = store
            .get_run("permanent-terminal-debt")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "non-terminal run disappeared from disk".to_string())?;
        assert_eq!(
            run.status,
            crate::tasks::task_runtime::TaskRunStatus::Running
        );
        Ok(())
    }

    #[tokio::test]
    async fn aborted_reporter_and_waiter_do_not_abort_owned_driver_settlement() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("tasks");
        let blocked_root = temp.path().join("tasks-blocked");
        let store = Arc::new(
            crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory_with_shadow_root(
                root.clone(),
            )
            .map_err(|error| error.to_string())?,
        );
        store
            .create_run(
                "aborted-shutdown-waiter",
                "workspace-a",
                "conversation",
                "message",
                crate::tasks::task_runtime::DomainProfile::General,
                "retain settlement ownership after waiter abort",
                "",
                crate::tasks::task_runtime::AttendedMode::Unattended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run(
                "aborted-shutdown-waiter",
                crate::tasks::task_runtime::TaskRunStatus::Running,
            )
            .map_err(|error| error.to_string())?;

        let pool = Arc::new(create_test_pool(2, false).await?);
        let execution = pool
            .acquire("aborted-shutdown-waiter")
            .await
            .map_err(|error| error.to_string())?;
        let cancel = echo_agent::agent::CancellationToken::new();
        let driver_cancel = cancel.clone();
        let admission = store
            .reserve_run_driver_admission("aborted-shutdown-waiter".to_string(), cancel)
            .map_err(|error| error.to_string())?;
        let generation_lease = store
            .lease_active_workspace_generation()
            .map_err(|error| error.to_string())?;
        let (cancel_observed_tx, cancel_observed_rx) = tokio::sync::oneshot::channel::<()>();
        let (continue_driver_tx, continue_driver_rx) = tokio::sync::oneshot::channel::<()>();
        let pool_for_driver = Arc::clone(&pool);
        let waiter = store
            .spawn_run_driver(
                admission,
                generation_lease,
                move |mut receipt_owner| async move {
                    receipt_owner.retain(pool_for_driver.retain_for_supervised_run(
                        "aborted-shutdown-waiter".to_string(),
                        execution,
                    ));
                    driver_cancel.cancelled().await;
                    cancel_observed_tx
                        .send(())
                        .map_err(|_| "shutdown cancel observer closed".to_string())?;
                    continue_driver_rx
                        .await
                        .map_err(|error| error.to_string())?;
                    std::fs::rename(&root, &blocked_root)
                        .map_err(|error| format!("block task root: {error}"))?;
                    std::fs::write(&root, b"block directory recreation")
                        .map_err(|error| format!("replace task root: {error}"))?;
                    Err::<(), String>("injected failure after shutdown waiter abort".to_string())
                },
            )
            .map_err(|error| error.to_string())?;
        drop(waiter);

        store.abort_next_run_driver_shutdown_reporter_for_test();
        let first_shutdown_store = Arc::clone(&store);
        let first_shutdown =
            tokio::spawn(async move { first_shutdown_store.shutdown_run_drivers().await });
        tokio::time::timeout(Duration::from_secs(2), cancel_observed_rx)
            .await
            .map_err(|_| "owned shutdown did not cancel the driver".to_string())?
            .map_err(|_| "driver cancel observer closed".to_string())?;
        first_shutdown.abort();
        if first_shutdown.await.is_ok() {
            return Err("first shutdown waiter was not aborted".to_string());
        }
        continue_driver_tx
            .send(())
            .map_err(|_| "parked driver receiver closed".to_string())?;

        let shutdown_error =
            tokio::time::timeout(Duration::from_secs(2), store.shutdown_run_drivers())
                .await
                .map_err(|_| "second shutdown waiter did not observe owned settlement".to_string())?
                .err()
                .ok_or_else(|| "permanent debt was hidden after waiter abort".to_string())?;
        assert_eq!(shutdown_error.abandoned_settlements.len(), 1);
        assert!(
            shutdown_error
                .driver_errors
                .iter()
                .any(|error| error.contains("shutdown reporter failed"))
        );
        let diagnostic = shutdown_error
            .abandoned_settlements
            .first()
            .ok_or_else(|| "abandoned settlement diagnostic is missing".to_string())?;
        assert_eq!(diagnostic.run_id, "aborted-shutdown-waiter");
        assert!(diagnostic.driver_token.is_some());
        assert_eq!(store.active_run_driver_count()?, 0);
        assert_eq!(store.active_run_driver_receipt_count()?, 0);
        let repeated_error = store
            .shutdown_run_drivers()
            .await
            .err()
            .ok_or_else(|| "repeated shutdown lost its typed degradation".to_string())?;
        assert_eq!(repeated_error, shutdown_error);
        tokio::time::timeout(Duration::from_secs(2), pool.shutdown())
            .await
            .map_err(|_| "pool shutdown remained blocked after waiter abort".to_string())??;

        std::fs::remove_file(temp.path().join("tasks")).map_err(|error| error.to_string())?;
        std::fs::rename(temp.path().join("tasks-blocked"), temp.path().join("tasks"))
            .map_err(|error| error.to_string())?;
        let run = store
            .get_run("aborted-shutdown-waiter")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "abandoned run disappeared".to_string())?;
        assert_eq!(
            run.status,
            crate::tasks::task_runtime::TaskRunStatus::Running
        );
        Ok(())
    }
}
