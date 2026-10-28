// 只由原普通历史测试模块include；不提供产品API或第二执行器。
use echo_agent::state::journal::CheckpointStore as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Instant;
use tracing_subscriber::layer::SubscriberExt as _;

mod harness {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/echo-agent-app-core/tests/support/performance_baseline.rs"
    ));
}

#[derive(Serialize, Deserialize)]
struct Dataset {
    case: String,
    fixed_root: PathBuf,
    source_root: PathBuf,
    registry_root: PathBuf,
    transcript_root: PathBuf,
    log_root: PathBuf,
    stream_directory: PathBuf,
    journal_identity: String,
    projection_through_at_seed: Option<u64>,
    project_root: PathBuf,
    workspace_id: String,
    conversation_id: String,
    objects: usize,
    events: u64,
    checkpoint_variant: String,
    tail: u64,
    retained_floor: u64,
    original_rows: Vec<StoredMessage>,
    selected_rows: Vec<StoredMessage>,
    expected_gui: Value,
    expected_fork: Value,
    original_publications: Vec<ChatEventEnvelope>,
    canonical_sha256: String,
    projection_files: BTreeMap<String, String>,
    units: Value,
}

fn argument(name: &str) -> anyhow::Result<String> {
    harness::required(name).map_err(|error| anyhow::anyhow!(error.to_string()))
}

fn fixed_root() -> anyhow::Result<PathBuf> {
    harness::root().map_err(|error| anyhow::anyhow!(error.to_string()))
}

fn canonical_sha<T: Serialize>(value: &T) -> anyhow::Result<String> {
    Ok(sha(
        &echo_agent::utils::canonical_json::canonical_json_bytes(value)?,
    ))
}

fn expected_views(
    rows: Vec<StoredMessage>,
    workspace: &str,
    conversation: &str,
    original: &[ChatEventEnvelope],
) -> anyhow::Result<Value> {
    let facts = vec![original.to_vec(); rows.len()];
    let views = project_conversation_messages(
        rows,
        None,
        &std::collections::HashSet::new(),
        Some((workspace, conversation, &facts)),
    )
    .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    Ok(serde_json::to_value(views)?)
}

// 只观测派生文件字节；validate不会打开journal或调用可修复projection的查询。
fn projection_files(log_root: &Path) -> anyhow::Result<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    for stream in std::fs::read_dir(log_root)? {
        let path = stream?.path().join("native-publications");
        if !path.is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(&path)? {
            let file = entry?.path();
            anyhow::ensure!(
                file.is_file() && !file.is_symlink(),
                "派生夹具必须是普通文件"
            );
            let relative = file.strip_prefix(log_root)?.to_string_lossy().into_owned();
            result.insert(relative, sha(&std::fs::read(file)?));
        }
    }
    Ok(result)
}

fn source_stream_directory(log_root: &Path) -> anyhow::Result<PathBuf> {
    let streams = std::fs::read_dir(log_root)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    anyhow::ensure!(
        streams.len() == 1,
        "本单conversation夹具必须只有一个原stream目录"
    );
    Ok(streams.into_iter().next().context("原stream目录缺失")?)
}

fn coverage_point(stream: &Path) -> anyhow::Result<(String, u64)> {
    // 用原通用CheckpointStore读取并校验frame；不另写digest/恢复器，也不修复缺文件。
    let store = echo_agent::state::journal::FileCheckpointStore::<Value>::open(
        stream.join("native-publications/coverage.json"),
    );
    let frame = store.load()?.context("原coverage尚未完整结算")?;
    anyhow::ensure!(
        frame
            .state
            .get("first_observed_sequence")
            .and_then(Value::as_u64)
            == Some(1),
        "原coverage没有覆盖from-1前缀"
    );
    Ok((frame.journal_identity.as_str().to_string(), frame.sequence))
}

fn physical_retention(stream: &Path) -> anyhow::Result<(usize, Option<u64>)> {
    let mut segments = 0_usize;
    for entry in std::fs::read_dir(stream)? {
        let path = entry?.path();
        if path.extension().and_then(|part| part.to_str()) == Some("jsonl") {
            let name = path
                .file_stem()
                .and_then(|part| part.to_str())
                .context("原segment名称非UTF-8")?;
            anyhow::ensure!(
                name.chars().count() == 20 && name.chars().all(|ch| ch.is_ascii_digit()),
                "不是原数字journal segment"
            );
            segments = segments.checked_add(1).context("segment计数溢出")?;
        }
    }
    let marker = stream.join(".retained-floor.json");
    let floor = match std::fs::read(marker) {
        Ok(bytes) => {
            let value: Value = serde_json::from_slice(&bytes)?;
            anyhow::ensure!(
                value.get("cleanup_pending").and_then(Value::as_bool) == Some(false),
                "物理retention仍待结算，不能在oracle重试后冒成功"
            );
            Some(
                value
                    .get("retained_floor")
                    .and_then(Value::as_u64)
                    .context("retention floor缺失")?,
            )
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    Ok((segments, floor))
}

fn assert_recovery_point(
    dataset: &Dataset,
    log: &ChatEventLog,
    safe_point: bool,
) -> anyhow::Result<()> {
    // 必须先直接观察结算，再调用可能触发barrier/cleanup重试的原replay/pub oracle。
    let (segments, marker_floor) = physical_retention(&dataset.stream_directory)?;
    anyhow::ensure!(
        segments > 0 && segments <= ChatEventRetention::default().max_segments,
        "原默认physical prune未结算"
    );
    let (identity, through) = coverage_point(&dataset.stream_directory)?;
    let head = if safe_point {
        dataset.events.checked_add(1).context("序号溢出")?
    } else {
        dataset.events
    };
    anyhow::ensure!(
        identity == dataset.journal_identity && through == head,
        "原generation/head/coverage不符"
    );
    let replay = log.replay(
        &dataset.workspace_id,
        Some(&dataset.conversation_id),
        &dataset.conversation_id,
        0,
    )?;
    anyhow::ensure!(
        replay.latest_cursor == head,
        "same-head场景出现未注册的新append"
    );
    let floor = replay
        .retained_earliest_cursor
        .context("retention floor缺失")?;
    anyhow::ensure!(
        !(safe_point || dataset.retained_floor > 1) || marker_floor.is_some(),
        "已prune输入缺原retention marker"
    );
    anyhow::ensure!(
        marker_floor.is_none_or(|marker| marker == floor),
        "原marker与原authority floor不同"
    );
    if safe_point {
        anyhow::ensure!(
            floor
                > dataset
                    .original_publications
                    .last()
                    .map_or(0, |event| event.sequence),
            "safe-point没有真实默认prune"
        );
    } else {
        anyhow::ensure!(
            floor == dataset.retained_floor,
            "原已冻结floor发生未注册改变"
        );
    }
    Ok(())
}

async fn read_selected_rows(
    dataset: &Dataset,
) -> anyhow::Result<(Vec<StoredMessage>, Vec<StoredMessage>)> {
    let store = FileConversationStore::new(&dataset.transcript_root)?;
    let rows = store.get_messages(&dataset.conversation_id).await?;
    let mut selected = Vec::new();
    for wanted in &dataset.selected_rows {
        anyhow::ensure!(wanted.id.is_some(), "真实store未分配稳定Message身份");
        let matches = rows
            .iter()
            .filter(|row| row.id == wanted.id && row.conversation_id == wanted.conversation_id)
            .collect::<Vec<_>>();
        anyhow::ensure!(matches.len() == 1, "原canonical行缺失或身份重复");
        selected.push((*matches.first().context("原canonical行缺失")?).clone());
    }
    Ok((rows, selected))
}

async fn selected_rows(dataset: &Dataset) -> anyhow::Result<Vec<StoredMessage>> {
    let (rows, selected) = read_selected_rows(dataset).await?;
    anyhow::ensure!(
        canonical_sha(&rows)? == dataset.canonical_sha256,
        "完整canonical源已改变"
    );
    anyhow::ensure!(
        canonical_sha(&selected)? == canonical_sha(&dataset.selected_rows)?,
        "原选定Message逐字改变"
    );
    Ok(selected)
}

fn append_default_retention_load(
    log: &ChatEventLog,
    workspace: &str,
    conversation: &str,
    safe_points: bool,
) -> anyhow::Result<()> {
    let retention = ChatEventRetention::default();
    let bytes = usize::try_from(retention.segment_rollover_bytes)?
        .checked_div(2)
        .context("默认segment size不可分")?;
    let payload = "x".repeat(bytes);
    let identity = EventIdentity::for_chat(
        Some(conversation.into()),
        "perf-retention-root",
        "perf-retention-root",
        None,
    )?
    .with_execution_id("perf-retention-execution")?;
    let mut sequence = 0_u64;
    for _ in 0..retention
        .max_segments
        .checked_add(3)
        .context("段计数溢出")?
    {
        for _ in 0..3 {
            sequence = sequence.checked_add(1).context("事件序号溢出")?;
            let envelope = echo_agent::agent::EventEnvelope::new(
                &identity,
                sequence,
                None,
                echo_agent::agent::AgentEvent::Token(payload.clone()),
            )?;
            log.append(
                workspace,
                Some(conversation),
                "perf-retention-root",
                ChatDriverEvent::Agent(Box::new(envelope)),
            )?;
        }
        if safe_points {
            append_safe_point(log, workspace, conversation)?;
        }
    }
    Ok(())
}

fn append_safe_point(
    log: &ChatEventLog,
    workspace: &str,
    conversation: &str,
) -> anyhow::Result<ChatEventEnvelope> {
    Ok(log.append(
        workspace,
        Some(conversation),
        "perf-retention-root",
        ChatDriverEvent::TurnConfiguration {
            permission_mode: "default".into(),
            approval_policy: "on-request".into(),
            attachments: Vec::new(),
        },
    )?)
}

async fn seed(root: &Path) -> anyhow::Result<()> {
    harness::empty_seed_root(root).map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let case = argument("EKO_PERF_CASE")?;
    anyhow::ensure!(
        [
            "native-history-gui",
            "native-history-fork",
            "native-history-safe-point"
        ]
        .contains(&case.as_str()),
        "未注册的native历史case"
    );
    let objects = harness::usize_arg("EKO_PERF_OBJECTS")
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    anyhow::ensure!(
        [1, 2].contains(&objects),
        "native轴只允许原真实final1/progress+final2"
    );
    if case != "native-history-gui" {
        anyhow::ensure!(objects == 2, "Fork/safe-point保完整原始两条native Message");
    }
    let variant = argument("EKO_PERF_CHECKPOINT")?;
    anyhow::ensure!(
        ["valid", "missing", "tail"].contains(&variant.as_str()),
        "损坏/foreign保独立正确性反控，不进成功负载"
    );
    let target = argument("EKO_PERF_EVENTS")?.parse::<u64>()?;
    let tail = argument("EKO_PERF_TAIL")?.parse::<u64>()?;
    anyhow::ensure!(
        if variant == "tail" {
            target == 8193 && tail == 4096
        } else {
            tail == 0
        },
        "非法native tail协议"
    );
    if case == "native-history-safe-point" {
        anyhow::ensure!(
            target == 0 && variant == "valid",
            "safe-point必须显式使用events0/default-retention输入"
        );
    }
    let nested = tempfile::Builder::new()
        .prefix("actual-source-")
        .tempdir_in(root)?;
    let fixture = StatuslessPublicationFixture::real_source_with_root(nested).await?;
    let (final_row, _, _) = source_retention_loader_baseline(&fixture)?;
    let selected = if objects == 1 {
        vec![final_row]
    } else {
        vec![fixture.row.clone(), final_row]
    };
    let source_root = fixture._root.path().to_path_buf();
    let workspace = fixture.workspace.id.to_string();
    let conversation = fixture.conversation.to_string();
    let expected_gui = expected_views(
        selected.clone(),
        &workspace,
        &conversation,
        &fixture.journal,
    )?;
    let prefix = branch_prefix(&fixture.seed.rows, 0, "performance-reply-prefix", true)
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let expected_fork = expected_views(prefix, &workspace, &conversation, &fixture.journal)?;
    let log = Arc::new(ChatEventLog::open(
        &fixture.log_path,
        ChatEventRetention::default(),
    )?);
    if case == "native-history-safe-point" || target == 0 {
        anyhow::ensure!(
            variant == "valid" && tail == 0,
            "默认retention输入使用valid协议"
        );
        append_default_retention_load(
            &log,
            &workspace,
            &conversation,
            case != "native-history-safe-point",
        )?;
    } else {
        anyhow::ensure!(
            [4097, 8193].contains(&target),
            "native冷前缀只冻结4097/8193代表点"
        );
        let before = log.replay(&workspace, Some(&conversation), &conversation, 0)?;
        let cut = target.checked_sub(tail).context("tail超过head")?;
        anyhow::ensure!(before.latest_cursor < cut, "原producer前缀超过声明cut");
        for sequence in before.latest_cursor..target {
            log.append(
                &workspace,
                Some(&conversation),
                "later-cold-history-root",
                ChatDriverEvent::TurnStatus {
                    status: "running".into(),
                },
            )?;
            if variant == "tail" && sequence.saturating_add(1) == cut {
                for row in &selected {
                    let message = echo_agent::memory::restore_message(row)?;
                    log.native_history_publications_async(
                        &workspace,
                        &conversation,
                        &message,
                        &echo_agent::agent::CancellationToken::new(),
                    )
                    .await?;
                }
            }
        }
        if variant == "valid" {
            for row in &selected {
                let message = echo_agent::memory::restore_message(row)?;
                log.native_history_publications_async(
                    &workspace,
                    &conversation,
                    &message,
                    &echo_agent::agent::CancellationToken::new(),
                )
                .await?;
            }
        }
    }
    let replay = log.replay(&workspace, Some(&conversation), &conversation, 0)?;
    if case != "native-history-safe-point" && target > 0 {
        anyhow::ensure!(
            replay.latest_cursor == target,
            "真实journal记录数不等于声明的native规模"
        );
    }
    let floor = replay
        .retained_earliest_cursor
        .context("原journal floor缺失")?;
    if target == 0 && case != "native-history-safe-point" {
        anyhow::ensure!(
            floor
                > fixture
                    .journal
                    .last()
                    .context("原publication为空")?
                    .sequence,
            "必须证明默认retention真实裁过原pub"
        );
    } else {
        anyhow::ensure!(floor == 1, "未裁场景原前缀必须完整");
    }
    drop(log);
    let stream_directory = source_stream_directory(&fixture.log_path)?;
    let (journal_identity, checkpoint_through) = coverage_point(&stream_directory)?;
    if case != "native-history-safe-point" {
        let (segments, _) = physical_retention(&stream_directory)?;
        anyhow::ensure!(
            segments <= ChatEventRetention::default().max_segments,
            "seed的physical retention尚未结算"
        );
    }
    if variant == "missing" {
        for entry in std::fs::read_dir(&fixture.log_path)? {
            let derived = entry?.path().join("native-publications");
            if derived.is_dir() {
                std::fs::remove_dir_all(derived)?;
            }
        }
        anyhow::ensure!(
            projection_files(&fixture.log_path)?.is_empty(),
            "首次采用必须没有派生projection"
        );
    }
    let dataset = Dataset {
        case,
        fixed_root: root.into(),
        source_root: source_root.clone(),
        registry_root: source_root.join("registry"),
        transcript_root: core::workspace::layout::WorkspaceLayout::state_dir(
            &fixture.workspace.root,
        ),
        log_root: fixture.log_path.clone(),
        stream_directory,
        journal_identity,
        projection_through_at_seed: if variant == "missing" {
            None
        } else {
            Some(checkpoint_through)
        },
        project_root: fixture.workspace.root.clone(),
        workspace_id: workspace,
        conversation_id: conversation,
        objects,
        events: replay.latest_cursor,
        checkpoint_variant: variant,
        tail,
        retained_floor: floor,
        canonical_sha256: canonical_sha(&fixture.seed.rows)?,
        original_rows: fixture.seed.rows.clone(),
        selected_rows: selected,
        expected_gui,
        expected_fork,
        original_publications: fixture.journal.clone(),
        projection_files: projection_files(&fixture.log_path)?,
        units: json!({"objects":"eligible canonical assistant Messages; independent real source rows", "events":"ordinary ChatEventLog records, not TaskRun journal", "network":"offline existing ResourceChainModel/MockTool seed only; no model calls in measured caller", "native_render":"not measured; no App/WebView/IPC/paint claim", "io":"test-utils private trace logical attempt/success/failure; instrumentation overhead included"}),
    };
    harness::write_json(&root.join("fixture.json"), &dataset)
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    // 只能保留本次EKO_PERF_ROOT里的真实生成树；下次进程恢复原绝对路径。
    let _kept = fixture._root.keep();
    Ok(())
}

#[derive(Clone, Default, Serialize)]
struct IoCount {
    attempted: u64,
    succeeded: u64,
    failed: u64,
}
type IoCounts = BTreeMap<String, IoCount>;

#[derive(Clone)]
struct IoCollector {
    root: PathBuf,
    counts: Arc<std::sync::Mutex<IoCounts>>,
}
#[derive(Default)]
struct IoFields {
    path: String,
    operation: String,
    stage: String,
}
impl tracing::field::Visit for IoFields {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        match field.name() {
            "path" => self.path = value.into(),
            "operation" => self.operation = value.into(),
            "stage" => self.stage = value.into(),
            _ => {}
        }
    }
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "path" {
            self.path = format!("{value:?}");
        }
    }
}
impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for IoCollector {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        if event.metadata().target() != "eko::native_history::perf_io" {
            return;
        }
        let mut fields = IoFields::default();
        event.record(&mut fields);
        if !Path::new(&fields.path).starts_with(&self.root) {
            return;
        }
        let mut counts = self
            .counts
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let entry = counts.entry(fields.operation).or_default();
        match fields.stage.as_str() {
            "attempt" => entry.attempted = entry.attempted.saturating_add(1),
            "success" => entry.succeeded = entry.succeeded.saturating_add(1),
            "failure" => entry.failed = entry.failed.saturating_add(1),
            _ => {}
        }
    }
}
impl IoCollector {
    fn snapshot(&self) -> IoCounts {
        self.counts
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
    fn since(&self, before: &IoCounts) -> anyhow::Result<IoCounts> {
        let mut delta = self.snapshot();
        for (operation, count) in &mut delta {
            let old = before.get(operation).cloned().unwrap_or_default();
            count.attempted = count
                .attempted
                .checked_sub(old.attempted)
                .context("I/O计数倒退")?;
            count.succeeded = count
                .succeeded
                .checked_sub(old.succeeded)
                .context("I/O计数倒退")?;
            count.failed = count
                .failed
                .checked_sub(old.failed)
                .context("I/O计数倒退")?;
            anyhow::ensure!(
                count.attempted == count.succeeded.saturating_add(count.failed),
                "I/O操作未真实结算"
            );
            anyhow::ensure!(count.failed == 0, "成功样本中存在I/O失败");
        }
        anyhow::ensure!(
            delta
                .get("coverage_load")
                .is_some_and(|count| count.succeeded > 0),
            "缺原test-utils观测，不能以0冒无I/O"
        );
        Ok(delta)
    }
}

async fn assert_publications(dataset: &Dataset, log: &Arc<ChatEventLog>) -> anyhow::Result<()> {
    let originals = dataset
        .original_publications
        .iter()
        .map(|event| Ok((event.event_id.clone(), canonical_sha(event)?)))
        .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
    anyhow::ensure!(
        originals.len() == dataset.original_publications.len(),
        "原pub oracle存在重复事件身份"
    );
    for row in &dataset.selected_rows {
        let message = echo_agent::memory::restore_message(row)?;
        let events = log
            .native_history_publications_async(
                &dataset.workspace_id,
                &dataset.conversation_id,
                &message,
                &echo_agent::agent::CancellationToken::new(),
            )
            .await?;
        let mut seen = BTreeSet::new();
        for event in &events {
            anyhow::ensure!(seen.insert(&event.event_id), "原pub重复");
            anyhow::ensure!(
                originals.get(&event.event_id) == Some(&canonical_sha(event)?),
                "原pub完整envelope或身份改变"
            );
        }
        let actual = project_stored_message_with_publications(
            row.clone(),
            Some(&dataset.workspace_id),
            Some(&dataset.conversation_id),
            &events,
        )
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let expected = project_stored_message_with_publications(
            row.clone(),
            Some(&dataset.workspace_id),
            Some(&dataset.conversation_id),
            &dataset.original_publications,
        )
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        anyhow::ensure!(
            serde_json::to_value(actual)? == serde_json::to_value(expected)?,
            "原完整pub选择/最终确认发生变化"
        );
    }
    selected_rows(dataset).await?;
    Ok(())
}

fn emit(
    dataset: &Dataset,
    mode: &str,
    phase: &str,
    index: usize,
    elapsed: u128,
    counts: IoCounts,
    boundary: &str,
) -> anyhow::Result<()> {
    harness::emit(&format!("native-history/{}/complete-caller", dataset.case), mode, phase, index, elapsed, "success", json!({
        "ok":true,"scope":boundary,"objects":dataset.objects,"events":dataset.events,"checkpoint":dataset.checkpoint_variant,"tail":dataset.tail,
        "canonical_sha256":dataset.canonical_sha256,"canonical_pub_dto_checked":true,"io_attempt_success_failure":counts,"io_units":"logical calls; not OS syscalls/physical bytes","io_instrumentation":"existing test-utils private trace; collector overhead included","external_model_services":"excluded by caller path; not an OS network observer","webview_paint":"not measured"
    })).map_err(|error| anyhow::anyhow!(error.to_string()))
}

async fn gui_sample(dataset: &Dataset, io: &IoCollector, mode: &str) -> anyhow::Result<()> {
    let cancel = echo_agent::agent::CancellationToken::new();
    if mode == "process-cold" {
        anyhow::ensure!(
            projection_files(&dataset.log_root)? == dataset.projection_files,
            "cold输入已被预修复"
        );
        let before = io.snapshot();
        let timer = Instant::now();
        let (original, rows) = read_selected_rows(dataset).await?;
        let log = Arc::new(ChatEventLog::open(
            &dataset.log_root,
            ChatEventRetention::default(),
        )?);
        let views = project_conversation_messages_with_native_history(
            rows,
            None,
            &std::collections::HashSet::new(),
            &dataset.workspace_id,
            &dataset.conversation_id,
            &log,
            &cancel,
        )
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let elapsed = timer.elapsed().as_nanos();
        let counts = io.since(&before)?;
        assert_recovery_point(dataset, &log, false)?;
        anyhow::ensure!(
            canonical_sha(&original)? == dataset.canonical_sha256,
            "cold完整canonical源已改变"
        );
        anyhow::ensure!(
            serde_json::to_value(views)? == dataset.expected_gui,
            "cold完整DTO不符"
        );
        assert_publications(dataset, &log).await?;
        return emit(
            dataset,
            mode,
            &argument("EKO_PERF_PHASE")?,
            argument("EKO_PERF_SAMPLE_INDEX")?.parse()?,
            elapsed,
            counts,
            "fresh process FileConversationStore/open+read+canonical selection+ChatEventLog/open+actual GUI history helper complete await; validation after timer; no Tauri IPC/workspace fences/WebView",
        );
    }
    anyhow::ensure!(mode == "hot", "GUI历史仅支持原hot/process-cold协议");
    let rows = selected_rows(dataset).await?;
    let log = Arc::new(ChatEventLog::open(
        &dataset.log_root,
        ChatEventRetention::default(),
    )?);
    project_conversation_messages_with_native_history(
        rows.clone(),
        None,
        &std::collections::HashSet::new(),
        &dataset.workspace_id,
        &dataset.conversation_id,
        &log,
        &cancel,
    )
    .await
    .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    for (phase, samples) in [("warmup", 5_usize), ("measurement", 30_usize)] {
        for index in 0..samples {
            let input = rows.clone();
            let before = io.snapshot();
            let timer = Instant::now();
            let views = project_conversation_messages_with_native_history(
                input,
                None,
                &std::collections::HashSet::new(),
                &dataset.workspace_id,
                &dataset.conversation_id,
                &log,
                &cancel,
            )
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            let elapsed = timer.elapsed().as_nanos();
            let counts = io.since(&before)?;
            assert_recovery_point(dataset, &log, false)?;
            anyhow::ensure!(
                serde_json::to_value(views)? == dataset.expected_gui,
                "hot完整DTO不符"
            );
            assert_publications(dataset, &log).await?;
            emit(
                dataset,
                mode,
                phase,
                index,
                elapsed,
                counts,
                "same-head same Arc<ChatEventLog>; actual GUI helper complete await; initial catchup/input clone/oracle excluded; validation after timer; instrumented test-utils, no IPC/WebView",
            )?;
        }
    }
    Ok(())
}

async fn fork_sample(dataset: &Dataset, io: &IoCollector, mode: &str) -> anyhow::Result<()> {
    let before_setup = projection_files(&dataset.log_root)?;
    let registry = Arc::new(core::workspace::registry::WorkspaceRegistry::with_base_dir(
        dataset.registry_root.clone(),
    )?);
    let app = AppFixture::open(
        &dataset.source_root.join("perf-app"),
        registry,
        &dataset.log_root,
        &dataset.project_root,
    )
    .await?;
    anyhow::ensure!(
        projection_files(&dataset.log_root)? == before_setup,
        "Fork boot提前修复了冷投影"
    );
    let result = async {
        let control = app.app.workspace_control_for_scope(&dataset.workspace_id).await?;
        let runtime = control.runtime();
        anyhow::ensure!(projection_files(&dataset.log_root)? == before_setup, "Fork workspace装配提前修复了冷投影");
        let phases = if mode == "hot" { vec![("warmup".to_string(), 5_usize), ("measurement".to_string(), 30_usize)] } else {
            anyhow::ensure!(mode == "process-cold", "Fork仅支持原hot/process-cold协议");
            vec![(argument("EKO_PERF_PHASE")?, 1)]
        };
        for (phase, samples) in phases {
            for ordinal in 0..samples {
                let index = if mode == "hot" { ordinal } else { argument("EKO_PERF_SAMPLE_INDEX")?.parse()? };
                let before = io.snapshot(); let timer = Instant::now();
                let (_, mut prefix, _) = reply_fork_prefix(&app.app, runtime, &dataset.conversation_id, 0, "performance-reply-prefix").await.map_err(|error| anyhow::anyhow!(error.to_string()))?;
                attach_reply_fork_native_history_imports(&app.app, runtime, &dataset.conversation_id, &mut prefix).await.map_err(|error| anyhow::anyhow!(error.to_string()))?;
                let elapsed = timer.elapsed().as_nanos(); let counts = io.since(&before)?;
                assert_recovery_point(dataset, &app.app.storage.chat_events, false)?;
                let views = project_conversation_messages(prefix.clone(), None, &std::collections::HashSet::new(), None).map_err(|error| anyhow::anyhow!(error.to_string()))?;
                anyhow::ensure!(serde_json::to_value(views)? == dataset.expected_fork, "Fork原pub/import完整DTO不符");
                for row in &prefix {
                    let imported = row.attachments_json.as_deref().and_then(AttachmentsPayload::parse).and_then(|payload| payload.native_history_import);
                    if let Some(import) = imported {
                        let message = echo_agent::memory::restore_message(row)?;
                        verify_native_history_import(&message, &import).map_err(|error| anyhow::anyhow!(error.to_string()))?;
                        for event in &import.events {
                            let actual = canonical_sha(event)?;
                            let originals = dataset.original_publications.iter().filter(|original| original.event_id == event.event_id).map(canonical_sha).collect::<anyhow::Result<Vec<_>>>()?;
                            anyhow::ensure!(originals.len() == 1 && originals.first() == Some(&actual), "Fork import原envelope不符");
                        }
                    }
                }
                assert_publications(dataset, &app.app.storage.chat_events).await?;
                emit(dataset, mode, &phase, index, elapsed, counts, "actual reply_fork_prefix+validate publication+attach_reply_fork_native_history_imports; App/Workspace bootstrap and close excluded; no target/worktree/copy/network/model execution")?;
            }
        }
        Ok::<(), anyhow::Error>(())
    }.await;
    let close = app.close().await;
    result?;
    close?;
    Ok(())
}

async fn safe_point_sample(dataset: &Dataset, io: &IoCollector, mode: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        mode == "process-cold",
        "变更型safe-point只用原fresh process10协议，不在同输入上重复追加"
    );
    let log = Arc::new(ChatEventLog::open(
        &dataset.log_root,
        ChatEventRetention::default(),
    )?);
    let (before_segments, _) = physical_retention(&dataset.stream_directory)?;
    anyhow::ensure!(
        before_segments > ChatEventRetention::default().max_segments,
        "safe-point输入已经被提前prune"
    );
    let before = io.snapshot();
    let timer = Instant::now();
    let event = append_safe_point(&log, &dataset.workspace_id, &dataset.conversation_id)?;
    let elapsed = timer.elapsed().as_nanos();
    let counts = io.since(&before)?;
    anyhow::ensure!(
        counts
            .get("retention_prune")
            .is_some_and(|count| count.succeeded > 0),
        "测量函数未完成真实retention，不能由oracle补完"
    );
    assert_recovery_point(dataset, &log, true)?;
    anyhow::ensure!(
        event.sequence == dataset.events.checked_add(1).context("序号溢出")?,
        "safe-point不是单次真实追加"
    );
    assert_publications(dataset, &log).await?;
    emit(
        dataset,
        mode,
        &argument("EKO_PERF_PHASE")?,
        argument("EKO_PERF_SAMPLE_INDEX")?.parse()?,
        elapsed,
        counts,
        "fresh-process original safe-point append includes lazy first stream open/verify/recovery+durability+publication checkpoint+default retention; facade constructor/setup and validation excluded",
    )
}

#[tokio::test]
#[ignore = "manual source-qualified original performance runner and explicit Root release/setup/quiet window"]
async fn performance_baseline_native_history() -> anyhow::Result<()> {
    harness::check_release().map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let root = fixed_root()?;
    anyhow::ensure!(
        core::data_root::user_data_dir() == root.join("global-data"),
        "原runner必须在本fixture内隔离EKO_DATA_DIR"
    );
    let action = argument("EKO_PERF_ACTION")?;
    let observation_root = if action == "seed" {
        root.clone()
    } else {
        let dataset: Dataset = harness::read_json(&root.join("fixture.json"))
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        anyhow::ensure!(
            dataset.fixed_root == root
                && dataset.source_root.starts_with(&root)
                && dataset.log_root.starts_with(&dataset.source_root),
            "原fixture绝对身份/日志根改变"
        );
        dataset.log_root
    };
    let io = IoCollector {
        root: observation_root,
        counts: Arc::new(std::sync::Mutex::new(BTreeMap::new())),
    };
    tracing::subscriber::set_global_default(tracing_subscriber::registry().with(io.clone()))
        .map_err(|error| anyhow::anyhow!("无法安装独占test-utils I/O观察: {error}"))?;
    match action.as_str() {
        "seed" => seed(&root).await,
        "validate" | "sample" => {
            let dataset: Dataset = harness::read_json(&root.join("fixture.json"))
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            anyhow::ensure!(dataset.fixed_root == root, "dataset绝对权威根改变");
            anyhow::ensure!(
                canonical_sha(&dataset.original_rows)? == dataset.canonical_sha256,
                "原canonical oracle摘要改变"
            );
            if argument("EKO_PERF_ACTION")? == "validate" {
                selected_rows(&dataset).await?;
                anyhow::ensure!(
                    projection_files(&dataset.log_root)? == dataset.projection_files,
                    "冷投影在freeze前被修复/篡改"
                );
                anyhow::ensure!(
                    expected_views(
                        dataset.selected_rows.clone(),
                        &dataset.workspace_id,
                        &dataset.conversation_id,
                        &dataset.original_publications
                    )? == dataset.expected_gui,
                    "原full-pub oracle不符"
                );
                return Ok(());
            }
            let (outer_phase, outer_index) =
                harness::phase_index().map_err(|error| anyhow::anyhow!(error.to_string()))?;
            let mode = argument("EKO_PERF_MODE")?;
            if mode == "hot" {
                anyhow::ensure!(
                    outer_phase == "measurement" && outer_index == 0,
                    "hot外层调用必须保持原协议"
                );
            }
            match dataset.case.as_str() {
                "native-history-gui" => gui_sample(&dataset, &io, &mode).await,
                "native-history-fork" => fork_sample(&dataset, &io, &mode).await,
                "native-history-safe-point" => safe_point_sample(&dataset, &io, &mode).await,
                _ => anyhow::bail!("未知原native历史case"),
            }
        }
        _ => anyhow::bail!("未知原runner action"),
    }
}
