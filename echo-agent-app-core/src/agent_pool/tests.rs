#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[tokio::test]
    async fn acquired_cached_and_future_pool_agents_keep_final_native_manager_policy() -> TestResult
    {
        use crate::infra::native_manager_runtime_contract::{Case, Fixture};
        let fixture = Fixture::new().await?;
        // 该 bootstrap 只有框架默认工具，不手工安装产品 provider；由原 pool 最终接线负责。
        let primary = echo_agent::agent::ReactAgentBuilder::new()
            .llm_client(Arc::new(echo_agent::testing::MockLlmClient::new()))
            .enable_tools()
            .working_dir(fixture.root.path().to_path_buf())
            .build();
        let mut primary = match primary {
            Ok(primary) => primary,
            Err(error) => {
                let closed = fixture.finish().await;
                return Err(format!(
                    "pool bootstrap fixture failed: {error}; cleanup={closed:?}"
                ));
            }
        };
        primary.set_sandbox_executor(fixture.sandbox.clone());
        let mut pool =
            AgentPool::new_for_test(AgentHandle::new(primary), None, None, 3, false).await;
        // 复用已有测试构造接缝，仅替换本地数据位置和原服务，不访问用户数据根。
        pool.shared.workspace_data_root = Some(fixture.root.path().join("pool-product"));
        pool.shared.command_cell_runtime = Some(fixture.cells.clone());
        pool.shared.product_data_io = Some(fixture.io.clone());
        pool.shared.sandbox_manager = Some(fixture.sandbox.clone());
        pool.shared.local_skill_executor = Some(fixture.sandbox.clone());
        pool.apply_working_dir(Some(fixture.root.path().to_path_buf()))
            .await;
        let outcome = async {
            for (label, conversation) in [
                ("pool-acquired", "native-first"),
                ("pool-cached", "native-first"),
                ("pool-future", "native-second"),
            ] {
                let case = Case::new(&fixture, label)?;
                pool.set_llm_client_override_for_test(case.client.clone())
                    .await;
                let lease = pool
                    .acquire(conversation)
                    .await
                    .map_err(|error| error.to_string())?;
                // 缓存实例的模型 transport 也沿原 handle 替换，provider/guard 从未注入测试值。
                lease
                    .agent()
                    .write(|agent| {
                        let original_owned_critic =
                            agent.critic_owner() == Some(crate::infra::EKO_MODEL_CRITIC_OWNER);
                        agent.set_llm_client(case.client.clone());
                        // 缓存实例保有上一轮 Critic：沿既有 owned API 同步它的 Mock transport，真实 verifier/阈值不变。
                        if original_owned_critic {
                            agent.set_owned_critic(
                                crate::infra::EKO_MODEL_CRITIC_OWNER,
                                Arc::new(
                                    echo_agent::agent::critic::LlmCritic::new(case.client.clone())
                                        .with_pass_threshold(7.0),
                                ),
                            );
                        }
                    })
                    .await;
                let target: Arc<dyn echo_agent::agent::Agent> =
                    Arc::from(lease.agent().to_boxed_agent().await);
                let settled = case.run(&fixture, target).await;
                drop(lease);
                settled?;
            }
            Ok::<(), String>(())
        }
        .await;
        let pool_closed = pool.shutdown().await;
        drop(pool);
        let fixture_closed = fixture.finish().await;
        eprintln!(
            "final pool manager fixture cleanup: outcome={outcome:?}; pool={pool_closed:?}; fixture={fixture_closed:?}"
        );
        outcome?;
        pool_closed?;
        fixture_closed
    }
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
                        .await?;
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
            root: root.clone(),
            project_root: None,
            kind: WorkspaceKind::General,
            metadata: crate::workspace::WorkspaceMetadata::new(
                crate::workspace::ProjectLocation::Local { path: root.clone() },
            ),
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
    async fn linked_project_agents_and_skills_reach_workspace_agents() -> TestResult {
        let seed = Arc::new(create_test_pool(3, false).await?);
        seed.update_mcp_config_snapshot(Default::default()).await;
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace_root = temp.path().join("workspace-data");
        let project_root = temp.path().join("linked-project");
        let skill_dir = project_root.join(".eko/skills/project-review");
        let old_project_root = temp.path().join("old-project");
        let old_skill_dir = old_project_root.join(".eko/skills/project-review");
        std::fs::create_dir_all(&workspace_root).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&old_skill_dir).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(project_root.join(".git")).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&skill_dir).map_err(|error| error.to_string())?;
        std::fs::write(project_root.join("AGENTS.md"), "LINKED_PROJECT_AGENT_RULE")
            .map_err(|error| error.to_string())?;
        std::fs::write(
            old_skill_dir.join("SKILL.md"),
            "---\nname: project-review\ndescription: Old linked project\n---\n# Old",
        )
        .map_err(|error| error.to_string())?;
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: project-review\ndescription: Review linked project\n---\n# Review",
        )
        .map_err(|error| error.to_string())?;
        let canonical_project_root = project_root
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let seed_primary = seed
            .primary_agent()
            .await
            .map_err(|error| error.to_string())?;
        crate::skills_hub::load_project_skills(&seed_primary, Some(&old_project_root))
            .await
            .map_err(|error| error.to_string())?;
        seed_primary
            .write_async(|agent| {
                Box::pin(async move {
                    agent
                        .tag_skills_source(&["project-review".to_string()], "plugin:old-project")
                        .await;
                })
            })
            .await;
        let seed_generation = seed_primary.read(|agent| agent.skill_descriptors()).await;
        let mut publication = seed
            .begin_plugin_publication()
            .await
            .map_err(|error| error.to_string())?;
        publication
            .prepare(AgentPluginGeneration::new(
                1,
                seed_generation,
                Vec::new(),
                None,
            ))
            .await
            .map_err(|error| error.to_string())?;
        publication
            .commit()
            .await
            .map_err(|error| error.to_string())?;

        let now = chrono::Utc::now();
        let workspace = crate::workspace::Workspace {
            id: crate::workspace::WorkspaceId::from_name("linked-project-context"),
            name: "Linked project context".to_string(),
            root: workspace_root.clone(),

            project_root: Some(project_root.clone()),
            kind: WorkspaceKind::General,
            metadata: crate::workspace::WorkspaceMetadata::new(
                crate::workspace::ProjectLocation::Local {
                    path: project_root.clone(),
                },
            ),
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
        let conversation = runtime
            .pool()
            .acquire("linked-project-conversation")
            .await
            .map_err(|error| error.to_string())?;

        for agent in [runtime.primary_agent(), conversation.agent()] {
            let (has_skill, descriptor, context) = agent
                .read(|agent| {
                    (
                        agent.has_skill("project-review"),
                        agent
                            .skill_descriptors()
                            .into_iter()
                            .find(|descriptor| descriptor.name == "project-review")
                            .map(|descriptor| (descriptor.source, descriptor.location)),
                        agent.context().clone(),
                    )
                })
                .await;
            let (source, location) = descriptor.ok_or_else(|| {
                "project-review descriptor was not retained in the workspace Agent".to_string()
            })?;
            assert!(has_skill);
            assert_eq!(source.as_deref(), Some("eko:project-skill:project-review"));
            assert!(location.starts_with(canonical_project_root.as_path()));
            let messages = context
                .lock()
                .await
                .messages()
                .iter()
                .filter_map(|message| message.content.as_text_ref())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(messages.contains("LINKED_PROJECT_AGENT_RULE"));
        }
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
    async fn side_conversation_model_override_replaces_the_cached_child_agent() -> TestResult {
        use echo_agent::agent::Agent;

        let agent = create_test_agent_handle()?;
        let mut config = EkoConfig {
            configured_models: vec![
                crate::config::ConfiguredModel {
                    id: "local:a".to_string(),
                    display_name: "A".to_string(),
                    provider: "local".to_string(),
                    model: "a".to_string(),
                    ..crate::config::ConfiguredModel::default()
                },
                crate::config::ConfiguredModel {
                    id: "local:b".to_string(),
                    display_name: "B".to_string(),
                    provider: "local".to_string(),
                    model: "b".to_string(),
                    ..crate::config::ConfiguredModel::default()
                },
            ],
            ..EkoConfig::default()
        };
        config.model.default_model_id = Some("local:a".to_string());
        config.model_providers.insert(
            "local".to_string(),
            crate::config::ModelProviderConfig {
                base_url: Some("http://127.0.0.1:11434/v1/chat/completions".to_string()),
                ..Default::default()
            },
        );
        let pool = AgentPool::new_for_test_with_config(&agent, None, None, 3, false, config).await;

        pool.configure_side_conversation("side-1", Some("local:a"))
            .await
            .map_err(|error| error.to_string())?;
        let first = pool
            .acquire("side-1")
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            first
                .agent()
                .read(|child| child.model_name().to_string())
                .await,
            "a"
        );
        drop(first);

        pool.configure_side_conversation("side-1", Some("local:b"))
            .await
            .map_err(|error| error.to_string())?;
        let second = pool
            .acquire("side-1")
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            second
                .agent()
                .read(|child| child.model_name().to_string())
                .await,
            "b"
        );
        drop(second);
        assert!(
            pool.configure_side_conversation("side-1", Some("missing:model"))
                .await
                .is_err()
        );
        assert_eq!(
            pool.conversation_model_override("side-1").await.as_deref(),
            Some("local:b")
        );
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

    async fn assert_default_subagents_use_selected_pool_store(
        pool: &AgentPool,
        conversation: &str,
        selected: &Arc<dyn echo_agent::memory::Store>,
        other_scope: &Arc<dyn echo_agent::memory::Store>,
    ) -> TestResult {
        let namespace = ["eko-store-owner-contract"];
        let sentinel = format!("{conversation}-selected");
        selected
            .put(&namespace, &sentinel, serde_json::json!(conversation))
            .await
            .map_err(|error| error.to_string())?;
        let lease = pool
            .acquire(conversation)
            .await
            .map_err(|error| error.to_string())?;
        let (parent_store, registry, manager) = lease
            .agent()
            .read(|agent| {
                (
                    agent.store().cloned(),
                    agent.subagent_registry().clone(),
                    agent.memory_layer_manager().cloned(),
                )
            })
            .await;
        let parent_store = parent_store.ok_or_else(|| "池父 Agent 没有 Store".to_string())?;
        assert!(Arc::ptr_eq(&parent_store, selected));
        let expected_manager = pool
            .shared
            .review_integration
            .as_ref()
            .ok_or_else(|| "原 Review owner 缺失".to_string())?
            .lease_generation()
            .map_err(|error| error.to_string())?
            .layer_manager()
            .map_err(|error| error.to_string())?;
        assert!(
            manager
                .as_ref()
                .is_some_and(|manager| Arc::ptr_eq(manager, &expected_manager))
        );
        let consumers = pool
            .agents
            .read()
            .await
            .get(conversation)
            .map(|agent| agent.model_consumers.clone())
            .ok_or_else(|| "实际池 Agent 未发布".to_string())?;
        for role in ["reviewer", "implementer"] {
            let inherited = consumers
                .inherited_handle_for_test(role)
                .ok_or_else(|| format!("原继承实例缺失: {role}"))?;
            let inherited_store = inherited
                .read(|agent| agent.store().cloned())
                .await
                .ok_or_else(|| format!("继承实例 Store 缺失: {role}"))?;
            // 真实 registry→默认 FnAgentFactory；不替换 factory 或注册测试替身。
            let fork = registry
                .create_fresh_agent(role)
                .await
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("原 Fork 工厂缺失: {role}"))?;
            let fork_store = consumers
                .fork_memory_store_for_test(role)
                .await
                .ok_or_else(|| format!("真实 Fork 没有 Store 观察: {role}"))?;
            if role == "reviewer" {
                for write_tool in ["remember", "forget"] {
                    assert!(!fork.tool_names().iter().any(|name| name == write_tool));
                    assert!(
                        !inherited
                            .read(|agent| agent.tool_names().iter().any(|name| name == write_tool))
                            .await
                    );
                }
            }
            for (kind, child_store) in [("inherited", inherited_store), ("fork", fork_store)] {
                assert!(
                    Arc::ptr_eq(&child_store, selected),
                    "{conversation}/{role}/{kind} 未使用所选 generation 的 Store"
                );
                let item = child_store
                    .get(&namespace, &sentinel)
                    .await
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| {
                        format!("{conversation}/{role}/{kind} 未读到所选 Store sentinel")
                    })?;
                assert_eq!(item.value, serde_json::json!(conversation));
                let key = format!("{conversation}-{role}-{kind}");
                child_store
                    .put(&namespace, &key, serde_json::json!(kind))
                    .await
                    .map_err(|error| error.to_string())?;
                assert_eq!(
                    selected
                        .get(&namespace, &key)
                        .await
                        .map_err(|error| error.to_string())?
                        .map(|item| item.value),
                    Some(serde_json::json!(kind))
                );
                assert!(
                    other_scope
                        .get(&namespace, &key)
                        .await
                        .map_err(|error| error.to_string())?
                        .is_none()
                );
            }
            drop(fork);
        }
        assert!(
            other_scope
                .get(&namespace, &sentinel)
                .await
                .map_err(|error| error.to_string())?
                .is_none()
        );
        drop(lease);
        Ok(())
    }

    #[tokio::test]
    async fn global_pool_default_inherited_and_real_fork_keep_selected_store_owner() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::create_dir_all(temp.path().join(".git")).map_err(|error| error.to_string())?;
        let selected = Arc::new(echo_agent::memory::InMemoryStore::new())
            as Arc<dyn echo_agent::memory::Store>;
        let other = Arc::new(echo_agent::memory::InMemoryStore::new())
            as Arc<dyn echo_agent::memory::Store>;
        let integration = Arc::new(crate::evolution::ReviewIntegration::new_scoped(
            echo_agent::evolution::ReviewConfig::default(),
            temp.path().join(".eko"),
            selected.clone(),
            "global".to_string(),
            "global".to_string(),
        ));
        let manager = integration
            .lease_generation()
            .map_err(|error| error.to_string())?
            .layer_manager()
            .map_err(|error| error.to_string())?;
        let primary = create_test_agent_handle()?;
        primary
            .write(|agent| agent.install_memory_layer_manager(manager))
            .await
            .map_err(|error| error.to_string())?;
        let pool =
            AgentPool::new_for_test(primary, Some(integration), Some(selected.clone()), 3, false)
                .await;
        pool.apply_working_dir(Some(temp.path().to_path_buf()))
            .await;
        let result = assert_default_subagents_use_selected_pool_store(
            &pool,
            "global-memory-owner",
            &selected,
            &other,
        )
        .await;
        pool.shutdown().await?;
        result
    }

    #[tokio::test]
    async fn workspace_pool_default_inherited_and_real_forks_keep_scope_store_owner() -> TestResult
    {
        let seed = Arc::new(create_test_pool(3, false).await?);
        seed.update_mcp_config_snapshot(Default::default()).await;
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let registry = crate::workspace::runtime::WorkspaceRuntimeRegistry::new();
        let mut runtimes = Vec::new();
        for name in ["memory-owner-workspace-a", "memory-owner-workspace-b"] {
            let root = temp.path().join(name);
            std::fs::create_dir_all(root.join(".git")).map_err(|error| error.to_string())?;
            let now = chrono::Utc::now();
            let workspace = crate::workspace::Workspace {
                id: crate::workspace::WorkspaceId::from_name(name),
                name: name.to_string(),
                root: root.clone(),
                project_root: None,
                kind: WorkspaceKind::General,
                metadata: crate::workspace::WorkspaceMetadata::new(
                    crate::workspace::ProjectLocation::Local { path: root },
                ),
                product_data_generation: String::new(),
                created_at: now,
                last_active: now,
            };
            let host = registry
                .get_or_open(workspace)
                .await
                .map_err(|error| error.to_string())?;
            runtimes.push(
                host.get_or_open_execution(&seed)
                    .await
                    .map_err(|error| error.to_string())?,
            );
        }
        let first = runtimes
            .first()
            .ok_or_else(|| "工作区 A 缺失".to_string())?;
        let second = runtimes.get(1).ok_or_else(|| "工作区 B 缺失".to_string())?;
        let first_generation = first
            .review_integration()
            .lease_generation()
            .map_err(|error| error.to_string())?;
        let second_generation = second
            .review_integration()
            .lease_generation()
            .map_err(|error| error.to_string())?;
        let first_store = first_generation.memory_store();
        let second_store = second_generation.memory_store();
        assert!(!Arc::ptr_eq(&first_store, &second_store));
        assert!(!Arc::ptr_eq(
            &first_generation
                .layer_manager()
                .map_err(|error| error.to_string())?,
            &second_generation
                .layer_manager()
                .map_err(|error| error.to_string())?
        ));
        for (runtime, selected, other, conversation) in [
            (
                first,
                &first_store,
                &second_store,
                "workspace-a-memory-owner",
            ),
            (
                second,
                &second_store,
                &first_store,
                "workspace-b-memory-owner",
            ),
        ] {
            let result = assert_default_subagents_use_selected_pool_store(
                &runtime.pool(),
                conversation,
                selected,
                other,
            )
            .await;
            runtime.pool().shutdown().await?;
            result?;
        }
        seed.shutdown().await?;
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
            .await
            .map_err(|error| error.to_string())?;
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
            assert_eq!(
                handle.read(|agent| agent.thinking().cloned()).await,
                thinking
            );
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
                .and_then(|pooled| pooled.model_consumers.inherited_handle_for_test("reviewer"))
                .ok_or_else(|| format!("{key} has no inherited Subagent"))?;
            assert_eq!(
                inherited.read(|agent| agent.thinking().cloned()).await,
                thinking
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn thinking_publication_does_not_wait_for_busy_agents() -> TestResult {
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
    async fn unbounded_primary_existing_and_future_agents_have_no_product_iteration_cap()
    -> TestResult {
        // 此控制必须从 EKO 真实主工厂启动；通用 framework 测试模板仍保留自身默认预算。
        let mut config = EkoConfig::default();
        config.agent.max_iterations = 10;
        let created = crate::infra::create_agent_with_diagnostics(
            &crate::infra::AgentCreateParams::default(),
            &config,
        )
        .await?;
        let pool =
            AgentPool::new_for_test(AgentHandle::new(created.agent), None, None, 3, false).await;
        pool.app_config.write().await.agent.max_iterations = 10;
        let existing = pool
            .acquire("iterations-existing")
            .await
            .map_err(|error| error.to_string())?;

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
                usize::MAX
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
            usize::MAX
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
    async fn permanent_terminal_debt_retains_original_receipts_until_durable_retry() -> TestResult {
        use crate::tasks::task_runtime::store::RunDriverExecutionReceipt;
        struct CountedMemoryRelease {
            probe: MemoryReleaseProbe,
            releases: Arc<std::sync::atomic::AtomicUsize>,
        }
        impl RunDriverExecutionReceipt for CountedMemoryRelease {
            fn release(self: Box<Self>) -> futures::future::BoxFuture<'static, ()> {
                let Self { probe, releases } = *self;
                Box::pin(async move {
                    Box::new(probe).release().await;
                    let mut count = releases.load(Ordering::SeqCst);
                    loop {
                        match releases.compare_exchange_weak(
                            count,
                            count.saturating_add(1),
                            Ordering::SeqCst,
                            Ordering::SeqCst,
                        ) {
                            Ok(_) => break,
                            Err(current) => count = current,
                        }
                    }
                })
            }
        }
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("tasks");
        let blocked_root = temp.path().join("tasks-blocked");
        let store = Arc::new(
            crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory_with_shadow_root(
                root.clone(),
            )
            .map_err(|error| error.to_string())?,
        );
        let _resource_scope = store.test_resource_scope();
        let physical_fault = store.block_shadow_after_physical_completion_for_test(
            "permanent-terminal-debt",
            blocked_root.clone(),
        )?;

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
        let memory_releases = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted_releases = memory_releases.clone();
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
                    receipt_owner
                        .retain(CountedMemoryRelease {
                            probe: MemoryReleaseProbe {
                                pool: Arc::clone(&pool_for_driver),
                                released_after_pool: release_probe,
                            },
                            releases: counted_releases,
                        })
                        .map_err(|error| error.to_string())?;
                    receipt_owner
                        .retain(pool_for_driver.retain_for_supervised_run(
                            "permanent-terminal-debt".to_string(),
                            pool_execution,
                        ))
                        .map_err(|error| error.to_string())?;

                    Err::<(), String>("injected permanent driver failure".to_string())
                },
            )
            .map_err(|error| error.to_string())?;
        let driver_error = waiter
            .await
            .map_err(|error| error.to_string())?
            .err()
            .ok_or_else(|| "permanent driver failure was not reported".to_string())?;
        let physical_event = physical_fault
            .lock()
            .map_err(|_| "physical completion fixture lock poisoned".to_string())?
            .clone()
            .ok_or_else(|| "physical completion did not reach fault boundary".to_string())??;
        assert_eq!(physical_event.run_id, "permanent-terminal-debt");
        assert_eq!(
            physical_event
                .payload
                .get("physical_owner_intent")
                .and_then(|intent| intent.get("physical_terminal_observed"))
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );
        assert!(!driver_error.is_empty());
        assert!(
            driver_error.contains("injected permanent driver failure")
                && driver_error.contains("terminal settlement failed")
                && driver_error.contains("run not found: permanent-terminal-debt"),
            "actual error: {driver_error}"
        );

        let canonical_journal = blocked_root.join("permanent-terminal-debt/events.jsonl");
        let journal_before_missing =
            std::fs::read(&canonical_journal).map_err(|error| error.to_string())?;
        let original_frame: serde_json::Value = serde_json::from_str(
            std::str::from_utf8(&journal_before_missing)
                .map_err(|error| error.to_string())?
                .lines()
                .next()
                .ok_or_else(|| "original journal frame missing".to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let journal_identity = original_frame
            .get("journal_identity")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "original journal identity missing".to_string())?;
        // The debt already exists. Losing the root entirely must not be
        // interpreted as evidence that the original owner never started.
        std::fs::remove_file(&root).map_err(|error| error.to_string())?;
        assert!(!root.exists());
        let shutdown_error =
            tokio::time::timeout(Duration::from_secs(2), store.shutdown_run_drivers())
                .await
                .map_err(|_| "TaskRun driver shutdown timed out on permanent debt".to_string())?
                .err()
                .ok_or_else(|| "permanent settlement debt was not reported".to_string())?;
        assert_eq!(shutdown_error.abandoned_settlements.len(), 1);
        let debt = shutdown_error
            .abandoned_settlements
            .first()
            .ok_or_else(|| "original terminal debt diagnostic missing".to_string())?;
        assert_eq!(debt.run_id, "permanent-terminal-debt");
        assert!(debt.driver_token.is_some());
        assert_eq!(
            debt.root,
            std::fs::canonicalize(temp.path())
                .map_err(|error| error.to_string())?
                .join("tasks")
        );
        assert_eq!(
            debt.target,
            crate::tasks::task_runtime::TaskRunStatus::Failed
        );
        assert!(!debt.error.is_empty());
        assert_eq!(store.active_run_driver_count()?, 1);
        assert_eq!(store.active_run_driver_receipt_count()?, 2);
        assert!(!released_after_pool.load(Ordering::SeqCst));
        assert_eq!(memory_releases.load(Ordering::SeqCst), 0);
        assert!(store.begin_workspace_transition().await.is_err());
        assert!(
            tokio::time::timeout(Duration::from_secs(2), pool.shutdown())
                .await
                .map_err(|_| "unknown pool ownership was not observable".to_string())?
                .is_err()
        );
        assert_eq!(store.active_run_driver_count()?, 1);
        assert_eq!(store.active_run_driver_receipt_count()?, 2);
        assert!(!released_after_pool.load(Ordering::SeqCst));
        assert_eq!(memory_releases.load(Ordering::SeqCst), 0);
        assert_eq!(
            std::fs::read(&canonical_journal).map_err(|error| error.to_string())?,
            journal_before_missing
        );
        let owner_id = physical_event
            .payload
            .get("physical_owner_intent")
            .and_then(|intent| intent.get("owner_id"))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "original physical owner id missing".to_string())?;
        if root.exists() {
            // Cached authority may rematerialize derived projections while the
            // original journal is unavailable. Preserve those files separately
            // before restoring the same original directory; never discard or
            // substitute a canonical journal to make retry succeed.
            assert!(root.is_dir() && !root.is_symlink());
            let projected_run = root.join("permanent-terminal-debt");
            for entry in std::fs::read_dir(&root).map_err(|error| error.to_string())? {
                let entry = entry.map_err(|error| error.to_string())?;
                assert_eq!(entry.path(), projected_run);
                assert!(
                    entry
                        .file_type()
                        .map_err(|error| error.to_string())?
                        .is_dir()
                );
            }
            for entry in std::fs::read_dir(&projected_run).map_err(|error| error.to_string())? {
                let entry = entry.map_err(|error| error.to_string())?;
                let name = entry.file_name();
                assert!(name == "run-state.json" || name == "checkpoint.json");
                assert!(
                    entry
                        .file_type()
                        .map_err(|error| error.to_string())?
                        .is_file()
                );
            }
            assert!(!projected_run.join("events.jsonl").exists());
            let projection: crate::tasks::task_runtime::RunStateSnapshot = serde_json::from_slice(
                &std::fs::read(projected_run.join("run-state.json"))
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            let completed_sequence =
                u64::try_from(physical_event.seq).map_err(|error| error.to_string())?;
            assert_eq!(projection.journal_sequence, completed_sequence);
            assert_eq!(
                projection.run.status,
                crate::tasks::task_runtime::TaskRunStatus::Running
            );
            assert_eq!(projection.event_index.physical_owner_intents.len(), 1);
            let projected_owner = projection
                .event_index
                .physical_owner_intents
                .first()
                .ok_or_else(|| "rebuilt original owner projection missing".to_string())?;
            assert_eq!(projected_owner.owner_id, owner_id);
            assert!(projected_owner.physical_terminal_observed);
            let checkpoint: serde_json::Value = serde_json::from_slice(
                &std::fs::read(projected_run.join("checkpoint.json"))
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            assert_eq!(
                checkpoint
                    .get("journal_identity")
                    .and_then(serde_json::Value::as_str),
                Some(journal_identity)
            );
            assert_eq!(
                checkpoint
                    .get("sequence")
                    .and_then(serde_json::Value::as_u64),
                Some(completed_sequence)
            );

            std::fs::rename(&root, temp.path().join("tasks-derived-projections"))
                .map_err(|error| error.to_string())?;
        }
        std::fs::rename(temp.path().join("tasks-blocked"), temp.path().join("tasks"))
            .map_err(|error| error.to_string())?;
        let restored_journal = temp
            .path()
            .join("tasks/permanent-terminal-debt/events.jsonl");
        assert_eq!(
            std::fs::read(&restored_journal).map_err(|error| error.to_string())?,
            journal_before_missing
        );
        let restored = store
            .get_run_state("permanent-terminal-debt")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "restored original source missing".to_string())?;
        assert_eq!(restored.event_index.physical_owner_intents.len(), 1);
        let original = restored
            .event_index
            .physical_owner_intents
            .first()
            .ok_or_else(|| "restored original intent missing".to_string())?;
        assert_eq!(original.owner_id, owner_id);
        assert!(original.physical_terminal_observed);
        store
            .retry_run_settlement_debts()
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(store.active_run_driver_receipt_count()?, 0);
        assert!(released_after_pool.load(Ordering::SeqCst));
        assert_eq!(memory_releases.load(Ordering::SeqCst), 1);
        let after_retry = std::fs::read(&restored_journal).map_err(|error| error.to_string())?;
        assert!(after_retry.starts_with(&journal_before_missing));
        let events = store
            .list_events("permanent-terminal-debt", 0)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            events
                .iter()
                .filter(|event| {
                    event.event_type
                        == crate::tasks::task_runtime::RuntimeEventKind::RecoveryResolved
                        && event
                            .payload
                            .get("physical_owner_intent")
                            .and_then(|intent| intent.get("owner_id"))
                            .and_then(serde_json::Value::as_str)
                            == Some(owner_id)
                })
                .count(),
            1
        );
        assert!(
            store
                .get_run_state("permanent-terminal-debt")
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "settled source missing".to_string())?
                .event_index
                .physical_owner_intents
                .is_empty()
        );
        store
            .retry_run_settlement_debts()
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            std::fs::read(&restored_journal).map_err(|error| error.to_string())?,
            after_retry
        );
        assert_eq!(memory_releases.load(Ordering::SeqCst), 1);
        let transition = store
            .begin_workspace_transition()
            .await
            .map_err(|error| error.to_string())?;
        drop(transition);
        tokio::time::timeout(Duration::from_secs(2), pool.shutdown())
            .await
            .map_err(|_| "known original receipt did not release the pool".to_string())??;
        let run = store
            .get_run("permanent-terminal-debt")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "non-terminal run disappeared from disk".to_string())?;
        assert_eq!(
            run.status,
            crate::tasks::task_runtime::TaskRunStatus::Failed
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
        let _resource_scope = store.test_resource_scope();
        let physical_fault = store.block_shadow_after_physical_completion_for_test(
            "aborted-shutdown-waiter",
            blocked_root.clone(),
        )?;
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
                    receipt_owner
                        .retain(pool_for_driver.retain_for_supervised_run(
                            "aborted-shutdown-waiter".to_string(),
                            execution,
                        ))
                        .map_err(|error| error.to_string())?;
                    driver_cancel.cancelled().await;
                    cancel_observed_tx
                        .send(())
                        .map_err(|_| "shutdown cancel observer closed".to_string())?;
                    continue_driver_rx
                        .await
                        .map_err(|error| error.to_string())?;

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
        let physical_event = physical_fault
            .lock()
            .map_err(|_| "physical completion fixture lock poisoned".to_string())?
            .clone()
            .ok_or_else(|| "physical completion did not reach fault boundary".to_string())??;
        assert_eq!(physical_event.run_id, "aborted-shutdown-waiter");
        assert_eq!(
            physical_event
                .payload
                .get("physical_owner_intent")
                .and_then(|intent| intent.get("physical_terminal_observed"))
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );
        assert_eq!(shutdown_error.abandoned_settlements.len(), 1);
        let debt = shutdown_error
            .abandoned_settlements
            .first()
            .ok_or_else(|| "original terminal debt diagnostic missing".to_string())?;
        assert_eq!(debt.run_id, "aborted-shutdown-waiter");
        assert!(debt.driver_token.is_some());
        assert_eq!(
            debt.root,
            std::fs::canonicalize(temp.path())
                .map_err(|error| error.to_string())?
                .join("tasks")
        );
        assert_eq!(
            debt.target,
            crate::tasks::task_runtime::TaskRunStatus::Cancelled
        );
        assert!(!debt.error.is_empty());
        assert!(
            shutdown_error
                .driver_errors
                .iter()
                .any(|error| error.contains("shutdown reporter failed"))
        );
        assert_eq!(store.active_run_driver_count()?, 1);
        assert_eq!(store.active_run_driver_receipt_count()?, 1);
        let repeated_error = store
            .shutdown_run_drivers()
            .await
            .err()
            .ok_or_else(|| "repeated shutdown lost its typed degradation".to_string())?;
        assert_eq!(repeated_error, shutdown_error);
        assert!(
            tokio::time::timeout(Duration::from_secs(2), pool.shutdown())
                .await
                .map_err(|_| "unknown original lease did not report its owner".to_string())?
                .is_err()
        );

        std::fs::remove_file(temp.path().join("tasks")).map_err(|error| error.to_string())?;
        std::fs::rename(temp.path().join("tasks-blocked"), temp.path().join("tasks"))
            .map_err(|error| error.to_string())?;
        store
            .retry_run_settlement_debts()
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(store.active_run_driver_receipt_count()?, 0);
        tokio::time::timeout(Duration::from_secs(2), pool.shutdown())
            .await
            .map_err(|_| "settled original receipt remained active".to_string())??;
        let run = store
            .get_run("aborted-shutdown-waiter")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "abandoned run disappeared".to_string())?;
        assert_eq!(
            run.status,
            crate::tasks::task_runtime::TaskRunStatus::Cancelled
        );
        Ok(())
    }

    fn domain_components_for_test(
        root: &std::path::Path,
        content: &str,
    ) -> Result<crate::domain_plugin::PreparedDomainComponents, String> {
        use crate::domain_plugin::*;
        let binding = test_binding(false)?;
        let mut identity = binding.snapshot.plugin;
        identity.content_sha256 = content_digest(&content)?;
        let declaration = DomainDeclaration {
            schema_version: 1,
            method: binding.snapshot.method,
            default_subagent_roles: Default::default(),
            allowed_tools: Vec::new(),
            required_artifacts: Vec::new(),
            execution_checks: Vec::new(),
            acceptance_criteria: Vec::new(),
            review_checklist: Vec::new(),
            inputs: Vec::new(),
        };
        let mut components = PreparedDomainComponents::default();
        components.plugins.insert(
            identity.plugin_id.clone(),
            PreparedDomainPlugin {
                closure: vec![identity.plugin_id.clone()],
                identity,
                root: root.to_path_buf(),
                declaration: Some(declaration),
                inputs: Vec::new(),
                binding_error: None,
            },
        );
        Ok(components)
    }

    async fn publish_domain_for_test(
        pool: &AgentPool,
        components: crate::domain_plugin::PreparedDomainComponents,
    ) -> TestResult {
        let mut publication = pool
            .begin_plugin_publication()
            .await
            .map_err(|error| error.to_string())?;
        publication
            .prepare(
                AgentPluginGeneration::new(1, Vec::new(), Vec::new(), None)
                    .with_domain_components(components, None),
            )
            .await?;
        publication.commit().await
    }

    #[tokio::test]
    async fn domain_target_lease_checks_its_own_generation_not_default_pool() -> TestResult {
        let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
        let original = create_test_pool(2, false).await?;
        let target = create_test_pool(2, false).await?;
        let drifted = create_test_pool(2, false).await?;
        publish_domain_for_test(
            &original,
            domain_components_for_test(directory.path(), "default content")?,
        )
        .await?;
        publish_domain_for_test(
            &target,
            domain_components_for_test(directory.path(), "target content")?,
        )
        .await?;
        publish_domain_for_test(
            &drifted,
            domain_components_for_test(directory.path(), "changed target content")?,
        )
        .await?;
        let target_lease = target
            .acquire("target")
            .await
            .map_err(|error| error.to_string())?;
        let admitted_components = &target_lease
            .domain_generation
            .as_ref()
            .ok_or("admitted domain data missing")?
            .0;
        assert!(Arc::ptr_eq(
            admitted_components,
            &target.agent_generation.read().await.domain_components
        ));
        let binding = target.bind_domain(&target_lease, "evidence-domain").await?;
        target_lease.verify_domain_binding(&binding).await?;
        assert!(
            original
                .verify_domain_binding(&target_lease, &binding)
                .await
                .is_err()
        );
        let drifted_lease = drifted
            .acquire("target")
            .await
            .map_err(|error| error.to_string())?;
        assert!(drifted_lease.verify_domain_binding(&binding).await.is_err());
        let same_target = create_test_pool(2, false).await?;
        publish_domain_for_test(
            &same_target,
            domain_components_for_test(directory.path(), "target content")?,
        )
        .await?;
        let same_lease = same_target
            .acquire("target")
            .await
            .map_err(|error| error.to_string())?;
        same_lease.verify_domain_binding(&binding).await?;
        drop((target_lease, drifted_lease, same_lease));
        assert_eq!(target.active_execution_count(), 0);
        assert_eq!(drifted.active_execution_count(), 0);
        assert_eq!(same_target.active_execution_count(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn domain_same_name_provider_replacement_is_rejected_without_leaking_lease() -> TestResult
    {
        let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
        let pool = create_test_pool(2, false).await?;
        publish_domain_for_test(
            &pool,
            domain_components_for_test(directory.path(), "content")?,
        )
        .await?;
        let lease = pool
            .acquire("conversation")
            .await
            .map_err(|error| error.to_string())?;
        let binding = pool.bind_domain(&lease, "evidence-domain").await?;
        let registry = lease
            .agent()
            .read(|agent| agent.subagent_registry().clone())
            .await;
        let mut definition = registry
            .list_available()
            .await
            .into_iter()
            .find(|definition| definition.name == "explorer")
            .ok_or("explorer missing")?;
        definition.kind = echo_agent::subagent::SubagentKind::Plugin {
            source: "unrelated-provider".to_string(),
        };
        registry
            .register(definition, Box::new(create_test_agent()?))
            .await;
        assert!(lease.verify_domain_binding(&binding).await.is_err());
        assert_eq!(pool.active_execution_count(), 1);
        drop(lease);
        assert_eq!(pool.active_execution_count(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn cancelled_domain_publication_wait_does_not_release_original_execution() -> TestResult {
        let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
        let pool = Arc::new(create_test_pool(2, false).await?);
        publish_domain_for_test(
            &pool,
            domain_components_for_test(directory.path(), "content")?,
        )
        .await?;
        let lease = pool
            .acquire("conversation")
            .await
            .map_err(|error| error.to_string())?;
        let binding = pool.bind_domain(&lease, "evidence-domain").await?;
        let waiter_pool = pool.clone();
        let waiter =
            tokio::spawn(async move { waiter_pool.begin_plugin_publication().await.map(|_| ()) });
        tokio::time::timeout(Duration::from_secs(2), async {
            while !pool.workspace_transitioning.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "publication did not close admission")?;
        assert!(!waiter.is_finished());
        assert_eq!(pool.active_execution_count(), 1);
        waiter.abort();
        assert!(waiter.await.is_err_and(|error| error.is_cancelled()));
        lease.verify_domain_binding(&binding).await?;
        assert_eq!(pool.active_execution_count(), 1);
        drop(lease);
        assert_eq!(pool.active_execution_count(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn domain_physical_projection_outlives_taken_parent_cell_without_another_slot()
    -> TestResult {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let pool = create_test_pool(2, false).await?;
        publish_domain_for_test(&pool, domain_components_for_test(dir.path(), "physical")?).await?;
        let execution = pool
            .acquire("physical-parent")
            .await
            .map_err(|error| error.to_string())?;
        let binding = pool.bind_domain(&execution, "evidence-domain").await?;
        let process_before = pool.process_agent_execution.snapshot().active;
        let cell = Arc::new(std::sync::Mutex::new(Some(execution)));
        let guard = {
            let owner = cell.lock().map_err(|_| "parent cell unavailable")?;
            owner
                .as_ref()
                .ok_or("parent receipt missing")?
                .domain_resource_guard("physical-run", &binding, None)?
        };
        let child = guard.clone();
        cell.lock().map_err(|_| "parent cell unavailable")?.take();
        drop(guard);
        assert_eq!(pool.active_execution_count(), 1);
        assert_eq!(
            pool.process_agent_execution.snapshot().active,
            process_before
        );
        let publication =
            tokio::time::timeout(Duration::from_millis(20), pool.begin_plugin_publication()).await;
        assert!(
            publication.is_err(),
            "taking the parent cell must not release actual child admission"
        );
        drop(child);
        assert_eq!(pool.active_execution_count(), 0);
        let publication =
            tokio::time::timeout(Duration::from_secs(2), pool.begin_plugin_publication())
                .await
                .map_err(|_| "settled projection kept publication blocked")?
                .map_err(|error| error.to_string())?;
        drop(publication);
        Ok(())
    }

    #[tokio::test]
    async fn domain_final_physical_projection_completes_original_supervised_cache_retirement()
    -> TestResult {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let pool = Arc::new(create_test_pool(2, false).await?);
        publish_domain_for_test(&pool, domain_components_for_test(dir.path(), "physical")?).await?;
        let execution = pool
            .acquire("__task__:physical-retirement")
            .await
            .map_err(|error| error.to_string())?;
        let binding = pool.bind_domain(&execution, "evidence-domain").await?;
        let guard = execution.domain_resource_guard("physical-retirement", &binding, None)?;
        let receipt =
            pool.retain_for_supervised_run("__task__:physical-retirement".to_string(), execution);
        let release = tokio::spawn(async move {
            crate::tasks::task_runtime::store::RunDriverExecutionReceipt::release(Box::new(
                receipt,
            ))
            .await;
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(20), async {
                while !release.is_finished() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .is_err()
        );
        assert_eq!(pool.active_execution_count(), 1);
        assert_eq!(pool.pool_size().await, 1);
        drop(guard);
        tokio::time::timeout(Duration::from_secs(2), release)
            .await
            .map_err(|_| "original supervised release did not settle")?
            .map_err(|error| error.to_string())?;
        assert_eq!(pool.active_execution_count(), 0);
        assert_eq!(pool.pool_size().await, 0);
        Ok(())
    }

    async fn domain_dispatch_fixture() -> TestResult<(
        tempfile::TempDir,
        Arc<AgentPool>,
        AgentHandle,
        Arc<crate::tasks::task_runtime::store::TaskRuntimeStore>,
        AgentPoolExecutionLease,
        crate::domain_plugin::DomainBinding,
        String,
    )> {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let primary = AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .llm_client(Arc::new(echo_agent::testing::MockLlmClient::new()))
                .enable_tools()
                .enable_subagent()
                .register_agent_dispatch_tool()
                .build()
                .map_err(|error| error.to_string())?,
        );
        let pool = Arc::new(AgentPool::new_for_test(primary.clone(), None, None, 2, false).await);
        let mut components = domain_components_for_test(dir.path(), "dispatch")?;
        let mut definition = echo_agent::subagent::SubagentDefinition::new(
            "domain-expert",
            "Inspect domain evidence",
        );
        definition.kind = echo_agent::subagent::SubagentKind::Plugin {
            source: "evidence-domain".to_string(),
        };
        definition.access_mode = echo_agent::subagent::SubagentAccessMode::ReadOnly;
        components.record_prepared_role(&definition)?;
        publish_domain_for_test(&pool, components).await?;
        pool.install_domain_registration_admission(&primary).await;
        let key = format!("__task__:{}", uuid::Uuid::new_v4());
        let lease = pool
            .acquire(&key)
            .await
            .map_err(|error| error.to_string())?;
        let binding = pool.bind_domain(&lease, "evidence-domain").await?;
        let store = Arc::new(
            crate::tasks::task_runtime::store::TaskRuntimeStore::new_in_memory()
                .map_err(|error| error.to_string())?,
        );
        let run_id = uuid::Uuid::new_v4().to_string();
        store
            .create_run_for_active_workspace_with_domain(
                &run_id,
                "domain-conversation",
                "root",
                crate::tasks::task_runtime::DomainProfile::General,
                "inspect",
                "task",
                crate::tasks::task_runtime::AttendedMode::Attended,
                crate::tasks::task_runtime::TaskRunExecutionProfile::orchestrated(
                    crate::tasks::task_runtime::RunPlanPolicy::AllowDirect,
                ),
                Some(&binding),
            )
            .map_err(|error| error.to_string())?;
        Ok((dir, pool, primary, store, lease, binding, run_id))
    }

    fn domain_dispatch_context(
        lease: &AgentPoolExecutionLease,
        binding: &crate::domain_plugin::DomainBinding,
        store: Arc<crate::tasks::task_runtime::store::TaskRuntimeStore>,
        run_id: &str,
        cancel: echo_agent::agent::CancellationToken,
    ) -> TestResult<echo_agent::tools::ToolContext> {
        let projection =
            crate::tasks::task_runtime::compact_context::task_runtime_projection_registry()
                .retain_for_invocation(run_id, store)?;
        Ok(echo_agent::tools::ToolContext {
            run_id: Some(run_id.to_string()),
            conversation_id: Some("domain-conversation".to_string()),
            turn_id: Some("domain-turn".to_string()),
            message_id: Some("root".to_string()),
            execution_id: Some("domain-parent".to_string()),
            call_id: Some("domain-call".to_string()),
            cancel: Some(Arc::new(cancel)),
            delegation_policy: Some(echo_agent::tasks::NestedDelegationPolicy {
                can_spawn_subagents: true,
                delegate_depth: 0,
                max_delegate_depth: 1,
            }),
            resource_guards: vec![lease.domain_resource_guard(
                run_id,
                binding,
                Some(Arc::new(projection)),
            )?],
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn actual_agent_tool_checks_same_registration_provider_and_definition_before_factory()
    -> TestResult {
        for change in ["valid", "foreign-provider", "same-name-new-definition"] {
            let (_dir, pool, primary, store, lease, binding, run_id) =
                domain_dispatch_fixture().await?;
            let registry = primary
                .read(|agent| agent.subagent_registry().clone())
                .await;
            let mut definition = echo_agent::subagent::SubagentDefinition::new(
                "domain-expert",
                "Inspect domain evidence",
            );
            definition.kind = echo_agent::subagent::SubagentKind::Plugin {
                source: if change == "foreign-provider" {
                    "unrelated-provider"
                } else {
                    "evidence-domain"
                }
                .to_string(),
            };
            definition.access_mode = echo_agent::subagent::SubagentAccessMode::ReadOnly;
            if change == "same-name-new-definition" {
                definition.system_prompt = Some("Different method".to_string());
            }
            let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let counted = calls.clone();
            registry
                .register_factory(
                    definition,
                    Arc::new(echo_agent::subagent::FnAgentFactory::new(move || {
                        counted.fetch_add(1, Ordering::SeqCst);
                        Box::pin(async {
                            Ok(Box::new(
                                echo_agent::testing::MockAgent::new("domain-expert")
                                    .with_response("evidence inspected"),
                            )
                                as Box<dyn echo_agent::agent::Agent>)
                        })
                            as futures::future::BoxFuture<
                                'static,
                                echo_agent::error::Result<Box<dyn echo_agent::agent::Agent>>,
                            >
                    })),
                )
                .await;
            let tool = primary
                .read(|agent| agent.tool_manager().get_tool("agent_tool"))
                .await
                .ok_or("agent_tool missing")?;
            let context = domain_dispatch_context(
                &lease,
                &binding,
                store,
                &run_id,
                CancellationToken::new(),
            )?;
            let params = std::collections::HashMap::from([
                ("agent_name".to_string(), serde_json::json!("domain-expert")),
                ("task".to_string(), serde_json::json!("inspect evidence")),
            ]);
            let result = tool.execute_with_context(params, &context).await;
            if change == "valid" {
                let result = result.map_err(|error| error.to_string())?;
                assert!(
                    result.success,
                    "the original bound registration should execute"
                );
                assert_eq!(calls.load(Ordering::SeqCst), 1);
            } else {
                assert!(
                    result.as_ref().map_or(true, |result| !result.success),
                    "invalid domain registration must be rejected"
                );
                assert_eq!(
                    calls.load(Ordering::SeqCst),
                    0,
                    "rejection must precede factory effects"
                );
            }
            drop(context);
            drop(lease);
            assert_eq!(pool.active_execution_count(), 0);
        }
        Ok(())
    }

    #[tokio::test]
    async fn actual_background_agent_tool_retains_original_physical_lease_until_cancelled_child_settles()
    -> TestResult {
        let (_dir, pool, primary, store, lease, binding, run_id) =
            domain_dispatch_fixture().await?;
        let registry = primary
            .read(|agent| agent.subagent_registry().clone())
            .await;
        let mut definition = echo_agent::subagent::SubagentDefinition::new(
            "domain-expert",
            "Inspect domain evidence",
        );
        definition.kind = echo_agent::subagent::SubagentKind::Plugin {
            source: "evidence-domain".to_string(),
        };
        definition.access_mode = echo_agent::subagent::SubagentAccessMode::ReadOnly;
        let started = Arc::new(tokio::sync::Semaphore::new(0));
        let factory_started = started.clone();
        registry
            .register_factory(
                definition,
                Arc::new(echo_agent::subagent::FnAgentFactory::new(move || {
                    factory_started.add_permits(1);
                    Box::pin(async {
                        Ok(Box::new(
                            echo_agent::testing::MockAgent::new("domain-expert").with_event_script(
                                vec![echo_agent::testing::MockAgentStep::Delay(
                                    Duration::from_secs(3600),
                                )],
                            ),
                        )
                            as Box<dyn echo_agent::agent::Agent>)
                    })
                        as futures::future::BoxFuture<
                            'static,
                            echo_agent::error::Result<Box<dyn echo_agent::agent::Agent>>,
                        >
                })),
            )
            .await;
        let cancel = CancellationToken::new();
        let context = domain_dispatch_context(&lease, &binding, store, &run_id, cancel.clone())?;
        let tool = primary
            .read(|agent| agent.tool_manager().get_tool("agent_tool"))
            .await
            .ok_or("agent_tool missing")?;
        let result = tool
            .execute_with_context(
                std::collections::HashMap::from([
                    ("agent_name".to_string(), serde_json::json!("domain-expert")),
                    ("task".to_string(), serde_json::json!("inspect evidence")),
                    ("background".to_string(), serde_json::json!(true)),
                    ("mode".to_string(), serde_json::json!("fork")),
                ]),
                &context,
            )
            .await
            .map_err(|error| error.to_string())?;
        assert!(result.success, "valid background dispatch must be accepted");
        tokio::time::timeout(Duration::from_secs(2), started.acquire())
            .await
            .map_err(|_| "factory did not start")?
            .map_err(|_| "factory start gate closed")?
            .forget();
        let process_active = pool.process_agent_execution.snapshot().active;
        let key = lease
            .physical
            .admission
            .as_ref()
            .map(|(_, key)| key.clone())
            .ok_or("physical key missing")?;
        let receipt = pool.retain_for_supervised_run(key, lease);
        drop(context);
        let release = tokio::spawn(async move {
            crate::tasks::task_runtime::store::RunDriverExecutionReceipt::release(Box::new(
                receipt,
            ))
            .await;
        });
        assert_eq!(pool.active_execution_count(), 1);
        assert_eq!(
            pool.process_agent_execution.snapshot().active,
            process_active
        );
        assert!(!release.is_finished());
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(3), release)
            .await
            .map_err(|_| "original supervised receipt did not settle after real child closure")?
            .map_err(|error| error.to_string())?;
        assert_eq!(pool.active_execution_count(), 0);
        assert_eq!(pool.pool_size().await, 0);
        Ok(())
    }

    #[tokio::test]
    async fn actual_agent_tool_uses_target_pool_retention_and_original_run_identity() -> TestResult
    {
        let (_original_dir, original, _primary, store, original_lease, binding, run_id) =
            domain_dispatch_fixture().await?;
        let (
            _target_dir,
            target,
            target_primary,
            _target_store,
            target_lease,
            target_binding,
            _target_run,
        ) = domain_dispatch_fixture().await?;
        assert_eq!(
            binding, target_binding,
            "the target control must have identical prepared domain inputs"
        );
        target_lease.verify_domain_binding(&binding).await?;
        let registry = target_primary
            .read(|agent| agent.subagent_registry().clone())
            .await;
        let mut definition = echo_agent::subagent::SubagentDefinition::new(
            "domain-expert",
            "Inspect domain evidence",
        );
        definition.kind = echo_agent::subagent::SubagentKind::Plugin {
            source: "evidence-domain".to_string(),
        };
        definition.access_mode = echo_agent::subagent::SubagentAccessMode::ReadOnly;
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = calls.clone();
        registry
            .register_factory(
                definition,
                Arc::new(echo_agent::subagent::FnAgentFactory::new(move || {
                    counted.fetch_add(1, Ordering::SeqCst);
                    Box::pin(async {
                        Ok(Box::new(
                            echo_agent::testing::MockAgent::new("domain-expert")
                                .with_response("target evidence inspected"),
                        )
                            as Box<dyn echo_agent::agent::Agent>)
                    })
                        as futures::future::BoxFuture<
                            'static,
                            echo_agent::error::Result<Box<dyn echo_agent::agent::Agent>>,
                        >
                })),
            )
            .await;
        let tool = target_primary
            .read(|agent| agent.tool_manager().get_tool("agent_tool"))
            .await
            .ok_or("agent_tool missing")?;
        let parameters = std::collections::HashMap::from([
            ("agent_name".to_string(), serde_json::json!("domain-expert")),
            (
                "task".to_string(),
                serde_json::json!("inspect target evidence"),
            ),
        ]);
        let foreign = domain_dispatch_context(
            &original_lease,
            &binding,
            store.clone(),
            &run_id,
            CancellationToken::new(),
        )?;
        let result = tool
            .execute_with_context(parameters.clone(), &foreign)
            .await;
        assert!(result.as_ref().map_or(true, |result| !result.success));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "a default-pool proof cannot admit another actual target"
        );
        let mut actual = domain_dispatch_context(
            &target_lease,
            &binding,
            store,
            &run_id,
            CancellationToken::new(),
        )?;
        actual.run_id = Some(uuid::Uuid::new_v4().to_string());
        let result = tool.execute_with_context(parameters.clone(), &actual).await;
        assert!(result.as_ref().map_or(true, |result| !result.success));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "wrong Run identity must not become General"
        );
        actual.run_id = Some(run_id);
        let result = tool
            .execute_with_context(parameters, &actual)
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            result.success,
            "same binding on the real target lease must execute"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        drop(actual);
        drop(foreign);
        drop(target_lease);
        drop(original_lease);
        assert_eq!(target.active_execution_count(), 0);
        assert_eq!(original.active_execution_count(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn actual_general_agent_tool_keeps_unbound_plugin_self_extension() -> TestResult {
        let (_dir, pool, primary, store, lease, _binding, _run_id) =
            domain_dispatch_fixture().await?;
        let run_id = uuid::Uuid::new_v4().to_string();
        store
            .create_run_for_active_workspace_with_profile(
                &run_id,
                "general-conversation",
                "root",
                crate::tasks::task_runtime::DomainProfile::General,
                "ordinary chat",
                "chat",
                crate::tasks::task_runtime::AttendedMode::Attended,
                crate::tasks::task_runtime::TaskRunExecutionProfile::orchestrated(
                    crate::tasks::task_runtime::RunPlanPolicy::AllowDirect,
                ),
            )
            .map_err(|error| error.to_string())?;
        let _projection =
            crate::tasks::task_runtime::compact_context::task_runtime_projection_registry()
                .retain_for_invocation(&run_id, store)?;
        let registry = primary
            .read(|agent| agent.subagent_registry().clone())
            .await;
        let mut definition = echo_agent::subagent::SubagentDefinition::new(
            "user-extension",
            "Ordinary user extension",
        );
        definition.kind = echo_agent::subagent::SubagentKind::Plugin {
            source: "user-selected-unrelated-package".to_string(),
        };
        registry
            .register(
                definition,
                Box::new(
                    echo_agent::testing::MockAgent::new("user-extension")
                        .with_response("General extension completed"),
                ),
            )
            .await;
        let tool = primary
            .read(|agent| agent.tool_manager().get_tool("agent_tool"))
            .await
            .ok_or("agent_tool missing")?;
        let result = tool
            .execute_with_context(
                std::collections::HashMap::from([
                    (
                        "agent_name".to_string(),
                        serde_json::json!("user-extension"),
                    ),
                    ("task".to_string(), serde_json::json!("ordinary task")),
                ]),
                &echo_agent::tools::ToolContext {
                    run_id: Some(run_id),
                    ..Default::default()
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            result.success,
            "an unbound General run keeps the original self-extension boundary"
        );
        drop(lease);
        assert_eq!(pool.active_execution_count(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn gui_domain_turn_reuses_original_token_at_process_capacity_one() -> TestResult {
        use crate::chat_driver::{ChatDriverEvent, ChatSink};
        use crate::tasks::task_runtime::{RunTurnBinding, RunTurnOrigin, TurnVisibility};
        struct ObserveOriginalAdmission {
            pool: Arc<AgentPool>,
            maximum_tokens: Arc<AtomicUsize>,
            events: Arc<std::sync::Mutex<Vec<ChatDriverEvent>>>,
        }
        impl ChatSink for ObserveOriginalAdmission {
            fn on_event(&self, event: ChatDriverEvent) -> bool {
                if let Ok(mut events) = self.events.lock() {
                    events.push(event);
                }
                let Ok(leases) = self.pool.admission.leases.lock() else {
                    return false;
                };
                self.maximum_tokens
                    .fetch_max(leases.live.len(), Ordering::SeqCst);
                self.pool.process_agent_execution.snapshot().active == 1
            }
        }
        let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
        let mut pool = create_test_pool(1, false).await?;
        pool.process_agent_execution = Arc::new(AgentExecutionGovernor::new(1));
        let pool = Arc::new(pool);
        // Keep the real default router: classification consumes its own reply
        // before the single Agent execution consumes the unchanged answer.
        let client = Arc::new(echo_agent::testing::MockLlmClient::new().with_responses([
            r#"{"intent":"FALLBACK","skill":null,"confidence":1.0}"#,
            "## Summary\nExact evidence inspected; no changes required.",
            r#"{"score":9.0,"passed":true,"feedback":"Exact evidence inspected","suggestions":[]}"#,
        ]));
        pool.set_llm_client_override_for_test(client.clone()).await;
        publish_domain_for_test(&pool, domain_components_for_test(directory.path(), "gui")?)
            .await?;
        let conversation = uuid::Uuid::new_v4().to_string();
        let turn_id = uuid::Uuid::new_v4().to_string();
        let execution = pool
            .acquire(&conversation)
            .await
            .map_err(|error| error.to_string())?;
        let agent = execution.agent();
        let held_execution = Arc::new(std::sync::Mutex::new(Some(execution)));
        let store = Arc::new(
            crate::tasks::task_runtime::TaskRuntimeStore::new_in_memory()
                .map_err(|error| error.to_string())?,
        );
        let _original_resource_scope = store.test_resource_scope();
        let workspace = store.active_workspace_id();
        let control = crate::foreground_turn::ForegroundTurnControl::default();
        let foreground = control
            .begin_scoped(
                &workspace,
                crate::foreground_turn::ForegroundTurnSurface::Gui,
                &conversation,
                &turn_id,
            )
            .map_err(|error| error.to_string())?;
        foreground.retain_execution(held_execution.clone());
        let maximum_tokens = Arc::new(AtomicUsize::new(0));
        let observed_events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let resources = Arc::new(crate::chat_resources::ChatResources {
            execution_scope: crate::workspace::WorkspaceExecutionScope::workspace(
                &crate::workspace::WorkspaceId::from_name(&workspace),
                directory.path(),
            ),
            workspace_io_receipt: None,
            pool: Some(pool.clone()),
            store: Some(store.clone()),
            sink: Arc::new(ObserveOriginalAdmission {
                pool: pool.clone(),
                maximum_tokens: maximum_tokens.clone(),
                events: observed_events.clone(),
            }),
            webhook_emitter: None,
            conv_id: Some(conversation.clone()),
            root_message_id: turn_id.clone(),
            attachments: Vec::new(),
            cancel: foreground.cancellation_token(),
            review_integration: None,
            memory_generation: None,
            human_loop_provider: None,
        });
        let prepared = crate::prepared_turn::PreparedUserTurn {
            instruction: "Inspect this evidence".to_string(),
            resources: Vec::new(),
            authorship: crate::prepared_turn::InstructionAuthorship::User,
            agent_input: None,
        };
        let outcome = tokio::time::timeout(
            Duration::from_secs(5),
            crate::chat_driver::with_foreground_pool_execution(
                Arc::downgrade(&held_execution),
                crate::foreground_turn::drive_foreground_chat_with_bound_ingress(
                    foreground,
                    &agent,
                    &prepared,
                    resources,
                    Some(RunTurnBinding {
                        domain_plugin: Some("evidence-domain".to_string()),
                        run_id: None,
                        turn_id: turn_id.clone(),
                        root_message_id: turn_id.clone(),
                        origin: RunTurnOrigin::User,
                        transcript_visibility: TurnVisibility::Visible,
                        expected_resume: None,
                    }),
                    Arc::new(|_| Box::pin(async { Ok(()) })),
                    |_| async { Ok(()) },
                ),
            ),
        )
        .await
        .map_err(|_| {
            let physical = held_execution.try_lock().ok().and_then(|owner| {
                owner.as_ref().map(|execution| {
                    (
                        execution.physical.projections.load(Ordering::Acquire),
                        Arc::strong_count(&execution.physical),
                    )
                })
            });
            let run_id = crate::tasks::task_runtime::task_tools::formal_run_id_for_turn(&turn_id);
            // This fixture uses only its explicit Mock transport and static
            // inputs. Read original facts after failure; never change a gate
            // or retain another physical owner to produce a diagnostic.
            let diagnostic = serde_json::json!({
                "run_id": run_id,
                "turn_id": turn_id,
                "model_calls": client.call_count(),
                "observed_tokens": maximum_tokens.load(Ordering::SeqCst),
                "original_physical": physical,
                "fixture_model_messages": client.all_calls(),
                "observed_events": observed_events.try_lock()
                    .map(|events| events.clone())
                    .map_err(|_| "fixture event trace unavailable"),
                "run_state": store.get_run_state(&run_id).map_err(|error| error.to_string()),
                "plan": store.get_plan(&run_id).map_err(|error| error.to_string()),
                "completion_gate": store.completion_gate_report(&run_id)
                    .map_err(|error| error.to_string()),
                "journal": store.list_events(&run_id, 0).map_err(|error| error.to_string()),
            });
            format!("GUI domain turn timed out: {diagnostic}")
        })?
        .map_err(|error| error.to_string())?;
        assert_eq!(outcome, crate::chat_driver::TurnOutcome::Completed);
        assert_eq!(client.call_count(), 3);
        let run_id = crate::tasks::task_runtime::task_tools::formal_run_id_for_turn(&turn_id);
        let snapshot = store
            .get_run_state(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or("domain RunCreated missing")?;
        let binding = snapshot
            .domain_binding
            .as_ref()
            .ok_or("domain binding missing")?;
        binding.validate()?;
        let requests = client.all_calls();
        assert_eq!(
            requests
                .iter()
                .filter(|messages| {
                    messages.len() == 1
                        && messages
                            .first()
                            .and_then(|message| message.content.as_text())
                            .is_some_and(|text| text.starts_with("You are an intent classifier"))
                })
                .count(),
            1,
            "the original default router classifies exactly once"
        );
        assert_eq!(
            requests
                .iter()
                .filter(|messages| {
                    // 原指令和独立领域投影必须落在同一次真实请求内。
                    messages.iter().any(|message| {
                        message
                            .content
                            .as_text()
                            .is_some_and(|text| text.starts_with(&prepared.instruction))
                    }) && messages.iter().any(|message| {
                        message.content.as_text().is_some_and(|text| {
                            text.contains(
                                crate::tasks::task_runtime::compact_context::DOMAIN_METHOD_MARKER,
                            ) && text.contains(&binding.snapshot.method)
                        })
                    })
                })
                .count(),
            1,
            "the bound request reaches the actual Agent exactly once"
        );
        assert_eq!(
            maximum_tokens.load(Ordering::SeqCst),
            1,
            "GUI prepare must not issue another counted token"
        );
        assert_eq!(pool.process_agent_execution.snapshot().active, 1);
        assert!(snapshot.domain_binding.is_some());
        assert_eq!(
            snapshot.run.status,
            crate::tasks::task_runtime::TaskRunStatus::Completed
        );
        let events = store
            .list_events(&run_id, 0)
            .map_err(|error| error.to_string())?;
        for kind in [
            crate::tasks::task_runtime::RuntimeEventKind::RunTurnStarted,
            crate::tasks::task_runtime::RuntimeEventKind::RunTurnFinished,
        ] {
            assert_eq!(
                events
                    .iter()
                    .filter(|event| event.event_type == kind)
                    .count(),
                1,
                "the real default-router turn must not enter a continuation or retry"
            );
        }
        assert_eq!(store.active_run_driver_count()?, 0);
        assert!(
            control
                .snapshot_scoped(
                    &workspace,
                    crate::foreground_turn::ForegroundTurnSurface::Gui,
                    &conversation
                )
                .is_none()
        );
        held_execution
            .lock()
            .map_err(|_| "GUI original cell unavailable")?
            .take();
        assert_eq!(pool.active_execution_count(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn gui_existing_lease_rejects_foreign_pool_and_resumed_generation_drift_before_model()
    -> TestResult {
        use crate::tasks::task_runtime::{
            AttendedMode, DomainProfile, RunPlanPolicy, RunTurnBinding, TaskRunExecutionProfile,
            TaskRunResumeIdentity, TaskRunStatus, TaskRuntimeStore,
        };
        for mismatched_pool in [true, false] {
            let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
            let source = Arc::new(create_test_pool(2, false).await?);
            let target = Arc::new(create_test_pool(2, false).await?);
            publish_domain_for_test(
                &source,
                domain_components_for_test(directory.path(), "original-gui")?,
            )
            .await?;
            publish_domain_for_test(
                &target,
                domain_components_for_test(directory.path(), "changed-gui")?,
            )
            .await?;
            let client =
                Arc::new(echo_agent::testing::MockLlmClient::new().with_response("must not run"));
            source
                .set_llm_client_override_for_test(client.clone())
                .await;
            target
                .set_llm_client_override_for_test(client.clone())
                .await;
            let conversation = uuid::Uuid::new_v4().to_string();
            let source_lease = source
                .acquire(&conversation)
                .await
                .map_err(|error| error.to_string())?;
            let original_binding = source.bind_domain(&source_lease, "evidence-domain").await?;
            let execution = if mismatched_pool {
                source_lease
            } else {
                drop(source_lease);
                target
                    .acquire(&conversation)
                    .await
                    .map_err(|error| error.to_string())?
            };
            let agent = execution.agent();
            let held_execution = Arc::new(std::sync::Mutex::new(Some(execution)));
            let store =
                Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
            let _original_resource_scope = store.test_resource_scope();
            let run_id = uuid::Uuid::new_v4().to_string();
            store
                .create_run_for_active_workspace_with_domain(
                    &run_id,
                    &conversation,
                    "root",
                    DomainProfile::General,
                    "original goal",
                    "task",
                    AttendedMode::Attended,
                    TaskRunExecutionProfile::orchestrated(RunPlanPolicy::AllowDirect),
                    Some(&original_binding),
                )
                .map_err(|error| error.to_string())?;
            store
                .transition_run(&run_id, TaskRunStatus::Running)
                .map_err(|error| error.to_string())?;
            store
                .transition_run(&run_id, TaskRunStatus::Paused)
                .map_err(|error| error.to_string())?;
            let before = store
                .get_run_state(&run_id)
                .map_err(|error| error.to_string())?
                .ok_or("original Run missing")?;
            let mut binding = RunTurnBinding::resume_expected(
                TaskRunResumeIdentity::capture(&before),
                uuid::Uuid::new_v4().to_string(),
            );
            binding.domain_plugin = Some("evidence-domain".to_string());
            let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
            let resources = Arc::new(crate::chat_resources::ChatResources {
                execution_scope: crate::workspace::WorkspaceExecutionScope::workspace(
                    &crate::workspace::WorkspaceId::from_name(&store.active_workspace_id()),
                    directory.path(),
                ),
                workspace_io_receipt: None,
                pool: Some(target),
                store: Some(store.clone()),
                sink: Arc::new(crate::chat_driver::ChannelChatSink::new(sender)),
                webhook_emitter: None,
                conv_id: Some(conversation),
                root_message_id: binding.turn_id.clone(),
                attachments: Vec::new(),
                cancel: CancellationToken::new(),
                review_integration: None,
                memory_generation: None,
                human_loop_provider: None,
            });
            let turn = crate::prepared_turn::PreparedUserTurn {
                instruction: "resume".to_string(),
                resources: Vec::new(),
                authorship: crate::prepared_turn::InstructionAuthorship::User,
                agent_input: None,
            };
            let result = crate::chat_driver::with_foreground_pool_execution(
                Arc::downgrade(&held_execution),
                crate::chat_driver::drive_chat_turn(&agent, &turn, resources, Some(binding)),
            )
            .await;
            let error = result
                .err()
                .ok_or("mismatched original GUI lease unexpectedly ran")?
                .to_string();
            assert!(
                error.contains(if mismatched_pool {
                    "this pool's original live execution lease"
                } else {
                    "conflicts with the original TaskRun binding"
                }),
                "unexpected admission diagnostic: {error}"
            );
            assert_eq!(client.call_count(), 0);
            let after = store
                .get_run_state(&run_id)
                .map_err(|error| error.to_string())?
                .ok_or("Run disappeared")?;
            assert_eq!(after.run.status, before.run.status);
            assert_eq!(after.domain_binding, before.domain_binding);
            assert_eq!(after.journal_sequence, before.journal_sequence);
            held_execution
                .lock()
                .map_err(|_| "GUI original cell unavailable")?
                .take();
        }
        Ok(())
    }

    #[tokio::test]
    async fn actual_agent_tool_keeps_required_method_when_projection_reads_close_after_admission()
    -> TestResult {
        let (_dir, _pool, primary, store, lease, binding, run_id) =
            domain_dispatch_fixture().await?;
        let registry = primary
            .read(|agent| agent.subagent_registry().clone())
            .await;
        let mut definition = echo_agent::subagent::SubagentDefinition::new(
            "domain-expert",
            "Inspect domain evidence",
        );
        definition.kind = echo_agent::subagent::SubagentKind::Plugin {
            source: "evidence-domain".to_string(),
        };
        definition.access_mode = echo_agent::subagent::SubagentAccessMode::ReadOnly;
        let client = Arc::new(
            echo_agent::testing::MockLlmClient::new()
                .with_response("Exact bound evidence inspected"),
        );
        let factory_client = client.clone();
        let close_after_admission = store.clone();
        let factories = Arc::new(AtomicUsize::new(0));
        let factory_count = factories.clone();
        registry.register_factory(definition, Arc::new(echo_agent::subagent::FnAgentFactory::new(move || {
            let store = close_after_admission.clone();
            let client = factory_client.clone();
            let factories = factory_count.clone();
            Box::pin(async move {
                // F invokes this actual factory only after the real EKO policy
                // accepted the selected registration. This makes a later
                // TaskRuntimeOperation projection read fail without a new fault API.
                store.begin_operation_shutdown().map_err(echo_agent::error::ReactError::Other)?;
                factories.fetch_add(1, Ordering::SeqCst);
                let mut agent = echo_agent::agent::ReactAgentBuilder::new().name("domain-expert")
                    .llm_client(client).build()?;
                crate::tasks::task_runtime::compact_context::install_domain_context_projection(&mut agent);
                Ok(Box::new(agent) as Box<dyn echo_agent::agent::Agent>)
            }) as futures::future::BoxFuture<'static, echo_agent::error::Result<Box<dyn echo_agent::agent::Agent>>>
        }))).await;
        let context = domain_dispatch_context(
            &lease,
            &binding,
            store.clone(),
            &run_id,
            CancellationToken::new(),
        )?;
        let tool = primary
            .read(|agent| agent.tool_manager().get_tool("agent_tool"))
            .await
            .ok_or("agent_tool missing")?;
        let result = tool
            .execute_with_context(
                std::collections::HashMap::from([
                    ("agent_name".to_string(), serde_json::json!("domain-expert")),
                    ("task".to_string(), serde_json::json!("inspect evidence")),
                ]),
                &context,
            )
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            result.success,
            "an already admitted invocation keeps its required immutable method"
        );
        assert_eq!(factories.load(Ordering::SeqCst), 1);
        let read_run = run_id.clone();
        let closed_read = crate::tasks::task_runtime::TaskRuntimeOperation::new(store)
            .run_store(
                "negative control: original projector read after operation shutdown",
                move |store| store.get_run_state(&read_run),
            )
            .await;
        assert!(
            closed_read.is_err(),
            "the late operation read must really be unavailable"
        );
        let requests = client.all_calls();
        let messages = requests
            .first()
            .ok_or("admitted child did not reach the model")?;
        assert!(
            messages
                .iter()
                .filter_map(|message| message.content.as_text())
                .any(|text| text
                    .contains(crate::tasks::task_runtime::compact_context::DOMAIN_METHOD_MARKER)
                    && text.contains(&binding.snapshot.method))
        );
        drop(context);
        drop(lease);
        Ok(())
    }
    #[tokio::test]
    async fn startup_slow_creation_does_not_block_cached_acquire_or_resurrect_retired_key()
    -> anyhow::Result<()> {
        use crate::project::startup_tests::CountingFiles;
        let temp = tempfile::tempdir()?;
        let files = Arc::new(CountingFiles::new(temp.path().to_path_buf()));
        let mut pool = create_test_pool(4, false)
            .await
            .map_err(anyhow::Error::msg)?;
        let store = crate::project::startup_tests::task_store(files.clone())?;
        let _resource_scope = store.test_resource_scope();
        pool.shared.sandbox_manager = store
            .project_environment()
            .map(|environment| environment.executor);
        pool.shared.task_runtime_store = Some(store.clone());
        pool.shared.tool_manager = None;
        pool.shared.file_system = Some(files.clone());
        *pool.working_dir.write().await = Some(temp.path().to_path_buf());
        *pool.project_root.write().await = Some(temp.path().to_path_buf());
        let cached = pool.acquire("existing").await?;
        let original = cached.agent();
        drop(cached);
        let pool = Arc::new(pool);
        files.gate.enabled.store(true, Ordering::SeqCst);
        struct Release(Arc<crate::project::startup_tests::ReadGate>);
        impl Drop for Release {
            fn drop(&mut self) {
                self.0.release.add_permits(1);
            }
        }
        let release = Release(files.gate.clone());
        let pending_pool = pool.clone();
        let pending = tokio::spawn(async move { pending_pool.acquire_for_turn("slow").await });
        tokio::time::timeout(Duration::from_secs(10), files.gate.reached.acquire())
            .await??
            .forget();
        let existing =
            tokio::time::timeout(Duration::from_secs(2), pool.acquire("existing")).await??;
        assert!(Arc::ptr_eq(existing.agent().inner(), original.inner()));
        drop(existing);
        let retirement = pool.begin_conversation_retirement("slow")?;
        let retirement_pool = pool.clone();
        let retiring = tokio::spawn(async move {
            retirement_pool
                .complete_conversation_retirement(retirement)
                .await
        });
        tokio::task::yield_now().await;
        assert!(
            !retiring.is_finished(),
            "retirement must retain its closed key until original creation returns"
        );
        drop(release);
        assert!(matches!(
            pending.await?,
            Err(PoolError::ConversationRetirementPending { .. })
        ));
        assert!(!retiring.await??);
        assert!(pool.lease_existing("slow").await?.is_none());
        assert_eq!(
            files.opened.load(Ordering::SeqCst),
            files.closed.load(Ordering::SeqCst)
        );
        pool.shutdown().await.map_err(anyhow::Error::msg)?;
        Ok(())
    }

    #[tokio::test]
    async fn startup_raw_acquire_completes_cancelled_deferred_agent_without_losing_identity()
    -> anyhow::Result<()> {
        use crate::project::startup_tests::CountingFiles;
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("project");
        std::fs::create_dir(&root)?;
        let files = Arc::new(CountingFiles::new(root.clone()));
        std::fs::write(root.join("AGENTS.md"), "RAW_CURRENT_RULE")?;
        let mut pool = create_test_pool(4, false)
            .await
            .map_err(anyhow::Error::msg)?;
        let store = crate::project::startup_tests::task_store(files.clone())?;
        let _resource_scope = store.test_resource_scope();
        pool.shared.sandbox_manager = store
            .project_environment()
            .map(|environment| environment.executor);
        pool.shared.task_runtime_store = Some(store.clone());
        pool.shared.tool_manager = None;
        pool.shared.file_system = Some(files.clone());
        *pool.working_dir.write().await = Some(temp.path().join("product-data"));
        *pool.project_root.write().await = Some(root.clone());
        let deferred = pool.acquire_for_turn("cancelled-before-driver").await?;
        let original = deferred.agent();
        assert_eq!(files.count("read", &root.join("AGENTS.md")), 0);
        assert!(
            pool.acquire("cancelled-before-driver").await.is_err(),
            "raw access must not wait on an active uninitialized invocation"
        );
        drop(deferred);
        let raw = pool.acquire("cancelled-before-driver").await?;
        assert!(Arc::ptr_eq(raw.agent().inner(), original.inner()));
        assert_eq!(files.count("read", &root.join("AGENTS.md")), 1);
        let context = raw.agent().read(|agent| agent.context().clone()).await;
        assert!(
            context
                .lock()
                .await
                .messages()
                .iter()
                .filter_map(|m| m.content.as_text())
                .any(|text| text.contains("RAW_CURRENT_RULE"))
        );
        drop(raw);
        let raw = pool.acquire("cancelled-before-driver").await?;
        assert_eq!(files.count("read", &root.join("AGENTS.md")), 1);
        drop(raw);
        // 规则失败仍不能把缓存实例标为已初始化；修正后使用同一个实例恢复。
        std::fs::write(root.join("AGENTS.md"), [0xff])?;
        let deferred = pool.acquire_for_turn("failed-preparation").await?;
        let failed_handle = deferred.agent();
        drop(deferred);
        assert!(pool.acquire("failed-preparation").await.is_err());
        std::fs::write(root.join("AGENTS.md"), "RECOVERED_RULE")?;
        let recovered = pool.acquire("failed-preparation").await?;
        assert!(Arc::ptr_eq(
            recovered.agent().inner(),
            failed_handle.inner()
        ));
        drop(recovered);
        assert_eq!(
            files.opened.load(Ordering::SeqCst),
            files.closed.load(Ordering::SeqCst)
        );
        pool.shutdown().await.map_err(anyhow::Error::msg)?;
        Ok(())
    }

    #[tokio::test]
    async fn startup_retirement_wait_does_not_hold_creation_needed_by_active_turn()
    -> anyhow::Result<()> {
        let pool = Arc::new(
            create_test_pool(4, false)
                .await
                .map_err(anyhow::Error::msg)?,
        );
        let active = pool.acquire("active").await?;
        let retirement = pool.begin_conversation_retirement("active")?;
        let waiting_pool = pool.clone();
        let waiting = tokio::spawn(async move {
            waiting_pool
                .complete_conversation_retirement(retirement)
                .await
        });
        // 原 active lease 保证等待不能结束；下一次创建必须仍能完成。
        tokio::task::yield_now().await;
        let other = tokio::time::timeout(
            Duration::from_secs(10),
            pool.acquire("needed-to-settle-active"),
        )
        .await??;
        drop(other);
        drop(active);
        assert!(tokio::time::timeout(Duration::from_secs(10), waiting).await???);
        pool.shutdown().await.map_err(anyhow::Error::msg)?;
        Ok(())
    }

    #[tokio::test]
    async fn startup_active_turn_publishes_readiness_for_steer_and_raw_readers()
    -> anyhow::Result<()> {
        use crate::project::startup_tests::CountingFiles;
        let temp = tempfile::tempdir()?;
        std::fs::write(temp.path().join("AGENTS.md"), "ACTIVE_CURRENT_RULE")?;
        let files = Arc::new(CountingFiles::new(temp.path().to_path_buf()));
        let store = crate::project::startup_tests::task_store(files.clone())?;
        let _resource_scope = store.test_resource_scope();
        let mut pool = Box::pin(create_test_pool(4, false))
            .await
            .map_err(anyhow::Error::msg)?;
        pool.shared.sandbox_manager = store
            .project_environment()
            .map(|environment| environment.executor);
        pool.shared.task_runtime_store = Some(store.clone());
        pool.shared.tool_manager = None;
        pool.shared.file_system = Some(files.clone());
        *pool.working_dir.write().await = Some(temp.path().to_path_buf());
        *pool.project_root.write().await = Some(temp.path().to_path_buf());
        let client = Arc::new(
            echo_agent::testing::MockLlmClient::new()
                .with_response(r#"{"intent":"FALLBACK","skill":null,"confidence":1.0}"#)
                .with_stream_script(vec![
                    echo_agent::testing::StreamChunk::Delay(Duration::from_secs(60)),
                    echo_agent::testing::StreamChunk::Terminal {
                        finish_reason: Some("stop".into()),
                        usage: None,
                    },
                ]),
        );
        pool.set_llm_client_override_for_test(client).await;
        let pool = Arc::new(pool);
        let execution = Box::pin(pool.acquire_for_turn("active-context")).await?;
        let handle = execution.agent();
        let original = handle.clone();
        assert!(
            !execution
                .context_initialization_for(&original)
                .ok_or_else(|| anyhow::anyhow!("missing readiness"))?
                .load(Ordering::Acquire)
        );
        let held = Arc::new(Mutex::new(Some(execution)));
        struct Sink(Arc<tokio::sync::Semaphore>);
        impl crate::chat_driver::ChatSink for Sink {
            fn on_event(&self, event: crate::chat_driver::ChatDriverEvent) -> bool {
                if matches!(event, crate::chat_driver::ChatDriverEvent::Agent(_)) {
                    self.0.add_permits(1);
                }
                true
            }
        }
        let reached = Arc::new(tokio::sync::Semaphore::new(0));
        let cancel = CancellationToken::new();
        let resources = Arc::new(crate::chat_resources::ChatResources {
            execution_scope: crate::workspace::WorkspaceExecutionScope::global(
                temp.path().to_path_buf(),
            ),
            workspace_io_receipt: None,
            pool: Some(pool.clone()),
            store: None,
            sink: Arc::new(Sink(reached.clone())),
            webhook_emitter: None,
            conv_id: Some("active-context".into()),
            root_message_id: "active-context-turn".into(),
            attachments: Vec::new(),
            cancel: cancel.clone(),
            review_integration: None,
            memory_generation: None,
            human_loop_provider: None,
        });
        let foreign_client =
            Arc::new(echo_agent::testing::MockLlmClient::new().with_response("foreign done"));
        let mut foreign = echo_agent::agent::ReactAgentBuilder::new()
            .model("foreign-context")
            .llm_client(foreign_client)
            .file_system(files.clone())
            .auto_project_rules(false)
            .working_dir(temp.path().to_path_buf())
            .build()?;
        foreign.set_sandbox_executor(Arc::new(
            echo_agent::sandbox::SandboxManager::local_sandbox(),
        ));
        foreign.set_pre_model_context_projector(Some(Arc::new(
            crate::turn_context::EkoContextProjector::new(
                crate::tasks::task_runtime::compact_context::task_runtime_projection_registry(),
                crate::turn_context::turn_prompt_context_registry(),
            ),
        )));
        let foreign = AgentHandle::new(foreign);
        let foreign_turn = crate::prepared_turn::PreparedUserTurn {
            instruction: "other Agent".into(),
            resources: Vec::new(),
            authorship: crate::prepared_turn::InstructionAuthorship::User,
            agent_input: None,
        };
        let foreign_resources = crate::chat_resources::ChatResources {
            execution_scope: resources.execution_scope.clone(),
            workspace_io_receipt: None,
            pool: Some(pool.clone()),
            store: None,
            sink: Arc::new(Sink(reached.clone())),
            webhook_emitter: None,
            conv_id: Some("foreign-context".into()),
            root_message_id: "foreign-context-turn".into(),
            attachments: Vec::new(),
            cancel: CancellationToken::new(),
            review_integration: None,
            memory_generation: None,
            human_loop_provider: None,
        };
        let foreign_outcome = crate::chat_driver::with_foreground_pool_execution(
            Arc::downgrade(&held),
            Box::pin(crate::chat_driver::drive_chat(
                &foreign,
                &foreign_turn,
                Arc::new(foreign_resources),
            )),
        )
        .await
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        assert_eq!(foreign_outcome, crate::chat_driver::TurnOutcome::Completed);
        assert!(
            !held
                .lock()
                .map_err(|_| anyhow::anyhow!("lease lock poisoned"))?
                .as_ref()
                .and_then(|execution| execution.context_initialization_for(&original))
                .ok_or_else(|| anyhow::anyhow!("missing original marker"))?
                .load(Ordering::Acquire)
        );
        // 外来实例的事件不作为下一次原实例已开始的证据。
        while let Ok(permit) = reached.try_acquire() {
            permit.forget();
        }
        let weak = Arc::downgrade(&held);
        let drive = tokio::spawn(Box::pin(async move {
            let turn = crate::prepared_turn::PreparedUserTurn {
                instruction: "你好".into(),
                resources: Vec::new(),
                authorship: crate::prepared_turn::InstructionAuthorship::User,
                agent_input: None,
            };
            crate::chat_driver::with_foreground_pool_execution(
                weak,
                Box::pin(crate::chat_driver::drive_chat(&handle, &turn, resources)),
            )
            .await
        }));
        struct Cancel(CancellationToken);
        impl Drop for Cancel {
            fn drop(&mut self) {
                self.0.cancel();
            }
        }
        let cancel_guard = Cancel(cancel.clone());
        tokio::time::timeout(Duration::from_secs(10), reached.acquire())
            .await??
            .forget();
        assert!(!drive.is_finished());
        let raw = tokio::time::timeout(
            Duration::from_secs(2),
            Box::pin(pool.acquire("active-context")),
        )
        .await??;
        assert!(Arc::ptr_eq(raw.agent().inner(), original.inner()));
        assert_eq!(files.count("read", &temp.path().join("AGENTS.md")), 2);
        drop(raw);
        drop(cancel_guard);
        let outcome = tokio::time::timeout(Duration::from_secs(10), drive)
            .await??
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        assert_eq!(outcome, crate::chat_driver::TurnOutcome::Cancelled);
        held.lock()
            .map_err(|_| anyhow::anyhow!("lease lock poisoned"))?
            .take();
        pool.shutdown().await.map_err(anyhow::Error::msg)?;
        Ok(())
    }
    #[tokio::test]
    async fn startup_deferred_raw_keeps_explicit_outer_root_across_nested_git_repository()
    -> anyhow::Result<()> {
        use crate::project::startup_tests::CountingFiles;
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("outer");
        let nested = root.join("nested");
        std::fs::create_dir_all(&nested)?;
        for directory in [&root, &nested] {
            let status = std::process::Command::new("git")
                .args(["init", "-q"])
                .arg(directory)
                .status()?;
            anyhow::ensure!(status.success(), "fixture git init failed");
        }
        std::fs::write(root.join("AGENTS.md"), "NESTED_OUTER_SCOPE")?;
        std::fs::write(nested.join("AGENTS.md"), "NESTED_INNER_SCOPE")?;
        let files = Arc::new(CountingFiles::new(root.clone()));
        let store = crate::project::startup_tests::task_store(files.clone())?;
        let _resource_scope = store.test_resource_scope();
        let mut pool = Box::pin(create_test_pool(4, false))
            .await
            .map_err(anyhow::Error::msg)?;
        pool.shared.sandbox_manager = store
            .project_environment()
            .map(|environment| environment.executor);
        pool.shared.task_runtime_store = Some(store.clone());
        pool.shared.tool_manager = None;
        pool.shared.file_system = Some(files);
        *pool.project_root.write().await = Some(root);
        *pool.working_dir.write().await = Some(nested);
        let eager = Box::pin(pool.acquire("nested-eager")).await?;
        let eager_handle = eager.agent();
        drop(eager);
        let deferred = Box::pin(pool.acquire_for_turn("nested-deferred")).await?;
        let original = deferred.agent();
        drop(deferred);
        let raw = Box::pin(pool.acquire("nested-deferred")).await?;
        assert!(Arc::ptr_eq(raw.agent().inner(), original.inner()));
        for handle in [eager_handle, raw.agent()] {
            let context = handle.read(|agent| agent.context().clone()).await;
            let text = context
                .lock()
                .await
                .messages()
                .iter()
                .filter_map(|message| message.content.as_text())
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(text.matches("NESTED_OUTER_SCOPE").count(), 1);
            assert_eq!(text.matches("NESTED_INNER_SCOPE").count(), 1);
        }
        drop(raw);
        pool.shutdown().await.map_err(anyhow::Error::msg)?;
        Ok(())
    }
}
