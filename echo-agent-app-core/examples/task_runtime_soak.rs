use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail, ensure};
use chrono::Utc;
use clap::Parser;
use echo_agent::state::journal::{CheckpointStore, FileCheckpointStore};
use echo_agent_app_core::api::tasks::task_runtime::store::{
    RunTurnClaimOutcome, RunTurnCompletion,
};
use echo_agent_app_core::api::tasks::task_runtime::{
    Artifact, ArtifactKind, AttendedMode, BootAutoResumeOutcome, DomainProfile, ExecutionMode,
    PlanTask, PlanTaskKind, RunPauseReason, RunTurnOrigin, RunTurnStatus, TaskPlan,
    TaskRunResumeIdentity, TaskRunStatus, TaskRuntimeStore, TurnVisibility, commit_eko_task_plan,
    task_goal_sha256,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const LEDGER_SCHEMA_VERSION: u32 = 1;
const HEARTBEAT_SECONDS: u64 = 30;
const RECOVERY_EVERY_ENDED_TURNS: u64 = 120;
const PROVIDER: &str = "deterministic_local_soak_v1";
const GOAL: &str = "Verify EKO long-horizon Goal, checkpoint, recovery, and accounting invariants";

#[derive(Debug, Parser)]
#[command(name = "task-runtime-soak")]
struct Args {
    /// Required real active duration. Only the ordered M5 gates are accepted.
    #[arg(long, env = "EKO_SOAK_HOURS")]
    hours: u64,

    /// Durable output directory. Defaults to .eko/soak/m5-{hours}h.
    #[arg(long, env = "EKO_SOAK_OUTPUT_DIR")]
    output_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SoakStatus {
    Running,
    Interrupted,
    Failed,
    Passed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SoakConfiguration {
    provider: String,
    heartbeat_seconds: u64,
    recovery_every_ended_turns: u64,
    external_network: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RuntimeMetrics {
    event_count: u64,
    event_tail_seq: i64,
    turns_ended: u64,
    turns_failed: u64,
    tokens_used: u64,
    compaction_count: u64,
    next_turn_ordinal: u64,
    failure_fingerprints: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FinalEvidence {
    run_status: String,
    active_turn: bool,
    goal_sha256: String,
    event_log_sha256: String,
    checkpoint_state_hash: String,
    run_state_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SoakLedger {
    schema_version: u32,
    duration_hours: u64,
    required_active_millis: u64,
    run_id: String,
    commit: String,
    configuration: SoakConfiguration,
    status: SoakStatus,
    started_at: String,
    updated_at: String,
    completed_at: Option<String>,
    active_elapsed_millis: u64,
    process_starts: u64,
    runtime_reopens: u64,
    recoveries: u64,
    last_recovery_after_ended_turns: u64,
    metrics: RuntimeMetrics,
    failure_fingerprints: Vec<String>,
    final_evidence: Option<FinalEvidence>,
}

impl SoakLedger {
    fn new(hours: u64, required_active_millis: u64, run_id: String, commit: String) -> Self {
        let now = Utc::now().to_rfc3339();
        Self {
            schema_version: LEDGER_SCHEMA_VERSION,
            duration_hours: hours,
            required_active_millis,
            run_id,
            commit,
            configuration: SoakConfiguration {
                provider: PROVIDER.to_string(),
                heartbeat_seconds: HEARTBEAT_SECONDS,
                recovery_every_ended_turns: RECOVERY_EVERY_ENDED_TURNS,
                external_network: false,
            },
            status: SoakStatus::Running,
            started_at: now.clone(),
            updated_at: now,
            completed_at: None,
            active_elapsed_millis: 0,
            process_starts: 0,
            runtime_reopens: 0,
            recoveries: 0,
            last_recovery_after_ended_turns: 0,
            metrics: RuntimeMetrics::default(),
            failure_fingerprints: Vec::new(),
            final_evidence: None,
        }
    }

    fn validate_resume(&self, hours: u64, commit: &str, run_id: &str) -> Result<()> {
        ensure!(
            self.schema_version == LEDGER_SCHEMA_VERSION,
            "soak ledger schema mismatch"
        );
        ensure!(self.duration_hours == hours, "soak duration mismatch");
        ensure!(self.commit == commit, "soak commit mismatch");
        ensure!(self.run_id == run_id, "soak run identity mismatch");
        ensure!(
            self.configuration.provider == PROVIDER
                && self.configuration.heartbeat_seconds == HEARTBEAT_SECONDS
                && self.configuration.recovery_every_ended_turns == RECOVERY_EVERY_ENDED_TURNS
                && !self.configuration.external_network,
            "soak configuration mismatch"
        );
        match self.status {
            SoakStatus::Running | SoakStatus::Interrupted => Ok(()),
            SoakStatus::Failed => bail!(
                "failed soak cannot be resumed; fix the failure and restart this duration in a new output directory"
            ),
            SoakStatus::Passed => bail!("this soak duration already passed"),
        }
    }

    fn update_active_elapsed(&mut self, base_millis: u64, process_started: std::time::Instant) {
        let process_millis =
            u64::try_from(process_started.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.active_elapsed_millis = base_millis.saturating_add(process_millis);
        self.updated_at = Utc::now().to_rfc3339();
    }

    fn merge_metrics(&mut self, metrics: RuntimeMetrics) {
        for fingerprint in &metrics.failure_fingerprints {
            if !self.failure_fingerprints.contains(fingerprint) {
                self.failure_fingerprints.push(fingerprint.clone());
            }
        }
        self.metrics = metrics;
    }

    fn record_fatal(&mut self, error: &anyhow::Error) {
        let detail = error
            .to_string()
            .chars()
            .filter(|character| !character.is_control())
            .take(600)
            .collect::<String>();
        let fingerprint = hex::encode(Sha256::digest(detail.as_bytes()));
        if !self.failure_fingerprints.contains(&fingerprint) {
            self.failure_fingerprints.push(fingerprint);
        }
        self.status = SoakStatus::Failed;
        self.updated_at = Utc::now().to_rfc3339();
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(
        matches!(args.hours, 12 | 24 | 48),
        "--hours must be one of the ordered M5 gates: 12, 24, or 48"
    );
    let required_active_millis = args
        .hours
        .checked_mul(60)
        .and_then(|value| value.checked_mul(60))
        .and_then(|value| value.checked_mul(1_000))
        .ok_or_else(|| anyhow!("soak duration overflow"))?;
    let repo_root = git_repo_root()?;
    ensure_clean_worktree(&repo_root)?;
    let commit = git_head(&repo_root)?;
    let output_dir = args
        .output_dir
        .unwrap_or_else(|| repo_root.join(format!(".eko/soak/m5-{}h", args.hours)));
    run_soak(args.hours, required_active_millis, &commit, &output_dir).await
}

async fn run_soak(
    hours: u64,
    required_active_millis: u64,
    commit: &str,
    output_dir: &Path,
) -> Result<()> {
    std::fs::create_dir_all(output_dir)
        .with_context(|| format!("create soak output {}", output_dir.display()))?;
    let task_root = output_dir.join("tasks");
    let ledger_path = output_dir.join("ledger.json");
    let run_id = format!("m5-soak-{hours}h");
    let (mut ledger, created_ledger) =
        load_or_create_ledger(&ledger_path, hours, required_active_millis, &run_id, commit)?;
    if created_ledger && task_root.join(&run_id).exists() {
        bail!(
            "soak TaskRun exists without its ledger; use a new output directory instead of guessing elapsed time"
        );
    }
    ledger.validate_resume(hours, commit, &run_id)?;
    ledger.process_starts = ledger.process_starts.saturating_add(1);
    ledger.status = SoakStatus::Running;
    ledger.updated_at = Utc::now().to_rfc3339();
    write_ledger(&ledger_path, &ledger)?;

    let base_millis = ledger.active_elapsed_millis;
    let process_started = std::time::Instant::now();
    let mut store = match open_runtime(&task_root, &mut ledger).await {
        Ok(store) => store,
        Err(error) => {
            ledger.record_fatal(&error);
            ledger.update_active_elapsed(base_millis, process_started);
            write_ledger(&ledger_path, &ledger)?;
            return Err(error);
        }
    };
    let goal_sha256 = task_goal_sha256(GOAL);
    let mut ticker = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(HEARTBEAT_SECONDS),
        Duration::from_secs(HEARTBEAT_SECONDS),
    );
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let remaining = required_active_millis.saturating_sub(base_millis);
    let deadline = tokio::time::Instant::now() + Duration::from_millis(remaining);

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let cycle = run_cycle(&store, &run_id).and_then(|_| {
                    validate_runtime(&store, &run_id, &goal_sha256)
                });
                let metrics = match cycle {
                    Ok(metrics) => metrics,
                    Err(error) => {
                        ledger.record_fatal(&error);
                        ledger.update_active_elapsed(base_millis, process_started);
                        let _ = store.request_pause_with_reason(
                            &run_id,
                            RunPauseReason::User,
                            Some("M5 soak stopped after a fatal invariant failure"),
                        );
                        write_ledger(&ledger_path, &ledger)?;
                        return Err(error);
                    }
                };
                ledger.merge_metrics(metrics.clone());
                if metrics.turns_ended > 0
                    && metrics.turns_ended % RECOVERY_EVERY_ENDED_TURNS == 0
                    && ledger.last_recovery_after_ended_turns < metrics.turns_ended
                {
                    ledger.runtime_reopens = ledger.runtime_reopens.saturating_add(1);
                    store = match open_runtime(&task_root, &mut ledger).await {
                        Ok(store) => store,
                        Err(error) => {
                            ledger.record_fatal(&error);
                            ledger.update_active_elapsed(base_millis, process_started);
                            write_ledger(&ledger_path, &ledger)?;
                            return Err(error);
                        }
                    };
                    ledger.last_recovery_after_ended_turns = metrics.turns_ended;
                    ledger.merge_metrics(validate_runtime(&store, &run_id, &goal_sha256)?);
                }
                ledger.update_active_elapsed(base_millis, process_started);
                write_ledger(&ledger_path, &ledger)?;
            }
            signal = tokio::signal::ctrl_c() => {
                signal.context("install Ctrl-C handler")?;
                ledger.update_active_elapsed(base_millis, process_started);
                if let Ok(metrics) = validate_runtime(&store, &run_id, &goal_sha256) {
                    ledger.merge_metrics(metrics);
                }
                ensure!(
                    store.request_pause_with_reason(
                        &run_id,
                        RunPauseReason::User,
                        Some("M5 soak interrupted; resume with the same command"),
                    )?,
                    "interrupted soak did not durably pause its TaskRun"
                );
                ledger.status = SoakStatus::Interrupted;
                write_ledger(&ledger_path, &ledger)?;
                bail!("soak interrupted after {} active milliseconds; resume the same command", ledger.active_elapsed_millis);
            }
            _ = tokio::time::sleep_until(deadline) => break,
        }
    }

    ledger.update_active_elapsed(base_millis, process_started);
    ensure!(
        ledger.active_elapsed_millis >= required_active_millis,
        "soak active duration ended before its required gate"
    );
    let before_pause = validate_runtime(&store, &run_id, &goal_sha256)?;
    ledger.merge_metrics(before_pause);
    ensure!(
        store.request_pause_with_reason(
            &run_id,
            RunPauseReason::User,
            Some("M5 soak duration completed"),
        )?,
        "completed soak did not durably pause its TaskRun"
    );
    let metrics = validate_runtime(&store, &run_id, &goal_sha256)?;
    ledger.merge_metrics(metrics);
    ledger.final_evidence = Some(final_evidence(&store, &task_root, &run_id, &goal_sha256)?);
    ledger.status = SoakStatus::Passed;
    ledger.completed_at = Some(Utc::now().to_rfc3339());
    ledger.updated_at = Utc::now().to_rfc3339();
    write_ledger(&ledger_path, &ledger)?;
    println!("{}", serde_json::to_string_pretty(&ledger)?);
    Ok(())
}

fn load_or_create_ledger(
    path: &Path,
    hours: u64,
    required_active_millis: u64,
    run_id: &str,
    commit: &str,
) -> Result<(SoakLedger, bool)> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(|ledger| (ledger, false))
            .context("decode existing soak ledger"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok((
            SoakLedger::new(
                hours,
                required_active_millis,
                run_id.to_string(),
                commit.to_string(),
            ),
            true,
        )),
        Err(error) => Err(error).context("read existing soak ledger"),
    }
}

async fn open_runtime(task_root: &Path, ledger: &mut SoakLedger) -> Result<Arc<TaskRuntimeStore>> {
    let store = Arc::new(
        TaskRuntimeStore::new_in_memory_with_shadow_root(task_root)
            .context("open file-backed TaskRuntime store")?,
    );
    let existing = store.get_run(&ledger.run_id)?;
    let created = existing.is_none();
    if created {
        store.create_run(
            &ledger.run_id,
            "test",
            &format!("soak:{}h", ledger.duration_hours),
            &format!("soak-root:{}h", ledger.duration_hours),
            DomainProfile::General,
            GOAL,
            "m5_soak",
            AttendedMode::Unattended,
        )?;
    }
    if store.get_plan(&ledger.run_id)?.is_none() {
        commit_eko_task_plan(store.clone(), soak_plan(&ledger.run_id))
            .await
            .map_err(|error| anyhow!("commit soak plan: {error}"))?;
    }
    let status = store
        .get_run(&ledger.run_id)?
        .ok_or_else(|| anyhow!("soak TaskRun disappeared"))?
        .status;
    if status == TaskRunStatus::Pending {
        store.transition_run(&ledger.run_id, TaskRunStatus::Running)?;
    }
    let continuation = store
        .get_run_state(&ledger.run_id)?
        .and_then(|snapshot| snapshot.continuation);
    if !continuation
        .as_ref()
        .is_some_and(|state| state.enabled && state.auto_resume_after_restart)
    {
        store.configure_run_continuation(&ledger.run_id, true, true, None, None)?;
    }
    if !created
        && store
            .get_run(&ledger.run_id)?
            .is_some_and(|run| run.status == TaskRunStatus::Running)
    {
        let recovered = u64::try_from(store.recover_incomplete()?).unwrap_or(u64::MAX);
        ledger.recoveries = ledger.recoveries.saturating_add(recovered);
    }
    let state = store
        .get_run_state(&ledger.run_id)?
        .ok_or_else(|| anyhow!("soak run-state projection missing"))?;
    match state.run.status {
        TaskRunStatus::Running => {}
        TaskRunStatus::Paused => match state
            .continuation
            .as_ref()
            .and_then(|continuation| continuation.pause.as_ref())
            .map(|pause| pause.reason)
        {
            Some(RunPauseReason::BootRecovery) => {
                match store.resume_task_run_after_boot(&ledger.run_id, true, false)? {
                    BootAutoResumeOutcome::Resumed(_) => {}
                    BootAutoResumeOutcome::WaitingUntil(deadline) => {
                        bail!("unexpected provider retry deadline during soak recovery: {deadline}")
                    }
                    BootAutoResumeOutcome::Blocked(blockers) => {
                        let details = blockers
                            .iter()
                            .map(|blocker| blocker.as_str())
                            .collect::<Vec<_>>()
                            .join(",");
                        bail!("soak boot recovery blocked: {details}");
                    }
                }
            }
            Some(RunPauseReason::User) => {
                let snapshot = store
                    .get_run_state(&ledger.run_id)?
                    .ok_or_else(|| anyhow::anyhow!("resume snapshot missing"))?;
                let expected = TaskRunResumeIdentity::capture(&snapshot);
                store.resume_task_run_expected(&expected)?;
            }
            other => bail!("soak cannot resume from pause reason {other:?}"),
        },
        other => bail!("soak cannot continue from terminal run status {other:?}"),
    }
    Ok(store)
}

fn soak_plan(run_id: &str) -> TaskPlan {
    TaskPlan {
        plan_id: format!("{run_id}-plan"),
        run_id: run_id.to_string(),
        revision: 1,
        domain_profile: DomainProfile::General,
        goal_revision: 1,
        goal_sha256: task_goal_sha256(GOAL),
        assumptions: vec!["deterministic local provider; fault domains run separately".to_string()],
        risks: vec!["host interruption extends active-time completion".to_string()],
        execution_mode: ExecutionMode::Sequential,
        tasks: vec![PlanTask {
            id: "soak-runtime-invariants".to_string(),
            title: "Maintain long-horizon runtime invariants".to_string(),
            description: "Exercise checkpoint, continuation, accounting, and boot recovery"
                .to_string(),
            kind: PlanTaskKind::Summary,
            agent_role: "general".to_string(),
            domain_profile: DomainProfile::General,
            depends_on: Vec::new(),
            parallel_group: None,
            execution_target: None,
            files: Vec::new(),
            allowed_tools: Vec::new(),
            required_artifacts: Vec::new(),
            execution_checks: Vec::new(),
            acceptance_criteria: Vec::new(),
            retry_count: 0,
            max_retries: 1,
            failure_fingerprint: None,
            status: echo_agent::tasks::TaskStatus::Pending,
            claim: None,
            sort_order: 0,
        }],
    }
}

fn run_cycle(store: &TaskRuntimeStore, run_id: &str) -> Result<()> {
    let state = store
        .get_run_state(run_id)?
        .and_then(|snapshot| snapshot.continuation)
        .ok_or_else(|| anyhow!("soak continuation missing"))?;
    let ordinal = state.next_turn_ordinal.max(1);
    let turn_id = format!("soak-turn-{ordinal}");
    match store.claim_run_turn(
        run_id,
        &turn_id,
        RunTurnOrigin::Continuation,
        TurnVisibility::Internal,
    )? {
        RunTurnClaimOutcome::Started(_) => {}
        RunTurnClaimOutcome::NotSubmitted(reason) => {
            bail!("soak RunTurn was not submitted: {reason:?}")
        }
    }
    let usage_id = format!("soak-usage-{ordinal}");
    ensure!(
        !store.account_run_turn_usage(run_id, &turn_id, &usage_id, 1, 2)?,
        "unexpected token budget exhaustion"
    );
    ensure!(
        !store.account_run_turn_usage(run_id, &turn_id, &usage_id, 1, 2)?,
        "duplicate usage changed the budget outcome"
    );
    store.add_artifact(&Artifact {
        id: format!("soak-evidence-{ordinal}"),
        run_id: run_id.to_string(),
        task_id: Some("soak-runtime-invariants".to_string()),
        kind: ArtifactKind::Trace,
        title: format!("Soak heartbeat {ordinal}"),
        path: None,
        metadata: serde_json::json!({
            "provider": PROVIDER,
            "turn_ordinal": ordinal,
            "external_network": false,
        }),
    })?;
    if ordinal % 10 == 0 {
        let compaction_id = format!("soak-compaction-{ordinal}");
        store.record_run_turn_compaction(run_id, &turn_id, &compaction_id)?;
        store.record_run_turn_compaction(run_id, &turn_id, &compaction_id)?;
    }
    let finished = store.finish_run_turn(
        run_id,
        RunTurnCompletion {
            turn_id: &turn_id,
            status: RunTurnStatus::Ended,
            elapsed_seconds: 1,
            final_message_id: Some(&format!("soak-message-{ordinal}")),
            error_fingerprint: None,
        },
    )?;
    ensure!(
        finished.active_turn.is_none(),
        "finished RunTurn remained active"
    );
    let replayed = store.finish_run_turn(
        run_id,
        RunTurnCompletion {
            turn_id: &turn_id,
            status: RunTurnStatus::Failed,
            elapsed_seconds: 99,
            final_message_id: None,
            error_fingerprint: Some("duplicate_must_not_replace_terminal"),
        },
    )?;
    ensure!(
        replayed
            .last_turn
            .as_ref()
            .is_some_and(|turn| turn.status == RunTurnStatus::Ended),
        "duplicate terminal delivery replaced the authoritative outcome"
    );
    Ok(())
}

fn validate_runtime(
    store: &TaskRuntimeStore,
    run_id: &str,
    expected_goal_sha256: &str,
) -> Result<RuntimeMetrics> {
    let events = store.list_events(run_id, 0)?;
    ensure!(!events.is_empty(), "soak event authority is empty");
    for (index, event) in events.iter().enumerate() {
        let expected = i64::try_from(index)
            .unwrap_or(i64::MAX)
            .checked_add(1)
            .ok_or_else(|| anyhow!("event sequence overflow"))?;
        ensure!(event.seq == expected, "non-contiguous event sequence");
        ensure!(event.run_id == run_id, "cross-run event detected");
    }
    let snapshot = store
        .get_run_state(run_id)?
        .ok_or_else(|| anyhow!("soak snapshot missing"))?;
    ensure!(
        snapshot.run.goal_sha256 == expected_goal_sha256,
        "TaskRun Goal hash drifted"
    );
    let plan = store
        .get_plan(run_id)?
        .ok_or_else(|| anyhow!("soak plan missing"))?;
    ensure!(
        plan.goal_revision == snapshot.run.goal_revision
            && plan.goal_sha256 == snapshot.run.goal_sha256,
        "Goal/Plan binding drifted"
    );
    let replayed = store
        .diagnose_full_journal_projection(run_id)?
        .ok_or_else(|| anyhow!("full journal diagnostic projection missing"))?;
    ensure!(
        serde_json::to_value(&snapshot)? == serde_json::to_value(&replayed)?,
        "checkpoint-backed snapshot differs from full journal projection"
    );
    let continuation = snapshot
        .continuation
        .ok_or_else(|| anyhow!("soak continuation projection missing"))?;
    ensure!(
        continuation.active_turn.is_none(),
        "soak validation found an active RunTurn"
    );
    ensure!(
        continuation.blocker_audit.is_none(),
        "soak progress failed to clear blocker audit"
    );
    let mut turns_ended = 0_u64;
    let mut turns_failed = 0_u64;
    let mut fingerprints = BTreeSet::new();
    for event in &events {
        if event.event_type
            != echo_agent_app_core::api::tasks::task_runtime::RuntimeEventKind::RunTurnFinished
        {
            continue;
        }
        match event
            .payload
            .get("status")
            .and_then(serde_json::Value::as_str)
        {
            Some("ended") => turns_ended = turns_ended.saturating_add(1),
            Some("failed") => turns_failed = turns_failed.saturating_add(1),
            _ => {}
        }
        if let Some(fingerprint) = event
            .payload
            .get("error_fingerprint")
            .and_then(serde_json::Value::as_str)
        {
            fingerprints.insert(fingerprint.to_string());
        }
    }
    Ok(RuntimeMetrics {
        event_count: u64::try_from(events.len()).unwrap_or(u64::MAX),
        event_tail_seq: events.last().map(|event| event.seq).unwrap_or(0),
        turns_ended,
        turns_failed,
        tokens_used: continuation.tokens_used,
        compaction_count: u64::from(continuation.compaction_count),
        next_turn_ordinal: continuation.next_turn_ordinal,
        failure_fingerprints: fingerprints.into_iter().collect(),
    })
}

fn final_evidence(
    store: &TaskRuntimeStore,
    task_root: &Path,
    run_id: &str,
    expected_goal_sha256: &str,
) -> Result<FinalEvidence> {
    let snapshot = store
        .get_run_state(run_id)?
        .ok_or_else(|| anyhow!("final run-state missing"))?;
    ensure!(
        snapshot.run.status == TaskRunStatus::Paused,
        "successful soak did not end Paused"
    );
    let event_bytes = std::fs::read(task_root.join(run_id).join("events.jsonl"))?;
    let checkpoint = FileCheckpointStore::<serde_json::Value>::open(
        task_root.join(run_id).join("checkpoint.json"),
    )
    .load()?
    .ok_or_else(|| anyhow!("final checkpoint missing"))?;
    let event_tail = store
        .list_events(run_id, 0)?
        .last()
        .map(|event| u64::try_from(event.seq))
        .transpose()?
        .unwrap_or_default();
    ensure!(
        checkpoint.sequence == event_tail,
        "final checkpoint does not cover the journal tail"
    );
    let checkpoint_state_bytes =
        echo_agent::utils::canonical_json::canonical_json_bytes(&checkpoint.state)?;
    let checkpoint_state_hash = hex::encode(Sha256::digest(checkpoint_state_bytes));
    let run_state_bytes = serde_json::to_vec(&snapshot)?;
    Ok(FinalEvidence {
        run_status: snapshot.run.status.as_str().to_string(),
        active_turn: snapshot
            .continuation
            .as_ref()
            .is_some_and(|state| state.active_turn.is_some()),
        goal_sha256: expected_goal_sha256.to_string(),
        event_log_sha256: hex::encode(Sha256::digest(event_bytes)),
        checkpoint_state_hash,
        run_state_sha256: hex::encode(Sha256::digest(run_state_bytes)),
    })
}

fn write_ledger(path: &Path, ledger: &SoakLedger) -> Result<()> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("soak ledger has no parent directory"))?;
    std::fs::create_dir_all(parent)?;
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("tmp.{}.{}", std::process::id(), unique));
    let write_result = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp)?;
        serde_json::to_writer_pretty(&mut file, ledger)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)?;
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    write_result
}

fn git_repo_root() -> Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("run git rev-parse --show-toplevel")?;
    ensure!(
        output.status.success(),
        "current directory is not a git repo"
    );
    let root = String::from_utf8(output.stdout)?.trim().to_string();
    ensure!(!root.is_empty(), "git returned an empty repository root");
    Ok(PathBuf::from(root))
}

fn git_head(repo_root: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_root)
        .output()
        .context("read soak commit")?;
    ensure!(output.status.success(), "git rev-parse HEAD failed");
    let commit = String::from_utf8(output.stdout)?.trim().to_string();
    ensure!(!commit.is_empty(), "git returned an empty commit");
    Ok(commit)
}

fn ensure_clean_worktree(repo_root: &Path) -> Result<()> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repo_root)
        .output()
        .context("inspect soak worktree")?;
    ensure!(output.status.success(), "git status failed");
    ensure!(
        output.stdout.is_empty(),
        "soak requires a clean committed worktree"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn short_driver_exercises_real_store_and_boot_recovery() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let task_root = temp.path().join("tasks");
        let mut ledger = SoakLedger::new(
            12,
            43_200_000,
            "m5-soak-test".to_string(),
            "test".to_string(),
        );
        let mut store = open_runtime(&task_root, &mut ledger).await?;
        let goal_sha256 = task_goal_sha256(GOAL);
        for _ in 0..3 {
            run_cycle(&store, &ledger.run_id)?;
        }
        let before = validate_runtime(&store, &ledger.run_id, &goal_sha256)?;
        assert_eq!(before.turns_ended, 3);
        assert_eq!(before.tokens_used, 9);
        ledger.runtime_reopens = ledger.runtime_reopens.saturating_add(1);
        store = open_runtime(&task_root, &mut ledger).await?;
        assert_eq!(ledger.recoveries, 1);
        let recovered = validate_runtime(&store, &ledger.run_id, &goal_sha256)?;
        assert_eq!(recovered.turns_ended, 3);
        assert!(store.request_pause(&ledger.run_id)?);
        let evidence = final_evidence(&store, &task_root, &ledger.run_id, &goal_sha256)?;
        assert_eq!(evidence.run_status, "paused");
        assert!(!evidence.active_turn);
        Ok(())
    }
}
