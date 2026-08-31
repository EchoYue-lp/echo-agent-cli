#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::task_runtime::command_cells::{
        CommandCellWatchResult, CommandCellWatchAcknowledgement, CommandCellWatchReceipt,
        CommandCellWatchState,
    };
    use crate::tasks::task_runtime::types::{
        BackgroundCellArtifactStatus, BackgroundCellPhase, BackgroundCellState,
        BackgroundCellTerminalCause,
    };
    use echo_agent::agent::{AgentEvent, EventEnvelope, EventIdentity, ToolInvocation};
    use echo_agent::tools::ToolResult;

    #[derive(Default)]
    struct CapturingSink {
        journaled: Mutex<Vec<ChatEventEnvelope>>,
    }

    impl crate::chat_driver::ChatSink for CapturingSink {
        fn on_event(&self, _event: ChatDriverEvent) -> bool {
            false
        }

        fn on_journaled_event(&self, envelope: ChatEventEnvelope) -> bool {
            self.journaled
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(envelope);
            true
        }
    }

    fn agent_event(turn: &str, sequence: u64, text: &str) -> Result<ChatDriverEvent, String> {
        let identity =
            EventIdentity::for_chat(Some("conversation-1".to_string()), turn, turn, None)
                .map_err(|error| error.to_string())?;
        EventEnvelope::new(
            &identity,
            sequence,
            None,
            AgentEvent::Token(text.to_string()),
        )
        .map(|event| ChatDriverEvent::Agent(Box::new(event)))
        .map_err(|error| error.to_string())
    }

    fn append_status(log: &ChatEventLog, status: &str) -> Result<(), String> {
        log.append(
            "workspace-1",
            Some("conversation-1"),
            "turn-1",
            ChatDriverEvent::TurnStatus {
                status: status.to_string(),
            },
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
    }

    fn segment_count(log: &ChatEventLog) -> Result<usize, String> {
        let stream = stream_id("workspace-1", Some("conversation-1"), "turn-1")
            .map_err(|error| error.to_string())?;
        let cached = log
            .stream_journal(&stream, false)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "stream is missing".to_string())?;
        let guard = lock_cached_stream(&cached);
        let authority = guard
            .as_ref()
            .ok_or_else(|| "stream authority is missing".to_string())?;
        Ok(authority.journal.segments().len())
    }

    fn recovered_pin_records(log: &ChatEventLog) -> Result<usize, String> {
        let stream = stream_id("workspace-1", Some("conversation-1"), "turn-1")
            .map_err(|error| error.to_string())?;
        let cached = log
            .stream_journal(&stream, false)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "stream is missing".to_string())?;
        let guard = lock_cached_stream(&cached);
        Ok(guard
            .as_ref()
            .ok_or_else(|| "stream authority is missing".to_string())?
            .pins
            .recovered_records)
    }

    fn command_cell_watch() -> CommandCellWatchResult {
        let now = Utc::now();
        CommandCellWatchResult {
            receipt: CommandCellWatchReceipt {
                execution_id: "await-execution".to_string(),
                watch_generation: 1,
                cell_id: "cell".to_string(),
                workspace_id: "workspace-1".to_string(),
                conversation_id: "conversation-1".to_string(),
                run_id: None,
                root_turn_id: "turn-1".to_string(),
                state: CommandCellWatchState::Settled,
                started_at: now,
                settled_at: Some(now),
            },
            cell: BackgroundCellState {
                cell_id: "cell".to_string(),
                name: "test".to_string(),
                command_hash: "sha256:test".to_string(),
                turn_id: Some("turn-1".to_string()),
                execution_id: Some("cell-execution".to_string()),
                call_id: Some("call".to_string()),
                phase: BackgroundCellPhase::Succeeded,
                terminal_cause: Some(BackgroundCellTerminalCause::Exited),
                terminal_message: None,
                exit_code: Some(0),
                artifact_status: BackgroundCellArtifactStatus::BelowThreshold,
                artifact_message: None,
                total_output_bytes: 2,
                output_truncated: false,
                output_excerpt: Some("ok".to_string()),
                artifact_path: None,
                artifact_sha256: None,
                started_at: now,
                finished_at: Some(now),
            },
        }
    }

    fn active_chat_cell(cell_id: &str) -> BackgroundCellState {
        let mut cell = command_cell_watch().cell;
        cell.cell_id = cell_id.to_string();
        cell.phase = BackgroundCellPhase::Running;
        cell.terminal_cause = None;
        cell.exit_code = None;
        cell.artifact_status = BackgroundCellArtifactStatus::Writing;
        cell.finished_at = None;
        cell
    }

    #[test]
    fn command_cell_watch_ready_requires_exact_stream_and_settled_generation()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = ChatEventLog::open(temp.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        let mutations: [fn(&mut CommandCellWatchResult); 3] = [
            |result: &mut CommandCellWatchResult| {
                result.receipt.workspace_id = "other-workspace".to_string();
            },
            |result: &mut CommandCellWatchResult| {
                result.receipt.conversation_id = "other-conversation".to_string();
            },
            |result: &mut CommandCellWatchResult| {
                result.receipt.root_turn_id = "other-turn".to_string();
            },
        ];
        for mutate in mutations {
            let mut result = command_cell_watch();
            mutate(&mut result);
            assert!(matches!(
                log.append(
                    "workspace-1",
                    Some("conversation-1"),
                    "turn-1",
                    ChatDriverEvent::CommandCellWatchReady {
                        result: Box::new(result),
                    },
                ),
                Err(ChatEventLogError::InvalidIdentity(_))
            ));
        }

        for result in [
            {
                let mut result = command_cell_watch();
                result.receipt.watch_generation = 0;
                result
            },
            {
                let mut result = command_cell_watch();
                result.receipt.state = CommandCellWatchState::Started;
                result.receipt.settled_at = None;
                result
            },
        ] {
            assert!(matches!(
                log.append(
                    "workspace-1",
                    Some("conversation-1"),
                    "turn-1",
                    ChatDriverEvent::CommandCellWatchReady {
                        result: Box::new(result),
                    },
                ),
                Err(ChatEventLogError::InvalidEvent(_))
            ));
        }
        Ok(())
    }

    #[test]
    fn boot_recovery_closes_ordinary_chat_orphan_once() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("chat-events");
        let log = ChatEventLog::open(&root, ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        log.append(
            "workspace-1",
            Some("conversation-1"),
            "turn-1",
            ChatDriverEvent::CommandCellStarted {
                cell: Box::new(active_chat_cell("orphan-chat-cell")),
            },
        )
        .map_err(|error| error.to_string())?;
        drop(log);

        let restarted = ChatEventLog::open(&root, ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        assert_eq!(
            restarted
                .recover_orphan_command_cells()
                .map_err(|error| error.to_string())?,
            1
        );
        assert_eq!(
            restarted
                .recover_orphan_command_cells()
                .map_err(|error| error.to_string())?,
            0
        );
        let replay = restarted
            .replay("workspace-1", Some("conversation-1"), "turn-1", 0)
            .map_err(|error| error.to_string())?;
        let settled = replay
            .events
            .iter()
            .filter_map(|event| match &event.payload {
                ChatDriverEvent::CommandCellSettled { cell }
                    if cell.cell_id == "orphan-chat-cell" =>
                {
                    Some(cell.as_ref())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(settled.len(), 1);
        let cell = settled
            .first()
            .ok_or_else(|| "recovered Chat cell missing".to_string())?;
        assert_eq!(cell.phase, BackgroundCellPhase::Failed);
        assert_eq!(
            cell.terminal_cause,
            Some(BackgroundCellTerminalCause::Interrupted)
        );
        assert_eq!(cell.artifact_status, BackgroundCellArtifactStatus::Failed);
        Ok(())
    }

    #[test]
    fn live_terminal_wins_orphan_recovery_without_duplicate_terminal() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let pause = Arc::new((std::sync::Barrier::new(2), std::sync::Barrier::new(2)));
        let log = Arc::new(
            ChatEventLog::open(
                temp.path().join("chat-events"),
                ChatEventRetention::default(),
            )
            .map_err(|error| error.to_string())?
            .with_orphan_recovery_pause(Arc::clone(&pause)),
        );
        let started = active_chat_cell("racing-chat-cell");
        log.append(
            "workspace-1",
            Some("conversation-1"),
            "turn-1",
            ChatDriverEvent::CommandCellStarted {
                cell: Box::new(started.clone()),
            },
        )
        .map_err(|error| error.to_string())?;
        let recovering = Arc::clone(&log);
        let recovery = std::thread::spawn(move || recovering.recover_orphan_command_cells());
        pause.0.wait();

        let mut terminal = started;
        terminal.phase = BackgroundCellPhase::Succeeded;
        terminal.terminal_cause = Some(BackgroundCellTerminalCause::Exited);
        terminal.exit_code = Some(0);
        terminal.artifact_status = BackgroundCellArtifactStatus::BelowThreshold;
        terminal.finished_at = Some(Utc::now());
        log.append(
            "workspace-1",
            Some("conversation-1"),
            "turn-1",
            ChatDriverEvent::CommandCellSettled {
                cell: Box::new(terminal),
            },
        )
        .map_err(|error| error.to_string())?;
        pause.1.wait();
        assert_eq!(
            recovery
                .join()
                .map_err(|_| "orphan recovery thread panicked".to_string())?
                .map_err(|error| error.to_string())?,
            0
        );
        let replay = log
            .replay("workspace-1", Some("conversation-1"), "turn-1", 0)
            .map_err(|error| error.to_string())?;
        let terminals = replay
            .events
            .iter()
            .filter_map(|event| match &event.payload {
                ChatDriverEvent::CommandCellSettled { cell }
                    if cell.cell_id == "racing-chat-cell" =>
                {
                    Some(cell.phase)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(terminals, vec![BackgroundCellPhase::Succeeded]);
        Ok(())
    }

    #[test]
    fn rust_wire_model_losslessly_accepts_frontend_fixture() -> Result<(), String> {
        let fixture = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../web-frontend/src/fixtures/chat-event-envelope-v4.json"
        ));
        let expected: serde_json::Value =
            serde_json::from_str(fixture).map_err(|error| error.to_string())?;
        let envelopes: Vec<ChatEventEnvelope> =
            serde_json::from_value(expected.clone()).map_err(|error| error.to_string())?;
        assert_eq!(
            serde_json::to_value(envelopes).map_err(|error| error.to_string())?,
            expected
        );
        Ok(())
    }

    #[test]
    fn typed_round_trip_preserves_framework_envelope_and_cursor() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("events");
        let log = ChatEventLog::open(&root, ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        log.append(
            "workspace-1",
            Some("conversation-1"),
            "turn-1",
            agent_event("turn-1", 7, "你好")?,
        )
        .map_err(|error| error.to_string())?;
        append_status(&log, "completed")?;
        drop(log);

        let reopened = ChatEventLog::open(root, ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        let replay = reopened
            .replay("workspace-1", Some("conversation-1"), "ignored", 0)
            .map_err(|error| error.to_string())?;
        assert_eq!(replay.latest_cursor, 2);
        let first = replay
            .events
            .first()
            .ok_or_else(|| "missing event".to_string())?;
        assert!(
            matches!(&first.payload, ChatDriverEvent::Agent(agent) if matches!(&agent.payload, AgentEvent::Token(text) if text == "你好"))
        );
        Ok(())
    }

    #[test]
    fn replay_cap_and_retained_gap_have_distinct_cursors() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = ChatEventLog::open(
            temp.path(),
            ChatEventRetention {
                segment_rollover_bytes: 1,
                max_segments: 2,
                max_replay_events: 2,
            },
        )
        .map_err(|error| error.to_string())?;
        for sequence in 1..=4 {
            log.append(
                "workspace-1",
                Some("conversation-1"),
                "turn-1",
                agent_event("turn-1", sequence, "delta")?,
            )
            .map_err(|error| error.to_string())?;
        }
        append_status(&log, "completed")?;
        let replay = log
            .replay("workspace-1", Some("conversation-1"), "turn-1", 0)
            .map_err(|error| error.to_string())?;
        assert!(replay.truncated);
        assert_eq!(replay.retained_earliest_cursor, Some(4));
        assert_eq!(replay.returned_earliest_cursor, Some(4));
        assert_eq!(replay.latest_cursor, 5);
        assert_eq!(replay.events.len(), 2);
        Ok(())
    }

    #[test]
    fn unacknowledged_command_cell_watch_pins_then_acknowledgement_converges() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = ChatEventLog::open(
            temp.path(),
            ChatEventRetention {
                segment_rollover_bytes: 1,
                max_segments: 1,
                max_replay_events: 1,
            },
        )
        .map_err(|error| error.to_string())?;
        let result = command_cell_watch();
        let event = || ChatDriverEvent::CommandCellWatchReady {
            result: Box::new(result.clone()),
        };
        let first = log
            .append("workspace-1", Some("conversation-1"), "turn-1", event())
            .map_err(|error| error.to_string())?;
        let duplicate = log
            .append("workspace-1", Some("conversation-1"), "turn-1", event())
            .map_err(|error| error.to_string())?;
        assert_eq!(first.event_id, duplicate.event_id);
        for _ in 0..3 {
            append_status(&log, "completed")?;
        }
        assert!(segment_count(&log)? > 1);
        assert_eq!(
            log.pending_command_cell_watches("workspace-1", "conversation-1", "turn-1")
                .map_err(|error| error.to_string())?,
            vec![result.clone()]
        );
        log.append(
            "workspace-1",
            Some("conversation-1"),
            "turn-1",
            ChatDriverEvent::CommandCellWatchAcknowledged {
                acknowledgement: CommandCellWatchAcknowledgement {
                    execution_id: result.receipt.execution_id,
                    watch_generation: result.receipt.watch_generation,
                    cell_id: result.receipt.cell_id,
                    workspace_id: result.receipt.workspace_id,
                    conversation_id: result.receipt.conversation_id,
                    root_turn_id: result.receipt.root_turn_id,
                    acknowledged_turn_id: "next-turn".to_string(),
                    outcome:
                        crate::tasks::task_runtime::command_cells::CommandCellWatchDeliveryOutcome::Drained,
                },
            },
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(segment_count(&log)?, 1);
        Ok(())
    }

    #[test]
    fn every_surface_journals_before_render_and_projects_tools() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = Arc::new(
            ChatEventLog::open(temp.path().join("events"), ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        );
        let tools = Arc::new(
            ToolExecutionRepository::open(temp.path().join("tools"))
                .map_err(|error| error.to_string())?,
        );
        for (offset, surface) in [
            ChatSurface::Gui,
            ChatSurface::Tui,
            ChatSurface::Cli,
            ChatSurface::Channel,
        ]
        .into_iter()
        .enumerate()
        {
            let captured = Arc::new(CapturingSink::default());
            let turn = format!("turn-{offset}");
            let sink = bind_surface_chat_sink(
                surface,
                captured.clone(),
                log.clone(),
                tools.clone(),
                "workspace-1",
                Some("conversation-1".to_string()),
                &turn,
            );
            assert!(sink.on_event(ChatDriverEvent::TurnStatus {
                status: "running".to_string(),
            }));
            let identity =
                EventIdentity::for_chat(Some("conversation-1".to_string()), &turn, &turn, None)
                    .map_err(|error| error.to_string())?;
            let call_id = format!("call-{offset}");
            let call = EventEnvelope::new(
                &identity,
                1,
                None,
                AgentEvent::ToolCall {
                    call_id: call_id.clone(),
                    invocation: ToolInvocation {
                        requested_name: "shell".to_string(),
                        requested_args: serde_json::json!({"command": "requested"}),
                        name: "sandbox_shell".to_string(),
                        args: serde_json::json!({"command": "effective"}),
                        rewrites: Vec::new(),
                    },
                },
            )
            .map_err(|error| error.to_string())?;
            assert!(sink.on_event(ChatDriverEvent::Agent(Box::new(call))));
            let result = EventEnvelope::new(
                &identity,
                2,
                None,
                AgentEvent::ToolResult {
                    call_id,
                    name: "sandbox_shell".to_string(),
                    result: ToolResult::success("done"),
                },
            )
            .map_err(|error| error.to_string())?;
            assert!(sink.on_event(ChatDriverEvent::Agent(Box::new(result))));
            assert_eq!(
                captured
                    .journaled
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .len(),
                3
            );
        }
        assert_eq!(
            log.replay("workspace-1", Some("conversation-1"), "ignored", 0)
                .map_err(|error| error.to_string())?
                .events
                .len(),
            12
        );
        assert_eq!(
            tools
                .summaries_for_conversation("workspace-1", "conversation-1")
                .len(),
            4
        );
        Ok(())
    }

    #[test]
    fn scoped_deletion_releases_authorities_and_preserves_other_workspace() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = ChatEventLog::open(temp.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        for workspace in ["workspace-1", "workspace-2"] {
            log.append(
                workspace,
                Some("conversation-1"),
                "turn-1",
                ChatDriverEvent::TurnStatus {
                    status: "completed".to_string(),
                },
            )
            .map_err(|error| error.to_string())?;
        }
        log.remove_conversation("workspace-1", "conversation-1")
            .map_err(|error| error.to_string())?;
        assert!(
            log.replay("workspace-1", Some("conversation-1"), "turn-1", 0)
                .map_err(|error| error.to_string())?
                .events
                .is_empty()
        );
        assert_eq!(
            log.replay("workspace-2", Some("conversation-1"), "turn-1", 0)
                .map_err(|error| error.to_string())?
                .events
                .len(),
            1
        );
        log.remove_workspace("workspace-2")
            .map_err(|error| error.to_string())?;
        assert!(
            log.replay("workspace-2", Some("conversation-1"), "turn-1", 0)
                .map_err(|error| error.to_string())?
                .events
                .is_empty()
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn replaced_root_and_stream_symlinks_fail_closed() -> Result<(), String> {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("events");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&outside).map_err(|error| error.to_string())?;
        let log = ChatEventLog::open(&root, ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        append_status(&log, "completed")?;
        let stream = stream_id("workspace-1", Some("conversation-1"), "turn-1")
            .map_err(|error| error.to_string())?;
        let stream_dir = log.stream_dir(&stream);
        let backup = temp.path().join("stream-backup");
        fs::rename(&stream_dir, &backup).map_err(|error| error.to_string())?;
        symlink(&outside, &stream_dir).map_err(|error| error.to_string())?;
        assert!(
            log.replay("workspace-1", Some("conversation-1"), "turn-1", 0)
                .is_err()
        );
        fs::remove_file(&stream_dir).map_err(|error| error.to_string())?;
        fs::rename(&backup, &stream_dir).map_err(|error| error.to_string())?;
        let root_backup = temp.path().join("root-backup");
        fs::rename(&root, &root_backup).map_err(|error| error.to_string())?;
        symlink(&outside, &root).map_err(|error| error.to_string())?;
        assert!(append_status(&log, "completed").is_err());
        Ok(())
    }

    #[test]
    fn durability_policy_preserves_delta_and_safe_point_classes() -> Result<(), String> {
        assert_eq!(
            append_durability(&agent_event("turn-1", 1, "delta")?),
            FileDurability::Flush
        );
        assert_eq!(
            append_durability(&ChatDriverEvent::TurnStatus {
                status: "completed".to_string(),
            }),
            FileDurability::SyncData
        );
        assert!(should_maintain_retention(
            FileDurability::SyncData,
            &JournalDurabilityStatus::Confirmed,
        ));
        assert!(!should_maintain_retention(
            FileDurability::Flush,
            &JournalDurabilityStatus::Confirmed,
        ));
        assert!(!should_maintain_retention(
            FileDurability::SyncData,
            &JournalDurabilityStatus::Degraded {
                error: "barrier failed after the full record committed".to_string(),
            },
        ));
        assert!(should_mark_barrier_pending(
            FileDurability::SyncData,
            &JournalDurabilityStatus::Degraded {
                error: "one committed sequence still owes a barrier".to_string(),
            },
        ));
        assert!(!should_mark_barrier_pending(
            FileDurability::SyncData,
            &JournalDurabilityStatus::Confirmed,
        ));
        Ok(())
    }

    #[test]
    fn outer_content_hash_is_stable_across_unordered_payload_maps() -> Result<(), String> {
        let fixture = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../web-frontend/src/fixtures/chat-event-envelope-v4.json"
        ));
        let mut fixture: serde_json::Value =
            serde_json::from_str(fixture).map_err(|error| error.to_string())?;
        let metadata = fixture
            .pointer_mut("/1/payload/event/payload/data/result/metadata")
            .and_then(serde_json::Value::as_object_mut)
            .ok_or_else(|| "tool-result metadata fixture missing".to_string())?;
        for key in ["zeta", "alpha", "gamma", "beta"] {
            metadata.insert(key.to_string(), serde_json::Value::String(key.to_string()));
        }
        let payload = fixture
            .pointer("/1/payload")
            .cloned()
            .ok_or_else(|| "tool-result payload fixture missing".to_string())?;
        let first: ChatDriverEvent =
            serde_json::from_value(payload.clone()).map_err(|error| error.to_string())?;
        let second: ChatDriverEvent =
            serde_json::from_value(payload).map_err(|error| error.to_string())?;
        let timestamp = DateTime::parse_from_rfc3339("2026-08-16T00:00:01Z")
            .map_err(|error| error.to_string())?
            .with_timezone(&Utc);
        let hash = |payload: &ChatDriverEvent| {
            envelope_content_hash(EnvelopeIntegrity {
                schema_version: CHAT_EVENT_SCHEMA_VERSION,
                sequence: 2,
                stream_id: r#"["workspace-1","fixture-conversation"]"#,
                workspace_id: "workspace-1",
                conversation_id: Some("fixture-conversation"),
                root_turn_id: "fixture-message",
                turn_id: "fixture-turn",
                message_id: "fixture-message",
                timestamp,
                payload,
            })
        };
        assert_eq!(
            hash(&first).map_err(|error| error.to_string())?,
            hash(&second).map_err(|error| error.to_string())?
        );
        Ok(())
    }

    #[test]
    fn workspace_isolation_and_cross_conversation_rejection_hold() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = ChatEventLog::open(temp.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        for workspace in ["workspace-a", "workspace-b"] {
            log.append(
                workspace,
                Some("conversation-1"),
                "turn-1",
                agent_event("turn-1", 1, workspace)?,
            )
            .map_err(|error| error.to_string())?;
        }
        assert_eq!(
            log.replay("workspace-a", Some("conversation-1"), "turn-1", 0)
                .map_err(|error| error.to_string())?
                .events
                .len(),
            1
        );
        assert_eq!(
            log.replay("workspace-b", Some("conversation-1"), "turn-1", 0)
                .map_err(|error| error.to_string())?
                .events
                .len(),
            1
        );
        assert!(matches!(
            log.append(
                "workspace-a",
                Some("conversation-2"),
                "turn-1",
                agent_event("turn-1", 2, "wrong conversation")?,
            ),
            Err(ChatEventLogError::InvalidIdentity(_))
        ));
        Ok(())
    }

    #[test]
    fn invalid_nested_schema_and_turn_status_fail_on_append_and_replay() -> Result<(), String> {
        for invalid_kind in ["framework_schema", "turn_status"] {
            let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
            let log = ChatEventLog::open(temp.path(), ChatEventRetention::default())
                .map_err(|error| error.to_string())?;
            let mut invalid = if invalid_kind == "framework_schema" {
                agent_event("turn-1", 1, "invalid")?
            } else {
                ChatDriverEvent::TurnStatus {
                    status: "future_terminal".to_string(),
                }
            };
            if let ChatDriverEvent::Agent(envelope) = &mut invalid {
                envelope.schema_version =
                    echo_agent::agent::AGENT_EVENT_SCHEMA_VERSION.saturating_add(1);
            }
            assert!(matches!(
                log.append("workspace-1", Some("conversation-1"), "turn-1", invalid,),
                Err(ChatEventLogError::InvalidEvent(_))
            ));

            let persisted_payload = if invalid_kind == "framework_schema" {
                let mut event = agent_event("turn-1", 1, "invalid replay")?;
                if let ChatDriverEvent::Agent(envelope) = &mut event {
                    envelope.schema_version =
                        echo_agent::agent::AGENT_EVENT_SCHEMA_VERSION.saturating_add(1);
                }
                event
            } else {
                ChatDriverEvent::TurnStatus {
                    status: "future_terminal".to_string(),
                }
            };
            let selected = stream_id("workspace-1", Some("conversation-1"), "turn-1")
                .map_err(|error| error.to_string())?;
            let cached = log
                .stream_journal(&selected, true)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "stream missing".to_string())?;
            let guard = lock_cached_stream(&cached);
            guard
                .as_ref()
                .ok_or_else(|| "authority missing".to_string())?
                .journal
                .append(PersistedChatEvent {
                    schema_version: CHAT_EVENT_SCHEMA_VERSION,
                    stream_id: selected,
                    workspace_id: "workspace-1".to_string(),
                    conversation_id: Some("conversation-1".to_string()),
                    root_turn_id: "turn-1".to_string(),
                    turn_id: "turn-1".to_string(),
                    message_id: "turn-1".to_string(),
                    timestamp: Utc::now(),
                    payload: persisted_payload,
                })
                .map_err(|error| error.to_string())?;
            drop(guard);
            assert!(matches!(
                log.replay("workspace-1", Some("conversation-1"), "turn-1", 0),
                Err(ChatEventLogError::Corrupt { .. })
            ));
            drop(cached);
            drop(log);
            let reopened = ChatEventLog::open(temp.path(), ChatEventRetention::default())
                .map_err(|error| error.to_string())?;
            assert!(matches!(
                reopened.append(
                    "workspace-1",
                    Some("conversation-1"),
                    "turn-1",
                    ChatDriverEvent::TurnStatus {
                        status: "running".to_string(),
                    },
                ),
                Err(ChatEventLogError::Corrupt { .. })
            ));
        }
        Ok(())
    }

    #[test]
    fn persistence_failure_never_reaches_renderer() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("events");
        let log = Arc::new(
            ChatEventLog::open(&root, ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        );
        fs::remove_dir(&root).map_err(|error| error.to_string())?;
        fs::write(&root, b"not a directory").map_err(|error| error.to_string())?;
        let captured = Arc::new(CapturingSink::default());
        let tools = Arc::new(
            ToolExecutionRepository::open(temp.path().join("tools"))
                .map_err(|error| error.to_string())?,
        );
        let sink = bind_surface_chat_sink(
            ChatSurface::Gui,
            captured.clone(),
            log,
            tools,
            "workspace-1",
            Some("conversation-1".to_string()),
            "turn-1",
        );
        assert!(!sink.on_event(ChatDriverEvent::TurnStatus {
            status: "running".to_string(),
        }));
        assert!(
            captured
                .journaled
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn one_locked_stream_does_not_block_another_conversation() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = Arc::new(
            ChatEventLog::open(temp.path(), ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        );
        log.append(
            "workspace-1",
            Some("blocked"),
            "blocked-turn",
            ChatDriverEvent::TurnStatus {
                status: "running".to_string(),
            },
        )
        .map_err(|error| error.to_string())?;
        let blocked_id = stream_id("workspace-1", Some("blocked"), "blocked-turn")
            .map_err(|error| error.to_string())?;
        let blocked = log
            .stream_journal(&blocked_id, false)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "blocked stream missing".to_string())?;
        let blocked_guard = lock_cached_stream(&blocked);
        let free_log = Arc::clone(&log);
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            let result = free_log
                .append(
                    "workspace-1",
                    Some("free"),
                    "free-turn",
                    ChatDriverEvent::TurnStatus {
                        status: "running".to_string(),
                    },
                )
                .map(|_| ())
                .map_err(|error| error.to_string());
            let _ = tx.send(result);
        });
        rx.recv_timeout(std::time::Duration::from_secs(2))
            .map_err(|error| format!("independent stream blocked: {error}"))??;
        drop(blocked_guard);
        handle
            .join()
            .map_err(|_| "independent stream thread failed".to_string())?;
        Ok(())
    }

    #[test]
    fn incremental_pin_projection_does_not_rescan_pinned_history() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("events");
        let retention = ChatEventRetention {
            segment_rollover_bytes: 1,
            max_segments: 1,
            max_replay_events: 1,
        };
        let log = ChatEventLog::open(&root, retention).map_err(|error| error.to_string())?;
        let result = command_cell_watch();
        log.append(
            "workspace-1",
            Some("conversation-1"),
            "turn-1",
            ChatDriverEvent::CommandCellWatchReady {
                result: Box::new(result),
            },
        )
        .map_err(|error| error.to_string())?;
        for _ in 0..4 {
            append_status(&log, "completed")?;
        }
        drop(log);

        let reopened = ChatEventLog::open(root, retention).map_err(|error| error.to_string())?;
        assert_eq!(
            reopened
                .pending_command_cell_watches("workspace-1", "conversation-1", "turn-1")
                .map_err(|error| error.to_string())?
                .len(),
            1
        );
        let recovered_once = recovered_pin_records(&reopened)?;
        assert!(recovered_once > 0);
        for _ in 0..12 {
            append_status(&reopened, "completed")?;
            assert_eq!(
                reopened
                    .pending_command_cell_watches("workspace-1", "conversation-1", "turn-1")
                    .map_err(|error| error.to_string())?
                    .len(),
                1
            );
        }
        assert_eq!(recovered_pin_records(&reopened)?, recovered_once);
        Ok(())
    }

    #[test]
    fn two_handles_share_pins_idempotency_deletion_and_recreation() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("events");
        let first = ChatEventLog::open(&root, ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        let second = ChatEventLog::open(&root, ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        let mismatched = ChatEventLog::open(
            &root,
            ChatEventRetention {
                segment_rollover_bytes: 1,
                ..ChatEventRetention::default()
            },
        )
        .map_err(|error| error.to_string())?;
        assert!(
            mismatched
                .replay("workspace-1", Some("conversation-1"), "turn-1", 0)
                .map_err(|error| error.to_string())?
                .events
                .is_empty()
        );

        let result = command_cell_watch();
        let ready = || ChatDriverEvent::CommandCellWatchReady {
            result: Box::new(result.clone()),
        };
        let original = first
            .append("workspace-1", Some("conversation-1"), "turn-1", ready())
            .map_err(|error| error.to_string())?;
        let duplicate = second
            .append("workspace-1", Some("conversation-1"), "turn-1", ready())
            .map_err(|error| error.to_string())?;
        assert_eq!(original.event_id, duplicate.event_id);
        let mut conflicting = result;
        conflicting.cell.output_excerpt = Some("conflict".to_string());
        assert!(matches!(
            second.append(
                "workspace-1",
                Some("conversation-1"),
                "turn-1",
                ChatDriverEvent::CommandCellWatchReady {
                    result: Box::new(conflicting),
                },
            ),
            Err(ChatEventLogError::InvalidEvent(_))
        ));

        first
            .remove_conversation("workspace-1", "conversation-1")
            .map_err(|error| error.to_string())?;
        assert!(
            second
                .replay("workspace-1", Some("conversation-1"), "turn-1", 0)
                .map_err(|error| error.to_string())?
                .events
                .is_empty()
        );
        second
            .append(
                "workspace-1",
                Some("conversation-1"),
                "turn-new",
                ChatDriverEvent::TurnStatus {
                    status: "completed".to_string(),
                },
            )
            .map_err(|error| error.to_string())?;
        assert_eq!(
            first
                .replay("workspace-1", Some("conversation-1"), "turn-new", 0)
                .map_err(|error| error.to_string())?
                .events
                .len(),
            1
        );
        Ok(())
    }

    #[test]
    fn deletion_holds_shared_lifecycle_barrier_against_reopen() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("events");
        let pause = Arc::new((std::sync::Barrier::new(2), std::sync::Barrier::new(2)));
        let deleting = Arc::new(
            ChatEventLog::open(&root, ChatEventRetention::default())
                .map_err(|error| error.to_string())?
                .with_deletion_pause(Arc::clone(&pause)),
        );
        let other = Arc::new(
            ChatEventLog::open(&root, ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        );
        deleting
            .append(
                "workspace-1",
                Some("conversation-1"),
                "turn-1",
                ChatDriverEvent::TurnStatus {
                    status: "completed".to_string(),
                },
            )
            .map_err(|error| error.to_string())?;
        let deletion_log = Arc::clone(&deleting);
        let deletion = std::thread::spawn(move || {
            deletion_log
                .remove_conversation("workspace-1", "conversation-1")
                .map_err(|error| error.to_string())
        });
        pause.0.wait();
        let reopen_log = Arc::clone(&other);
        let (tx, rx) = std::sync::mpsc::channel();
        let reopen = std::thread::spawn(move || {
            let result = reopen_log
                .append(
                    "workspace-1",
                    Some("conversation-1"),
                    "turn-race",
                    ChatDriverEvent::TurnStatus {
                        status: "running".to_string(),
                    },
                )
                .map(|_| ())
                .map_err(|error| error.to_string());
            let _ = tx.send(result);
        });
        assert!(matches!(
            rx.recv_timeout(std::time::Duration::from_millis(100)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        pause.1.wait();
        deletion
            .join()
            .map_err(|_| "deletion thread failed".to_string())??;
        let raced = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .map_err(|error| error.to_string())?;
        reopen
            .join()
            .map_err(|_| "reopen thread failed".to_string())?;
        if raced.is_err() {
            other
                .append(
                    "workspace-1",
                    Some("conversation-1"),
                    "turn-after-delete",
                    ChatDriverEvent::TurnStatus {
                        status: "completed".to_string(),
                    },
                )
                .map_err(|error| error.to_string())?;
        }
        assert_eq!(
            other
                .replay(
                    "workspace-1",
                    Some("conversation-1"),
                    "turn-after-delete",
                    0
                )
                .map_err(|error| error.to_string())?
                .events
                .len(),
            1
        );
        Ok(())
    }

    #[test]
    fn direct_conversation_deletion_ignores_unrelated_corrupt_stream() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("events");
        let log = ChatEventLog::open(&root, ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        append_status(&log, "completed")?;
        let unrelated = root.join("sha256_unrelated_corrupt_stream");
        fs::create_dir_all(&unrelated).map_err(|error| error.to_string())?;
        fs::write(
            unrelated.join("00000000000000000001.jsonl"),
            b"not a framework journal record\n",
        )
        .map_err(|error| error.to_string())?;
        log.remove_conversation("workspace-1", "conversation-1")
            .map_err(|error| error.to_string())?;
        assert!(unrelated.exists());
        Ok(())
    }

    #[test]
    fn swapped_real_stream_directories_fail_selected_identity_validation() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("events");
        let log = ChatEventLog::open(&root, ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        for conversation in ["conversation-a", "conversation-b"] {
            log.append(
                "workspace-1",
                Some(conversation),
                "turn-1",
                ChatDriverEvent::TurnStatus {
                    status: "completed".to_string(),
                },
            )
            .map_err(|error| error.to_string())?;
        }
        let a = log.stream_dir(
            &stream_id("workspace-1", Some("conversation-a"), "turn-1")
                .map_err(|error| error.to_string())?,
        );
        let b = log.stream_dir(
            &stream_id("workspace-1", Some("conversation-b"), "turn-1")
                .map_err(|error| error.to_string())?,
        );
        let swap = root.join("swap");
        fs::rename(&a, &swap).map_err(|error| error.to_string())?;
        fs::rename(&b, &a).map_err(|error| error.to_string())?;
        fs::rename(&swap, &b).map_err(|error| error.to_string())?;

        assert!(matches!(
            log.append(
                "workspace-1",
                Some("conversation-a"),
                "turn-2",
                ChatDriverEvent::TurnStatus {
                    status: "running".to_string(),
                },
            ),
            Err(ChatEventLogError::Corrupt { .. })
        ));
        Ok(())
    }

    #[test]
    fn two_handle_lru_bounds_strong_caches_and_recovers_evicted_pins() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("events");
        let first = ChatEventLog::open(&root, ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        let second = ChatEventLog::open(&root, ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        for index in 1..=(MAX_REGISTRY_ENTRIES_BEFORE_PRUNE + 16) {
            let conversation = format!("conversation-{index}");
            first
                .append(
                    "workspace-lru",
                    Some(&conversation),
                    "turn",
                    ChatDriverEvent::TurnStatus {
                        status: "completed".to_string(),
                    },
                )
                .map_err(|error| error.to_string())?;
            second
                .replay("workspace-lru", Some(&conversation), "turn", 0)
                .map_err(|error| error.to_string())?;
        }
        assert!(first.streams.len() <= MAX_CACHED_STREAMS);
        assert!(second.streams.len() <= MAX_CACHED_STREAMS);
        let canonical_root = fs::canonicalize(&root).map_err(|error| error.to_string())?;
        let registered_for_root = stream_authority_registry()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .keys()
            .filter(|path| path.starts_with(&canonical_root))
            .count();
        assert!(registered_for_root <= MAX_REGISTRY_ENTRIES_BEFORE_PRUNE + 1);
        Ok(())
    }

    #[test]
    fn pending_barrier_debt_is_not_evicted_under_cache_pressure() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = ChatEventLog::open(temp.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        append_status(&log, "completed")?;
        let protected = stream_id("workspace-1", Some("conversation-1"), "turn-1")
            .map_err(|error| error.to_string())?;
        let cached = log
            .stream_journal(&protected, false)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "protected stream missing".to_string())?;
        {
            let mut guard = lock_cached_stream(&cached);
            guard
                .as_mut()
                .ok_or_else(|| "protected authority missing".to_string())?
                .barrier_pending = true;
        }
        drop(cached);
        for index in 0..=(MAX_CACHED_STREAMS + 8) {
            let conversation = format!("pressure-{index}");
            log.append(
                "workspace-1",
                Some(&conversation),
                "turn",
                ChatDriverEvent::TurnStatus {
                    status: "completed".to_string(),
                },
            )
            .map_err(|error| error.to_string())?;
        }
        assert!(log.streams.contains_key(&protected));
        let cached = log
            .streams
            .get(&protected)
            .map(|entry| Arc::clone(entry.value()))
            .ok_or_else(|| "protected cache entry missing".to_string())?;
        lock_cached_stream(&cached)
            .as_mut()
            .ok_or_else(|| "protected authority missing".to_string())?
            .barrier_pending = false;
        drop(cached);
        for index in 0..=(MAX_CACHED_STREAMS + 8) {
            let conversation = format!("confirmed-pressure-{index}");
            log.append(
                "workspace-1",
                Some(&conversation),
                "turn",
                ChatDriverEvent::TurnStatus {
                    status: "completed".to_string(),
                },
            )
            .map_err(|error| error.to_string())?;
        }
        assert!(log.streams.len() <= MAX_CACHED_STREAMS);
        assert!(!log.streams.contains_key(&protected));
        Ok(())
    }

    #[test]
    fn concurrent_first_open_across_two_handles_assigns_one_exact_sequence() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("events");
        let first = Arc::new(
            ChatEventLog::open(&root, ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        );
        let second = Arc::new(
            ChatEventLog::open(&root, ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        );
        let start = Arc::new(std::sync::Barrier::new(33));
        let mut handles = Vec::new();
        for index in 0..32 {
            let log = if index % 2 == 0 {
                Arc::clone(&first)
            } else {
                Arc::clone(&second)
            };
            let start = Arc::clone(&start);
            handles.push(std::thread::spawn(move || {
                start.wait();
                let mut result = command_cell_watch();
                result.receipt.execution_id = format!("command_cell_watch-{index}");
                result.receipt.cell_id = format!("cell-{index}");
                result.cell.cell_id = format!("cell-{index}");
                log.append(
                    "workspace-1",
                    Some("conversation-1"),
                    &format!("root-{index}"),
                    ChatDriverEvent::CommandCellWatchReady {
                        result: Box::new(result),
                    },
                )
                .map_err(|error| error.to_string())
            }));
        }
        start.wait();
        let mut envelopes = Vec::new();
        for handle in handles {
            envelopes.push(
                handle
                    .join()
                    .map_err(|_| "concurrent append thread failed".to_string())??,
            );
        }
        let mut sequences = envelopes
            .iter()
            .map(|envelope| envelope.sequence)
            .collect::<Vec<_>>();
        sequences.sort_unstable();
        assert_eq!(sequences, (1_u64..=32).collect::<Vec<_>>());
        assert_eq!(
            envelopes
                .iter()
                .map(|envelope| envelope.event_id.as_str())
                .collect::<HashSet<_>>()
                .len(),
            32
        );
        assert_eq!(
            second
                .pending_command_cell_watches("workspace-1", "conversation-1", "ignored")
                .map_err(|error| error.to_string())?
                .len(),
            32
        );
        Ok(())
    }
}
