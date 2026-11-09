//! Explicitly invoked fixture/measurement harness. No production benchmark API.
use echo_agent::tasks::{DefaultTaskToolPolicy, TaskRevisionService};
use echo_agent_app_core::api::tasks::task_runtime::revisioned_runtime::EkoTaskGraphStore;
use echo_agent_app_core::api::tasks::task_runtime::{
    AttendedMode, DomainProfile, ExecutionMode, PlanTask, PlanTaskKind, TaskPatch, TaskPlan,
    TaskRunStatus, TaskRuntimeStore, TaskUpdateOperation, TaskUpdateRequest, apply_task_update,
    commit_task_plan, task_goal_sha256,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::error::Error;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

#[path = "support/performance_baseline.rs"]
mod harness;
use harness::{required, root};
type TestResult<T = ()> = Result<T, Box<dyn Error>>;

#[derive(Serialize, Deserialize)]
struct Dataset {
    case: String,
    objects: usize,
    statuses: BTreeMap<String, String>,
    conversation_hit_ids: Vec<String>,
    fixed_root: String,
}

fn emit(
    scenario: &str,
    mode: &str,
    phase: &str,
    sample_index: usize,
    elapsed: u128,
    correctness: serde_json::Value,
) -> TestResult {
    harness::emit(
        scenario,
        mode,
        phase,
        sample_index,
        elapsed,
        "success",
        correctness,
    )
}

async fn seed_runs(path: &Path, objects: usize) -> TestResult<Dataset> {
    if ![10, 100, 1000].contains(&objects) {
        return Err("run count must be 10, 100 or 1000".into());
    }
    let store = TaskRuntimeStore::open_for_workspace(path.join("tasks"), "baseline")?;
    let mut statuses = BTreeMap::new();
    let mut conversation_hit_ids = Vec::new();
    for index in 0..objects {
        let id = format!("run-{index:06}");
        let conversation = if index % 2 == 0 {
            "conversation-hit"
        } else {
            "conversation-other"
        };
        store.create_run(
            &id,
            "baseline",
            conversation,
            &format!("message-{index}"),
            DomainProfile::General,
            "文件基准 🧪",
            "task",
            AttendedMode::Attended,
        )?;
        if index % 2 == 1 {
            store.transition_run(&id, TaskRunStatus::Running)?;
            store.transition_run(&id, TaskRunStatus::Paused)?;
        }
        let run = store.get_run(&id)?.ok_or("seeded run missing")?;
        statuses.insert(id.clone(), run.status.as_str().to_string());
        if conversation == "conversation-hit" {
            conversation_hit_ids.push(id);
        }
    }
    Ok(Dataset {
        case: "runs".to_string(),
        objects,
        statuses,
        conversation_hit_ids,
        fixed_root: path.to_string_lossy().into_owned(),
    })
}

async fn seed_gui(path: &Path, objects: usize) -> TestResult<Dataset> {
    if ![1, 10, 50, 100].contains(&objects) {
        return Err("GUI graph must contain 1, 10, 50 or 100 tasks".into());
    }
    let store = Arc::new(TaskRuntimeStore::open_for_workspace(
        path.join("tasks"),
        "baseline",
    )?);
    let run_id = "gui-run";
    let goal = "验证合法任务图的真实组件 🧪";
    store.create_run(
        run_id,
        "baseline",
        "gui-conversation",
        "gui-message",
        DomainProfile::General,
        goal,
        "task",
        AttendedMode::Attended,
    )?;
    let tasks = (0..objects)
        .map(|index| -> TestResult<PlanTask> {
            Ok(PlanTask {
                id: format!("task-{index:03}"),
                title: format!("基准任务 {index:03} 🧪"),
                description: "核验文件与任务投影".to_string(),
                kind: PlanTaskKind::Investigation,
                // Independent tasks have depth 1. They still pass the production
                // default max_tasks/max_depth validator through commit_task_plan.
                agent_role: "general".to_string(),
                sort_order: i64::try_from(index)?.saturating_add(1),
                ..Default::default()
            })
        })
        .collect::<TestResult<Vec<_>>>()?;
    let accepted = commit_task_plan(
        store.clone(),
        TaskPlan {
            plan_id: format!("gui-plan-{objects}"),
            run_id: run_id.to_string(),
            revision: 1,
            domain_profile: DomainProfile::General,
            domain_binding: None,
            goal_revision: 1,
            goal_sha256: task_goal_sha256(goal),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Parallel,
            tasks,
        },
    )
    .await?;
    assert_eq!(accepted.tasks.len(), objects);
    store.transition_run(run_id, TaskRunStatus::Running)?;
    store.transition_run(run_id, TaskRunStatus::Paused)?;
    let mut snapshots = Vec::new();
    let snapshot = |stage: &str| -> TestResult<serde_json::Value> {
        let run = store.get_run(run_id)?.ok_or("GUI run missing")?;
        let plan = store
            .get_plan_revision(run_id)?
            .ok_or("accepted GUI plan missing")?;
        let todos = store.list_todos(run_id)?;
        assert_eq!(plan.tasks.len(), objects);
        assert_eq!(todos.len(), objects);
        for (task, todo) in plan.tasks.iter().zip(todos.iter()) {
            assert_eq!(task.id, todo.task_id);
            assert_eq!(task.title, todo.title);
            assert_eq!(todo.run_id, run.run_id);
        }
        assert_eq!(run.goal_sha256, plan.goal_sha256);
        Ok(json!({"stage":stage,"run":run,"plan":plan,"todos":todos}))
    };
    snapshots.push(snapshot("initial")?);
    let service = TaskRevisionService::new(
        Arc::new(EkoTaskGraphStore::new(store.clone())),
        Arc::new(DefaultTaskToolPolicy::new(run_id.to_string())),
    );
    for (revision, stage, changed) in [(1, "single-row", 1), (2, "batch", objects)] {
        let operations = accepted
            .tasks
            .iter()
            .take(changed)
            .map(|task| TaskUpdateOperation::Update {
                task_id: task.id.clone(),
                patch: Box::new(TaskPatch {
                    title: Some(format!("{} · {stage}", task.title)),
                    ..Default::default()
                }),
            })
            .collect();
        let updated = apply_task_update(
            &service,
            store.clone(),
            run_id,
            TaskUpdateRequest {
                base_revision: revision,
                reason: format!("fixture {stage} update"),
                operations,
            },
        )
        .await?;
        assert_eq!(updated.revision, revision.saturating_add(1));
        snapshots.push(snapshot(stage)?);
    }
    store.transition_run(run_id, TaskRunStatus::Running)?;
    snapshots.push(snapshot("run-state")?);
    std::fs::write(
        path.join("gui.json"),
        serde_json::to_vec_pretty(&json!({
            "qualification":"commit_task_plan + TaskRevisionService.apply_patch",
            "objects":objects,"graph_depth":1,"max_tasks":100,"max_depth":10,
            "snapshots":snapshots,"subagent_history":0,"event_history_axis":"not varied",
        }))?,
    )?;
    Ok(Dataset {
        case: "gui".to_string(),
        objects,
        statuses: BTreeMap::new(),
        conversation_hit_ids: Vec::new(),
        fixed_root: path.to_string_lossy().into_owned(),
    })
}

fn check_runs(store: &TaskRuntimeStore, fixture: &Dataset, query: &str) -> TestResult<usize> {
    match query {
        "all" | "pending" => {
            let statuses = if query == "pending" {
                vec![TaskRunStatus::Pending]
            } else {
                vec![
                    TaskRunStatus::Pending,
                    TaskRunStatus::Running,
                    TaskRunStatus::Paused,
                    TaskRunStatus::Cancelled,
                    TaskRunStatus::Failed,
                    TaskRunStatus::Completed,
                ]
            };
            let rows = store.list_runs_in(&statuses)?;
            let expected = fixture
                .statuses
                .iter()
                .filter(|(_, status)| query == "all" || status.as_str() == "pending")
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>();
            let mut actual = rows
                .into_iter()
                .map(|run| {
                    assert_eq!(
                        fixture.statuses.get(&run.run_id).map(String::as_str),
                        Some(run.status.as_str())
                    );
                    run.run_id
                })
                .collect::<Vec<_>>();
            actual.sort();
            assert_eq!(actual, expected);
            Ok(actual.len())
        }
        "conversation-hit" | "conversation-miss" => {
            let rows = store.list_runs_for_conversation(query)?;
            let mut actual = rows.into_iter().map(|run| run.run_id).collect::<Vec<_>>();
            actual.sort();
            let expected = if query == "conversation-hit" {
                fixture.conversation_hit_ids.clone()
            } else {
                Vec::new()
            };
            assert_eq!(actual, expected);
            Ok(actual.len())
        }
        _ => Err("unknown run query".into()),
    }
}

fn sample_runs(path: &Path, fixture: &Dataset, mode: &str) -> TestResult {
    if mode == "process-cold" {
        // Constructing only the root does not open a RunAuthority. The first
        // real run-state and Todo reads are inside this timer as well.
        let started = Instant::now();
        let store = TaskRuntimeStore::open_for_workspace(path.join("tasks"), "baseline")?;
        let state = store
            .get_run_state("run-000000")?
            .ok_or("cold run state missing")?;
        let todos = store.list_todos("run-000000")?;
        let elapsed = started.elapsed().as_nanos();
        assert_eq!(state.run.run_id, "run-000000");
        assert_eq!(
            fixture.statuses.get(&state.run.run_id).map(String::as_str),
            Some(state.run.status.as_str())
        );
        assert!(todos.is_empty());
        let sequence = serde_json::to_value(state)?
            .get("journal_sequence")
            .and_then(serde_json::Value::as_u64)
            .ok_or("journal sequence missing")?;
        assert!(sequence > 0);
        emit(
            "task-runtime/root+first-authority+todos",
            mode,
            "measurement",
            required("EKO_PERF_SAMPLE_INDEX")?.parse()?,
            elapsed,
            json!({"ok":true,"run_id":"run-000000","journal_sequence":sequence,"durability":"SyncData","boot_reconciliation":"not_measured","os_page_cache":"uncontrolled"}),
        )?;
        return Ok(());
    }
    if mode != "hot" {
        return Err("mode must be hot or process-cold".into());
    }
    let store = TaskRuntimeStore::open_for_workspace(path.join("tasks"), "baseline")?;
    // An empty status vector is an original no-op control, not all runs.
    assert!(store.list_runs_in(&[])?.is_empty());
    for query in ["all", "pending", "conversation-hit", "conversation-miss"] {
        for ordinal in 0_usize..35 {
            let started = Instant::now();
            let returned = check_runs(&store, fixture, query)?;
            let elapsed = started.elapsed().as_nanos();
            let (phase, index) = if ordinal < 5 {
                ("warmup", ordinal)
            } else {
                ("measurement", ordinal - 5)
            };
            emit(
                &format!("task-runtime/{query}+validation"),
                mode,
                phase,
                index,
                elapsed,
                json!({"ok":true,"returned":returned,"runs":fixture.objects,"authority_cache_capacity":128}),
            )?;
        }
    }
    // 期望集合在计时外逐run读取建立；被测路径始终是原有限页查询，不能用legacy全Vec截断代替。
    let mut expected = fixture
        .statuses
        .keys()
        .map(|id| -> TestResult<_> {
            Ok(store
                .get_run(id)?
                .ok_or("formal page fixture run missing")?)
        })
        .collect::<TestResult<Vec<_>>>()?;
    expected.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| right.run_id.cmp(&left.run_id))
    });
    for (label, statuses) in [
        ("all", Vec::new()),
        ("pending", vec![TaskRunStatus::Pending]),
    ] {
        let filtered = expected
            .iter()
            .filter(|run| statuses.is_empty() || statuses.contains(&run.status))
            .collect::<Vec<_>>();
        let first =
            store.query_task_runs_page("synthetic-baseline-generation", &statuses, 20, None)?;
        for (page_name, offset, cursor) in [
            ("first", 0_usize, None),
            ("next", 20_usize, first.next_continuation.as_deref()),
        ] {
            if offset > 0 && cursor.is_none() {
                continue; // 没有下一页的合法小集合不把第一页重复记成续页样本。
            }
            for ordinal in 0_usize..35 {
                let started = Instant::now();
                let page = store.query_task_runs_page(
                    "synthetic-baseline-generation",
                    &statuses,
                    20,
                    cursor,
                )?;
                let end = offset.saturating_add(20).min(filtered.len());
                let expected_page = filtered
                    .get(offset..end)
                    .ok_or("formal page expectation out of range")?;
                assert!(page.items.len() <= 20);
                assert_eq!(
                    page.items
                        .iter()
                        .map(|run| run.run_id.as_str())
                        .collect::<Vec<_>>(),
                    expected_page
                        .iter()
                        .map(|run| run.run_id.as_str())
                        .collect::<Vec<_>>()
                );
                assert_eq!(page.next_continuation.is_some(), end < filtered.len());
                let elapsed = started.elapsed().as_nanos();
                let (phase, index) = if ordinal < 5 {
                    ("warmup", ordinal)
                } else {
                    ("measurement", ordinal - 5)
                };
                emit(
                    &format!("task-runtime/formal-page/{label}/{page_name}+validation"),
                    "hot",
                    phase,
                    index,
                    elapsed,
                    json!({"ok":true,"returned":page.items.len(),"page_limit":20,"runs":fixture.objects,"next_continuation":page.next_continuation.is_some(),"scope":"actual query_task_runs_page; synthetic workspace generation; same instance repeated query under LRU capacity; no Tauri IPC","authority_cache_capacity":128,"cache_condition":"1000-run sets can evict authority entries; not an all-cache-hit claim","expectation":"per-run headers outside timer","timing_includes":"query plus page identity/count/continuation validation","not_applicable_next_page":"when no continuation, no duplicate first-page row"}),
                )?;
            }
        }
    }
    Ok(())
}

struct AppObserver {
    writes: std::sync::atomic::AtomicUsize,
    hooks: echo_agent::evolution::HookEvolutionObserver,
}

impl Default for AppObserver {
    fn default() -> Self {
        Self {
            writes: std::sync::atomic::AtomicUsize::new(0),
            hooks: echo_agent::evolution::HookEvolutionObserver::new(
                Arc::new(tokio::sync::RwLock::new(
                    echo_agent::skills::hooks::HookRegistry::new(),
                )),
                "baseline-session",
                "baseline-agent",
            ),
        }
    }
}
impl echo_agent::evolution::EvolutionObserver for AppObserver {
    fn on_memory_write<'a>(
        &'a self,
        key: &'a str,
        source: &'a str,
    ) -> futures::future::BoxFuture<'a, ()> {
        Box::pin(async move {
            echo_agent::evolution::EvolutionObserver::on_memory_write(&self.hooks, key, source)
                .await;
            self.writes
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        })
    }
    fn on_memory_layer_change<'a>(
        &'a self,
        key: &'a str,
        from: &'a str,
        to: &'a str,
    ) -> futures::future::BoxFuture<'a, ()> {
        echo_agent::evolution::EvolutionObserver::on_memory_layer_change(&self.hooks, key, from, to)
    }
}

fn app_memory_history(path: &Path, mutations: u64) -> TestResult {
    use echo_agent::evolution::{ChangeFilter, ChangeLog, JsonlChangeLog};
    let operations =
        match std::fs::read_to_string(path.join(".eko/evolution/memory-operations.jsonl")) {
            Ok(text) => text,
            Err(error) if mutations == 0 && error.kind() == std::io::ErrorKind::NotFound => {
                String::new()
            }
            Err(error) => return Err(error.into()),
        };
    // Observe persisted facts after the original manager has reconciled. This
    // is not another decoder/reducer and never repairs operation records.
    let mut sequence = 0_u64;
    let mut prepared = 0_u64;
    let mut settled = 0_u64;
    let mut pending = std::collections::BTreeSet::new();
    for line in operations.lines() {
        let frame: serde_json::Value = serde_json::from_str(line)?;
        for record in frame
            .get("records")
            .and_then(serde_json::Value::as_array)
            .ok_or("operation records missing")?
        {
            sequence = sequence
                .checked_add(1)
                .ok_or("operation sequence overflow")?;
            assert_eq!(
                record.get("sequence").and_then(serde_json::Value::as_u64),
                Some(sequence)
            );
            let event = record.get("event").ok_or("operation event missing")?;
            if let Some(batch) = event.get("Prepared") {
                let id = batch
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .ok_or("prepare identity missing")?;
                assert!(pending.insert(id.to_string()));
                prepared = prepared.saturating_add(1);
            } else if let Some(receipt) = event.get("Settled") {
                let id = receipt
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .ok_or("settle identity missing")?;
                assert!(pending.remove(id));
                settled = settled.saturating_add(1);
            } else {
                return Err("unexpected memory operation event".into());
            }
        }
    }
    assert!(pending.is_empty());
    assert_eq!(prepared, mutations);
    assert_eq!(settled, mutations);
    let audit = JsonlChangeLog::new(path.join(".eko/evolution/change-log.jsonl"))?;
    assert_eq!(
        u64::try_from(audit.query(&ChangeFilter::new())?.len())?,
        mutations
    );
    Ok(())
}

async fn app_memory_owner(
    path: &Path,
) -> TestResult<(
    echo_agent_app_core::api::agent_handle::AgentHandle,
    echo_agent_app_core::api::evolution::ReviewIntegration,
)> {
    let store = Arc::new(echo_agent::memory::FileStore::new(
        path.join(".eko/memory/store.json"),
    )?);
    let integration =
        echo_agent_app_core::api::evolution::ReviewIntegration::new_scoped_with_project_root(
            echo_agent::evolution::ReviewConfig::default(),
            path.join(".eko"),
            store,
            "baseline-memory".to_string(),
            "baseline-generation".to_string(),
            Some(path.to_path_buf()),
        );
    // The real projection target is retained. No model request is made.
    let primary = echo_agent_app_core::api::agent_handle::AgentHandle::new(
        echo_agent::agent::ReactAgentBuilder::new()
            .model("baseline-offline")
            .llm_client(Arc::new(
                echo_agent::testing::MockLlmClient::new().with_model_name("baseline-offline"),
            ))
            .build()?,
    );
    integration.bind_rule_projection_primary(primary.clone());
    Ok((primary, integration))
}

async fn app_memory_sample(path: &Path, fixture: &serde_json::Value) -> TestResult {
    use echo_agent::memory::{
        MemoryEvidence, MemoryEvidenceRole, MemoryMeta, MemoryProvenance, MemorySource,
        MemoryStatus, MemoryTrust, MemoryType,
    };
    let fixed = fixture
        .get("fixed_root")
        .and_then(serde_json::Value::as_str)
        .ok_or("memory root missing")?;
    assert_eq!(fixed, path.to_string_lossy());
    let objects = fixture
        .get("objects")
        .and_then(serde_json::Value::as_u64)
        .ok_or("memory entry count missing")?;
    let history = fixture
        .get("memory_history_mutations")
        .and_then(serde_json::Value::as_u64)
        .ok_or("memory history count missing")?;
    if required("EKO_PERF_MODE")? != "memory-app" {
        return Err("application memory requires memory-app mode".into());
    }
    let (_primary, integration) = app_memory_owner(path).await?;
    let observer = Arc::new(AppObserver::default());
    integration.set_evolution_observer(observer.clone());
    let lease = integration.lease_generation()?;
    let manager = lease.layer_manager()?;
    manager.reconcile_pending().await?;
    let initial = lease.settle_hot_memory_projection().await;
    assert_eq!(
        initial.status,
        echo_agent_app_core::api::evolution::MemoryProjectionSettlementStatus::Settled
    );
    assert!(initial.primary_bound);
    assert_eq!(
        u64::try_from(
            lease
                .memory_store()
                .list(echo_agent::evolution::layer::WARM_NAMESPACE)
                .await?
                .len()
        )?,
        objects
    );
    app_memory_history(path, history)?;
    assert!(manager.locate("measured-draft").await?.is_none());
    let meta = MemoryMeta::new(
        MemoryType::ProjectFact,
        MemorySource::AutoExtracted,
        "基准 🧪",
    )
    .with_status(MemoryStatus::Draft)
    .with_provenance(MemoryProvenance::draft(
        MemoryTrust::User,
        vec![MemoryEvidence::new(
            MemoryEvidenceRole::User,
            "项目使用文件保存记忆",
        )],
    ));
    let started = Instant::now();
    let promotion = manager
        .write_memory("measured-draft", "应用结算事实 🧪", meta)
        .await?;
    let core_ns = started.elapsed().as_nanos();
    let settlement_started = Instant::now();
    let settled = lease.settle_hot_memory_projection().await;
    let settlement_ns = settlement_started.elapsed().as_nanos();
    let elapsed = started.elapsed().as_nanos();
    assert!(promotion.is_none());
    assert_eq!(
        settled.status,
        echo_agent_app_core::api::evolution::MemoryProjectionSettlementStatus::Settled
    );
    assert!(settled.primary_bound);
    assert!(settled.error.is_none());
    assert!(!settled.revision.is_empty());
    assert_eq!(settled.authority_scope, "baseline-memory");
    assert_eq!(settled.workspace_generation, "baseline-generation");
    assert_eq!(observer.writes.load(std::sync::atomic::Ordering::SeqCst), 1);
    let entry = manager
        .locate("measured-draft")
        .await?
        .ok_or("app Draft missing")?
        .1;
    assert_eq!(entry.content, "应用结算事实 🧪");
    assert_eq!(entry.meta.status, MemoryStatus::Draft);
    assert!(entry.meta.provenance.has_user_evidence());
    assert!(entry.meta.provenance.approval.is_none());
    assert_eq!(
        u64::try_from(
            lease
                .memory_store()
                .list(echo_agent::evolution::layer::WARM_NAMESPACE)
                .await?
                .len()
        )?,
        objects.saturating_add(1)
    );
    app_memory_history(path, history.saturating_add(1))?;
    let (phase, index) = harness::phase_index()?;
    harness::emit(
        "memory/full-write+EKO-generation-settlement",
        "fresh-copy",
        &phase,
        index,
        elapsed,
        "success",
        json!({"ok":true,"core_write_ns":core_ns.to_string(),"projection_settlement_ns":settlement_ns.to_string(),
            "settlement":settled,"draft":true,"promotion":"none","initial_entries":objects,"initial_mutation_history":history,
            "observer_writes":1,"hook_rules":0,"model_requests":0}),
    )
}

#[tokio::test]
#[ignore = "explicit coordinator lease and frozen input required; no default benchmark execution"]
async fn performance_baseline_eko() -> TestResult {
    harness::check_release()?;
    let path = root()?;
    match required("EKO_PERF_ACTION")?.as_str() {
        "seed" => {
            harness::empty_seed_root(&path)?;
            let objects = harness::usize_arg("EKO_PERF_OBJECTS")?;
            let fixture = match required("EKO_PERF_CASE")?.as_str() {
                "runs" => seed_runs(&path, objects).await?,
                "gui" => seed_gui(&path, objects).await?,
                _ => return Err("case must be runs or gui".into()),
            };
            harness::write_json(&path.join("fixture.json"), &fixture)?;
            Ok(())
        }
        "sample" => {
            let raw: serde_json::Value = harness::read_json(&path.join("fixture.json"))?;
            if raw.get("case").and_then(serde_json::Value::as_str) == Some("memory") {
                return app_memory_sample(&path, &raw).await;
            }
            let fixture: Dataset = serde_json::from_value(raw)?;
            assert_eq!(fixture.fixed_root, path.to_string_lossy());
            if fixture.case != "runs" {
                return Err(
                    "GUI snapshot measurement belongs to the actual RTL/native harness".into(),
                );
            }
            sample_runs(&path, &fixture, &required("EKO_PERF_MODE")?)
        }
        "validate" => {
            let raw: serde_json::Value = harness::read_json(&path.join("fixture.json"))?;
            if raw.get("case").and_then(serde_json::Value::as_str) == Some("memory") {
                assert_eq!(
                    raw.get("fixed_root").and_then(serde_json::Value::as_str),
                    Some(path.to_string_lossy().as_ref())
                );
                let (_primary, integration) = app_memory_owner(&path).await?;
                let lease = integration.lease_generation()?;
                lease.layer_manager()?.reconcile_pending().await?;
                let settled = lease.settle_hot_memory_projection().await;
                assert_eq!(
                    settled.status,
                    echo_agent_app_core::api::evolution::MemoryProjectionSettlementStatus::Settled
                );
                let count = raw
                    .get("objects")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or("memory entries missing")?;
                assert_eq!(
                    u64::try_from(
                        lease
                            .memory_store()
                            .list(echo_agent::evolution::layer::WARM_NAMESPACE)
                            .await?
                            .len()
                    )?,
                    count
                );
                app_memory_history(
                    &path,
                    raw.get("memory_history_mutations")
                        .and_then(serde_json::Value::as_u64)
                        .ok_or("memory history missing")?,
                )?;
            } else {
                let fixture: Dataset = serde_json::from_value(raw)?;
                assert_eq!(fixture.fixed_root, path.to_string_lossy());
                let store = TaskRuntimeStore::open_for_workspace(path.join("tasks"), "baseline")?;
                if fixture.case == "runs" {
                    for query in ["all", "pending", "conversation-hit", "conversation-miss"] {
                        check_runs(&store, &fixture, query)?;
                    }
                } else {
                    let value: serde_json::Value = harness::read_json(&path.join("gui.json"))?;
                    assert_eq!(
                        value.get("objects").and_then(serde_json::Value::as_u64),
                        Some(u64::try_from(fixture.objects)?)
                    );
                }
            }
            Ok(())
        }
        _ => Err("action must be seed, validate or sample".into()),
    }
}
