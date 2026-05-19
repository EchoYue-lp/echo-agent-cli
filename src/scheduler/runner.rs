//! Cron 调度运行器
//!
//! 后台 tokio 任务，按 cron 表达式触发 Agent 对话。

use chrono::Utc;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use super::task::{CronTask, CronTaskStatus, TaskStore};
use crate::agent_handle::AgentHandle;
use echo_agent::agent::{Agent, CancellationToken};

/// 调度运行器
#[derive(Clone)]
pub struct SchedulerRunner {
    store: TaskStore,
    agent: AgentHandle,
    tasks: Arc<RwLock<Vec<CronTask>>>,
    cancel: CancellationToken,
}

impl SchedulerRunner {
    /// 创建调度运行器
    pub fn new(agent: AgentHandle, cancel: CancellationToken) -> Self {
        let store = TaskStore::new();
        let tasks = match store.load_all() {
            Ok(t) => t,
            Err(e) => {
                warn!("Failed to load cron tasks: {e}, starting empty");
                Vec::new()
            }
        };
        let enabled = tasks.iter().filter(|t| t.status == CronTaskStatus::Enabled).count();
        info!("Scheduler initialized: {} tasks loaded ({} enabled)", tasks.len(), enabled);
        Self {
            store,
            agent,
            tasks: Arc::new(RwLock::new(tasks)),
            cancel,
        }
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
        let tasks = self.tasks.read().await;
        let now = Utc::now();

        for task in tasks.iter() {
            if task.status != CronTaskStatus::Enabled {
                continue;
            }

            // 检查是否在最近 30 秒内应该触发
            if let Ok(next_run) = task.next_run() {
                let diff = (next_run - now).num_seconds();
                // 如果下次触发时间在 -30s ~ 0s 之间，说明刚刚到达触发时刻
                if diff >= -30 && diff <= 0 {
                    let task_id = task.id.clone();
                    let prompt = task.prompt.clone();
                    // 并行触发所有到期任务
                    let self_ = self.clone();
                    tokio::spawn(async move {
                        self_.execute_task(&task_id, &prompt).await;
                    });
                }
            }
        }
    }

    /// 执行一个定时任务
    async fn execute_task(&self, task_id: &str, prompt: &str) {
        info!("Executing cron task: {task_id}");
        let prompt = prompt.to_string();
        let guard = self.agent.inner().read().await;
        let result = guard.chat(&prompt).await;

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
        let task_id = task.id.clone();
        drop(tasks);

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
