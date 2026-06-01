//! Cron 调度运行器
//!
//! 后台 tokio 任务，按 cron 表达式触发 Agent 对话。
//! 每次触发通过 BackgroundTaskService 提交为独立的 AgentChat 任务，
//! 获得任务追踪、重试和持久化能力。

use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use super::task::{CronTask, CronTaskStatus, TaskStore};
use crate::agent_handle::AgentHandle;
use crate::tasks::background::BackgroundTaskKind;
use crate::tasks::service::BackgroundTaskService;
use echo_agent::agent::CancellationToken;

/// 调度运行器
#[derive(Clone)]
pub struct SchedulerRunner {
    store: TaskStore,
    agent: AgentHandle,
    task_service: Option<Arc<BackgroundTaskService>>,
    tasks: Arc<RwLock<Vec<CronTask>>>,
    /// Track last fire time per task_id to prevent double-firing
    last_fired: Arc<RwLock<HashMap<String, DateTime<Utc>>>>,
    cancel: CancellationToken,
}

impl SchedulerRunner {
    /// 创建调度运行器（使用文件存储）
    pub fn new(agent: AgentHandle, cancel: CancellationToken) -> Self {
        Self::with_store(agent, cancel, None)
    }

    /// 创建调度运行器（可选 Store 后端）
    pub fn with_store(
        agent: AgentHandle,
        cancel: CancellationToken,
        backend: Option<Arc<dyn echo_agent::memory::Store>>,
    ) -> Self {
        let store = match backend {
            Some(b) => TaskStore::with_store(b),
            None => TaskStore::new(),
        };
        let tasks = match store.load_all() {
            Ok(t) => t,
            Err(e) => {
                warn!("Failed to load cron tasks: {e}, starting empty");
                Vec::new()
            }
        };
        let enabled = tasks
            .iter()
            .filter(|t| t.status == CronTaskStatus::Enabled)
            .count();
        info!(
            "Scheduler initialized: {} tasks loaded ({} enabled)",
            tasks.len(),
            enabled
        );
        Self {
            store,
            agent,
            task_service: None,
            tasks: Arc::new(RwLock::new(tasks)),
            last_fired: Arc::new(RwLock::new(HashMap::new())),
            cancel,
        }
    }

    /// Set the BackgroundTaskService for task submission.
    /// 获取 TaskStore 引用（Clone 是廉价的，共享底层 Arc）
    pub fn store(&self) -> TaskStore {
        self.store.clone()
    }

    pub fn set_task_service(&mut self, service: Arc<BackgroundTaskService>) {
        self.task_service = Some(service);
    }

    /// 后台启动调度循环
    pub fn spawn(self: Arc<Self>) {
        tokio::spawn(async move {
            self.run_loop().await;
        });
    }

    /// 主循环：每 30 秒检查一次是否有任务需要触发
    async fn run_loop(&self) {
        info!("Scheduler loop started");
        loop {
            tokio::select! {
                _ = self.cancel.cancelled() => {
                    info!("Scheduler loop stopped");
                    break;
                }
                _ = tokio::time::sleep(tokio::time::Duration::from_secs(30)) => {
                    self.tick().await;
                }
            }
        }
    }

    /// 单次 tick：检查所有启用任务是否到达触发时间
    async fn tick(&self) {
        // 1. 收集需要触发的任务（持有读锁期间只做判断，不执行）
        let to_fire: Vec<(String, String, String)> = {
            let tasks = self.tasks.read().await;
            let mut fired = self.last_fired.write().await;
            let now = Utc::now();
            let mut pending = Vec::new();

            for task in tasks.iter() {
                if task.status != CronTaskStatus::Enabled {
                    continue;
                }

                // 检查是否在最近 30 秒内应该触发
                if let Ok(next_run) = task.next_run() {
                    let diff = (next_run - now).num_seconds();
                    // 如果下次触发时间在 -30s ~ 0s 之间，说明刚刚到达触发时刻
                    if diff >= -30 && diff <= 0 {
                        // Prevent double-firing: skip if already fired within this window
                        if let Some(last) = fired.get(&task.id) {
                            let since_last = (now - *last).num_seconds();
                            if since_last < 30 {
                                continue;
                            }
                        }
                        fired.insert(task.id.clone(), now);
                        pending.push((task.id.clone(), task.name.clone(), task.prompt.clone()));
                    }
                }
            }
            pending
        }; // 读锁在此处释放

        // 2. 在锁外执行任务（避免 fire_task → execute_direct 尝试写锁导致死锁）
        for (task_id, name, prompt) in &to_fire {
            self.fire_task(task_id, name, prompt).await;
        }
    }

    /// Fire a cron task — submits via BackgroundTaskService if available,
    /// falls back to direct agent.chat() otherwise.
    async fn fire_task(&self, task_id: &str, name: &str, prompt: &str) {
        info!("Firing cron task: {name} ({task_id})");

        if let Some(ref service) = self.task_service {
            // Submit as a tracked background task
            let description = format!("Cron [{name}]: {prompt}");
            let kind = BackgroundTaskKind::AgentChat {
                prompt: prompt.to_string(),
                session_id: None,
            };
            match service
                .submit(kind, &description, Some("cron".to_string()))
                .await
            {
                Ok(bg_task_id) => {
                    debug!("Cron task {task_id} submitted as background task {bg_task_id}");
                }
                Err(e) => {
                    warn!(
                        "Failed to submit cron task via BackgroundTaskService: {e}, falling back to direct execution"
                    );
                    self.execute_direct(task_id, prompt).await;
                }
            }
        } else {
            // Fallback: direct execution (no tracking)
            self.execute_direct(task_id, prompt).await;
        }
    }

    /// Direct execution fallback — used when BackgroundTaskService is not available.
    async fn execute_direct(&self, task_id: &str, prompt: &str) {
        use echo_agent::agent::Agent;
        let guard = self.agent.inner().read().await;
        let result = guard.chat(prompt).await;

        let summary = match result {
            Ok(answer) => {
                debug!("Cron task {task_id} completed: {} chars", answer.len());
                answer.chars().take(500).collect()
            }
            Err(e) => {
                warn!("Cron task {task_id} failed: {e}");
                format!("ERROR: {e}")
            }
        };

        if let Err(e) = self.store.update_last_run(task_id, &summary) {
            warn!("Failed to update cron task {task_id}: {e}");
        }

        // Reload tasks from store to get updated last_run_at
        if let Ok(updated) = self.store.load_all() {
            let mut tasks = self.tasks.write().await;
            *tasks = updated;
        }
    }

    // ── 管理 API ──────────────────────────────────────────────────

    /// 添加任务
    pub async fn add_task(&self, task: CronTask) -> anyhow::Result<()> {
        self.store.add(task.clone())?;
        let mut tasks = self.tasks.write().await;
        tasks.push(task);
        Ok(())
    }

    /// 删除任务
    pub async fn remove_task(&self, id: &str) -> anyhow::Result<bool> {
        let removed = self.store.remove(id)?;
        if removed {
            let mut tasks = self.tasks.write().await;
            tasks.retain(|t| t.id != id);
        }
        Ok(removed)
    }

    /// 启用/禁用任务
    pub async fn set_status(&self, id: &str, status: CronTaskStatus) -> anyhow::Result<bool> {
        let changed = self.store.set_status(id, status)?;
        if changed {
            let mut tasks = self.tasks.write().await;
            if let Some(t) = tasks.iter_mut().find(|t| t.id == id) {
                t.status = status;
            }
        }
        Ok(changed)
    }

    /// 列出所有任务
    pub async fn list_tasks(&self) -> Vec<CronTask> {
        self.tasks.read().await.clone()
    }

    /// 手动触发一次任务
    pub async fn run_task_once(&self, id: &str) -> anyhow::Result<String> {
        let tasks = self.tasks.read().await;
        let task = tasks
            .iter()
            .find(|t| t.id == id)
            .ok_or_else(|| anyhow::anyhow!("Task not found: {id}"))?;
        let prompt = task.prompt.clone();
        let name = task.name.clone();
        let task_id = task.id.clone();
        drop(tasks);

        self.fire_task(&task_id, &name, &prompt).await;

        // For manual runs, also do direct execution to return result immediately
        use echo_agent::agent::Agent;
        let guard = self.agent.inner().read().await;
        let result = guard.chat(&prompt).await?;

        let summary: String = result.chars().take(500).collect();

        if let Err(e) = self.store.update_last_run(&task_id, &summary) {
            warn!("Failed to update cron task: {e}");
        }
        if let Ok(updated) = self.store.load_all() {
            let mut tasks = self.tasks.write().await;
            *tasks = updated;
        }

        Ok(result)
    }

    /// 重新加载任务（从磁盘）
    pub async fn reload(&self) -> anyhow::Result<usize> {
        let tasks = self.store.load_all()?;
        let count = tasks.len();
        let mut guard = self.tasks.write().await;
        *guard = tasks;
        Ok(count)
    }
}
