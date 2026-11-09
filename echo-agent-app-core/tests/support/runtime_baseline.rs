// Included only inside the original store unit-test boundary.
mod harness {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/support/performance_baseline.rs"
    ));
}
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

#[derive(Serialize, Deserialize)]
struct RuntimeFixture {
    case: String,
    fixed_root: String,
    events: usize,
    header_events: usize,
    usage_observations: usize,
    checkpoint_variant: String,
    checkpoint_sequence: usize,
    replay_tail: usize,
    plan_revision: u64,
    run_id: String,
    turn_id: String,
    tokens_used: u64,
}

struct SeedProgress {
    sequence: usize,
    cut: usize,
    pending: Vec<RuntimeJournalEvent>,
    checkpoint: Option<Vec<u8>>,
}

fn append_usage(
    store: &TaskRuntimeStore,
    root: &Path,
    run_id: &str,
    event: RuntimeJournalEvent,
    progress: &mut SeedProgress,
) -> harness::HarnessResult {
    progress.pending.push(event);
    progress.sequence = progress
        .sequence
        .checked_add(1)
        .ok_or("seed sequence overflow")?;
    if progress.pending.len() == 512 || progress.sequence == progress.cut {
        store
            .shadow
            .append_event_batch(run_id, std::mem::take(&mut progress.pending))?;
    }
    if progress.sequence == progress.cut {
        store.shadow.rewrite_plan(run_id)?;
        progress.checkpoint = Some(std::fs::read(
            root.join("tasks").join(run_id).join("checkpoint.json"),
        )?);
    }
    Ok(())
}

async fn seed_runtime(
    root: &Path,
    events: usize,
    tail: usize,
    variant: &str,
) -> harness::HarnessResult<RuntimeFixture> {
    harness::empty_seed_root(root)?;
    if !["valid", "tail", "missing", "corrupt"].contains(&variant)
        || tail >= events
        || (variant == "tail" && tail == 0)
        || (variant != "tail" && tail != 0)
    {
        return Err("invalid checkpoint protocol".into());
    }
    let store = Arc::new(TaskRuntimeStore::open_for_workspace(
        root.join("tasks"),
        "baseline",
    )?);
    let run_id = "journal-run";
    let turn_id = "journal-turn";
    let goal = "合法 usage 历史恢复 🧪";
    store.create_run(
        run_id,
        "baseline",
        "journal-conversation",
        "journal-root",
        DomainProfile::General,
        goal,
        "task",
        AttendedMode::Unattended,
    )?;
    let plan = crate::tasks::task_runtime::commit_task_plan(
        store.clone(),
        TaskPlan {
            plan_id: "journal-plan".to_string(),
            run_id: run_id.to_string(),
            revision: 1,
            domain_profile: DomainProfile::General,
            domain_binding: None,
            goal_revision: 1,
            goal_sha256: task_goal_sha256(goal),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Sequential,
            tasks: vec![PlanTask {
                id: "task-a".to_string(),
                title: "恢复独立 usage 事实".to_string(),
                kind: PlanTaskKind::Investigation,
                agent_role: "general".to_string(),
                sort_order: 1,
                ..Default::default()
            }],
        },
    )
    .await?;
    store.configure_run_continuation(run_id, true, false, None, None)?;
    store.transition_run(run_id, TaskRunStatus::Running)?;
    store.claim_run_turn(
        run_id,
        turn_id,
        RunTurnOrigin::Continuation,
        TurnVisibility::Internal,
    )?;
    let header = store.list_events(run_id, 0)?.len();
    if events <= header || events.saturating_sub(tail) <= header {
        return Err("event target below legal header".into());
    }
    let mut progress = SeedProgress {
        sequence: header,
        cut: events.saturating_sub(tail),
        pending: Vec::with_capacity(512),
        checkpoint: None,
    };
    // This is one interrupted, active turn with many distinct accepted usage
    // observations. Every event changes usage and its dedupe identity. It is
    // neither many tasks nor fabricated successful/no-op turn completions.
    for ordinal in header..events {
        let provider = format!("synthetic-usage-{ordinal}");
        append_usage(
            &store,
            root,
            run_id,
            RuntimeJournalEvent::for_append(
                run_id,
                None,
                None,
                RuntimeEventKind::RunTurnUsageAccounted,
                serde_json::json!({
                    "event_id":format!("{run_id}:{turn_id}:usage:{provider}"),"turn_id":turn_id,
                    "provider_event_id":provider,"input_tokens":1,"output_tokens":2,"source_scope":"primary_turn",
                    "pause_reason":null,"pause_detail":null,
                }),
            ),
            &mut progress,
        )?;
    }
    if !progress.pending.is_empty() {
        store
            .shadow
            .append_event_batch(run_id, std::mem::take(&mut progress.pending))?;
    }
    store.shadow.rewrite_plan(run_id)?;
    let fixture = RuntimeFixture {
        case: "runtime-history".to_string(),
        fixed_root: root.to_string_lossy().into_owned(),
        events,
        header_events: header,
        usage_observations: events.saturating_sub(header),
        checkpoint_variant: variant.to_string(),
        checkpoint_sequence: if variant == "tail" {
            events.saturating_sub(tail)
        } else if variant == "valid" {
            events
        } else {
            0
        },
        replay_tail: if variant == "tail" {
            tail
        } else if variant == "valid" {
            0
        } else {
            events
        },
        plan_revision: plan.revision,
        run_id: run_id.to_string(),
        turn_id: turn_id.to_string(),
        tokens_used: u64::try_from(events.saturating_sub(header))?
            .checked_mul(3)
            .ok_or("token counter overflow")?,
    };
    validate_runtime(&store, &fixture)?;
    let direct = store
        .diagnose_full_journal_projection(run_id)?
        .ok_or("full-fold control missing")?;
    assert_eq!(
        serde_json::to_value(direct)?,
        serde_json::to_value(store.get_run_state(run_id)?.ok_or("state missing")?)?
    );
    // Mutate only the discardable checkpoint after the original owners close.
    // Its original journal generation/absolute root identity is never rewritten.
    drop(store);
    let checkpoint = root.join("tasks").join(run_id).join("checkpoint.json");
    match variant {
        "valid" => {}
        "tail" => std::fs::write(
            &checkpoint,
            progress
                .checkpoint
                .ok_or("checkpoint cut was not captured")?,
        )?,
        "missing" => std::fs::remove_file(&checkpoint)?,
        "corrupt" => std::fs::write(&checkpoint, b"{invalid checkpoint fixture")?,
        _ => return Err("unknown variant".into()),
    }
    harness::write_json(&root.join("fixture.json"), &fixture)?;
    Ok(fixture)
}

#[tokio::test]
#[ignore = "small persisted correctness control; explicit setup slot, no performance rows"]
async fn runtime_baseline_setup_control() -> harness::HarnessResult {
    for variant in ["valid", "tail", "missing", "corrupt"] {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("workspace");
        let fixture =
            seed_runtime(&root, 64, if variant == "tail" { 8 } else { 0 }, variant).await?;
        let store = Arc::new(TaskRuntimeStore::open_for_workspace(
            root.join("tasks"),
            "baseline",
        )?);
        let stats = store.shadow.rewrite_plan_with_stats(&fixture.run_id)?;
        assert_eq!(stats.seq, 64);
        assert_eq!(stats.folded_events, fixture.replay_tail);
        assert_eq!(stats.used_checkpoint, fixture.checkpoint_sequence > 0);
        validate_runtime(&store, &fixture)?;
        let owner = crate::tasks::task_runtime::TaskRunBootReconciler::for_store(&store);
        assert_eq!(owner.recover_once().await?, 1);
        let state = store
            .get_run_state(&fixture.run_id)?
            .ok_or("control boot state missing")?;
        assert_eq!(state.run.status, TaskRunStatus::Paused);
        let continuation = state.continuation.ok_or("control continuation missing")?;
        assert!(continuation.active_turn.is_none());
        let last = continuation.last_turn.ok_or("turn settlement missing")?;
        assert_eq!(last.status, RunTurnStatus::Failed);
        assert_eq!(
            last.error_fingerprint.as_deref(),
            Some("process_interrupted")
        );
    }
    Ok(())
}

fn validate_runtime(store: &TaskRuntimeStore, fixture: &RuntimeFixture) -> harness::HarnessResult {
    let state = store
        .get_run_state(&fixture.run_id)?
        .ok_or("recovered run missing")?;
    assert_eq!(state.run.run_id, fixture.run_id);
    assert_eq!(state.run.workspace_id, "baseline");
    assert_eq!(state.run.status, TaskRunStatus::Running);
    assert_eq!(state.journal_sequence, u64::try_from(fixture.events)?);
    let continuation = state.continuation.ok_or("continuation missing")?;
    assert_eq!(continuation.tokens_used, fixture.tokens_used);
    assert_eq!(
        continuation
            .active_turn
            .ok_or("active interrupted turn missing")?
            .turn_id,
        fixture.turn_id
    );
    assert_eq!(
        state.event_index.accounted_usage.len(),
        fixture.usage_observations
    );
    let plan = store
        .get_plan_revision(&fixture.run_id)?
        .ok_or("accepted plan missing")?;
    assert_eq!(plan.revision, fixture.plan_revision);
    assert_eq!(plan.tasks.len(), 1);
    assert_eq!(plan.goal_sha256, state.run.goal_sha256);
    let todos = store.list_todos(&fixture.run_id)?;
    assert_eq!(todos.len(), 1);
    assert_eq!(todos.first().ok_or("todo missing")?.task_id, "task-a");
    Ok(())
}

async fn recover_sample(
    root: &Path,
    fixture: &RuntimeFixture,
    mode: &str,
) -> harness::HarnessResult {
    if mode == "hot" {
        if fixture.checkpoint_variant != "valid" {
            return Err("event-page hot group requires the valid-checkpoint fixture".into());
        }
        let store = TaskRuntimeStore::open_for_workspace(root.join("tasks"), "baseline")?;
        let state = store
            .get_run_state(&fixture.run_id)?
            .ok_or("event-page run missing")?;
        assert_eq!(state.journal_sequence, u64::try_from(fixture.events)?);
        for (position, after) in [
            ("head", 0_usize),
            ("tail", fixture.events.saturating_sub(256)),
        ] {
            for ordinal in 0_usize..35 {
                let started = Instant::now();
                let page =
                    store.query_task_event_page(&fixture.run_id, i64::try_from(after)?, 256)?;
                assert_eq!(
                    page.events.len(),
                    fixture.events.saturating_sub(after).min(256)
                );
                for (index, event) in page.events.iter().enumerate() {
                    assert_eq!(event.run_id, fixture.run_id);
                    assert_eq!(
                        usize::try_from(event.seq)?,
                        after.saturating_add(index).saturating_add(1)
                    );
                }
                let next = after.saturating_add(page.events.len());
                assert_eq!(page.next_after_sequence, i64::try_from(next)?);
                assert_eq!(page.has_more, next < fixture.events);
                let elapsed = started.elapsed().as_nanos();
                let (phase, index) = if ordinal < 5 {
                    ("warmup", ordinal)
                } else {
                    ("measurement", ordinal - 5)
                };
                harness::emit(
                    &format!("task-runtime/event-page/{position}+validation"),
                    "hot",
                    phase,
                    index,
                    elapsed,
                    "success",
                    serde_json::json!({"ok":true,"events_in_journal":fixture.events,"returned":page.events.len(),"after_sequence":after.to_string(),"next_after_sequence":page.next_after_sequence.to_string(),"has_more":page.has_more,"page_limit":256,"scope":"actual query_task_event_page; warmed original authority; no boot or IPC latency","timing_includes":"query and contiguous identity/cursor/count validation","checkpoint":"valid","durability":"SyncData"}),
                )?;
            }
        }
        return Ok(());
    }
    let started = Instant::now();
    let store = Arc::new(TaskRuntimeStore::open_for_workspace(
        root.join("tasks"),
        "baseline",
    )?);
    if mode == "runtime-boot" {
        let owner = crate::tasks::task_runtime::TaskRunBootReconciler::for_store(&store);
        let recovered = owner.recover_once().await?;
        let state = store
            .get_run_state(&fixture.run_id)?
            .ok_or("boot state missing")?;
        let elapsed = started.elapsed().as_nanos();
        assert_eq!(recovered, 1);
        assert_eq!(state.run.status, TaskRunStatus::Paused);
        assert!(state.journal_sequence > u64::try_from(fixture.events)?);
        let continuation = state.continuation.ok_or("boot continuation missing")?;
        assert!(continuation.active_turn.is_none());
        assert_eq!(continuation.tokens_used, fixture.tokens_used);
        let last = continuation
            .last_turn
            .ok_or("boot must settle original interrupted turn")?;
        assert_eq!(last.turn_id, fixture.turn_id);
        assert_eq!(last.status, RunTurnStatus::Failed);
        assert_eq!(
            last.error_fingerprint.as_deref(),
            Some("process_interrupted")
        );
        let (phase, index) = harness::phase_index()?;
        return harness::emit(
            "runtime/full-boot-owner-reconciliation",
            "process-cold",
            &phase,
            index,
            elapsed,
            "success",
            serde_json::json!({"ok":true,"events_before":fixture.events,"usage_observations":fixture.usage_observations,
                "checkpoint":fixture.checkpoint_variant,"recovered":recovered,"terminal_status":"paused","turn_status":last.status.as_str(),
                "turn_error_fingerprint":last.error_fingerprint,
                "journal_sequence_after":state.journal_sequence,"durability":"SyncData","os_page_cache":"uncontrolled"}),
        );
    }
    if mode != "runtime-cold" {
        return Err("runtime mode must be runtime-cold or runtime-boot".into());
    }
    // The existing test observation delegates to the original RunAuthority's
    // real open/replay/projection path; it does not replace the decoder/reducer.
    let stats = store.shadow.rewrite_plan_with_stats(&fixture.run_id)?;
    let state = store
        .get_run_state(&fixture.run_id)?
        .ok_or("cold state missing")?;
    let todos = store.list_todos(&fixture.run_id)?;
    let elapsed = started.elapsed().as_nanos();
    assert_eq!(stats.seq, i64::try_from(fixture.events)?);
    assert_eq!(stats.used_checkpoint, fixture.checkpoint_sequence > 0);
    assert_eq!(stats.folded_events, fixture.replay_tail);
    assert_eq!(state.journal_sequence, u64::try_from(fixture.events)?);
    assert_eq!(todos.len(), 1);
    validate_runtime(&store, fixture)?;
    let (phase, index) = harness::phase_index()?;
    harness::emit(
        "runtime/root+validated-journal+checkpoint+tail+projection",
        "process-cold",
        &phase,
        index,
        elapsed,
        "success",
        serde_json::json!({"ok":true,"events":fixture.events,"usage_observations":fixture.usage_observations,"checkpoint":fixture.checkpoint_variant,
            "checkpoint_sequence":fixture.checkpoint_sequence,"folded_events":stats.folded_events,"durability":"SyncData",
            "first_trigger":"original RunAuthority refresh via existing test stats adapter","os_page_cache":"uncontrolled"}),
    )
}

#[tokio::test]
#[ignore = "explicit retained release harness; coordinator seed/measurement lease required"]
async fn performance_baseline_runtime() -> harness::HarnessResult {
    harness::check_release()?;
    let root = harness::root()?;
    match harness::required("EKO_PERF_ACTION")?.as_str() {
        "seed" => {
            let events = harness::usize_arg("EKO_PERF_EVENTS")?;
            if ![10000, 100000].contains(&events) {
                return Err("official history axis is 10k/100k events".into());
            }
            let tail = harness::usize_arg("EKO_PERF_TAIL")?;
            seed_runtime(
                &root,
                events,
                tail,
                &harness::required("EKO_PERF_CHECKPOINT")?,
            )
            .await?;
            Ok(())
        }
        "validate" => {
            // Deliberately only inspect frozen metadata: opening a missing or
            // corrupt checkpoint here would repair it before cold measurement.
            let fixture: RuntimeFixture = harness::read_json(&root.join("fixture.json"))?;
            assert_eq!(fixture.fixed_root, root.to_string_lossy());
            assert_eq!(
                fixture.events,
                fixture
                    .header_events
                    .saturating_add(fixture.usage_observations)
            );
            Ok(())
        }
        "sample" => {
            let fixture: RuntimeFixture = harness::read_json(&root.join("fixture.json"))?;
            assert_eq!(fixture.fixed_root, root.to_string_lossy());
            recover_sample(&root, &fixture, &harness::required("EKO_PERF_MODE")?).await
        }
        _ => Err("unknown runtime action".into()),
    }
}
