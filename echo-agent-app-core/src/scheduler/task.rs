//! Cron 任务定义与持久化
//!
//! TaskStore 支持两种后端：
//! - `Store` trait（SQLite / InMemory 等）— 推荐
//! - 文件存储（`~/.echo-agent/scheduler/tasks.json`）— 向后兼容

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

/// 定时任务
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronTask {
    /// 唯一 ID (UUID)
    pub id: String,
    /// 任务名称
    pub name: String,
    /// Cron 表达式 (5 字段: 分 时 日 月 周)
    pub cron_expr: String,
    /// 发送给 Agent 的 prompt
    pub prompt: String,
    /// 任务状态
    pub status: CronTaskStatus,
    /// 上次执行时间
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<String>,
    /// 上次执行结果摘要
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_result: Option<String>,
    /// 创建时间
    pub created_at: String,
}

/// 任务状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CronTaskStatus {
    /// 启用
    Enabled,
    /// 禁用
    Disabled,
}

impl CronTask {
    /// 创建新的定时任务
    pub fn new(name: &str, cron_expr: &str, prompt: &str) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            cron_expr: cron_expr.to_string(),
            prompt: prompt.to_string(),
            status: CronTaskStatus::Enabled,
            last_run_at: None,
            last_result: None,
            created_at: Utc::now().to_rfc3339(),
        }
    }

    /// 解析 cron 表达式，返回下一次触发时间
    pub fn next_run(&self) -> anyhow::Result<DateTime<Utc>> {
        use cron::Schedule;
        use std::str::FromStr;
        let schedule = Schedule::from_str(&self.cron_expr)?;
        schedule
            .upcoming(Utc)
            .next()
            .ok_or_else(|| anyhow::anyhow!("No upcoming runs for cron expression"))
    }
}

/// Cron task storage — uses `Store` trait (SQLite) when available, falls back to file-based JSON.
#[derive(Clone)]
pub struct TaskStore {
    /// Optional Store backend (SQLite / InMemory)
    backend: Option<Arc<dyn echo_agent::memory::Store>>,
    /// Legacy file path (used as fallback and for migration)
    path: PathBuf,
}

const CRON_NAMESPACE: &[&str] = &["scheduler", "cron_tasks"];
const CRON_KEY: &str = "all_cron_tasks";

impl TaskStore {
    fn default_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home)
            .join(".echo-agent")
            .join("scheduler")
            .join("tasks.json")
    }

    /// Open or create task store with file-based backend (legacy).
    pub fn new() -> Self {
        let path = Self::default_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        Self {
            backend: None,
            path,
        }
    }

    /// Create task store backed by a `Store` trait implementation (e.g. SQLite).
    /// Automatically migrates data from the legacy file if it exists.
    pub fn with_store(store: Arc<dyn echo_agent::memory::Store>) -> Self {
        let path = Self::default_path();
        let mut ts = Self {
            backend: Some(store),
            path: path.clone(),
        };
        // Migrate from legacy file if it exists
        if path.exists() {
            if let Ok(tasks) = ts.load_from_file() {
                if !tasks.is_empty() {
                    if let Err(e) = ts.save_to_backend(&tasks) {
                        tracing::warn!("Failed to migrate cron tasks to store: {e}");
                    } else {
                        tracing::info!("Migrated {} cron tasks from file to store", tasks.len());
                        // Remove legacy file after successful migration
                        let _ = std::fs::remove_file(&path);
                    }
                }
            }
        }
        ts
    }

    /// Load all tasks — tries backend first, falls back to file.
    pub fn load_all(&self) -> anyhow::Result<Vec<CronTask>> {
        if self.backend.is_some() {
            self.load_from_backend()
        } else {
            self.load_from_file()
        }
    }

    /// Save all tasks to the active backend.
    pub fn save_all(&self, tasks: &[CronTask]) -> anyhow::Result<()> {
        if self.backend.is_some() {
            self.save_to_backend(tasks)
        } else {
            self.save_to_file(tasks)
        }
    }

    // ── Store backend ──

    fn load_from_backend(&self) -> anyhow::Result<Vec<CronTask>> {
        let store = self.backend.as_ref().unwrap();
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| anyhow::anyhow!("No tokio runtime available for async store access"))?;
        let result = tokio::task::block_in_place(|| {
            handle.block_on(async { store.get(CRON_NAMESPACE, CRON_KEY).await })
        });
        match result {
            Ok(Some(item)) => {
                let tasks: Vec<CronTask> = serde_json::from_value(item.value)
                    .map_err(|e| anyhow::anyhow!("Failed to deserialize cron tasks: {e}"))?;
                Ok(tasks)
            }
            Ok(None) => Ok(Vec::new()),
            Err(e) => Err(anyhow::anyhow!("Failed to load cron tasks from store: {e}")),
        }
    }

    fn save_to_backend(&self, tasks: &[CronTask]) -> anyhow::Result<()> {
        let store = self.backend.as_ref().unwrap();
        let value = serde_json::to_value(tasks)?;
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| anyhow::anyhow!("No tokio runtime available for async store access"))?;
        tokio::task::block_in_place(|| {
            handle.block_on(async { store.put(CRON_NAMESPACE, CRON_KEY, value).await })
        })
        .map_err(|e| anyhow::anyhow!("Failed to save cron tasks to store: {e}"))
    }

    // ── File backend (legacy) ──

    fn load_from_file(&self) -> anyhow::Result<Vec<CronTask>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let data = std::fs::read_to_string(&self.path)?;
        if data.trim().is_empty() {
            return Ok(Vec::new());
        }
        serde_json::from_str(&data).map_err(|e| anyhow::anyhow!("Failed to parse tasks: {e}"))
    }

    fn save_to_file(&self, tasks: &[CronTask]) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(tasks)?;
        std::fs::write(&self.path, json)?;
        Ok(())
    }

    /// 添加任务
    pub fn add(&self, task: CronTask) -> anyhow::Result<()> {
        let mut tasks = self.load_all()?;
        tasks.push(task);
        self.save_all(&tasks)
    }

    /// 删除任务
    pub fn remove(&self, id: &str) -> anyhow::Result<bool> {
        let mut tasks = self.load_all()?;
        let before = tasks.len();
        tasks.retain(|t| t.id != id);
        if tasks.len() < before {
            self.save_all(&tasks)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// 更新任务状态
    pub fn set_status(&self, id: &str, status: CronTaskStatus) -> anyhow::Result<bool> {
        let mut tasks = self.load_all()?;
        let found = tasks.iter_mut().find(|t| t.id == id);
        if let Some(task) = found {
            task.status = status;
            self.save_all(&tasks)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// 记录上次执行结果
    pub fn update_last_run(&self, id: &str, result: &str) -> anyhow::Result<()> {
        let mut tasks = self.load_all()?;
        if let Some(task) = tasks.iter_mut().find(|t| t.id == id) {
            task.last_run_at = Some(Utc::now().to_rfc3339());
            task.last_result = Some(result.chars().take(500).collect());
            self.save_all(&tasks)?;
        }
        Ok(())
    }

    /// 获取单个任务
    pub fn get(&self, id: &str) -> anyhow::Result<Option<CronTask>> {
        let tasks = self.load_all()?;
        Ok(tasks.into_iter().find(|t| t.id == id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cron_task_create() {
        let task = CronTask::new("daily-report", "0 9 * * *", "生成日报");
        assert_eq!(task.name, "daily-report");
        assert_eq!(task.cron_expr, "0 9 * * *");
        assert_eq!(task.status, CronTaskStatus::Enabled);
    }

    #[test]
    fn test_cron_task_next_run() {
        let task = CronTask::new("test", "0 0 9 * * * *", "hello");
        let next = task.next_run();
        assert!(next.is_ok());
    }

    #[test]
    fn test_task_store_crud() {
        let dir = std::env::temp_dir().join(format!("echo-scheduler-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();

        let store = TaskStore {
            backend: None,
            path: dir.join("tasks.json"),
        };

        let task = CronTask::new("test-task", "*/5 * * * *", "test prompt");
        let id = task.id.clone();

        store.add(task).unwrap();
        let tasks = store.load_all().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, id);

        store.set_status(&id, CronTaskStatus::Disabled).unwrap();
        let task = store.get(&id).unwrap().unwrap();
        assert_eq!(task.status, CronTaskStatus::Disabled);

        let removed = store.remove(&id).unwrap();
        assert!(removed);
        assert!(store.load_all().unwrap().is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }
}
