#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn historical_hitl_without_identity_keeps_payload_hash_through_real_journal_reopen()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let payloads = [
            serde_json::json!({"source":"approval_request","event":{"request_id":"reusable","tool_name":"read_file","args":{"path":"中文"},"prompt":"approve"}}),
            serde_json::json!({"source":"input_request","event":{"request_id":"reusable","prompt":"中文\n🙂"}}),
            serde_json::json!({"source":"selection_request","event":{"request_id":"reusable","prompt":"Choose","options":["继续"],"task_id":null,"context":null,"phase":null}}),
        ];
        let mut hashes = Vec::new();
        {
            let log = ChatEventLog::open(directory.path(), ChatEventRetention::default())?;
            for raw in &payloads {
                let event: ChatDriverEvent = serde_json::from_value(raw.clone())?;
                assert_eq!(serde_json::to_value(&event)?, *raw);
                hashes.push(log.append("w", Some("c"), "root", event)?.content_hash);
            }
        }
        let reopened = ChatEventLog::open(directory.path(), ChatEventRetention::default())?;
        let replay = reopened.replay("w", Some("c"), "root", 0)?;
        assert_eq!(replay.events.len(), payloads.len());
        for ((event, raw), hash) in replay.events.iter().zip(&payloads).zip(hashes) {
            assert_eq!(serde_json::to_value(&event.payload)?, *raw);
            assert_eq!(event.content_hash, hash);
        }
        Ok(())
    }
    #[test]
    fn paired_hitl_uses_current_continuation_turn_in_original_journal() -> anyhow::Result<()> {
        use crate::hitl::{HitlRequestIdentity, HitlRequestKind, HitlRequestScope};
        let directory = tempfile::tempdir()?;
        let identity = HitlRequestIdentity {
            request_id: "request".to_string(),
            response_target: "target".to_string(),
            kind: HitlRequestKind::Input,
            scope: HitlRequestScope {
                workspace_id: Some("w".to_string()),
                host_generation: Some("host".to_string()),
                conversation_id: Some("c".to_string()),
                root_turn_id: Some("root".to_string()),
                turn_id: Some("continuation".to_string()),
                ..Default::default()
            },
        };
        let hashes = {
            let log = ChatEventLog::open(directory.path(), ChatEventRetention::default())?;
            let request = ChatDriverEvent::InputRequest {
                request_id: "request".to_string(),
                identity: Some(identity.clone()),
                prompt: "中文\n🙂".to_string(),
            };
            let mut malformed = request.clone();
            if let ChatDriverEvent::InputRequest {
                identity: Some(identity),
                ..
            } = &mut malformed
            {
                identity.kind = HitlRequestKind::Approval;
            }
            assert!(log.append("w", Some("c"), "root", malformed).is_err());
            let mut malformed = request.clone();
            if let ChatDriverEvent::InputRequest { request_id, .. } = &mut malformed {
                *request_id = "foreign-id".to_string();
            }
            assert!(log.append("w", Some("c"), "root", malformed).is_err());
            assert!(
                log.append("foreign-workspace", Some("c"), "root", request.clone())
                    .is_err()
            );
            assert!(
                log.append("w", Some("foreign-conversation"), "root", request.clone())
                    .is_err()
            );
            assert!(
                log.append("w", Some("c"), "foreign-root", request.clone())
                    .is_err()
            );
            [
                log.append("w", Some("c"), "root", request)?.content_hash,
                log.append(
                    "w",
                    Some("c"),
                    "root",
                    ChatDriverEvent::HitlRequestReleased { identity },
                )?
                .content_hash,
            ]
        };
        let log = ChatEventLog::open(directory.path(), ChatEventRetention::default())?;
        let replay = log.replay("w", Some("c"), "root", 0)?;
        assert_eq!(replay.events.len(), 2);
        for (event, hash) in replay.events.iter().zip(hashes) {
            assert_eq!(event.turn_id, "continuation");
            assert_eq!(event.message_id, "root");
            assert_eq!(event.content_hash, hash);
        }
        Ok(())
    }

    use crate::tasks::task_runtime::command_cells::{
        CommandCellWatchAcknowledgement, CommandCellWatchReceipt, CommandCellWatchResult,
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
        tool_updates: Mutex<Vec<String>>,
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

        fn on_tool_execution_projection(
            &self,
            update: &crate::tool_execution_projection::ToolExecutionProjectionUpdate,
        ) -> bool {
            self.tool_updates
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(update.summary.id.clone());
            true
        }
    }

    #[derive(Default)]
    struct RejectingJournaledSink {
        journaled_attempts: std::sync::atomic::AtomicUsize,
    }

    impl crate::chat_driver::ChatSink for RejectingJournaledSink {
        fn on_event(&self, _event: ChatDriverEvent) -> bool {
            false
        }

        fn on_journaled_event(&self, _envelope: ChatEventEnvelope) -> bool {
            self.journaled_attempts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            false
        }
    }

    fn tool_projection_update(
        id: &str,
    ) -> crate::tool_execution_projection::ToolExecutionProjectionUpdate {
        crate::tool_execution_projection::ToolExecutionProjectionUpdate {
            kind: crate::tool_execution_projection::ToolExecutionProjectionKind::Started,
            agent: "explorer".to_string(),
            summary: crate::tool_execution::ToolExecutionSummary {
                id: id.to_string(),
                call_id: "call-1".to_string(),
                owner: crate::tool_execution::ToolExecutionOwner::Subagent {
                    subagent_run_id: "subagent-1".to_string(),
                },
                workspace_id: "workspace-1".to_string(),
                conversation_id: Some("conversation-1".to_string()),
                run_id: Some("run-1".to_string()),
                name: "shell".to_string(),
                args_preview: "command=test".to_string(),
                status: crate::tool_execution::ToolExecutionStatus::Running,
                started_at: 1,
                finished_at: None,
                duration_ms: None,
                detail_ref: "tool-execution://tool-1".to_string(),
            },
        }
    }

    fn native_progress_fixture(text: impl Into<String>, sequence: u64) -> AgentEvent {
        AgentEvent::AssistantMessage {
            block: echo_agent::agent::AssistantMessageBlock {
                provider_source: None,
                block_id: format!("eko-native-fixture-{sequence}"),
                request_id: "eko-native-fixture-request".to_string(),
                sequence,
                phase: echo_agent::agent::AssistantPhase::Commentary,
                delta: text.into(),
                complete: false,
                provider: Some("eko-native-test-fixture".to_string()),
                reasoning: None,
            },
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
            native_progress_fixture(text, sequence),
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
    fn command_cell_watch_ready_requires_exact_stream_and_settled_generation() -> Result<(), String>
    {
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
            matches!(&first.payload, ChatDriverEvent::Agent(agent) if matches!(&agent.payload, AgentEvent::AssistantMessage { block } if block.delta == "你好"))
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
    fn unacknowledged_command_cell_watch_pins_then_acknowledgement_converges() -> Result<(), String>
    {
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
        let surfaces = [
            ChatSurface::Gui,
            ChatSurface::Tui,
            ChatSurface::Cli,
            ChatSurface::Channel,
            ChatSurface::Agent,
        ];
        for (offset, surface) in surfaces.into_iter().enumerate() {
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
            surfaces.len().saturating_mul(3)
        );
        assert_eq!(
            tools
                .summaries_for_conversation("workspace-1", "conversation-1")
                .len(),
            surfaces.len()
        );
        Ok(())
    }

    #[test]
    fn projected_live_event_routes_once_to_each_exact_surface_turn() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = Arc::new(
            ChatEventLog::open(temp.path().join("events"), ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        );
        let tools = Arc::new(
            ToolExecutionRepository::open(temp.path().join("tools"))
                .map_err(|error| error.to_string())?,
        );
        let gui = Arc::new(CapturingSink::default());
        let tui = Arc::new(CapturingSink::default());
        let other_turn = Arc::new(CapturingSink::default());
        let gui_bound = bind_surface_chat_sink(
            ChatSurface::Gui,
            gui.clone(),
            log.clone(),
            tools.clone(),
            "workspace-1",
            Some("conversation-1".to_string()),
            "turn-1",
        );
        let tui_bound = bind_surface_chat_sink(
            ChatSurface::Tui,
            tui.clone(),
            log.clone(),
            tools.clone(),
            "workspace-1",
            Some("conversation-1".to_string()),
            "turn-1",
        );
        let other_bound = bind_surface_chat_sink(
            ChatSurface::Cli,
            other_turn.clone(),
            log.clone(),
            tools,
            "workspace-1",
            Some("conversation-1".to_string()),
            "turn-2",
        );
        let envelope = log
            .append(
                "workspace-1",
                Some("conversation-1"),
                "turn-1",
                ChatDriverEvent::TurnStatus {
                    status: "running".to_string(),
                },
            )
            .map_err(|error| error.to_string())?;
        let updates = vec![tool_projection_update("tool-1")];

        assert!(log.deliver_projected_event(&envelope, &updates));
        assert!(log.deliver_projected_event(&envelope, &updates));
        for sink in [&gui, &tui] {
            assert_eq!(
                sink.journaled
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .len(),
                1
            );
            assert_eq!(
                sink.tool_updates
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .as_slice(),
                ["tool-1"]
            );
        }
        assert!(
            other_turn
                .journaled
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_empty()
        );
        drop((gui_bound, tui_bound, other_bound));
        Ok(())
    }

    #[test]
    fn dropped_live_sink_is_pruned_without_delivery() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = Arc::new(
            ChatEventLog::open(temp.path().join("events"), ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        );
        let tools = Arc::new(
            ToolExecutionRepository::open(temp.path().join("tools"))
                .map_err(|error| error.to_string())?,
        );
        {
            let sink = Arc::new(CapturingSink::default());
            let bound = bind_surface_chat_sink(
                ChatSurface::Gui,
                sink,
                log.clone(),
                tools,
                "workspace-1",
                Some("conversation-1".to_string()),
                "turn-1",
            );
            assert_eq!(log.live_sink_count(), 1);
            drop(bound);
        }
        assert_eq!(log.live_sink_count(), 0);
        Ok(())
    }

    #[test]
    fn failed_live_callback_does_not_rollback_committed_event() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = Arc::new(
            ChatEventLog::open(temp.path().join("events"), ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        );
        let tools = Arc::new(
            ToolExecutionRepository::open(temp.path().join("tools"))
                .map_err(|error| error.to_string())?,
        );
        let rejecting = Arc::new(RejectingJournaledSink::default());
        let accepting = Arc::new(CapturingSink::default());
        let rejecting_bound = bind_surface_chat_sink(
            ChatSurface::Gui,
            rejecting.clone(),
            log.clone(),
            tools.clone(),
            "workspace-1",
            Some("conversation-1".to_string()),
            "turn-1",
        );
        let accepting_bound = bind_surface_chat_sink(
            ChatSurface::Tui,
            accepting.clone(),
            log.clone(),
            tools,
            "workspace-1",
            Some("conversation-1".to_string()),
            "turn-1",
        );
        let envelope = log
            .append(
                "workspace-1",
                Some("conversation-1"),
                "turn-1",
                ChatDriverEvent::TurnStatus {
                    status: "running".to_string(),
                },
            )
            .map_err(|error| error.to_string())?;

        assert!(!log.deliver_projected_event(&envelope, &[]));
        assert_eq!(
            rejecting
                .journaled_attempts
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert_eq!(
            accepting
                .journaled
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .len(),
            1
        );
        assert_eq!(log.live_sink_count(), 1);
        assert_eq!(
            log.replay("workspace-1", Some("conversation-1"), "turn-1", 0)
                .map_err(|error| error.to_string())?
                .events
                .len(),
            1
        );
        drop((rejecting_bound, accepting_bound));
        Ok(())
    }

    #[test]
    fn journaled_sink_does_not_rebroadcast_its_own_event() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = Arc::new(
            ChatEventLog::open(temp.path().join("events"), ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        );
        let tools = Arc::new(
            ToolExecutionRepository::open(temp.path().join("tools"))
                .map_err(|error| error.to_string())?,
        );
        let producer = Arc::new(CapturingSink::default());
        let observer = Arc::new(CapturingSink::default());
        let producer_bound = bind_surface_chat_sink(
            ChatSurface::Gui,
            producer.clone(),
            log.clone(),
            tools.clone(),
            "workspace-1",
            Some("conversation-1".to_string()),
            "turn-1",
        );
        let observer_bound = bind_surface_chat_sink(
            ChatSurface::Tui,
            observer.clone(),
            log.clone(),
            tools,
            "workspace-1",
            Some("conversation-1".to_string()),
            "turn-1",
        );

        assert!(producer_bound.on_event(ChatDriverEvent::TurnStatus {
            status: "running".to_string(),
        }));
        assert_eq!(
            producer
                .journaled
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .len(),
            1
        );
        assert!(
            observer
                .journaled
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_empty()
        );
        assert_eq!(
            log.replay("workspace-1", Some("conversation-1"), "turn-1", 0)
                .map_err(|error| error.to_string())?
                .events
                .len(),
            1
        );
        drop(observer_bound);
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

    #[tokio::test]
    async fn command_cell_watch_recovery_ignores_unrelated_corrupt_stream() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("events");
        let log = Arc::new(
            ChatEventLog::open(&root, ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        );
        let result = command_cell_watch();
        let acknowledgement = CommandCellWatchAcknowledgement {
            execution_id: result.receipt.execution_id.clone(),
            watch_generation: result.receipt.watch_generation,
            cell_id: result.receipt.cell_id.clone(),
            workspace_id: result.receipt.workspace_id.clone(),
            conversation_id: result.receipt.conversation_id.clone(),
            root_turn_id: result.receipt.root_turn_id.clone(),
            acknowledged_turn_id: "recovery-turn".to_string(),
            outcome: crate::tasks::task_runtime::command_cells::CommandCellWatchDeliveryOutcome::OutcomeUnknown,
        };
        for event in [
            ChatDriverEvent::CommandCellWatchReady {
                result: Box::new(result),
            },
            ChatDriverEvent::CommandCellWatchDeliveryStarted {
                acknowledgement: acknowledgement.clone(),
            },
        ] {
            log.append("workspace-1", Some("conversation-1"), "turn-1", event)
                .map_err(|error| error.to_string())?;
        }
        let corrupt = root.join("sha256_unrelated_corrupt_stream");
        fs::create_dir_all(&corrupt).map_err(|error| error.to_string())?;
        fs::write(
            corrupt.join("00000000000000000001.jsonl"),
            b"not a framework journal record\n",
        )
        .map_err(|error| error.to_string())?;

        assert_eq!(
            log.settle_all_started_command_cell_watch_deliveries_async()
                .await
                .map_err(|error| error.to_string())?,
            1
        );
        let replay = log
            .replay("workspace-1", Some("conversation-1"), "turn-1", 0)
            .map_err(|error| error.to_string())?;
        assert!(replay.events.iter().any(|event| matches!(
            &event.payload,
            ChatDriverEvent::CommandCellWatchAcknowledged { acknowledgement: actual }
                if actual == &acknowledgement
        )));
        Ok(())
    }

    #[test]
    fn conversation_watch_scan_ignores_unrelated_corrupt_stream() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("events");
        let log = ChatEventLog::open(&root, ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        let expected = command_cell_watch();
        log.append(
            "workspace-1",
            Some("conversation-1"),
            "turn-1",
            ChatDriverEvent::CommandCellWatchReady {
                result: Box::new(expected.clone()),
            },
        )
        .map_err(|error| error.to_string())?;
        let corrupt = root.join("sha256_unrelated_corrupt_stream");
        fs::create_dir_all(&corrupt).map_err(|error| error.to_string())?;
        fs::write(
            corrupt.join("00000000000000000001.jsonl"),
            b"not a framework journal record\n",
        )
        .map_err(|error| error.to_string())?;

        assert_eq!(
            log.pending_command_cell_watches_for_conversation("workspace-1", "conversation-1")
                .map_err(|error| error.to_string())?,
            vec![expected]
        );
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
                    "turn-1",
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
                .pending_command_cell_watches("workspace-1", "conversation-1", "turn-1")
                .map_err(|error| error.to_string())?
                .len(),
            32
        );
        Ok(())
    }
    fn native_history_test_message() -> Result<(echo_agent::llm::types::Message, AgentEvent), String>
    {
        use echo_agent::agent::{AssistantMessageBlock, AssistantPhase};
        let block = AssistantMessageBlock {
            provider_source: None,
            block_id: "history-block".into(),
            request_id: "history-request".into(),
            sequence: 0,
            phase: AssistantPhase::Commentary,
            delta: "原批准进度🙂".into(),
            complete: true,
            provider: None,
            reasoning: None,
        };
        let mut message = echo_agent::llm::types::Message::assistant(block.delta.clone());
        message.assistant_blocks = Some(vec![block.clone()]);
        message.approved_assistant_blocks = Some(vec![block.clone()]);
        Ok((message, AgentEvent::AssistantMessage { block }))
    }

    fn append_native_history_test_event(
        log: &ChatEventLog,
        sequence: u64,
        event: AgentEvent,
    ) -> Result<ChatEventEnvelope, String> {
        let identity = EventIdentity::new("history-stream", "turn-1")
            .map_err(|error| error.to_string())?
            .with_conversation_id("conversation-1")
            .map_err(|error| error.to_string())?
            .with_message_id("turn-1")
            .map_err(|error| error.to_string())?
            .with_execution_id("history-execution")
            .map_err(|error| error.to_string())?;
        let event = EventEnvelope::new(&identity, sequence, None, event)
            .map_err(|error| error.to_string())?;
        log.append(
            "workspace-1",
            Some("conversation-1"),
            "turn-1",
            ChatDriverEvent::Agent(Box::new(event)),
        )
        .map_err(|error| error.to_string())
    }

    #[test]
    fn native_history_no_prune_safe_point_defers_projection_but_preserves_original_journal()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = ChatEventLog::open(temp.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        let (message, event) = native_history_test_message()?;
        let original = append_native_history_test_event(&log, 1, event)?;
        #[cfg(feature = "test-utils")]
        let observer = native_history_perf_io::Observation::begin(temp.path().to_path_buf())?;
        let safe_point = log
            .append(
                "workspace-1",
                Some("conversation-1"),
                "turn-1",
                ChatDriverEvent::TurnStatus {
                    status: "completed".into(),
                },
            )
            .map_err(|error| error.to_string())?;
        #[cfg(feature = "test-utils")]
        {
            let counts = observer.finish()?;
            eprintln!(
                "native-history no-prune I/O attempts/results: {}",
                serde_json::to_string(&counts).map_err(|error| error.to_string())?
            );
            assert!(
                counts
                    .get("coverage_save")
                    .is_none_or(|count| count.attempted == 0),
                "无retention工作时不得为safe point保存派生coverage"
            );
        }
        let stream = stream_id("workspace-1", Some("conversation-1"), "turn-1")
            .map_err(|error| error.to_string())?;
        assert!(
            !log.stream_dir(&stream)
                .join(NATIVE_HISTORY_DIRECTORY)
                .exists(),
            "未裁剪的原journal不依赖append后立即生成派生目录"
        );
        let original_records = vec![original.clone(), safe_point.clone()];
        let live = log
            .replay("workspace-1", Some("conversation-1"), "turn-1", 0)
            .map_err(|error| error.to_string())?;
        assert_eq!(live.retained_earliest_cursor, Some(1));
        assert_eq!(live.latest_cursor, safe_point.sequence);
        assert_eq!(
            serde_json::to_value(&live.events).map_err(|error| error.to_string())?,
            serde_json::to_value(&original_records).map_err(|error| error.to_string())?
        );
        drop(log);
        let cold = ChatEventLog::open(temp.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        let cold_records = cold
            .replay("workspace-1", Some("conversation-1"), "turn-1", 0)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            serde_json::to_value(&cold_records.events).map_err(|error| error.to_string())?,
            serde_json::to_value(&original_records).map_err(|error| error.to_string())?
        );
        let native = cold
            .native_history_publications("workspace-1", "conversation-1", &message)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            serde_json::to_value(&native).map_err(|error| error.to_string())?,
            serde_json::to_value(vec![original]).map_err(|error| error.to_string())?
        );
        Ok(())
    }

    #[test]
    fn native_history_checkpoint_preserves_exact_records_after_real_prune_and_cold_reopen()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let retention = ChatEventRetention {
            segment_rollover_bytes: 1,
            max_segments: 2,
            max_replay_events: 4,
        };
        let (message, event) = native_history_test_message()?;
        let log = ChatEventLog::open(temp.path(), retention).map_err(|error| error.to_string())?;
        let original = append_native_history_test_event(&log, 1, event)?;
        for _ in 0..12 {
            append_status(&log, "completed")?;
        }
        let replay = log
            .replay("workspace-1", Some("conversation-1"), "turn-1", 0)
            .map_err(|error| error.to_string())?;
        assert!(
            replay
                .retained_earliest_cursor
                .is_some_and(|floor| floor > original.sequence)
        );
        assert!(
            segment_count(&log)? <= retention.max_segments,
            "不能永久pin原native日志段"
        );
        drop(log);
        let cold = ChatEventLog::open(temp.path(), retention).map_err(|error| error.to_string())?;
        let events = cold
            .native_history_publications("workspace-1", "conversation-1", &message)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            serde_json::to_value(&events).map_err(|error| error.to_string())?,
            serde_json::to_value(vec![original]).map_err(|error| error.to_string())?
        );
        let view = crate::conversation_projection::project_native_history_publication(
            &message,
            "workspace-1",
            "conversation-1",
            &events,
        )?
        .ok_or_else(|| "native view missing".to_string())?;
        assert_eq!(view.blocks.len(), 1);
        assert!(!view.final_answer_confirmed);
        Ok(())
    }

    #[test]
    fn native_history_recovery_advances_one_bounded_batch_without_tail_skipping()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = ChatEventLog::open(temp.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        let (message, event) = native_history_test_message()?;
        let original = append_native_history_test_event(&log, 1, event)?;
        for _ in 0..NATIVE_HISTORY_PROJECTION_BATCH {
            log.append(
                "workspace-1",
                Some("conversation-1"),
                "turn-1",
                ChatDriverEvent::TurnStatus {
                    status: "running".into(),
                },
            )
            .map_err(|error| error.to_string())?;
        }
        let first = log.native_history_publications("workspace-1", "conversation-1", &message);
        assert!(
            matches!(first, Err(ChatEventLogError::NativeHistoryPending { through_sequence, latest_sequence })
            if through_sequence == u64::try_from(NATIVE_HISTORY_PROJECTION_BATCH).unwrap_or(u64::MAX)
                && latest_sequence == through_sequence.saturating_add(1))
        );
        let events = log
            .native_history_publications("workspace-1", "conversation-1", &message)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            events.first().map(|event| &event.event_id),
            Some(&original.event_id)
        );
        Ok(())
    }

    #[test]
    fn native_history_failed_checkpoint_keeps_original_segments_and_committed_append()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let retention = ChatEventRetention {
            segment_rollover_bytes: 1,
            max_segments: 1,
            max_replay_events: 64,
        };
        let log = ChatEventLog::open(temp.path(), retention).map_err(|error| error.to_string())?;
        let (message, event) = native_history_test_message()?;
        let original = append_native_history_test_event(&log, 1, event)?;
        let stream = stream_id("workspace-1", Some("conversation-1"), "turn-1")
            .map_err(|error| error.to_string())?;
        let checkpoint = log
            .stream_dir(&stream)
            .join(NATIVE_HISTORY_DIRECTORY)
            .join("coverage.json");
        // 原FileCheckpointStore真实读失败：不是注入假成功，不删除原journal event。
        fs::create_dir_all(
            checkpoint
                .parent()
                .ok_or_else(|| "coverage parent missing".to_string())?,
        )
        .map_err(|error| error.to_string())?;
        fs::write(&checkpoint, b"{broken-checkpoint").map_err(|error| error.to_string())?;
        for _ in 0..4 {
            append_status(&log, "completed")?;
        }
        let replay = log
            .replay("workspace-1", Some("conversation-1"), "turn-1", 0)
            .map_err(|error| error.to_string())?;
        assert_eq!(replay.retained_earliest_cursor, Some(1));
        assert!(
            replay
                .events
                .iter()
                .any(|event| event.event_id == original.event_id)
        );
        assert!(segment_count(&log)? > retention.max_segments);
        assert!(
            log.native_history_publications("workspace-1", "conversation-1", &message)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn native_history_foreign_generation_or_corrupt_anchor_never_authorizes_raw_fallback()
    -> Result<(), String> {
        let first = tempfile::tempdir().map_err(|error| error.to_string())?;
        let second = tempfile::tempdir().map_err(|error| error.to_string())?;
        let (message, event) = native_history_test_message()?;
        let logs = [
            ChatEventLog::open(first.path(), ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
            ChatEventLog::open(second.path(), ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        ];
        for log in &logs {
            append_native_history_test_event(log, 1, event.clone())?;
            append_status(log, "completed")?;
            // 注入foreign/corrupt之前，由原reader物化各自generation的派生文件。
            log.native_history_publications("workspace-1", "conversation-1", &message)
                .map_err(|error| error.to_string())?;
        }
        let key = native_publication_fact_key(
            &logs
                .first()
                .ok_or_else(|| "first log missing".to_string())?
                .replay("workspace-1", Some("conversation-1"), "turn-1", 0)
                .map_err(|error| error.to_string())?
                .events
                .into_iter()
                .next()
                .ok_or_else(|| "original event missing".to_string())?,
        )
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "block key missing".to_string())?;
        let stream = stream_id("workspace-1", Some("conversation-1"), "turn-1")
            .map_err(|error| error.to_string())?;
        let left = logs
            .first()
            .ok_or_else(|| "first log missing".to_string())?
            .stream_dir(&stream)
            .join(NATIVE_HISTORY_DIRECTORY)
            .join(format!("{key}.json"));
        let right_log = logs
            .get(1)
            .ok_or_else(|| "second log missing".to_string())?;
        let right = right_log
            .stream_dir(&stream)
            .join(NATIVE_HISTORY_DIRECTORY)
            .join(format!("{key}.json"));
        fs::copy(&left, &right).map_err(|error| error.to_string())?;
        assert!(
            right_log
                .native_history_publications("workspace-1", "conversation-1", &message)
                .is_err(),
            "同payload/scope也不能用另一journal generation的快照"
        );
        fs::write(&right, b"{corrupt").map_err(|error| error.to_string())?;
        assert!(
            right_log
                .native_history_publications("workspace-1", "conversation-1", &message)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn native_history_checkpoint_preserves_original_publication_ambiguity() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = ChatEventLog::open(temp.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        let (message, event) = native_history_test_message()?;
        append_native_history_test_event(&log, 1, event.clone())?;
        append_native_history_test_event(&log, 2, event)?;
        append_status(&log, "completed")?;
        let events = log
            .native_history_publications("workspace-1", "conversation-1", &message)
            .map_err(|error| error.to_string())?;
        assert_eq!(events.len(), 2, "不能去掉不同原ID的歧义见证");
        assert!(
            crate::conversation_projection::native_progress_publication_events(
                &message,
                "workspace-1",
                "conversation-1",
                &events
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn native_history_checkpoint_keeps_final_settled_and_later_product_failure_separate()
    -> Result<(), String> {
        use echo_agent::agent::{ExecutionStatus, ExecutionStatusKind};
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let retention = ChatEventRetention {
            segment_rollover_bytes: 1,
            max_segments: 2,
            max_replay_events: 4,
        };
        let log = ChatEventLog::open(temp.path(), retention).map_err(|error| error.to_string())?;
        let status = ExecutionStatus {
            adapter_generated: false,
            execution_id: "history-execution".into(),
            request_id: Some("history-request".into()),
            sequence: 4,
            phase: AgentEvent::FinalAnswer(String::new()).phase(),
            status: ExecutionStatusKind::Settled,
            reason: None,
        };
        let mut message = echo_agent::llm::types::Message::assistant("已发布正文🙂".into());
        let mut candidate = status.clone();
        candidate.status = ExecutionStatusKind::Candidate;
        message.execution_status = Some(candidate);
        let original_final = append_native_history_test_event(
            &log,
            1,
            AgentEvent::FinalAnswer("已发布正文🙂".into()),
        )?;
        let original_settled =
            append_native_history_test_event(&log, 2, AgentEvent::ExecutionStatus(status))?;
        append_status(&log, "completed")?;
        let before = log
            .native_history_publications("workspace-1", "conversation-1", &message)
            .map_err(|error| error.to_string())?;
        let view = crate::conversation_projection::project_native_history_publication(
            &message,
            "workspace-1",
            "conversation-1",
            &before,
        )?
        .ok_or_else(|| "view missing".to_string())?;
        assert!(view.final_answer_confirmed);
        let product_failed = log
            .append(
                "workspace-1",
                Some("conversation-1"),
                "turn-1",
                ChatDriverEvent::TurnStatus {
                    status: "failed".into(),
                },
            )
            .map_err(|error| error.to_string())?;
        for _ in 0..12 {
            log.append(
                "workspace-1",
                Some("conversation-1"),
                "another-root",
                ChatDriverEvent::TurnConfiguration {
                    permission_mode: "default".into(),
                    approval_policy: "on-request".into(),
                    attachments: Vec::new(),
                },
            )
            .map_err(|error| error.to_string())?;
        }
        assert!(
            log.replay("workspace-1", Some("conversation-1"), "turn-1", 0)
                .map_err(|error| error.to_string())?
                .retained_earliest_cursor
                .is_some_and(|floor| floor > product_failed.sequence)
        );
        drop(log);
        let cold = ChatEventLog::open(temp.path(), retention).map_err(|error| error.to_string())?;
        let facts = cold
            .native_history_publications("workspace-1", "conversation-1", &message)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            serde_json::to_value(&facts).map_err(|error| error.to_string())?,
            serde_json::to_value(vec![original_final, original_settled, product_failed])
                .map_err(|error| error.to_string())?
        );
        let view = crate::conversation_projection::project_native_history_publication(
            &message,
            "workspace-1",
            "conversation-1",
            &facts,
        )?
        .ok_or_else(|| "view missing".to_string())?;
        assert!(
            !view.final_answer_confirmed,
            "真实后继product失败不能被先前Settled/Candidate覆盖"
        );
        assert!(view.content.is_none());
        Ok(())
    }
    #[test]
    fn native_history_adoption_after_uncovered_prune_does_not_invent_unique_source()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let retention = ChatEventRetention {
            segment_rollover_bytes: 1,
            max_segments: 1,
            max_replay_events: 64,
        };
        let log = ChatEventLog::open(temp.path(), retention).map_err(|error| error.to_string())?;
        let (message, event) = native_history_test_message()?;
        append_native_history_test_event(&log, 1, event.clone())?;
        let surviving = append_native_history_test_event(&log, 2, event)?;
        let full = log
            .replay("workspace-1", Some("conversation-1"), "turn-1", 0)
            .map_err(|error| error.to_string())?;
        assert!(
            crate::conversation_projection::native_progress_publication_events(
                &message,
                "workspace-1",
                "conversation-1",
                &full.events
            )
            .is_err(),
            "完整原前缀明确有第二次不同publication，不能恢复成单一来源"
        );
        let stream = stream_id("workspace-1", Some("conversation-1"), "turn-1")
            .map_err(|error| error.to_string())?;
        let cached = log
            .stream_journal(&stream, false)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "stream missing".to_string())?;
        {
            let guard = lock_cached_stream(&cached);
            let authority = guard
                .as_ref()
                .ok_or_else(|| "authority missing".to_string())?;
            // 原旧产品未保存native coverage而直接prune的实际切点；不手工删除event文件。
            authority
                .journal
                .prune_closed_segments_before(surviving.sequence)
                .map_err(|error| error.to_string())?;
        }
        drop(cached);
        assert!(
            matches!(
                log.native_history_publications("workspace-1", "conversation-1", &message),
                Err(ChatEventLogError::NativeHistoryGap { .. })
            ),
            "suffix命中key也不能证明被裁prefix没有owner/歧义事实"
        );
        Ok(())
    }

    #[test]
    fn native_history_foreign_checkpoint_is_not_overwritten_by_later_append_maintenance()
    -> Result<(), String> {
        let left_dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let right_dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let left = ChatEventLog::open(left_dir.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        let right = ChatEventLog::open(right_dir.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        let (message, event) = native_history_test_message()?;
        let first = append_native_history_test_event(&left, 1, event.clone())?;
        append_status(&left, "completed")?;
        append_native_history_test_event(&right, 1, event.clone())?;
        append_status(&right, "completed")?;
        // 原reader仅准备合法基点；后续foreign字节与不覆盖/拒读反控保持原样。
        for log in [&left, &right] {
            log.native_history_publications("workspace-1", "conversation-1", &message)
                .map_err(|error| error.to_string())?;
        }
        let key = native_publication_fact_key(&first)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "native key missing".to_string())?;
        let stream = stream_id("workspace-1", Some("conversation-1"), "turn-1")
            .map_err(|error| error.to_string())?;
        let foreign_anchor = left
            .stream_dir(&stream)
            .join(NATIVE_HISTORY_DIRECTORY)
            .join(format!("{key}.json"));
        let original_anchor = right
            .stream_dir(&stream)
            .join(NATIVE_HISTORY_DIRECTORY)
            .join(format!("{key}.json"));
        let foreign_bytes = fs::read(&foreign_anchor).map_err(|error| error.to_string())?;
        fs::write(&original_anchor, &foreign_bytes).map_err(|error| error.to_string())?;
        append_native_history_test_event(&right, 2, event)?;
        append_status(&right, "completed")?;
        assert_eq!(
            fs::read(&original_anchor).map_err(|error| error.to_string())?,
            foreign_bytes,
            "maintenance不能把foreign anchor当None后静默覆写"
        );
        assert!(
            right
                .native_history_publications("workspace-1", "conversation-1", &message)
                .is_err()
        );
        let foreign_coverage = left
            .stream_dir(&stream)
            .join(NATIVE_HISTORY_DIRECTORY)
            .join("coverage.json");
        let original_coverage = right
            .stream_dir(&stream)
            .join(NATIVE_HISTORY_DIRECTORY)
            .join("coverage.json");
        let foreign_bytes = fs::read(foreign_coverage).map_err(|error| error.to_string())?;
        fs::write(&original_coverage, &foreign_bytes).map_err(|error| error.to_string())?;
        append_status(&right, "completed")?;
        assert_eq!(
            fs::read(&original_coverage).map_err(|error| error.to_string())?,
            foreign_bytes
        );
        assert!(
            right
                .native_history_publications("workspace-1", "conversation-1", &message)
                .is_err(),
            "foreign coverage不因后继safePoint被洗成同generation"
        );
        Ok(())
    }
    #[test]
    fn native_history_scoped_subset_preserves_full_journal_cross_scope_refusal()
    -> Result<(), String> {
        use echo_agent::agent::{ExecutionStatus, ExecutionStatusKind};
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = ChatEventLog::open(temp.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        let status = ExecutionStatus {
            adapter_generated: false,
            execution_id: "history-execution".into(),
            request_id: Some("history-request".into()),
            sequence: 4,
            phase: AgentEvent::FinalAnswer(String::new()).phase(),
            status: ExecutionStatusKind::Settled,
            reason: None,
        };
        let mut message = echo_agent::llm::types::Message::assistant("原正文".into());
        let mut candidate = status.clone();
        candidate.status = ExecutionStatusKind::Candidate;
        message.execution_status = Some(candidate);
        append_native_history_test_event(&log, 1, AgentEvent::FinalAnswer("原正文".into()))?;
        append_native_history_test_event(&log, 2, AgentEvent::ExecutionStatus(status))?;
        append_status(&log, "completed")?;
        let scope_b = EventIdentity::new("other-stream", "other-turn")
            .map_err(|error| error.to_string())?
            .with_conversation_id("conversation-1")
            .map_err(|error| error.to_string())?
            .with_message_id("other-turn")
            .map_err(|error| error.to_string())?
            .with_execution_id("history-execution")
            .map_err(|error| error.to_string())?;
        let conflicting = EventEnvelope::new(
            &scope_b,
            1,
            None,
            native_progress_fixture("另一消息scope的原事件", 1),
        )
        .map_err(|error| error.to_string())?;
        log.append(
            "workspace-1",
            Some("conversation-1"),
            "other-turn",
            ChatDriverEvent::Agent(Box::new(conflicting)),
        )
        .map_err(|error| error.to_string())?;
        let all = log
            .replay("workspace-1", Some("conversation-1"), "turn-1", 0)
            .map_err(|error| error.to_string())?
            .events;
        let full_view = crate::conversation_projection::project_native_history_publication(
            &message,
            "workspace-1",
            "conversation-1",
            &all,
        )?
        .ok_or_else(|| "full view missing".to_string())?;
        assert!(!full_view.final_answer_confirmed);
        let subset = log
            .native_history_publications("workspace-1", "conversation-1", &message)
            .map_err(|error| error.to_string())?;
        assert!(
            subset
                .iter()
                .any(|event| event.root_turn_id == "other-turn"),
            "有界子集不能抹掉原同execution跨消息scope的拒绝见证"
        );
        let subset_view = crate::conversation_projection::project_native_history_publication(
            &message,
            "workspace-1",
            "conversation-1",
            &subset,
        )?
        .ok_or_else(|| "subset view missing".to_string())?;
        assert!(!subset_view.final_answer_confirmed);
        assert!(subset_view.content.is_none());
        let key = native_publication_key("scope", &"history-execution")
            .map_err(|error| error.to_string())?;
        let stream = stream_id("workspace-1", Some("conversation-1"), "turn-1")
            .map_err(|error| error.to_string())?;
        // 只损坏此新temp投影，原journal不删；三条成功事实不能补掉丢失的scope拒绝见证。
        fs::remove_file(
            log.stream_dir(&stream)
                .join(NATIVE_HISTORY_DIRECTORY)
                .join(format!("{key}.json")),
        )
        .map_err(|error| error.to_string())?;
        assert!(matches!(
            log.native_history_publications("workspace-1", "conversation-1", &message),
            Err(ChatEventLogError::NativeHistoryGap { .. })
        ));
        Ok(())
    }
    #[test]
    fn native_history_complete_coverage_keeps_never_published_settlement_unconfirmed_after_prune()
    -> Result<(), String> {
        use echo_agent::agent::{ExecutionStatus, ExecutionStatusKind};
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let retention = ChatEventRetention {
            segment_rollover_bytes: 1,
            max_segments: 2,
            max_replay_events: 4,
        };
        let log = ChatEventLog::open(temp.path(), retention).map_err(|error| error.to_string())?;
        let mut message = echo_agent::llm::types::Message::assistant("尚未实际结算的原正文".into());
        message.execution_status = Some(ExecutionStatus {
            adapter_generated: false,
            execution_id: "history-execution".into(),
            request_id: Some("history-request".into()),
            sequence: 4,
            phase: AgentEvent::FinalAnswer(String::new()).phase(),
            status: ExecutionStatusKind::Candidate,
            reason: None,
        });
        let final_event = append_native_history_test_event(
            &log,
            1,
            AgentEvent::FinalAnswer("尚未实际结算的原正文".into()),
        )?;
        let complete_before = log
            .replay("workspace-1", Some("conversation-1"), "turn-1", 0)
            .map_err(|error| error.to_string())?
            .events;
        let before = crate::conversation_projection::project_native_history_publication(
            &message,
            "workspace-1",
            "conversation-1",
            &complete_before,
        )?
        .ok_or_else(|| "baseline view missing".to_string())?;
        assert!(!before.final_answer_confirmed && before.content.is_none());
        for _ in 0..12 {
            log.append(
                "workspace-1",
                Some("conversation-1"),
                "another-root",
                ChatDriverEvent::TurnConfiguration {
                    permission_mode: "default".into(),
                    approval_policy: "on-request".into(),
                    attachments: Vec::new(),
                },
            )
            .map_err(|error| error.to_string())?;
        }
        assert!(
            log.replay("workspace-1", Some("conversation-1"), "turn-1", 0)
                .map_err(|error| error.to_string())?
                .retained_earliest_cursor
                .is_some_and(|floor| floor > final_event.sequence)
        );
        drop(log);
        let cold = ChatEventLog::open(temp.path(), retention).map_err(|error| error.to_string())?;
        let original = cold
            .native_history_publications("workspace-1", "conversation-1", &message)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            serde_json::to_value(&original).map_err(|error| error.to_string())?,
            serde_json::to_value(vec![final_event]).map_err(|error| error.to_string())?
        );
        let after = crate::conversation_projection::project_native_history_publication(
            &message,
            "workspace-1",
            "conversation-1",
            &original,
        )?
        .ok_or_else(|| "restored view missing".to_string())?;
        assert_eq!(
            (
                after.final_answer_confirmed,
                after.content,
                after.unavailable
            ),
            (
                before.final_answer_confirmed,
                before.content,
                before.unavailable
            )
        );
        assert!(after.blocks.is_empty());
        Ok(())
    }
    #[cfg(feature = "test-utils")]
    #[tokio::test]
    #[ignore = "explicit private I/O diagnostic; not a latency sample"]
    async fn native_history_same_head_io_attempts_and_successes_diagnostic() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = Arc::new(
            ChatEventLog::open(temp.path(), ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        );
        let (message, event) = native_history_test_message()?;
        let original = append_native_history_test_event(&log, 1, event)?;
        let cancel = echo_agent::agent::CancellationToken::new();
        log.native_history_publications_async("workspace-1", "conversation-1", &message, &cancel)
            .await
            .map_err(|error| error.to_string())?;
        let observer = native_history_perf_io::Observation::begin(temp.path().to_path_buf())?;
        for _ in 0..2 {
            let events = log
                .native_history_publications_async(
                    "workspace-1",
                    "conversation-1",
                    &message,
                    &cancel,
                )
                .await
                .map_err(|error| error.to_string())?;
            assert_eq!(
                serde_json::to_value(events).map_err(|error| error.to_string())?,
                serde_json::to_value(vec![original.clone()]).map_err(|error| error.to_string())?
            );
        }
        let counts = observer.finish()?;
        for operation in [
            "journal_sync",
            "coverage_load",
            "coverage_save",
            "anchor_load",
        ] {
            let count = counts
                .get(operation)
                .ok_or("expected I/O observation missing")?;
            assert_eq!(count.attempted, 2);
            assert_eq!(count.succeeded, 2);
            assert_eq!(count.failed, 0);
        }
        assert!(
            counts
                .get("anchor_save")
                .is_none_or(|count| count.attempted == 0)
        );
        eprintln!(
            "native-history logical I/O attempts/results: {}",
            serde_json::to_string(&counts).map_err(|error| error.to_string())?
        );
        Ok(())
    }

    #[cfg(feature = "test-utils")]
    #[tokio::test]
    #[ignore = "explicit private failed-I/O diagnostic; not a successful performance sample"]
    async fn native_history_io_failed_load_is_not_counted_as_success() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = Arc::new(
            ChatEventLog::open(temp.path(), ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        );
        let (message, event) = native_history_test_message()?;
        append_native_history_test_event(&log, 1, event)?;
        let cancel = echo_agent::agent::CancellationToken::new();
        log.native_history_publications_async("workspace-1", "conversation-1", &message, &cancel)
            .await
            .map_err(|error| error.to_string())?;
        let stream = stream_id("workspace-1", Some("conversation-1"), "turn-1")
            .map_err(|error| error.to_string())?;
        let cached = log
            .stream_journal(&stream, false)
            .map_err(|error| error.to_string())?
            .ok_or("原stream缺失")?;
        let path = {
            let guard = lock_cached_stream(&cached);
            let authority = guard.as_ref().ok_or("原authority缺失")?;
            native_history_checkpoint::<NativePublicationCoverage>(authority, "coverage")
                .path()
                .to_path_buf()
        };
        // 仅破坏本fresh测试的派生文件；原journal与canonical事实不变。
        std::fs::write(path, b"{corrupt").map_err(|error| error.to_string())?;
        let observer = native_history_perf_io::Observation::begin(temp.path().to_path_buf())?;
        let result = log
            .native_history_publications_async("workspace-1", "conversation-1", &message, &cancel)
            .await;
        let counts = observer.finish()?;
        assert!(result.is_err());
        let load = counts.get("coverage_load").ok_or("缺失败load观测")?;
        assert_eq!(load.attempted, 1);
        assert_eq!(load.succeeded, 0);
        assert_eq!(load.failed, 1);
        assert!(
            counts
                .get("coverage_save")
                .is_none_or(|count| count.attempted == 0)
        );
        Ok(())
    }

    #[tokio::test]
    async fn native_history_cold_first_call_automatically_completes_4097_and_8193_records()
    -> Result<(), String> {
        for target in [4097_u64, 8193_u64] {
            let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
            let log = Arc::new(
                ChatEventLog::open(temp.path(), ChatEventRetention::default())
                    .map_err(|error| error.to_string())?,
            );
            let (message, event) = native_history_test_message()?;
            let original = append_native_history_test_event(&log, 1, event)?;
            for _ in 1..target {
                log.append(
                    "workspace-1",
                    Some("conversation-1"),
                    "turn-1",
                    ChatDriverEvent::TurnStatus {
                        status: "running".into(),
                    },
                )
                .map_err(|error| error.to_string())?;
            }
            let stream = stream_id("workspace-1", Some("conversation-1"), "turn-1")
                .map_err(|error| error.to_string())?;
            assert!(
                !log.stream_dir(&stream)
                    .join(NATIVE_HISTORY_DIRECTORY)
                    .exists(),
                "首次采用必须没有原投影，不借前序safePoint追平"
            );
            assert_eq!(
                log.replay("workspace-1", Some("conversation-1"), "turn-1", 0)
                    .map_err(|error| error.to_string())?
                    .retained_earliest_cursor,
                Some(1)
            );
            drop(log);
            let cold = Arc::new(
                ChatEventLog::open(temp.path(), ChatEventRetention::default())
                    .map_err(|error| error.to_string())?,
            );
            let cancel = echo_agent::agent::CancellationToken::new();
            // 一个正常调用，内部自动2/3批；调用者不捕获Pending或重试。
            let events = cold
                .native_history_publications_async(
                    "workspace-1",
                    "conversation-1",
                    &message,
                    &cancel,
                )
                .await
                .map_err(|error| error.to_string())?;
            assert_eq!(
                serde_json::to_value(events).map_err(|error| error.to_string())?,
                serde_json::to_value(vec![original]).map_err(|error| error.to_string())?
            );
            let cached = cold
                .stream_journal(&stream, false)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "cold stream missing".to_string())?;
            let guard = lock_cached_stream(&cached);
            let authority = guard
                .as_ref()
                .ok_or_else(|| "cold authority missing".to_string())?;
            let coverage =
                native_history_checkpoint::<NativePublicationCoverage>(authority, "coverage")
                    .load()
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| "coverage missing".to_string())?;
            assert_eq!(coverage.sequence, target);
            assert_eq!(coverage.state.first_observed_sequence, 1);
        }
        Ok(())
    }

    #[tokio::test]
    async fn native_history_cancellation_between_batches_keeps_original_progress_without_continuing()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = Arc::new(
            ChatEventLog::open(temp.path(), ChatEventRetention::default())
                .map_err(|error| error.to_string())?,
        );
        let (message, event) = native_history_test_message()?;
        append_native_history_test_event(&log, 1, event)?;
        for _ in 1..8193 {
            log.append(
                "workspace-1",
                Some("conversation-1"),
                "turn-1",
                ChatDriverEvent::TurnStatus {
                    status: "running".into(),
                },
            )
            .map_err(|error| error.to_string())?;
        }
        let barrier = Arc::new(NativeHistoryReadTestBarrier {
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        *log.native_history_read_test_barrier
            .lock()
            .map_err(|_| "test barrier poisoned".to_string())? = Some(barrier.clone());
        let cancel = echo_agent::agent::CancellationToken::new();
        let reader_log = log.clone();
        let reader_cancel = cancel.clone();
        let read = tokio::spawn(async move {
            reader_log
                .native_history_publications_async(
                    "workspace-1",
                    "conversation-1",
                    &message,
                    &reader_cancel,
                )
                .await
        });
        if tokio::time::timeout(
            std::time::Duration::from_secs(30),
            barrier.entered.notified(),
        )
        .await
        .is_err()
        {
            cancel.cancel();
            barrier.release.notify_one();
            let _ = read.await;
            return Err("测试未在30秒内到达首个真实批次边界".into());
        }
        cancel.cancel();
        barrier.release.notify_one();
        assert!(matches!(
            read.await.map_err(|error| error.to_string())?,
            Err(ChatEventLogError::NativeHistoryReadCancelled)
        ));
        let stream = stream_id("workspace-1", Some("conversation-1"), "turn-1")
            .map_err(|error| error.to_string())?;
        let cached = log
            .stream_journal(&stream, false)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "stream missing".to_string())?;
        let guard = lock_cached_stream(&cached);
        let authority = guard
            .as_ref()
            .ok_or_else(|| "authority missing".to_string())?;
        let coverage =
            native_history_checkpoint::<NativePublicationCoverage>(authority, "coverage")
                .load()
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "first-batch coverage missing".to_string())?;
        assert_eq!(
            coverage.sequence, 4096,
            "取消后不得偷偷完成后两批或当空结果成功"
        );
        assert_eq!(authority.journal.last_sequence(), 8193);
        Ok(())
    }

    #[test]
    fn native_history_continuation_keeps_frozen_head_and_rejects_replaced_original_slot()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = ChatEventLog::open(temp.path(), ChatEventRetention::default())
            .map_err(|error| error.to_string())?;
        let (message, event) = native_history_test_message()?;
        let original = append_native_history_test_event(&log, 1, event.clone())?;
        for _ in 1..4097 {
            log.append(
                "workspace-1",
                Some("conversation-1"),
                "turn-1",
                ChatDriverEvent::TurnStatus {
                    status: "running".into(),
                },
            )
            .map_err(|error| error.to_string())?;
        }
        let first = log
            .native_history_publication_read_step("workspace-1", "conversation-1", &message, None)
            .map_err(|error| error.to_string())?;
        let NativeHistoryReadStep::Pending(cursor) = first else {
            return Err("first batch did not retain continuation".into());
        };
        assert_eq!(cursor.head, 4097);
        log.append(
            "workspace-1",
            Some("conversation-1"),
            "turn-1",
            ChatDriverEvent::TurnStatus {
                status: "running".into(),
            },
        )
        .map_err(|error| error.to_string())?;
        let second = log
            .native_history_publication_read_step(
                "workspace-1",
                "conversation-1",
                &message,
                Some(cursor.clone()),
            )
            .map_err(|error| error.to_string())?;
        let NativeHistoryReadStep::Ready(events) = second else {
            return Err("fixed head chased unrelated append".into());
        };
        assert_eq!(
            events.first().map(|event| &event.event_id),
            Some(&original.event_id)
        );
        {
            let guard = lock_cached_stream(&cursor.cached);
            let authority = guard
                .as_ref()
                .ok_or_else(|| "original slot missing".to_string())?;
            let coverage =
                native_history_checkpoint::<NativePublicationCoverage>(authority, "coverage")
                    .load()
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| "coverage missing".to_string())?;
            assert_eq!(coverage.sequence, 4097, "原有限head不追逐并发尾部");
            assert_eq!(authority.journal.last_sequence(), 4098);
        }
        // 删除/重建使用原产品API，只作用于本新temp；旧读游标不能切到新owner。
        log.remove_conversation("workspace-1", "conversation-1")
            .map_err(|error| error.to_string())?;
        append_native_history_test_event(&log, 1, event)?;
        assert!(matches!(
            log.native_history_publication_read_step(
                "workspace-1",
                "conversation-1",
                &message,
                Some(cursor)
            ),
            Err(ChatEventLogError::NativeHistoryReadChanged)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn native_history_async_continuation_does_not_swallow_gap_or_foreign_corruption()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let log = Arc::new(
            ChatEventLog::open(
                temp.path(),
                ChatEventRetention {
                    segment_rollover_bytes: 1,
                    max_segments: 1,
                    max_replay_events: 64,
                },
            )
            .map_err(|error| error.to_string())?,
        );
        let (message, event) = native_history_test_message()?;
        append_native_history_test_event(&log, 1, event.clone())?;
        append_native_history_test_event(&log, 2, event)?;
        let stream = stream_id("workspace-1", Some("conversation-1"), "turn-1")
            .map_err(|error| error.to_string())?;
        let cached = log
            .stream_journal(&stream, false)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "stream missing".to_string())?;
        {
            let guard = lock_cached_stream(&cached);
            guard
                .as_ref()
                .ok_or_else(|| "authority missing".to_string())?
                .journal
                .prune_closed_segments_before(2)
                .map_err(|error| error.to_string())?;
        }
        let cancel = echo_agent::agent::CancellationToken::new();
        assert!(matches!(
            log.native_history_publications_async(
                "workspace-1",
                "conversation-1",
                &message,
                &cancel
            )
            .await,
            Err(ChatEventLogError::NativeHistoryGap { .. })
        ));
        let corrupt_path = log
            .stream_dir(&stream)
            .join(NATIVE_HISTORY_DIRECTORY)
            .join("coverage.json");
        fs::create_dir_all(
            corrupt_path
                .parent()
                .ok_or_else(|| "coverage parent missing".to_string())?,
        )
        .map_err(|error| error.to_string())?;
        fs::write(&corrupt_path, b"{corrupt").map_err(|error| error.to_string())?;
        assert!(
            log.native_history_publications_async(
                "workspace-1",
                "conversation-1",
                &message,
                &cancel
            )
            .await
            .is_err()
        );
        FileCheckpointStore::<NativePublicationCoverage>::open(&corrupt_path)
            .save(
                &JournalIdentity::new(),
                &NativePublicationCoverage {
                    first_observed_sequence: 1,
                    usage: None,
                    presentation: None,
                },
                0,
            )
            .map_err(|error| error.to_string())?;
        assert!(
            matches!(
                log.native_history_publications_async(
                    "workspace-1",
                    "conversation-1",
                    &message,
                    &cancel
                )
                .await,
                Err(ChatEventLogError::Corrupt { .. })
            ),
            "合法外代快照也不能被自动续读洗掉"
        );
        Ok(())
    }
}
