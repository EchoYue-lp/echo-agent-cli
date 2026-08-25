use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, ensure};
use clap::Parser;
use echo_agent::sandbox::SandboxManager;
use echo_agent::tools::cell::{
    CommandCellOwner, CommandCellPhase, CommandCellRegistry, CommandCellRequest,
};
use echo_agent_app_core::chat_driver::ChatDriverEvent;
use echo_agent_app_core::chat_event_log::{ChatEventLog, ChatEventRetention};
use echo_agent_app_core::tasks::task_runtime::command_cells::CommandCellRuntimeService;
use echo_agent_app_core::tasks::task_runtime::{
    ProcessExecutionResourceSnapshot, process_execution_resource_snapshot,
};
use echo_agent_app_core::workspace::{WorkspaceExecutionScope, WorkspaceId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const WORKSPACE_COUNT: usize = 3;
const CONVERSATIONS_PER_WORKSPACE: usize = 3;

#[derive(Debug, Parser)]
#[command(name = "lh6-concurrency-soak")]
struct Args {
    #[arg(long, default_value_t = 60)]
    duration_seconds: u64,
    #[arg(long)]
    output_dir: PathBuf,
}

#[derive(Clone)]
struct Address {
    workspace_id: String,
    conversation_id: String,
    registry: Arc<dyn CommandCellRegistry>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PeakResources {
    subagent_active: usize,
    write_active: usize,
    shell_active: usize,
    llm_active: usize,
}

impl PeakResources {
    fn observe(&mut self, snapshot: ProcessExecutionResourceSnapshot) -> Result<()> {
        ensure!(
            snapshot.within_limits(),
            "process resource governor exceeded its limit"
        );
        self.subagent_active = self.subagent_active.max(snapshot.subagent_active);
        self.write_active = self.write_active.max(snapshot.write_active);
        self.shell_active = self.shell_active.max(snapshot.shell_active);
        self.llm_active = self.llm_active.max(snapshot.llm_active);
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Ledger {
    schema_version: u32,
    status: String,
    commit: String,
    started_at: String,
    completed_at: Option<String>,
    required_active_millis: u64,
    active_elapsed_millis: u64,
    workspaces: usize,
    conversations: usize,
    cycles: u64,
    launches: u64,
    succeeded: u64,
    cancelled: u64,
    runtime_restarts: u64,
    routing_failures: u64,
    duplicate_terminal_failures: u64,
    resource_failures: u64,
    peak_resources: PeakResources,
    journal_sha256: Option<String>,
}

struct LedgerFailureGuard {
    path: PathBuf,
    armed: bool,
}

impl LedgerFailureGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for LedgerFailureGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Ok(bytes) = std::fs::read(&self.path) else {
            return;
        };
        let Ok(mut ledger) = serde_json::from_slice::<Ledger>(&bytes) else {
            return;
        };
        ledger.status = "failed".to_string();
        ledger.completed_at = Some(chrono::Utc::now().to_rfc3339());
        if let Ok(bytes) = serde_json::to_vec_pretty(&ledger) {
            let _ = echo_agent::utils::fs::atomic_write(&self.path, &bytes);
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(
        args.duration_seconds >= 60,
        "LH6 development concurrency smoke requires at least 60 active seconds"
    );
    let repo_root = git_repo_root()?;
    ensure_clean_worktree(&repo_root)?;
    let commit = git_head(&repo_root)?;
    std::fs::create_dir_all(&args.output_dir)
        .with_context(|| format!("create {}", args.output_dir.display()))?;
    let ledger_path = args.output_dir.join("ledger.json");
    ensure!(
        !ledger_path.exists(),
        "refusing to overwrite an existing LH6 ledger"
    );
    let journal_root = args.output_dir.join("chat-events");
    let chat_events = Arc::new(
        ChatEventLog::open(&journal_root, ChatEventRetention::default())
            .map_err(|error| anyhow!(error.to_string()))?,
    );
    let (mut service, mut product_data_io) = new_service(chat_events.clone())?;
    let mut addresses = build_addresses(&service);
    let started = std::time::Instant::now();
    let required_active_millis = args.duration_seconds.saturating_mul(1_000);
    let mut ledger = Ledger {
        schema_version: 1,
        status: "running".to_string(),
        commit,
        started_at: chrono::Utc::now().to_rfc3339(),
        completed_at: None,
        required_active_millis,
        active_elapsed_millis: 0,
        workspaces: WORKSPACE_COUNT,
        conversations: WORKSPACE_COUNT.saturating_mul(CONVERSATIONS_PER_WORKSPACE),
        cycles: 0,
        launches: 0,
        succeeded: 0,
        cancelled: 0,
        runtime_restarts: 0,
        routing_failures: 0,
        duplicate_terminal_failures: 0,
        resource_failures: 0,
        peak_resources: PeakResources::default(),
        journal_sha256: None,
    };
    write_ledger(&ledger_path, &ledger)?;
    let mut failure_guard = LedgerFailureGuard::new(ledger_path.clone());

    while u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX) < required_active_millis
    {
        ledger.cycles = ledger.cycles.saturating_add(1);
        run_cycle(&addresses, &chat_events, &mut ledger).await?;
        ledger.active_elapsed_millis =
            u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        write_ledger(&ledger_path, &ledger)?;

        if ledger.cycles.is_multiple_of(25) {
            service.shutdown().await.map_err(anyhow::Error::msg)?;
            product_data_io
                .join_shutdown()
                .await
                .map_err(anyhow::Error::msg)?;
            (service, product_data_io) = new_service(chat_events.clone())?;
            addresses = build_addresses(&service);
            ledger.runtime_restarts = ledger.runtime_restarts.saturating_add(1);
        }
        tokio::time::sleep(Duration::from_millis(750)).await;
    }

    service.shutdown().await.map_err(anyhow::Error::msg)?;
    product_data_io
        .join_shutdown()
        .await
        .map_err(anyhow::Error::msg)?;
    ledger.active_elapsed_millis = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    ensure!(
        ledger.routing_failures == 0,
        "routing failures were observed"
    );
    ensure!(
        ledger.duplicate_terminal_failures == 0,
        "duplicate terminal facts were observed"
    );
    ensure!(
        ledger.resource_failures == 0,
        "resource failures were observed"
    );
    ensure!(ledger.launches == ledger.succeeded.saturating_add(ledger.cancelled));
    ledger.journal_sha256 = Some(hash_tree(&journal_root)?);
    ledger.status = "passed".to_string();
    ledger.completed_at = Some(chrono::Utc::now().to_rfc3339());
    write_ledger(&ledger_path, &ledger)?;
    failure_guard.disarm();
    println!("{}", serde_json::to_string_pretty(&ledger)?);
    Ok(())
}

fn new_service(
    chat_events: Arc<ChatEventLog>,
) -> Result<(
    Arc<CommandCellRuntimeService>,
    echo_agent_app_core::product_data_io::ProductDataIoService,
)> {
    let product_data_io = echo_agent_app_core::product_data_io::ProductDataIoService::new();
    let service = CommandCellRuntimeService::new(
        Arc::new(SandboxManager::local_only()),
        chat_events,
        product_data_io.clone(),
    )
    .map_err(anyhow::Error::msg)?;
    Ok((service, product_data_io))
}

fn build_addresses(service: &Arc<CommandCellRuntimeService>) -> Vec<Address> {
    let mut addresses = Vec::new();
    for workspace in 0..WORKSPACE_COUNT {
        let workspace_id = WorkspaceId::from_name(&format!("lh6-workspace-{workspace}"));
        let scope = WorkspaceExecutionScope::workspace(&workspace_id, ".");
        let registry = service.scoped(scope, None);
        for conversation in 0..CONVERSATIONS_PER_WORKSPACE {
            addresses.push(Address {
                workspace_id: workspace_id.to_string(),
                conversation_id: format!("lh6-conversation-{workspace}-{conversation}"),
                registry: registry.clone(),
            });
        }
    }
    addresses
}

async fn run_cycle(
    addresses: &[Address],
    chat_events: &ChatEventLog,
    ledger: &mut Ledger,
) -> Result<()> {
    struct Accepted {
        address: Address,
        root_turn_id: String,
        cell_id: String,
        cancel_expected: bool,
    }

    let mut accepted = Vec::with_capacity(addresses.len());
    for (index, address) in addresses.iter().enumerate() {
        let root_turn_id = format!("lh6-root-{}-{index}", ledger.cycles);
        let cancel_expected = (ledger
            .cycles
            .saturating_add(u64::try_from(index).unwrap_or(0)))
            % 4
            == 0;
        let command = if cancel_expected {
            "printf begin; sleep 1; printf end"
        } else {
            "printf begin; sleep 0.2; printf end"
        };
        let receipt = address
            .registry
            .launch(CommandCellRequest {
                command: command.to_string(),
                timeout_secs: Some(5),
                owner: CommandCellOwner {
                    conversation_id: Some(address.conversation_id.clone()),
                    message_id: Some(root_turn_id.clone()),
                    call_id: Some(format!("lh6-call-{}-{index}", ledger.cycles)),
                    ..Default::default()
                },
                ..Default::default()
            })
            .await
            .map_err(|error| anyhow!(error.to_string()))?;
        ledger
            .peak_resources
            .observe(process_execution_resource_snapshot())?;
        if cancel_expected {
            ensure!(address.registry.stop(&receipt.cell_id));
        }
        ledger.launches = ledger.launches.saturating_add(1);
        accepted.push(Accepted {
            address: address.clone(),
            root_turn_id,
            cell_id: receipt.cell_id,
            cancel_expected,
        });
    }

    ledger
        .peak_resources
        .observe(process_execution_resource_snapshot())?;
    for item in &accepted {
        let mut cursor = 0_u64;
        let terminal = loop {
            let delta = item
                .address
                .registry
                .wait(&item.cell_id, cursor, 1_000)
                .await
                .map_err(|error| anyhow!(error.to_string()))?;
            cursor = delta.next_cursor;
            ledger
                .peak_resources
                .observe(process_execution_resource_snapshot())?;
            if delta.snapshot.phase.is_terminal() {
                break delta.snapshot;
            }
        };
        match terminal.phase {
            CommandCellPhase::Succeeded if !item.cancel_expected => {
                ledger.succeeded = ledger.succeeded.saturating_add(1);
            }
            CommandCellPhase::Cancelled if item.cancel_expected => {
                ledger.cancelled = ledger.cancelled.saturating_add(1);
            }
            other => return Err(anyhow!("unexpected cell phase {other:?}")),
        }
        let replay = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let replay = chat_events
                    .replay(
                        &item.address.workspace_id,
                        Some(&item.address.conversation_id),
                        &item.root_turn_id,
                        0,
                    )
                    .map_err(|error| anyhow!(error.to_string()))?;
                let settled = replay
                    .events
                    .iter()
                    .filter(|event| {
                        matches!(
                            &event.payload,
                            ChatDriverEvent::CommandCellSettled { cell }
                                if cell.cell_id == item.cell_id
                        )
                    })
                    .count();
                if settled > 0 {
                    return Ok::<_, anyhow::Error>(replay);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| anyhow!("cell terminal did not reach its durable journal"))??;
        let started = replay
            .events
            .iter()
            .filter(|event| {
                matches!(
                    &event.payload,
                    ChatDriverEvent::CommandCellStarted { cell } if cell.cell_id == item.cell_id
                )
            })
            .count();
        let settled = replay
            .events
            .iter()
            .filter(|event| {
                matches!(
                    &event.payload,
                    ChatDriverEvent::CommandCellSettled { cell } if cell.cell_id == item.cell_id
                )
            })
            .count();
        if started != 1 || settled != 1 {
            ledger.duplicate_terminal_failures =
                ledger.duplicate_terminal_failures.saturating_add(1);
            return Err(anyhow!(
                "cell {} had {started} starts and {settled} terminals",
                item.cell_id
            ));
        }
        let wrong_conversation = format!("{}-wrong", item.address.conversation_id);
        let wrong = chat_events
            .replay(
                &item.address.workspace_id,
                Some(&wrong_conversation),
                &item.root_turn_id,
                0,
            )
            .map_err(|error| anyhow!(error.to_string()))?;
        if !wrong.events.is_empty() {
            ledger.routing_failures = ledger.routing_failures.saturating_add(1);
            return Err(anyhow!("cell event crossed conversation identity"));
        }
    }
    Ok(())
}

fn write_ledger(path: &Path, ledger: &Ledger) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(ledger)?;
    echo_agent::utils::fs::atomic_write(path, &bytes).map_err(anyhow::Error::from)
}

fn hash_tree(root: &Path) -> Result<String> {
    fn collect(root: &Path, current: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
        for entry in std::fs::read_dir(current)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                collect(root, &path, files)?;
            } else if path.is_file() {
                files.push(path.strip_prefix(root)?.to_path_buf());
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    collect(root, root, &mut files)?;
    files.sort();
    let mut hasher = Sha256::new();
    for relative in files {
        hasher.update(relative.to_string_lossy().as_bytes());
        hasher.update([0]);
        hasher.update(std::fs::read(root.join(relative))?);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn git_repo_root() -> Result<PathBuf> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("run git rev-parse")?;
    ensure!(output.status.success(), "not inside a git repository");
    let root = String::from_utf8(output.stdout)?.trim().to_string();
    ensure!(!root.is_empty(), "git repository root is empty");
    Ok(PathBuf::from(root))
}

fn git_head(root: &Path) -> Result<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()?;
    ensure!(output.status.success(), "failed to read git HEAD");
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

fn ensure_clean_worktree(root: &Path) -> Result<()> {
    let output = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(root)
        .output()?;
    ensure!(output.status.success(), "failed to inspect git worktree");
    ensure!(
        output.stdout.is_empty(),
        "soak requires a clean committed worktree"
    );
    Ok(())
}
