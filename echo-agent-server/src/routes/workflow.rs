//! 工作流 API

use axum::{Json, extract::State};
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
///
/// Executes a stored workflow definition. For complex multi-step workflows
/// with checkpoint/resume, use the BackgroundTaskService API (`/api/tasks`)
/// with `kind: "workflow"` instead.
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

    // 构建执行计划并顺序执行所有步骤
    let mut results = Vec::new();
    let mut context_input = serde_json::to_string(&req.input).unwrap_or_default();

    for (i, step) in def.steps.iter().enumerate() {
        match step.step_type.as_str() {
            "prompt" => {
                let prompt = step.content.replace("{input}", &context_input);
                let prompt_for_agent = prompt.clone();

                // 执行 prompt 步骤
                match state
                    .connection
                    .agent
                    .read_async(|agent| Box::pin(async move { agent.chat(&prompt_for_agent).await }))
                    .await
                {
                    Ok(answer) => {
                        results.push(serde_json::json!({
                            "step": i,
                            "type": "prompt",
                            "content": prompt,
                            "output": answer,
                            "status": "completed"
                        }));
                        // 将输出作为下一步的输入
                        context_input = answer;
                    }
                    Err(e) => {
                        results.push(serde_json::json!({
                            "step": i,
                            "type": "prompt",
                            "content": prompt,
                            "status": "failed",
                            "error": e.to_string()
                        }));
                        return Ok(Json(WorkflowResult {
                            success: false,
                            output: serde_json::json!({"results": results}),
                            error: Some(format!("步骤 {} 执行失败: {}", i, e)),
                        }));
                    }
                }
            }
            "tool" => {
                let tool_name = step.tool_name.as_deref().unwrap_or(&step.content);
                let tool_args = step.tool_args.clone().unwrap_or_else(|| {
                    serde_json::json!({"input": context_input})
                });

                // 构造指令让 agent 调用指定工具
                let tool_prompt = format!(
                    "Please use the '{}' tool with the following arguments: {}. Execute the tool and return the result.",
                    tool_name,
                    serde_json::to_string(&tool_args).unwrap_or_default()
                );

                match state
                    .connection
                    .agent
                    .read_async(|agent| {
                        let tool_prompt = tool_prompt.clone();
                        Box::pin(async move { agent.chat(&tool_prompt).await })
                    })
                    .await
                {
                    Ok(output) => {
                        results.push(serde_json::json!({
                            "step": i,
                            "type": "tool",
                            "tool": tool_name,
                            "args": tool_args,
                            "output": output,
                            "status": "completed"
                        }));
                        // 将输出作为下一步的输入
                        context_input = output;
                    }
                    Err(e) => {
                        results.push(serde_json::json!({
                            "step": i,
                            "type": "tool",
                            "tool": tool_name,
                            "status": "failed",
                            "error": e.to_string()
                        }));
                        return Ok(Json(WorkflowResult {
                            success: false,
                            output: serde_json::json!({"results": results}),
                            error: Some(format!("步骤 {} 工具执行失败: {}", i, e)),
                        }));
                    }
                }
            }
            _ => {
                results.push(serde_json::json!({
                    "step": i,
                    "type": step.step_type,
                    "status": "skipped",
                    "reason": "unsupported step type"
                }));
            }
        }
    }

    let output = serde_json::json!({
        "steps_executed": results.len(),
        "results": results,
        "final_output": context_input
    });

    Ok(Json(WorkflowResult {
        success: true,
        output,
        error: None,
    }))
}
