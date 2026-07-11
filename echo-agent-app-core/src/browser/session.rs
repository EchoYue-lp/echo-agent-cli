use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use echo_agent::prelude::ToolResult;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock, broadcast};

use super::event::BrowserEvent;

pub const MAIN_TAB_OWNER: &str = "main";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BrowserSessionStatus {
    Starting,
    Ready,
    Navigating,
    Acting,
    Closed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserTab {
    pub id: String,
    pub index: usize,
    pub owner_run_id: Option<String>,
    pub url: Option<String>,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserSession {
    pub id: String,
    pub conversation_id: String,
    pub status: BrowserSessionStatus,
    #[serde(default)]
    pub developer_mode: bool,
    pub tabs: Vec<BrowserTab>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserObservation {
    pub session_id: String,
    pub tab_id: String,
    pub action: String,
    pub summary: String,
    pub truncated: bool,
    pub captured_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserLease {
    pub session_id: String,
    pub tab_id: String,
    pub tab_index: usize,
    pub opened: bool,
}

struct SessionState {
    session: BrowserSession,
    owner_tabs: HashMap<String, String>,
    next_tab_index: usize,
}

#[derive(Clone)]
pub struct BrowserSessionManager {
    sessions: Arc<RwLock<HashMap<String, SessionState>>>,
    events: broadcast::Sender<BrowserEvent>,
    operation_lock: Arc<Mutex<()>>,
    observation_char_limit: usize,
    metadata_dir: PathBuf,
}

impl BrowserSessionManager {
    pub fn new(metadata_dir: PathBuf, observation_char_limit: usize) -> Self {
        let (events, _) = broadcast::channel(256);
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            events,
            operation_lock: Arc::new(Mutex::new(())),
            observation_char_limit,
            metadata_dir,
        }
    }

    pub async fn restore_metadata(&self) {
        let mut entries = match tokio::fs::read_dir(&self.metadata_dir).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => {
                tracing::warn!(error = %error, "failed to read browser session metadata");
                return;
            }
        };
        let mut restored = HashMap::new();
        loop {
            let entry = match entries.next_entry().await {
                Ok(Some(entry)) => entry,
                Ok(None) => break,
                Err(error) => {
                    tracing::warn!(error = %error, "failed to enumerate browser session metadata");
                    break;
                }
            };
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let bytes = match tokio::fs::read(&path).await {
                Ok(bytes) => bytes,
                Err(error) => {
                    tracing::warn!(path = %path.display(), error = %error, "failed to read browser session metadata file");
                    continue;
                }
            };
            let mut session = match serde_json::from_slice::<BrowserSession>(&bytes) {
                Ok(session) => session,
                Err(error) => {
                    tracing::warn!(path = %path.display(), error = %error, "failed to parse browser session metadata file");
                    continue;
                }
            };
            session.status = BrowserSessionStatus::Closed;
            let next_tab_index = session
                .tabs
                .iter()
                .map(|tab| tab.index)
                .max()
                .unwrap_or(0)
                .saturating_add(1);
            restored.insert(
                session.conversation_id.clone(),
                SessionState {
                    session,
                    owner_tabs: HashMap::new(),
                    next_tab_index,
                },
            );
        }
        *self.sessions.write().await = restored;
    }

    pub fn subscribe(&self) -> broadcast::Receiver<BrowserEvent> {
        self.events.subscribe()
    }

    pub async fn lock_operation(&self) -> tokio::sync::OwnedMutexGuard<()> {
        self.operation_lock.clone().lock_owned().await
    }

    pub async fn lease_tab(
        &self,
        conversation_id: &str,
        owner_id: &str,
        owner_run_id: Option<&str>,
    ) -> BrowserLease {
        let mut sessions = self.sessions.write().await;
        if sessions
            .get(conversation_id)
            .is_some_and(|state| state.session.status == BrowserSessionStatus::Closed)
        {
            sessions.remove(conversation_id);
        }
        let state = sessions
            .entry(conversation_id.to_string())
            .or_insert_with(|| {
                let now = Utc::now();
                let session_id = format!("browser-{}", uuid::Uuid::new_v4());
                let main_tab = BrowserTab {
                    id: format!("tab-{}", uuid::Uuid::new_v4()),
                    index: 0,
                    owner_run_id: None,
                    url: None,
                    title: None,
                };
                let session = BrowserSession {
                    id: session_id,
                    conversation_id: conversation_id.to_string(),
                    status: BrowserSessionStatus::Starting,
                    developer_mode: false,
                    tabs: vec![main_tab.clone()],
                    created_at: now,
                    updated_at: now,
                };
                let mut owner_tabs = HashMap::new();
                owner_tabs.insert(MAIN_TAB_OWNER.to_string(), main_tab.id.clone());
                SessionState {
                    session,
                    owner_tabs,
                    next_tab_index: 1,
                }
            });

        if state.session.status == BrowserSessionStatus::Starting {
            state.session.status = BrowserSessionStatus::Ready;
            state.session.updated_at = Utc::now();
            let _ = self.events.send(BrowserEvent::SessionStarted {
                session: state.session.clone(),
            });
        }

        if let Some(tab_id) = state.owner_tabs.get(owner_id)
            && let Some(tab) = state.session.tabs.iter().find(|tab| &tab.id == tab_id)
        {
            let lease = BrowserLease {
                session_id: state.session.id.clone(),
                tab_id: tab.id.clone(),
                tab_index: tab.index,
                opened: false,
            };
            let session = state.session.clone();
            drop(sessions);
            self.persist(&session).await;
            return lease;
        }

        let tab = BrowserTab {
            id: format!("tab-{}", uuid::Uuid::new_v4()),
            index: state.next_tab_index,
            owner_run_id: owner_run_id.map(str::to_string),
            url: None,
            title: None,
        };
        state.next_tab_index = state.next_tab_index.saturating_add(1);
        state
            .owner_tabs
            .insert(owner_id.to_string(), tab.id.clone());
        state.session.tabs.push(tab.clone());
        state.session.updated_at = Utc::now();
        let _ = self.events.send(BrowserEvent::TabOpened {
            session_id: state.session.id.clone(),
            tab: tab.clone(),
        });
        let lease = BrowserLease {
            session_id: state.session.id.clone(),
            tab_id: tab.id,
            tab_index: tab.index,
            opened: true,
        };
        let session = state.session.clone();
        drop(sessions);
        self.persist(&session).await;
        lease
    }

    pub async fn open_tab(
        &self,
        conversation_id: &str,
        owner_id: &str,
        owner_run_id: Option<&str>,
    ) -> Option<BrowserLease> {
        let _ = self.lease_tab(conversation_id, MAIN_TAB_OWNER, None).await;
        let mut sessions = self.sessions.write().await;
        let state = sessions.get_mut(conversation_id)?;
        let tab = BrowserTab {
            id: format!("tab-{}", uuid::Uuid::new_v4()),
            index: state.next_tab_index,
            owner_run_id: owner_run_id.map(str::to_string),
            url: None,
            title: None,
        };
        state.next_tab_index = state.next_tab_index.saturating_add(1);
        state
            .owner_tabs
            .insert(owner_id.to_string(), tab.id.clone());
        state.session.tabs.push(tab.clone());
        state.session.updated_at = Utc::now();
        let _ = self.events.send(BrowserEvent::TabOpened {
            session_id: state.session.id.clone(),
            tab: tab.clone(),
        });
        let lease = BrowserLease {
            session_id: state.session.id.clone(),
            tab_id: tab.id,
            tab_index: tab.index,
            opened: true,
        };
        let session = state.session.clone();
        drop(sessions);
        self.persist(&session).await;
        Some(lease)
    }

    pub async fn select_tab(
        &self,
        conversation_id: &str,
        owner_id: &str,
        index: usize,
    ) -> Option<BrowserLease> {
        let mut sessions = self.sessions.write().await;
        let state = sessions.get_mut(conversation_id)?;
        let tab = state
            .session
            .tabs
            .iter()
            .find(|tab| tab.index == index)?
            .clone();
        state
            .owner_tabs
            .insert(owner_id.to_string(), tab.id.clone());
        state.session.updated_at = Utc::now();
        let lease = BrowserLease {
            session_id: state.session.id.clone(),
            tab_id: tab.id,
            tab_index: tab.index,
            opened: false,
        };
        let session = state.session.clone();
        drop(sessions);
        self.persist(&session).await;
        Some(lease)
    }

    pub async fn close_tab(&self, conversation_id: &str, index: usize) {
        let mut sessions = self.sessions.write().await;
        let Some(state) = sessions.get_mut(conversation_id) else {
            return;
        };
        let closed_id = state
            .session
            .tabs
            .iter()
            .find(|tab| tab.index == index)
            .map(|tab| tab.id.clone());
        let Some(closed_id) = closed_id else {
            return;
        };
        state.session.tabs.retain(|tab| tab.id != closed_id);
        for tab in &mut state.session.tabs {
            if tab.index > index {
                tab.index = tab.index.saturating_sub(1);
            }
        }
        state.owner_tabs.retain(|_, tab_id| tab_id != &closed_id);
        state.next_tab_index = state.session.tabs.len();
        state.session.updated_at = Utc::now();
        let session = state.session.clone();
        drop(sessions);
        self.persist(&session).await;
    }

    pub async fn set_status(&self, conversation_id: &str, status: BrowserSessionStatus) {
        let session = if let Some(state) = self.sessions.write().await.get_mut(conversation_id) {
            state.session.status = status;
            state.session.updated_at = Utc::now();
            Some(state.session.clone())
        } else {
            None
        };
        if let Some(session) = session {
            self.persist(&session).await;
        }
    }

    pub async fn set_developer_mode(&self, conversation_id: &str, enabled: bool) {
        let session = if let Some(state) = self.sessions.write().await.get_mut(conversation_id) {
            state.session.developer_mode = enabled;
            state.session.updated_at = Utc::now();
            Some(state.session.clone())
        } else {
            None
        };
        if let Some(session) = session {
            self.persist(&session).await;
        }
    }

    pub async fn developer_mode(&self, conversation_id: &str) -> bool {
        self.sessions
            .read()
            .await
            .get(conversation_id)
            .is_some_and(|state| state.session.developer_mode)
    }

    pub async fn update_url(&self, conversation_id: &str, tab_id: &str, url: &str) {
        let session = if let Some(state) = self.sessions.write().await.get_mut(conversation_id) {
            if let Some(tab) = state.session.tabs.iter_mut().find(|tab| tab.id == tab_id) {
                tab.url = Some(url.to_string());
            }
            state.session.updated_at = Utc::now();
            Some(state.session.clone())
        } else {
            None
        };
        if let Some(session) = session {
            self.persist(&session).await;
        }
    }

    pub fn observation(
        &self,
        lease: &BrowserLease,
        action: &str,
        result: &ToolResult,
    ) -> BrowserObservation {
        let total = result.output.chars().count();
        let summary = result
            .output
            .chars()
            .take(self.observation_char_limit)
            .collect::<String>();
        BrowserObservation {
            session_id: lease.session_id.clone(),
            tab_id: lease.tab_id.clone(),
            action: action.to_string(),
            summary,
            truncated: total > self.observation_char_limit,
            captured_at: Utc::now(),
        }
    }

    pub fn emit(&self, event: BrowserEvent) {
        let _ = self.events.send(event);
    }

    pub async fn sessions(&self) -> Vec<BrowserSession> {
        self.sessions
            .read()
            .await
            .values()
            .map(|state| state.session.clone())
            .collect()
    }

    pub async fn close_all(&self) {
        let mut sessions = self.sessions.write().await;
        let mut changed = Vec::new();
        for state in sessions.values_mut() {
            if state.session.status != BrowserSessionStatus::Closed {
                state.session.status = BrowserSessionStatus::Closed;
                state.session.updated_at = Utc::now();
                let _ = self.events.send(BrowserEvent::SessionClosed {
                    session_id: state.session.id.clone(),
                });
                changed.push(state.session.clone());
            }
        }
        drop(sessions);
        for session in changed {
            self.persist(&session).await;
        }
    }

    async fn persist(&self, session: &BrowserSession) {
        if let Err(error) = tokio::fs::create_dir_all(&self.metadata_dir).await {
            tracing::warn!(error = %error, "failed to create browser session metadata directory");
            return;
        }
        let bytes = match serde_json::to_vec_pretty(session) {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(error = %error, "failed to serialize browser session metadata");
                return;
            }
        };
        let path = self.metadata_dir.join(format!("{}.json", session.id));
        if let Err(error) = tokio::fs::write(&path, bytes).await {
            tracing::warn!(path = %path.display(), error = %error, "failed to persist browser session metadata");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn conversation_reuses_session_and_run_gets_own_tab() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let manager = BrowserSessionManager::new(temp.path().to_path_buf(), 32);
        let main = manager.lease_tab("conv-1", MAIN_TAB_OWNER, None).await;
        let main_again = manager.lease_tab("conv-1", MAIN_TAB_OWNER, None).await;
        let worker = manager.lease_tab("conv-1", "exec-1", Some("run-1")).await;
        let worker_again = manager.lease_tab("conv-1", "exec-1", Some("run-1")).await;

        assert_eq!(main.session_id, main_again.session_id);
        assert_eq!(main.tab_id, main_again.tab_id);
        assert_ne!(main.tab_id, worker.tab_id);
        assert_eq!(worker.tab_id, worker_again.tab_id);
        assert_eq!(worker.tab_index, 1);
        assert!(worker.opened);
        assert!(!worker_again.opened);
        Ok::<(), String>(())
    }

    #[tokio::test]
    async fn developer_mode_is_scoped_to_conversation() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let manager = BrowserSessionManager::new(temp.path().to_path_buf(), 32);
        manager.lease_tab("conv-1", MAIN_TAB_OWNER, None).await;
        manager.lease_tab("conv-2", MAIN_TAB_OWNER, None).await;
        manager.set_developer_mode("conv-1", true).await;

        assert!(manager.developer_mode("conv-1").await);
        assert!(!manager.developer_mode("conv-2").await);
        Ok(())
    }

    #[test]
    fn observation_is_utf8_safe_and_bounded() {
        let manager = BrowserSessionManager::new(PathBuf::from("unused"), 3);
        let lease = BrowserLease {
            session_id: "session".to_string(),
            tab_id: "tab".to_string(),
            tab_index: 0,
            opened: false,
        };
        let result = ToolResult::success("你好世界");
        let observation = manager.observation(&lease, "browser_snapshot", &result);

        assert_eq!(observation.summary, "你好世");
        assert!(observation.truncated);
    }

    #[tokio::test]
    async fn shutdown_marks_sessions_closed_and_emits_event() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let manager = BrowserSessionManager::new(temp.path().to_path_buf(), 32);
        let mut events = manager.subscribe();
        let lease = manager.lease_tab("conv-1", MAIN_TAB_OWNER, None).await;
        manager.close_all().await;

        let sessions = manager.sessions().await;
        assert_eq!(sessions.len(), 1);
        assert_eq!(
            sessions.first().map(|s| s.status),
            Some(BrowserSessionStatus::Closed)
        );

        let mut saw_closed = false;
        while let Ok(event) = events.try_recv() {
            if event
                == (BrowserEvent::SessionClosed {
                    session_id: lease.session_id.clone(),
                })
            {
                saw_closed = true;
            }
        }
        assert!(saw_closed);
        Ok::<(), String>(())
    }

    #[tokio::test]
    async fn restored_metadata_is_closed_and_new_use_starts_fresh_session() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let first = BrowserSessionManager::new(temp.path().to_path_buf(), 32);
        let old = first.lease_tab("conv-1", MAIN_TAB_OWNER, None).await;
        first.close_all().await;

        let restored = BrowserSessionManager::new(temp.path().to_path_buf(), 32);
        restored.restore_metadata().await;
        let metadata = restored.sessions().await;
        assert_eq!(
            metadata.first().map(|session| session.status),
            Some(BrowserSessionStatus::Closed)
        );

        let fresh = restored.lease_tab("conv-1", MAIN_TAB_OWNER, None).await;
        assert_ne!(fresh.session_id, old.session_id);
        Ok(())
    }

    #[tokio::test]
    async fn explicit_tab_changes_update_owner_lease() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let manager = BrowserSessionManager::new(temp.path().to_path_buf(), 32);
        let main = manager.lease_tab("conv-1", MAIN_TAB_OWNER, None).await;
        let opened = manager
            .open_tab("conv-1", MAIN_TAB_OWNER, None)
            .await
            .ok_or_else(|| "tab should open".to_string())?;
        assert_eq!(opened.tab_index, 1);
        assert_eq!(
            manager
                .lease_tab("conv-1", MAIN_TAB_OWNER, None)
                .await
                .tab_id,
            opened.tab_id
        );

        let selected = manager
            .select_tab("conv-1", MAIN_TAB_OWNER, main.tab_index)
            .await
            .ok_or_else(|| "main tab should still exist".to_string())?;
        assert_eq!(selected.tab_id, main.tab_id);

        manager.close_tab("conv-1", opened.tab_index).await;
        assert_eq!(
            manager.sessions().await.first().map(|s| s.tabs.len()),
            Some(1)
        );
        Ok(())
    }
}
