#[cfg(test)]
mod tests {
    use super::*;

    fn address() -> AgentAddress {
        AgentAddress::new(WorkspaceId::from_name("workspace-b"), "conversation-b")
    }

    fn group_member(role: &str) -> AgentGroupMember {
        AgentGroupMember {
            address: address(),
            subagent_role: role.to_string(),
            label: Some("Remote specialist".to_string()),
        }
    }

    fn no_delivery_recovery() -> Arc<dyn Fn(AgentAddress) + Send + Sync> {
        Arc::new(|_| {})
    }

    async fn mark_completed(
        router: &AgentRouter,
        claim: &AgentDeliveryClaim,
        turn_id: &str,
    ) -> Result<(), String> {
        router
            .begin_injection(claim, turn_id)
            .await
            .map_err(|error| error.to_string())?;
        router
            .mailbox_accepted(claim, turn_id)
            .await
            .map_err(|error| error.to_string())?;
        router
            .drained(claim, turn_id)
            .await
            .map_err(|error| error.to_string())?;
        router
            .turn_settled(
                claim,
                Some(turn_id.to_string()),
                AgentDeliveryOutcome::Completed,
                true,
                None,
                false,
                None,
            )
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    #[tokio::test]
    async fn delivery_supervisor_closes_admission_before_join() -> Result<(), String> {
        let supervisor = AgentDeliverySupervisor::default();
        let cancel = supervisor.cancellation_token();
        let started = Arc::new(tokio::sync::Notify::new());
        let task_started = Arc::clone(&started);
        supervisor
            .supervise(
                address(),
                no_delivery_recovery(),
                move |_cycle| async move {
                    task_started.notify_one();
                    cancel.cancelled().await;
                },
            )
            .map_err(|error| error.to_string())?;
        started.notified().await;

        supervisor
            .close_admission_and_cancel()
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            supervisor.supervise(address(), no_delivery_recovery(), |_cycle| {
                std::future::pending()
            },),
            Err(AgentRouterError::ShuttingDown)
        ));
        tokio::time::timeout(std::time::Duration::from_secs(1), supervisor.join())
            .await
            .map_err(|_| "delivery supervisor join ignored cancellation".to_string())?
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    #[tokio::test]
    async fn target_retirement_linearizes_admission_and_waits_for_active_driver()
    -> Result<(), String> {
        let supervisor = Arc::new(AgentDeliverySupervisor::default());
        let target = address();
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let task_started = Arc::clone(&started);
        let task_release = Arc::clone(&release);
        supervisor
            .supervise(
                target.clone(),
                no_delivery_recovery(),
                move |_cycle| async move {
                    task_started.notify_one();
                    task_release.notified().await;
                },
            )
            .map_err(|error| error.to_string())?;
        started.notified().await;

        let retiring_supervisor = Arc::clone(&supervisor);
        let retiring_target = target.clone();
        let retirement =
            tokio::spawn(async move { retiring_supervisor.retire_target(retiring_target).await });
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !supervisor.is_retiring_target(&target) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "target retirement did not close admission".to_string())?;
        assert!(matches!(
            supervisor.supervise(target.clone(), no_delivery_recovery(), |_cycle| async {}),
            Err(AgentRouterError::Retiring { .. })
        ));
        release.notify_one();
        let guard = retirement
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            supervisor.supervise(target.clone(), no_delivery_recovery(), |_cycle| async {}),
            Err(AgentRouterError::Retiring { .. })
        ));
        drop(guard);
        assert!(
            supervisor
                .supervise(target, no_delivery_recovery(), |cycle| async move {
                    let _ = cycle.complete();
                })
                .map_err(|error| error.to_string())?
        );
        supervisor
            .shutdown()
            .await
            .map_err(|error| error.to_string())
    }

    #[tokio::test]
    async fn workspace_retirement_blocks_only_that_workspace_delivery_admission()
    -> Result<(), String> {
        let supervisor = AgentDeliverySupervisor::default();
        let target = address();
        let guard = supervisor
            .retire_workspace(target.workspace_id.clone())
            .await
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            supervisor.supervise(target, no_delivery_recovery(), |_cycle| async {}),
            Err(AgentRouterError::Retiring { .. })
        ));
        let other = AgentAddress::new(WorkspaceId::from_name("other"), "conversation");
        assert!(
            supervisor
                .supervise(other, no_delivery_recovery(), |cycle| async move {
                    let _ = cycle.complete();
                })
                .map_err(|error| error.to_string())?
        );
        drop(guard);
        supervisor
            .shutdown()
            .await
            .map_err(|error| error.to_string())
    }

    #[tokio::test]
    async fn router_two_phase_retirement_closes_mutation_before_purge() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let target = address();
        let router = AgentRouter::new(temp.path().to_path_buf());
        router
            .enqueue(AgentMessage::user_text(
                None,
                target.clone(),
                "accepted before retirement",
            ))
            .await
            .map_err(|error| error.to_string())?;
        let guard = router
            .begin_target_retirement(target.clone())
            .map_err(|error| error.to_string())?;
        assert!(inbox_dir(temp.path(), &target).exists());
        assert!(matches!(
            router
                .enqueue(AgentMessage::user_text(
                    None,
                    target.clone(),
                    "rejected after retirement cut",
                ))
                .await,
            Err(AgentRouterError::Retiring { .. })
        ));
        assert!(matches!(
            router.claim_next(&target).await,
            Err(AgentRouterError::Retiring { .. })
        ));
        guard.purge().await.map_err(|error| error.to_string())?;
        assert!(!inbox_dir(temp.path(), &target).exists());
        drop(guard);
        assert!(
            router
                .records(&target)
                .await
                .map_err(|error| error.to_string())?
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn delivery_driver_panic_clears_active_and_reaches_shutdown_receipt() -> Result<(), String>
    {
        let supervisor = AgentDeliverySupervisor::default();
        let target = address();
        let workspace = target.workspace_id.clone();
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let task_started = Arc::clone(&started);
        let task_release = Arc::clone(&release);
        supervisor
            .supervise(
                target.clone(),
                no_delivery_recovery(),
                move |_cycle| async move {
                    task_started.notify_one();
                    task_release.notified().await;
                    let should_complete = std::hint::black_box(false);
                    assert!(should_complete, "injected delivery driver panic");
                },
            )
            .map_err(|error| error.to_string())?;
        started.notified().await;
        release.notify_one();

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while supervisor.has_active_workspace(&workspace) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "panicked delivery driver retained active target".to_string())?;
        assert!(!supervisor.has_active_workspace(&workspace));

        let error = supervisor
            .join()
            .await
            .err()
            .ok_or_else(|| "delivery driver panic was not reported".to_string())?;
        assert!(error.to_string().contains(target.conversation_id.as_str()));
        Ok(())
    }

    #[tokio::test]
    async fn dirty_delivery_is_restarted_after_driver_panic() -> Result<(), String> {
        let supervisor = Arc::new(AgentDeliverySupervisor::default());
        let target = address();
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let recovered = Arc::new(tokio::sync::Notify::new());
        let weak_supervisor = Arc::downgrade(&supervisor);
        let recovered_callback = Arc::clone(&recovered);
        let recover: Arc<dyn Fn(AgentAddress) + Send + Sync> = Arc::new(move |target| {
            let Some(supervisor) = weak_supervisor.upgrade() else {
                return;
            };
            let recovered = Arc::clone(&recovered_callback);
            let _ = supervisor.supervise(target, no_delivery_recovery(), move |cycle| async move {
                let _ = cycle.complete();
                recovered.notify_one();
            });
        });
        let task_started = Arc::clone(&started);
        let task_release = Arc::clone(&release);
        supervisor
            .supervise(target.clone(), recover, move |_cycle| async move {
                task_started.notify_one();
                task_release.notified().await;
                let should_complete = std::hint::black_box(false);
                assert!(should_complete, "injected dirty delivery panic");
            })
            .map_err(|error| error.to_string())?;
        started.notified().await;
        assert!(
            !supervisor
                .supervise(target, no_delivery_recovery(), |_cycle| {
                    std::future::pending()
                },)
                .map_err(|error| error.to_string())?,
            "dirty wake created a duplicate delivery owner"
        );
        release.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(1), recovered.notified())
            .await
            .map_err(|_| "dirty delivery was not restarted after panic".to_string())?;

        let error = supervisor
            .shutdown()
            .await
            .err()
            .ok_or_else(|| "recovered driver panic was not retained in receipt".to_string())?;
        assert!(error.to_string().contains("panicked"));
        Ok(())
    }

    #[tokio::test]
    async fn delayed_old_driver_drop_cannot_clear_replacement_generation() -> Result<(), String> {
        let supervisor = Arc::new(AgentDeliverySupervisor::default());
        let target = address();
        let a_removed = Arc::new(tokio::sync::Notify::new());
        let allow_a_drop = Arc::new(tokio::sync::Notify::new());
        let a_removed_task = Arc::clone(&a_removed);
        let allow_a_drop_task = Arc::clone(&allow_a_drop);
        supervisor
            .supervise(
                target.clone(),
                no_delivery_recovery(),
                move |cycle| async move {
                    let repeated = cycle.complete().unwrap_or(false);
                    assert!(!repeated, "driver A unexpectedly retained its generation");
                    a_removed_task.notify_one();
                    allow_a_drop_task.notified().await;
                },
            )
            .map_err(|error| error.to_string())?;
        a_removed.notified().await;

        let allow_b_cycles = Arc::new(tokio::sync::Notify::new());
        let b_completed = Arc::new(tokio::sync::Notify::new());
        let allow_b_cycles_task = Arc::clone(&allow_b_cycles);
        let b_completed_task = Arc::clone(&b_completed);
        let inserted = supervisor
            .supervise(
                target.clone(),
                no_delivery_recovery(),
                move |cycle| async move {
                    allow_b_cycles_task.notified().await;
                    let repeated = cycle.complete().unwrap_or(false);
                    assert!(repeated, "driver B lost its dirty notification");
                    let repeated = cycle.complete().unwrap_or(true);
                    assert!(!repeated, "driver B did not release its generation");
                    b_completed_task.notify_one();
                },
            )
            .map_err(|error| error.to_string())?;
        assert!(inserted, "driver B did not acquire the released target");
        assert!(
            !supervisor
                .supervise(target.clone(), no_delivery_recovery(), |_cycle| {
                    std::future::pending()
                },)
                .map_err(|error| error.to_string())?,
            "a second B wake created a duplicate owner"
        );

        allow_a_drop.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                let _ = supervisor.supervise(target.clone(), no_delivery_recovery(), |_cycle| {
                    std::future::pending()
                });
                let old_driver_collected = supervisor
                    .state
                    .lock()
                    .map(|state| state.driver_targets.len() == 1)
                    .unwrap_or(false);
                if old_driver_collected {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "driver A did not reach its delayed Drop barrier".to_string())?;

        {
            let state = supervisor
                .state
                .lock()
                .map_err(|_| "delivery supervisor state is unavailable".to_string())?;
            let b_generation = state
                .active
                .get(&target)
                .copied()
                .ok_or_else(|| "driver A Drop cleared driver B active owner".to_string())?;
            assert_eq!(
                state.dirty.get(&target),
                Some(&b_generation),
                "driver A Drop cleared driver B dirty notification"
            );
        }

        allow_b_cycles.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(1), b_completed.notified())
            .await
            .map_err(|_| "driver B did not complete both owned cycles".to_string())?;
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while supervisor.has_active_workspace(&target.workspace_id) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "driver B retained its active generation".to_string())?;
        supervisor
            .shutdown()
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    async fn drain_inbox(root: PathBuf, target: AgentAddress) -> Result<usize, String> {
        let router = AgentRouter::new(root);
        let mut delivered = 0usize;
        while let Some(claim) = router
            .claim_next(&target)
            .await
            .map_err(|error| error.to_string())?
        {
            mark_completed(&router, &claim, &claim.message.delivery_turn_id()).await?;
            delivered = delivered.saturating_add(1);
        }
        Ok(delivered)
    }

    #[tokio::test]
    async fn groups_persist_update_and_delete_without_runtime_state() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let router = AgentRouter::new(temp.path().to_path_buf());
        let leader = AgentAddress::new(WorkspaceId::from_name("workspace-a"), "conversation-a");
        let created = router
            .create_group(
                "Product team",
                leader.clone(),
                vec![group_member("explorer")],
            )
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(created.member_for_role("explorer"), created.members.first());
        drop(router);

        let restarted = AgentRouter::new(temp.path().to_path_buf());
        assert_eq!(
            restarted
                .list_groups()
                .await
                .map_err(|error| error.to_string())?,
            vec![created.clone()]
        );
        let updated = restarted
            .update_group(
                created.group_id.clone(),
                "Product delivery",
                leader,
                vec![group_member("reviewer")],
            )
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(updated.created_at, created.created_at);
        assert!(updated.updated_at >= created.updated_at);
        assert!(updated.member_for_role("reviewer").is_some());
        assert!(
            restarted
                .delete_group(&created.group_id)
                .await
                .map_err(|error| error.to_string())?
        );
        assert!(
            restarted
                .list_groups()
                .await
                .map_err(|error| error.to_string())?
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn groups_reject_duplicate_roles_addresses_and_leader_membership() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let router = AgentRouter::new(temp.path().to_path_buf());
        let leader = AgentAddress::new(WorkspaceId::from_name("workspace-a"), "conversation-a");
        let duplicate_address = vec![group_member("explorer"), group_member("reviewer")];
        assert!(matches!(
            router
                .create_group("Duplicate address", leader.clone(), duplicate_address)
                .await,
            Err(AgentRouterError::Validation(_))
        ));

        let mut duplicate_role = group_member("explorer");
        duplicate_role.address =
            AgentAddress::new(WorkspaceId::from_name("workspace-c"), "conversation-c");
        assert!(matches!(
            router
                .create_group(
                    "Duplicate role",
                    leader.clone(),
                    vec![group_member("explorer"), duplicate_role],
                )
                .await,
            Err(AgentRouterError::Validation(_))
        ));

        assert!(matches!(
            router
                .create_group(
                    "Leader member",
                    leader.clone(),
                    vec![AgentGroupMember {
                        address: leader,
                        subagent_role: "explorer".to_string(),
                        label: None,
                    }],
                )
                .await,
            Err(AgentRouterError::Validation(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn persisted_message_survives_restart_and_duplicate_retry() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let router = AgentRouter::new(temp.path().to_path_buf());
        let mut message = AgentMessage::user_text(None, address(), "question");
        message.message_id = "stable-message".to_string();

        let first = router
            .enqueue(message.clone())
            .await
            .map_err(|error| error.to_string())?;
        assert!(!first.duplicate);
        assert_eq!(first.durability, AgentDeliveryDurability::Confirmed);
        drop(router);

        let restarted = AgentRouter::new(temp.path().to_path_buf());
        let duplicate = restarted
            .enqueue(message.clone())
            .await
            .map_err(|error| error.to_string())?;
        assert!(duplicate.duplicate);
        assert_eq!(duplicate.durability, AgentDeliveryDurability::Unconfirmed);
        assert_eq!(duplicate.persisted_at, first.persisted_at);
        let mut later_retry = message.clone();
        later_retry.created_at += chrono::Duration::seconds(30);
        let later_duplicate = restarted
            .enqueue(later_retry)
            .await
            .map_err(|error| error.to_string())?;
        assert!(later_duplicate.duplicate);
        assert_eq!(
            restarted
                .pending(&address())
                .await
                .map_err(|e| e.to_string())?,
            vec![message]
        );
        Ok(())
    }

    #[tokio::test]
    async fn reply_identity_is_stable_for_one_causal_message() -> Result<(), String> {
        let source = AgentAddress::new(WorkspaceId::from_name("source"), "source-conversation");
        let target = address();
        let first = AgentMessage::agent_reply(
            target.clone(),
            source.clone(),
            "answer",
            "correlation",
            "causal-message",
        );
        let second = AgentMessage::agent_reply(
            target.clone(),
            source,
            "answer",
            "correlation",
            "causal-message",
        );
        assert_eq!(first.message_id, second.message_id);
        assert_eq!(first.delivery_turn_id(), second.delivery_turn_id());
        let other_target =
            AgentAddress::new(WorkspaceId::from_name("other-target"), "other-conversation");
        let other_reply = AgentMessage::agent_reply(
            other_target,
            target,
            "answer",
            "correlation",
            "causal-message",
        );
        assert_ne!(first.message_id, other_reply.message_id);

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let router = AgentRouter::new(temp.path().to_path_buf());
        router
            .enqueue(first)
            .await
            .map_err(|error| error.to_string())?;
        let duplicate = router
            .enqueue(second)
            .await
            .map_err(|error| error.to_string())?;
        assert!(duplicate.duplicate);
        Ok(())
    }

    #[tokio::test]
    async fn same_id_with_different_content_fails_closed() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let router = AgentRouter::new(temp.path().to_path_buf());
        let mut first = AgentMessage::user_text(None, address(), "first");
        first.message_id = "collision".to_string();
        router
            .enqueue(first.clone())
            .await
            .map_err(|error| error.to_string())?;
        let mut second = first;
        second.payload = AgentMessagePayload::Text {
            text: "second".to_string(),
        };

        assert!(matches!(
            router.enqueue(second).await,
            Err(AgentRouterError::IdCollision { .. })
        ));
        Ok(())
    }

    #[tokio::test]
    async fn corrupt_inbox_is_never_silently_replaced() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let target = address();
        let path = inbox_dir(temp.path(), &target).join("journal");
        let journal =
            SegmentedFileEventJournal::open(&path, INBOX_SEGMENT_BYTES, FileDurability::SyncData)
                .map_err(|error| error.to_string())?;
        journal
            .append(AgentInboxEvent::Persisted {
                message: AgentMessage::user_text(None, target.clone(), "persisted"),
                persisted_at: Utc::now(),
            })
            .map_err(|error| error.to_string())?;
        let segment = journal
            .segments()
            .into_iter()
            .find(|segment| segment.active)
            .map(|segment| segment.path)
            .ok_or_else(|| "active Agent inbox segment missing".to_string())?;
        drop(journal);
        use std::io::Write as _;
        let mut file = OpenOptions::new()
            .append(true)
            .open(&segment)
            .map_err(|error| error.to_string())?;
        file.write_all(b"{broken}\n")
            .map_err(|error| error.to_string())?;
        file.sync_data().map_err(|error| error.to_string())?;
        let router = AgentRouter::new(temp.path().to_path_buf());

        assert!(matches!(
            router.pending(&target).await,
            Err(AgentRouterError::Corrupt { .. })
        ));
        assert!(
            std::fs::read_to_string(segment)
                .map_err(|error| error.to_string())?
                .ends_with("{broken}\n")
        );
        Ok(())
    }

    #[tokio::test]
    async fn claims_are_fifo_deferred_and_terminally_settled() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let router = AgentRouter::new(temp.path().to_path_buf());
        let mut first = AgentMessage::user_text(None, address(), "first");
        first.message_id = "first".to_string();
        let mut second = AgentMessage::user_text(None, address(), "second");
        second.message_id = "second".to_string();
        router
            .enqueue(first.clone())
            .await
            .map_err(|error| error.to_string())?;
        router
            .enqueue(second.clone())
            .await
            .map_err(|error| error.to_string())?;

        let first_claim = router
            .claim_next(&address())
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "first claim missing".to_string())?;
        assert_eq!(first_claim.message, first);
        router
            .defer(&first_claim, "busy")
            .await
            .map_err(|error| error.to_string())?;
        let deadline = router
            .next_attempt_at(&address())
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "deferred claim lost its retry deadline".to_string())?;
        let delay = deadline
            .signed_duration_since(Utc::now())
            .to_std()
            .unwrap_or(std::time::Duration::ZERO);
        if !delay.is_zero() {
            tokio::time::sleep(delay.saturating_add(std::time::Duration::from_millis(5))).await;
        }
        let retry = router
            .claim_next(&address())
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "deferred claim missing".to_string())?;
        assert_eq!(retry.message.message_id, "first");
        assert_eq!(retry.attempt, 2);
        mark_completed(&router, &retry, "turn-first").await?;
        let second_claim = router
            .claim_next(&address())
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "second claim missing".to_string())?;
        assert_eq!(second_claim.message, second);
        router
            .turn_settled(
                &second_claim,
                None,
                AgentDeliveryOutcome::Failed,
                false,
                Some("permanent".to_string()),
                false,
                None,
            )
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            router
                .pending(&address())
                .await
                .map_err(|error| error.to_string())?
                .is_empty()
        );
        let records = router
            .records(&address())
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(records.len(), 2);
        assert_eq!(
            records
                .first()
                .map(|record| (record.phase, record.outcome, record.drained)),
            Some((
                AgentDeliveryPhase::TurnSettled,
                Some(AgentDeliveryOutcome::Completed),
                true,
            ))
        );
        assert_eq!(
            records
                .get(1)
                .map(|record| (record.phase, record.outcome, record.drained)),
            Some((
                AgentDeliveryPhase::TurnSettled,
                Some(AgentDeliveryOutcome::Failed),
                false,
            ))
        );
        Ok(())
    }

    #[tokio::test]
    async fn restart_reclaims_incomplete_attempt_and_rejects_stale_settlement() -> Result<(), String>
    {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let router = AgentRouter::new(temp.path().to_path_buf());
        let mut message = AgentMessage::user_text(None, address(), "recover");
        message.message_id = "recover".to_string();
        router
            .enqueue(message)
            .await
            .map_err(|error| error.to_string())?;
        let abandoned = router
            .claim_next(&address())
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "abandoned claim missing".to_string())?;
        drop(router);

        let restarted = AgentRouter::new(temp.path().to_path_buf());
        let recovered = restarted
            .claim_next(&address())
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "recovered claim missing".to_string())?;
        assert_eq!(recovered.attempt, 2);
        assert!(matches!(
            restarted
                .turn_settled(
                    &abandoned,
                    Some("stale".to_string()),
                    AgentDeliveryOutcome::Completed,
                    true,
                    None,
                    false,
                    None,
                )
                .await,
            Err(AgentRouterError::StaleClaim { .. })
        ));
        mark_completed(&restarted, &recovered, "recovered").await?;
        let duplicate = restarted
            .enqueue(recovered.message)
            .await
            .map_err(|error| error.to_string())?;
        assert!(duplicate.duplicate);
        assert_eq!(duplicate.phase, AgentDeliveryPhase::TurnSettled);
        assert_eq!(duplicate.outcome, Some(AgentDeliveryOutcome::Completed));
        assert!(duplicate.drained);
        Ok(())
    }

    #[tokio::test]
    async fn restart_never_reclaims_a_drained_attempt() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let target = address();
        let router = AgentRouter::new(temp.path().to_path_buf());
        let mut message = AgentMessage::user_text(None, target.clone(), "do not replay");
        message.message_id = "drained-before-restart".to_string();
        router
            .enqueue(message)
            .await
            .map_err(|error| error.to_string())?;
        let claim = router
            .claim_next(&target)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "claim missing".to_string())?;
        router
            .begin_injection(&claim, "turn-before-restart")
            .await
            .map_err(|error| error.to_string())?;
        router
            .mailbox_accepted(&claim, "turn-before-restart")
            .await
            .map_err(|error| error.to_string())?;
        router
            .drained(&claim, "turn-before-restart")
            .await
            .map_err(|error| error.to_string())?;
        drop(router);

        let restarted = AgentRouter::new(temp.path().to_path_buf());
        assert!(
            restarted
                .claim_next(&target)
                .await
                .map_err(|error| error.to_string())?
                .is_none()
        );
        let recovered = restarted
            .in_flight_claim(&target)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "drained recovery identity missing".to_string())?;
        assert_eq!(recovered.claim.attempt_id, claim.attempt_id);
        assert_eq!(recovered.claim.attempt, claim.attempt);
        assert_eq!(recovered.phase, AgentDeliveryPhase::Drained);
        assert_eq!(recovered.turn_id, "turn-before-restart");
        restarted
            .turn_settled(
                &recovered.claim,
                Some(recovered.turn_id.clone()),
                AgentDeliveryOutcome::OutcomeUnknown,
                true,
                Some("outcome indeterminate".to_string()),
                false,
                None,
            )
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            restarted
                .in_flight_claim(&target)
                .await
                .map_err(|error| error.to_string())?
                .is_none()
        );
        assert_eq!(
            restarted
                .records(&target)
                .await
                .map_err(|error| error.to_string())?
                .first()
                .map(|record| (record.phase, record.outcome, record.attempt)),
            Some((
                AgentDeliveryPhase::TurnSettled,
                Some(AgentDeliveryOutcome::OutcomeUnknown),
                1,
            ))
        );
        Ok(())
    }

    #[tokio::test]
    async fn effect_started_crash_preserves_attempt_and_actual_turn_without_replay()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let target = address();
        let router = AgentRouter::new(temp.path().to_path_buf());
        let mut message = AgentMessage::user_text(None, target.clone(), "started crash");
        message.message_id = "started-before-crash".to_string();
        router
            .enqueue(message)
            .await
            .map_err(|error| error.to_string())?;
        let claim = router
            .claim_next(&target)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "claim missing".to_string())?;
        router
            .begin_injection(&claim, "actual-active-turn")
            .await
            .map_err(|error| error.to_string())?;
        drop(router);

        let restarted = AgentRouter::new(temp.path().to_path_buf());
        assert!(
            restarted
                .claim_next(&target)
                .await
                .map_err(|error| error.to_string())?
                .is_none()
        );
        let in_flight = restarted
            .in_flight_claim(&target)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "started recovery missing".to_string())?;
        assert_eq!(in_flight.claim.attempt_id, claim.attempt_id);
        assert_eq!(in_flight.phase, AgentDeliveryPhase::Claimed);
        assert!(in_flight.effect_started);
        assert_eq!(in_flight.turn_id, "actual-active-turn");
        restarted
            .turn_settled(
                &in_flight.claim,
                Some(in_flight.turn_id.clone()),
                AgentDeliveryOutcome::OutcomeUnknown,
                false,
                Some("outcome unknown".to_string()),
                false,
                None,
            )
            .await
            .map_err(|error| error.to_string())?;
        let record = restarted
            .records(&target)
            .await
            .map_err(|error| error.to_string())?
            .into_iter()
            .next()
            .ok_or_else(|| "terminal record missing".to_string())?;
        assert_eq!(record.phase, AgentDeliveryPhase::TurnSettled);
        assert_eq!(record.outcome, Some(AgentDeliveryOutcome::OutcomeUnknown));
        assert!(!record.drained);
        assert_eq!(record.attempt, 1);
        assert_eq!(record.turn_id.as_deref(), Some("actual-active-turn"));
        Ok(())
    }

    #[tokio::test]
    async fn checkpointed_inbox_restarts_from_projection_and_retirement_forgets_history()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let target = address();
        let router = AgentRouter::new(temp.path().to_path_buf());
        for index in 0..70 {
            let mut message = AgentMessage::user_text(
                None,
                target.clone(),
                format!("checkpointed message {index}"),
            );
            message.message_id = format!("checkpointed-{index}");
            router
                .enqueue(message)
                .await
                .map_err(|error| error.to_string())?;
        }
        let checkpoint = inbox_dir(temp.path(), &target).join("projection.checkpoint.json");
        let frame = FileCheckpointStore::<AgentInboxProjection>::open(&checkpoint)
            .load()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "Agent inbox checkpoint was not compounded".to_string())?;
        assert_eq!(frame.sequence, INBOX_CHECKPOINT_EVERY);
        assert_eq!(frame.state.order.len(), INBOX_CHECKPOINT_EVERY as usize);
        assert!(
            !inbox_dir(temp.path(), &target)
                .join("events.jsonl")
                .exists()
        );
        drop(router);

        let restarted = AgentRouter::new(temp.path().to_path_buf());
        assert_eq!(
            restarted
                .records(&target)
                .await
                .map_err(|error| error.to_string())?
                .len(),
            70
        );
        let retirement = restarted
            .retire_target(target.clone())
            .await
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            restarted.records(&target).await,
            Err(AgentRouterError::Retiring { .. })
        ));
        drop(retirement);
        assert!(
            restarted
                .records(&target)
                .await
                .map_err(|error| error.to_string())?
                .is_empty()
        );
        let mut rebuilt = AgentMessage::user_text(None, target.clone(), "fresh generation");
        rebuilt.message_id = "fresh-generation".to_string();
        restarted
            .enqueue(rebuilt)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            restarted
                .records(&target)
                .await
                .map_err(|error| error.to_string())?
                .len(),
            1
        );
        Ok(())
    }

    fn apply_terminal_projection_lifecycle(
        projection: &mut AgentInboxProjection,
        target: &AgentAddress,
        index: usize,
        timestamp: DateTime<Utc>,
        text: &str,
    ) -> Result<(), String> {
        let message_id = format!("scale-message-{index}");
        let attempt_id = format!("scale-attempt-{index}");
        let turn_id = format!("scale-turn-{index}");
        let mut message = AgentMessage::user_text(None, target.clone(), text);
        message.message_id = message_id.clone();
        projection.apply_checked(&AgentInboxEvent::Persisted {
            message,
            persisted_at: timestamp,
        })?;
        projection.apply_checked(&AgentInboxEvent::Claimed {
            message_id: message_id.clone(),
            attempt_id: attempt_id.clone(),
            attempt: 1,
            claimed_at: timestamp,
        })?;
        projection.apply_checked(&AgentInboxEvent::EffectStarted {
            message_id: message_id.clone(),
            attempt_id: attempt_id.clone(),
            started_at: timestamp,
            turn_id: turn_id.clone(),
        })?;
        projection.apply_checked(&AgentInboxEvent::MailboxAccepted {
            message_id: message_id.clone(),
            attempt_id: attempt_id.clone(),
            accepted_at: timestamp,
            turn_id: turn_id.clone(),
        })?;
        projection.apply_checked(&AgentInboxEvent::Drained {
            message_id: message_id.clone(),
            attempt_id: attempt_id.clone(),
            drained_at: timestamp,
            turn_id: turn_id.clone(),
        })?;
        projection.apply_checked(&AgentInboxEvent::TurnSettled {
            message_id,
            attempt_id,
            settled_at: timestamp,
            turn_id: Some(turn_id),
            outcome: AgentDeliveryOutcome::Completed,
            drained: true,
            reason: None,
            retryable: false,
            next_attempt_at: None,
            reply_message_id: None,
        })
    }

    fn measure_terminal_projection(
        event_count: usize,
    ) -> Result<(std::time::Duration, std::time::Duration, usize), String> {
        let target = address();
        let mut projection = AgentInboxProjection::default();
        let timestamp = Utc::now();
        let halfway = event_count / 2;
        let started = std::time::Instant::now();
        let mut midpoint = started;
        for index in 0..event_count {
            if index == halfway {
                midpoint = std::time::Instant::now();
            }
            apply_terminal_projection_lifecycle(
                &mut projection,
                &target,
                index,
                timestamp,
                "scale terminal",
            )?;
        }
        let finished = std::time::Instant::now();
        assert_eq!(projection.order.len(), INBOX_TERMINAL_RETENTION);
        assert_eq!(projection.entries.len(), INBOX_TERMINAL_RETENTION);
        assert!(projection.frontier.is_empty());
        assert_eq!(
            projection
                .full_validation_count
                .load(std::sync::atomic::Ordering::Relaxed),
            0,
            "hot reducer mutation unexpectedly ran a full projection validation"
        );
        projection
            .validate(Path::new("scale-projection"), &target)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            projection
                .full_validation_count
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        let checkpoint_bytes = serde_json::to_vec(&projection)
            .map_err(|error| format!("projection checkpoint serialization failed: {error}"))?
            .len();
        Ok((
            midpoint.saturating_duration_since(started),
            finished.saturating_duration_since(midpoint),
            checkpoint_bytes,
        ))
    }

    #[test]
    fn terminal_projection_is_bounded_at_10k_and_100k_without_hot_full_validation()
    -> Result<(), String> {
        let (_ten_first, _ten_second, ten_checkpoint) = measure_terminal_projection(10_000)?;
        let (hundred_first, hundred_second, hundred_checkpoint) =
            measure_terminal_projection(100_000)?;
        let second_half_budget = hundred_first
            .saturating_mul(4)
            .saturating_add(std::time::Duration::from_millis(250));
        assert!(
            hundred_second <= second_half_budget,
            "100k terminal projection second half regressed: first={hundred_first:?}, second={hundred_second:?}, budget={second_half_budget:?}"
        );
        assert!(
            hundred_checkpoint <= ten_checkpoint.saturating_add(16 * 1024),
            "bounded checkpoint grew with terminal history: 10k={ten_checkpoint}, 100k={hundred_checkpoint}"
        );
        assert!(hundred_checkpoint <= 512 * 1024);
        Ok(())
    }

    #[test]
    fn near_max_terminal_payloads_obey_the_absolute_checkpoint_byte_budget() -> Result<(), String> {
        let target = address();
        let timestamp = Utc::now();
        let payload = "x".repeat(MAX_TEXT_CHARS);
        let mut projection = AgentInboxProjection::default();
        for index in 0..8 {
            apply_terminal_projection_lifecycle(
                &mut projection,
                &target,
                index,
                timestamp,
                &payload,
            )?;
        }
        projection
            .validate(Path::new("large-terminal-projection"), &target)
            .map_err(|error| error.to_string())?;
        assert!(projection.order.len() < 8);
        assert!(projection.terminal_retained_bytes <= INBOX_TERMINAL_RETENTION_BYTES);
        let checkpoint = serde_json::to_vec(&projection)
            .map_err(|error| format!("large checkpoint serialization failed: {error}"))?;
        assert!(
            checkpoint.len() <= 512 * 1024,
            "large terminal checkpoint exceeded absolute budget: {} bytes",
            checkpoint.len()
        );

        let mut oversized = AgentInboxProjection::default();
        let unicode_payload = "界".repeat(MAX_TEXT_CHARS);
        apply_terminal_projection_lifecycle(
            &mut oversized,
            &target,
            0,
            timestamp,
            &unicode_payload,
        )?;
        assert!(oversized.order.is_empty());
        assert_eq!(oversized.terminal_retained_bytes, 0);
        Ok(())
    }

    #[tokio::test]
    async fn hot_agent_inbox_mutations_do_not_run_full_projection_validation() -> Result<(), String>
    {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let target = address();
        let router = AgentRouter::new(temp.path().to_path_buf());
        let message = AgentMessage::user_text(None, target.clone(), "validation counter");
        router
            .enqueue(message)
            .await
            .map_err(|error| error.to_string())?;
        let claim = router
            .claim_next(&target)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "validation-counter claim is missing".to_string())?;
        router
            .begin_injection(&claim, "validation-turn")
            .await
            .map_err(|error| error.to_string())?;
        router
            .mailbox_accepted(&claim, "validation-turn")
            .await
            .map_err(|error| error.to_string())?;
        router
            .drained(&claim, "validation-turn")
            .await
            .map_err(|error| error.to_string())?;
        router
            .turn_settled(
                &claim,
                Some("validation-turn".to_string()),
                AgentDeliveryOutcome::Completed,
                true,
                None,
                false,
                None,
            )
            .await
            .map_err(|error| error.to_string())?;
        let authority = authority_for(temp.path(), &router.inboxes, &target)
            .map_err(|error| error.to_string())?;
        let validations = authority
            .with_projection(|projection| {
                Ok(projection
                    .full_validation_count
                    .load(std::sync::atomic::Ordering::Relaxed))
            })
            .map_err(|error| error.to_string())?;
        assert_eq!(validations, 1, "hot append reran full validation");
        Ok(())
    }

    #[tokio::test]
    async fn workspace_retirement_forgets_only_that_workspace() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let router = AgentRouter::new(temp.path().to_path_buf());
        let first = AgentAddress::new(WorkspaceId::from_name("retired"), "first");
        let second = AgentAddress::new(WorkspaceId::from_name("retired"), "second");
        let retained = AgentAddress::new(WorkspaceId::from_name("retained"), "third");
        for target in [&first, &second, &retained] {
            router
                .enqueue(AgentMessage::user_text(
                    None,
                    target.clone(),
                    "workspace retirement fixture",
                ))
                .await
                .map_err(|error| error.to_string())?;
        }
        let retirement = router
            .retire_workspace(first.workspace_id.clone())
            .await
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            router.records(&first).await,
            Err(AgentRouterError::Retiring { .. })
        ));
        assert_eq!(
            router
                .records(&retained)
                .await
                .map_err(|error| error.to_string())?
                .len(),
            1
        );
        drop(retirement);
        assert!(
            router
                .records(&first)
                .await
                .map_err(|error| error.to_string())?
                .is_empty()
        );
        assert!(
            router
                .records(&second)
                .await
                .map_err(|error| error.to_string())?
                .is_empty()
        );
        assert_eq!(
            router
                .records(&retained)
                .await
                .map_err(|error| error.to_string())?
                .len(),
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn three_workspace_inboxes_survive_restart_and_concurrent_drain() -> Result<(), String> {
        const MESSAGES_PER_WORKSPACE: usize = 32;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().to_path_buf();
        let targets = ["alpha", "beta", "gamma"]
            .into_iter()
            .map(|name| {
                AgentAddress::new(WorkspaceId::from_name(name), format!("{name}-conversation"))
            })
            .collect::<Vec<_>>();
        let router = AgentRouter::new(root.clone());
        let mut messages = Vec::new();
        for target in &targets {
            for offset in 0..MESSAGES_PER_WORKSPACE {
                let mut message =
                    AgentMessage::user_text(None, target.clone(), format!("message {offset}"));
                message.message_id = format!("{}-{offset}", target.workspace_id);
                router
                    .enqueue(message.clone())
                    .await
                    .map_err(|error| error.to_string())?;
                messages.push(message);
            }
            let abandoned = router
                .claim_next(target)
                .await
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("{} first claim missing", target.workspace_id))?;
            assert_eq!(abandoned.attempt, 1);
        }
        drop(router);

        let alpha = targets
            .first()
            .cloned()
            .ok_or_else(|| "alpha target missing".to_string())?;
        let beta = targets
            .get(1)
            .cloned()
            .ok_or_else(|| "beta target missing".to_string())?;
        let gamma = targets
            .get(2)
            .cloned()
            .ok_or_else(|| "gamma target missing".to_string())?;
        let (alpha_count, beta_count, gamma_count) = tokio::try_join!(
            drain_inbox(root.clone(), alpha),
            drain_inbox(root.clone(), beta),
            drain_inbox(root.clone(), gamma),
        )?;
        assert_eq!(alpha_count, MESSAGES_PER_WORKSPACE);
        assert_eq!(beta_count, MESSAGES_PER_WORKSPACE);
        assert_eq!(gamma_count, MESSAGES_PER_WORKSPACE);

        let restarted = AgentRouter::new(root);
        for target in &targets {
            let records = restarted
                .records(target)
                .await
                .map_err(|error| error.to_string())?;
            assert_eq!(records.len(), MESSAGES_PER_WORKSPACE);
            assert!(records.iter().all(|record| {
                record.phase == AgentDeliveryPhase::TurnSettled
                    && record.outcome == Some(AgentDeliveryOutcome::Completed)
                    && record.drained
            }));
            assert_eq!(
                records.iter().filter(|record| record.attempt == 2).count(),
                1
            );
        }
        for message in messages {
            let duplicate = restarted
                .enqueue(message)
                .await
                .map_err(|error| error.to_string())?;
            assert!(duplicate.duplicate);
            assert_eq!(duplicate.phase, AgentDeliveryPhase::TurnSettled);
            assert_eq!(duplicate.outcome, Some(AgentDeliveryOutcome::Completed));
            assert!(duplicate.drained);
        }
        Ok(())
    }
}
