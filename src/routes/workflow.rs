//! 工作流 API

use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::error::AppError;
use crate::state::{AppState, StoredWorkflow, WorkflowDef};
use echo_agent::agent::Agent;

// ── 类型定义 ─────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct WorkflowInfo {
    pub id: String,
    pub name: String,
    pub node_count: usize,
    pub edge_count: usize,
}

#[derive(Debug, Deserialize)]
pub struct CreateWorkflowRequest {
    pub id: String,
    pub definition: String, // YAML 或 JSON
}

#[derive(Debug, Deserialize)]
pub struct ExecuteWorkflowRequest {
    pub input: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct WorkflowResult {
    pub success: bool,
    pub output: serde_json::Value,
    pub error: Option<String>,
}

// ── API 处理器 ───────────────────────────────────────────────────

/// POST /api/workflow/create
pub async fn create_workflow(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateWorkflowRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    tracing::info!("创建工作流: {}", req.id);

    // 解析 JSON 定义
    let def: WorkflowDef = match serde_json::from_str(&req.definition) {
        Ok(d) => d,
        Err(e) => {
            return Err(AppError::Internal(format!(
                "工作流定义解析失败: {}. 请提供 JSON 格式: {{\"name\": \"...\", \"steps\": [...]}}",
                e
            )));
        }
    };

    let node_count = def.steps.len();
    let edge_count = node_count.saturating_sub(1);

    let workflow = StoredWorkflow {
        id: req.id.clone(),
        name: def.name.clone(),
        definition: req.definition.clone(),
        node_count,
        edge_count,
        created_at: chrono::Utc::now().to_rfc3339(),
        updated_at: chrono::Utc::now().to_rfc3339(),
    };

    let mut workflows = state.history.workflows.write().await;
    workflows.insert(req.id.clone(), workflow);

    Ok(Json(serde_json::json!({
        "success": true,
        "id": req.id,
        "name": def.name,
        "steps": node_count
    })))
}

/// GET /api/workflow
pub async fn list_workflows(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<WorkflowInfo>>, AppError> {
    let workflows = state.history.workflows.read().await;
    let list: Vec<WorkflowInfo> = workflows
        .values()
        .map(|w| WorkflowInfo {
            id: w.id.clone(),
            name: w.name.clone(),
            node_count: w.node_count,
            edge_count: w.edge_count,
        })
        .collect();
    Ok(Json(list))
}

/// GET /api/workflow/:id
pub async fn get_workflow(
    axum::extract::Path(id): axum::extract::Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<WorkflowInfo>, AppError> {
    let workflows = state.history.workflows.read().await;
    let w = workflows
        .get(&id)
        .ok_or_else(|| AppError::NotFound(format!("工作流 '{}' 未找到", id)))?;

    Ok(Json(WorkflowInfo {
        id: w.id.clone(),
        name: w.name.clone(),
        node_count: w.node_count,
        edge_count: w.edge_count,
    }))
}

/// DELETE /api/workflow/:id
pub async fn delete_workflow(
    axum::extract::Path(id): axum::extract::Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, AppError> {
    tracing::info!("删除工作流: {}", id);
    let mut workflows = state.history.workflows.write().await;
    workflows
        .remove(&id)
        .ok_or_else(|| AppError::NotFound(format!("工作流 '{}' 未找到", id)))?;

    Ok(Json(serde_json::json!({"success": true})))
}

/// POST /api/workflow/:id/execute
pub async fn execute_workflow(
    axum::extract::Path(id): axum::extract::Path<String>,
    State(state): State<Arc<AppState>>,
    Json(req): Json<ExecuteWorkflowRequest>,
) -> Result<Json<WorkflowResult>, AppError> {
    tracing::info!("执行工作流: {}", id);

    // 获取工作流定义
    let def = {
        let workflows = state.history.workflows.read().await;
        let w = workflows
            .get(&id)
            .ok_or_else(|| AppError::NotFound(format!("工作流 '{}' 未找到", id)))?;
        let parsed: WorkflowDef = serde_json::from_str(&w.definition)
            .map_err(|e| AppError::Internal(format!("工作流定义解析失败: {}", e)))?;
        parsed
    };

    // 构建执行计划
    let mut results = Vec::new();
    let context_input = serde_json::to_string(&req.input).unwrap_or_default();

    for (i, step) in def.steps.iter().enumerate() {
        match step.step_type.as_str() {
            "prompt" => {
                let prompt = step.content.replace("{input}", &context_input);
                results.push(serde_json::json!({
                    "step": i,
                    "type": "prompt",
                    "content": prompt,
                    "status": "recorded"
                }));
            }
            "tool" => {
                let tool_name = step.tool_name.as_deref().unwrap_or(&step.content);
                results.push(serde_json::json!({
                    "step": i,
                    "type": "tool",
                    "tool": tool_name,
                    "status": "recorded"
                }));
            }
            _ => {
                results.push(serde_json::json!({
                    "step": i,
                    "type": step.step_type,
                    "status": "skipped"
                }));
            }
        }
    }

    // 如果有 prompt 步骤，执行第一个
    let output = if let Some(prompt_step) = def.steps.iter().find(|s| s.step_type == "prompt") {
        let prompt = prompt_step.content.replace("{input}", &context_input);
        match state.connection.agent.read_async(|agent| Box::pin(async move { agent.chat(&prompt).await })).await {
            Ok(answer) => serde_json::json!({
                "answer": answer,
                "steps_executed": results.len(),
                "results": results
            }),
            Err(e) => {
                return Ok(Json(WorkflowResult {
                    success: false,
                    output: serde_json::json!({"results": results}),
                    error: Some(format!("执行失败: {}", e)),
                }));
            }
        }
    } else {
        serde_json::json!({
            "steps": results,
            "message": "工作流无 prompt 步骤，仅记录执行计划"
        })
    };

    Ok(Json(WorkflowResult {
        success: true,
        output,
        error: None,
    }))
}
