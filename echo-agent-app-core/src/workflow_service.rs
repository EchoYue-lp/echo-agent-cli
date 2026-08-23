//! EKO-owned workflow catalog and surface adapter.
//!
//! The framework remains the sole owner of declarative parsing and `Graph`
//! execution. This service owns local product persistence and exposes one
//! command entry used by GUI, TUI, CLI, and channel adapters.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, rename = "StoredWorkflow")]
pub struct StoredWorkflow {
    pub id: String,
    pub name: String,
    pub definition: String,
    pub node_count: usize,
    pub edge_count: usize,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, rename = "WorkflowExecution")]
pub struct WorkflowExecution {
    pub success: bool,
    pub workflow_id: String,
    pub path: Vec<String>,
    pub steps: usize,
    pub state: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, rename = "WorkflowMutationReceipt")]
pub struct WorkflowMutationReceipt {
    pub success: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowCatalog {
    workflows: BTreeMap<String, StoredWorkflow>,
}

#[derive(Debug, thiserror::Error)]
pub enum WorkflowServiceError {
    #[error("workflow service is unavailable: {0}")]
    Unavailable(String),
    #[error("workflow '{0}' was not found")]
    NotFound(String),
    #[error("workflow definition is invalid: {0}")]
    InvalidDefinition(String),
    #[error("workflow storage failed at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("workflow serialization failed: {0}")]
    Serialization(String),
    #[error("workflow execution failed: {0}")]
    Execution(String),
}

pub struct WorkflowService {
    path: PathBuf,
    catalog: RwLock<WorkflowCatalog>,
    initialization_error: Option<String>,
}

impl WorkflowService {
    pub fn default_path() -> PathBuf {
        echo_agent::paths::user_data_path("workflows.json")
    }

    pub fn at_default_path() -> Self {
        let path = Self::default_path();
        match Self::open(&path) {
            Ok(service) => service,
            Err(error) => {
                tracing::error!(%error, "workflow service initialization failed");
                Self {
                    path,
                    catalog: RwLock::new(WorkflowCatalog::default()),
                    initialization_error: Some(error.to_string()),
                }
            }
        }
    }

    pub fn open(path: impl Into<PathBuf>) -> Result<Self, WorkflowServiceError> {
        let path = path.into();
        let catalog = match echo_core::utils::fs::read_existing(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| WorkflowServiceError::Serialization(error.to_string()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                WorkflowCatalog::default()
            }
            Err(source) => {
                return Err(WorkflowServiceError::Io {
                    path: path.clone(),
                    source,
                });
            }
        };
        validate_catalog(&catalog)?;
        Ok(Self {
            path,
            catalog: RwLock::new(catalog),
            initialization_error: None,
        })
    }

    pub fn list(&self) -> Result<Vec<StoredWorkflow>, WorkflowServiceError> {
        self.ensure_available()?;
        Ok(read_catalog(&self.catalog)?
            .workflows
            .values()
            .cloned()
            .collect())
    }

    pub fn get(&self, id: &str) -> Result<StoredWorkflow, WorkflowServiceError> {
        self.ensure_available()?;
        read_catalog(&self.catalog)?
            .workflows
            .get(id)
            .cloned()
            .ok_or_else(|| WorkflowServiceError::NotFound(id.to_string()))
    }

    pub fn create(
        &self,
        name: String,
        definition: String,
    ) -> Result<StoredWorkflow, WorkflowServiceError> {
        self.ensure_available()?;
        let parsed = validate_definition(&definition)?;
        let name = if name.trim().is_empty() {
            parsed.name.trim()
        } else {
            name.trim()
        };
        if name.is_empty() {
            return Err(WorkflowServiceError::InvalidDefinition(
                "name must not be empty".to_string(),
            ));
        }
        let now = echo_agent::utils::time::now_local().to_rfc3339();
        let workflow = StoredWorkflow {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            definition,
            node_count: parsed.nodes.len(),
            edge_count: parsed.edges.len(),
            created_at: now.clone(),
            updated_at: now,
        };
        let mut catalog = write_catalog(&self.catalog)?;
        let mut updated = WorkflowCatalog {
            workflows: catalog.workflows.clone(),
        };
        updated
            .workflows
            .insert(workflow.id.clone(), workflow.clone());
        persist_catalog(&self.path, &updated)?;
        *catalog = updated;
        Ok(workflow)
    }

    pub fn delete(&self, id: &str) -> Result<(), WorkflowServiceError> {
        self.ensure_available()?;
        let mut catalog = write_catalog(&self.catalog)?;
        if !catalog.workflows.contains_key(id) {
            return Err(WorkflowServiceError::NotFound(id.to_string()));
        }
        let mut updated = WorkflowCatalog {
            workflows: catalog.workflows.clone(),
        };
        updated.workflows.remove(id);
        persist_catalog(&self.path, &updated)?;
        *catalog = updated;
        Ok(())
    }

    pub async fn execute(
        &self,
        id: &str,
        input: Option<serde_json::Value>,
    ) -> Result<WorkflowExecution, WorkflowServiceError> {
        let workflow = self.get(id)?;
        let graph = validate_definition(&workflow.definition)?
            .build_graph()
            .map_err(|error| WorkflowServiceError::InvalidDefinition(error.to_string()))?;
        let state = echo_agent::workflow::SharedState::new();
        state
            .set("input", input.unwrap_or_else(|| serde_json::json!({})))
            .map_err(|error| WorkflowServiceError::Execution(error.to_string()))?;
        let result = graph
            .run(state)
            .await
            .map_err(|error| WorkflowServiceError::Execution(error.to_string()))?;
        let state = result
            .state
            .to_json_value()
            .map_err(|error| WorkflowServiceError::Execution(error.to_string()))?;
        Ok(WorkflowExecution {
            success: true,
            workflow_id: workflow.id,
            path: result.path,
            steps: result.steps,
            state,
        })
    }

    pub async fn execute_command(&self, command: &str) -> Result<String, WorkflowServiceError> {
        let (action, remainder) = take_argument(command).unwrap_or(("list", ""));
        match action {
            "list" | "ls" => serde_json::to_string_pretty(&self.list()?)
                .map_err(|error| WorkflowServiceError::Serialization(error.to_string())),
            "show" | "get" => {
                let id = required_argument(
                    take_argument(remainder).map(|(value, _)| value),
                    "show <id>",
                )?;
                serde_json::to_string_pretty(&self.get(id)?)
                    .map_err(|error| WorkflowServiceError::Serialization(error.to_string()))
            }
            "delete" | "rm" => {
                let id = required_argument(
                    take_argument(remainder).map(|(value, _)| value),
                    "delete <id>",
                )?;
                self.delete(id)?;
                Ok(format!("Workflow {id} deleted."))
            }
            "run" | "execute" => {
                let (id, input) = take_argument(remainder).ok_or_else(|| {
                    WorkflowServiceError::InvalidDefinition(
                        "missing argument; usage: run <id> [json-input]".to_string(),
                    )
                })?;
                let input = Some(input)
                    .filter(|value| !value.trim().is_empty())
                    .map(serde_json::from_str)
                    .transpose()
                    .map_err(|error| {
                        WorkflowServiceError::InvalidDefinition(format!(
                            "workflow input must be JSON: {error}"
                        ))
                    })?;
                let result = self.execute(id, input).await?;
                serde_json::to_string_pretty(&result)
                    .map_err(|error| WorkflowServiceError::Serialization(error.to_string()))
            }
            "create" => {
                let (name, definition) = take_argument(remainder).ok_or_else(|| {
                    WorkflowServiceError::InvalidDefinition(
                        "missing argument; usage: create <name> <yaml-or-json|@path>".to_string(),
                    )
                })?;
                let definition = required_argument(
                    Some(definition),
                    "create <name> <yaml-or-json|@path>",
                )?;
                let definition = match definition.strip_prefix('@') {
                    Some(path) if !path.trim().is_empty() => fs::read_to_string(path).map_err(
                        |source| WorkflowServiceError::Io {
                            path: PathBuf::from(path),
                            source,
                        },
                    )?,
                    _ => definition.to_string(),
                };
                serde_json::to_string_pretty(&self.create(name.to_string(), definition)?)
                    .map_err(|error| WorkflowServiceError::Serialization(error.to_string()))
            }
            _ => Err(WorkflowServiceError::InvalidDefinition(
                "usage: workflow [list|show <id>|create <name> <definition|@path>|delete <id>|run <id> [json-input]]"
                    .to_string(),
            )),
        }
    }

    fn ensure_available(&self) -> Result<(), WorkflowServiceError> {
        match &self.initialization_error {
            Some(error) => Err(WorkflowServiceError::Unavailable(error.clone())),
            None => Ok(()),
        }
    }
}

fn take_argument(input: &str) -> Option<(&str, &str)> {
    let trimmed = input.trim_start();
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.split_once(char::is_whitespace) {
        Some((argument, remainder)) => Some((argument, remainder.trim_start())),
        None => Some((trimmed, "")),
    }
}

fn required_argument<'a>(
    value: Option<&'a str>,
    usage: &str,
) -> Result<&'a str, WorkflowServiceError> {
    value
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            WorkflowServiceError::InvalidDefinition(format!("missing argument; usage: {usage}"))
        })
}

fn parse_definition(
    definition: &str,
) -> Result<echo_agent::workflow::WorkflowDefinition, WorkflowServiceError> {
    if definition.trim().is_empty() {
        return Err(WorkflowServiceError::InvalidDefinition(
            "definition must not be empty".to_string(),
        ));
    }
    match echo_agent::workflow::WorkflowDefinition::from_yaml_str(definition) {
        Ok(parsed) => Ok(parsed),
        Err(yaml_error) => echo_agent::workflow::WorkflowDefinition::from_json_str(definition)
            .map_err(|json_error| {
                WorkflowServiceError::InvalidDefinition(format!(
                    "YAML parse failed: {yaml_error}; JSON parse failed: {json_error}"
                ))
            }),
    }
}

fn validate_definition(
    definition: &str,
) -> Result<echo_agent::workflow::WorkflowDefinition, WorkflowServiceError> {
    let parsed = parse_definition(definition)?;
    parsed
        .clone()
        .build_graph()
        .map_err(|error| WorkflowServiceError::InvalidDefinition(error.to_string()))?;
    Ok(parsed)
}

fn validate_catalog(catalog: &WorkflowCatalog) -> Result<(), WorkflowServiceError> {
    for (key, workflow) in &catalog.workflows {
        if key != &workflow.id || workflow.id.trim().is_empty() || workflow.name.trim().is_empty() {
            return Err(WorkflowServiceError::Serialization(format!(
                "workflow catalog identity mismatch for key '{key}'"
            )));
        }
        let parsed = validate_definition(&workflow.definition).map_err(|error| {
            WorkflowServiceError::Serialization(format!(
                "workflow '{}' is invalid: {error}",
                workflow.id
            ))
        })?;
        if workflow.node_count != parsed.nodes.len() || workflow.edge_count != parsed.edges.len() {
            return Err(WorkflowServiceError::Serialization(format!(
                "workflow '{}' catalog counts do not match its definition",
                workflow.id
            )));
        }
    }
    Ok(())
}

fn read_catalog(
    catalog: &RwLock<WorkflowCatalog>,
) -> Result<RwLockReadGuard<'_, WorkflowCatalog>, WorkflowServiceError> {
    catalog.read().map_err(|_| {
        WorkflowServiceError::Unavailable("workflow catalog lock is poisoned".to_string())
    })
}

fn write_catalog(
    catalog: &RwLock<WorkflowCatalog>,
) -> Result<RwLockWriteGuard<'_, WorkflowCatalog>, WorkflowServiceError> {
    catalog.write().map_err(|_| {
        WorkflowServiceError::Unavailable("workflow catalog lock is poisoned".to_string())
    })
}

fn persist_catalog(path: &Path, catalog: &WorkflowCatalog) -> Result<(), WorkflowServiceError> {
    let encoded = serde_json::to_vec_pretty(catalog)
        .map_err(|error| WorkflowServiceError::Serialization(error.to_string()))?;
    echo_core::utils::fs::atomic_write(path, &encoded).map_err(|source| WorkflowServiceError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKFLOW: &str = r#"
name: route
nodes:
  - name: route
    type: router
edges: []
entry: route
finish: [route]
"#;

    #[tokio::test]
    async fn service_owns_durable_crud_and_framework_graph_execution() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let path = temp.path().join("workflows.json");
        let service = WorkflowService::open(&path).map_err(|error| error.to_string())?;
        let created = service
            .create("route".to_string(), WORKFLOW.to_string())
            .map_err(|error| error.to_string())?;
        assert_eq!(created.node_count, 1);
        assert_eq!(created.edge_count, 0);
        let result = service
            .execute(&created.id, Some(serde_json::json!({"value": "ok"})))
            .await
            .map_err(|error| error.to_string())?;
        assert!(result.success);

        let reopened = WorkflowService::open(&path).map_err(|error| error.to_string())?;
        assert_eq!(
            reopened
                .get(&created.id)
                .map_err(|error| error.to_string())?,
            created
        );
        reopened
            .delete(&created.id)
            .map_err(|error| error.to_string())?;
        assert!(
            reopened
                .list()
                .map_err(|error| error.to_string())?
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn shared_surface_command_uses_the_same_catalog_and_graph() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let service = WorkflowService::open(temp.path().join("workflows.json"))
            .map_err(|error| error.to_string())?;
        let created_json = service
            .execute_command(&format!("create route {WORKFLOW}"))
            .await
            .map_err(|error| error.to_string())?;
        let created: StoredWorkflow =
            serde_json::from_str(&created_json).map_err(|error| error.to_string())?;

        let listed = service
            .execute_command("list")
            .await
            .map_err(|error| error.to_string())?;
        let listed: Vec<StoredWorkflow> =
            serde_json::from_str(&listed).map_err(|error| error.to_string())?;
        assert_eq!(listed, vec![created.clone()]);

        let execution = service
            .execute_command(&format!("run {} {{\"source\":\"surface\"}}", created.id))
            .await
            .map_err(|error| error.to_string())?;
        let execution: WorkflowExecution =
            serde_json::from_str(&execution).map_err(|error| error.to_string())?;
        assert!(execution.success);
        Ok(())
    }

    #[test]
    fn invalid_definition_never_enters_catalog() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let service = WorkflowService::open(temp.path().join("workflows.json"))
            .map_err(|error| error.to_string())?;
        assert!(
            service
                .create("broken".to_string(), "nodes: [".to_string())
                .is_err()
        );
        assert!(
            service
                .list()
                .map_err(|error| error.to_string())?
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn corrupted_or_semantically_inconsistent_catalog_fails_closed() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let path = temp.path().join("workflows.json");
        fs::write(&path, b"{not-json").map_err(|error| error.to_string())?;
        assert!(WorkflowService::open(&path).is_err());

        fs::remove_file(&path).map_err(|error| error.to_string())?;
        let service = WorkflowService::open(&path).map_err(|error| error.to_string())?;
        let created = service
            .create("route".to_string(), WORKFLOW.to_string())
            .map_err(|error| error.to_string())?;
        let encoded = fs::read(&path).map_err(|error| error.to_string())?;
        let mut catalog: WorkflowCatalog =
            serde_json::from_slice(&encoded).map_err(|error| error.to_string())?;
        let stored = catalog
            .workflows
            .get_mut(&created.id)
            .ok_or_else(|| "created workflow missing from catalog".to_string())?;
        stored.node_count = stored.node_count.saturating_add(1);
        fs::write(
            &path,
            serde_json::to_vec_pretty(&catalog).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        assert!(WorkflowService::open(path).is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn catalog_symlink_is_rejected_without_changing_external_file() -> Result<(), String> {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let external = temp.path().join("external.json");
        let path = temp.path().join("workflows.json");
        let original = br#"{"workflows":{}}"#;
        fs::write(&external, original).map_err(|error| error.to_string())?;
        symlink(&external, &path).map_err(|error| error.to_string())?;

        assert!(WorkflowService::open(&path).is_err());
        assert_eq!(
            fs::read(&external).map_err(|error| error.to_string())?,
            original
        );
        Ok(())
    }
}
