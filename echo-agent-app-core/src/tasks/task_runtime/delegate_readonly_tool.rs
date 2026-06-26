//! delegate_readonly 工具:让主 agent 在 ReAct 循环里派只读 worker。
//!
//! 设计:工具持 AgentHandle,execute 时 read_async 调
//! delegate_to_agent_with_parent_and_cancel。run_id 和 cancel 从 task_local 读取。
//!
//! 参考:echo-agent 的 AgentDispatchTool(hold Arc<SubagentExecutor> + cancel handle)。
//!
//! # 运行时拦截 (spec §3.1.1 强化)
//!
//! 若当前 run 已有 plan(主 agent 调过 task_create),说明这是一次正式编排,
//! 必须走 execute_plan → run_dag 路径(wave 调度 + 信号量限流 + 失败传播)。
//! 此时 delegate_readonly 拒绝直接调用,引导 LLM 使用 execute_plan。
//! 仅当 plan 为空(ad-hoc 探索)时才允许直接派 worker。

use std::sync::Arc;

use echo_agent::agent::AgentHandle;
use echo_agent::error;
use echo_agent::tools::{Tool, ToolParameters, ToolResult};
use futures::future::BoxFuture;
use tokio_util::sync::CancellationToken;

use super::store::TaskRuntimeStore;

/// 让主 agent 在 ReAct 循环里派只读 worker 跑独立 ReAct,返回 summary。
///
/// 新增 `store` 字段用于运行时拦截:若当前 run 已有 plan,拒绝直接调用,
/// 引导 LLM 使用 execute_plan(避免绕过 run_dag 调度层)。
pub struct DelegateReadonlyTool {
    pub agent_handle: AgentHandle,
    /// Optional store for plan-existence check. When absent (e.g. test code),
    /// the interception is silently skipped — backward-compatible.
    store: Option<Arc<TaskRuntimeStore>>,
}

impl DelegateReadonlyTool {
    pub fn new(agent_handle: AgentHandle) -> Self {
        Self {
            agent_handle,
            store: None,
        }
    }

    pub fn with_store(mut self, store: Arc<TaskRuntimeStore>) -> Self {
        self.store = Some(store);
        self
    }
}

impl Tool for DelegateReadonlyTool {
    fn name(&self) -> &str {
        "delegate_readonly"
    }

    fn description(&self) -> &str {
        "派一个只读 worker(独立 ReAct agent)执行任务并返回 summary。\
         用于调研/审查/分析类子任务。worker 跑独立 ReAct,不修改文件,返回结论给你。\n\
         \n\
         ⚠️ 重要:如果当前任务已经用 task_create 拆了计划,必须调 execute_plan \
         统一交给并行执行引擎,不要逐个 delegate_readonly。\
         仅在没有 plan 的 ad-hoc 探索阶段才直接使用本工具。\n\
         \n\
         可用 agent_role (必须从下列选择,不要自创):\n\
         - explorer: 探索代码库/数据/文献/配置/文档,建立领域地图\n\
         - reviewer: 审查代码 bug/分析方法/证据质量/安全边界\n\
         - planner: 规划验证/测试/可复现路径/综述结构\n\
         - summarizer: 汇总多个 worker 发现,综合结论"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "agent_role": {
                    "type": "string",
                    "enum": [
                        "explorer",
                        "reviewer",
                        "planner",
                        "summarizer"
                    ],
                    "description": "worker 角色名。必须从上述 enum 值中精确选择,不要自创。"
                },
                "task": {
                    "type": "string",
                    "description": "给 worker 的任务 prompt"
                }
            },
            "required": ["agent_role", "task"]
        })
    }

    fn execute<'a>(&'a self, params: ToolParameters) -> BoxFuture<'a, error::Result<ToolResult>> {
        Box::pin(async move {
            // 从 task_local 拿 run_id(与 task_* 工具一致)
            let run_id = match super::task_tools::require_run_id() {
                Ok(id) => id,
                Err(e) => return Ok(e),
            };

            // ── 运行时拦截: plan 存在时禁止直接 delegate_readonly ──
            // 若主 agent 已经 task_create 拆了 plan,说明这是一次正式编排。
            // 此时必须调 execute_plan 走 run_dag(wave 调度 + 信号量限流
            // + 失败传播),不允许逐个 delegate_readonly 绕过调度层。
            if let Some(ref store) = self.store {
                #[allow(clippy::collapsible_if)]
                // nested let-Ok guards read clearer than a let-chain here
                if let Ok(Some(plan)) = store.get_plan(&run_id) {
                    if !plan.tasks.is_empty() {
                        return Ok(ToolResult::error(
                            "当前任务已有计划(plan 中有 task),请调 execute_plan 统一交给并行执行引擎,\
                             不要逐个 delegate_readonly。引擎会按依赖关系自动并行/串行调度。",
                        ));
                    }
                }
            }

            let role = params
                .get("agent_role")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let task = params
                .get("task")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            if role.is_empty() || task.is_empty() {
                return Ok(ToolResult::error("agent_role 和 task 不能为空"));
            }

            // 从 task_local 拿 cancel token
            let cancel = super::task_tools::CURRENT_CANCEL
                .try_with(|c| c.clone())
                .unwrap_or_else(|_| CancellationToken::new());

            // Read delegate depth from task_local and increment by 1 for
            // the next delegation level. The framework's MAX_DELEGATE_DEPTH=3
            // guards against runaway recursion (see executor.rs).
            let depth = super::task_tools::CURRENT_DELEGATE_DEPTH
                .try_with(|d| d.get() + 1)
                .unwrap_or(0);

            let handle = self.agent_handle.clone();
            // Clone for post-dispatch token recording (the async move closure
            // below takes ownership of role/task/run_id).
            let role_for_usage = role.clone();
            let task_for_usage = task.clone();
            let run_id_for_usage = run_id.clone();
            let result = handle
                .read_async(|a| {
                    Box::pin(async move {
                        a.delegate_to_agent_with_parent_and_cancel(
                            &role, &task, &run_id, cancel, depth,
                        )
                        .await
                    })
                })
                .await;

            match result {
                Ok(subagent_result) => {
                    // 消费 token 统计(根因②: 之前只取 output 丢弃 usage)。
                    // 经 execute_plan 路径的 worker token 由 execute_task 统计;
                    // 这里补 delegate_readonly 路径,防御性确保两条路都不丢 token。
                    if let Some(ref store) = self.store {
                        let usage_payload = match &subagent_result.usage {
                            Some(stats) => stats.to_payload(&run_id_for_usage),
                            None => serde_json::json!({
                                "session_id": run_id_for_usage,
                                "model": "unknown",
                                "usage_reported": false,
                                "reason": "delegate_readonly: provider returned no usage",
                            }),
                        };
                        let worker_trace_id = format!("{run_id_for_usage}:{role_for_usage}");
                        let task_id = format!("delegate_{}", chrono::Utc::now().timestamp_millis());
                        let title: String = task_for_usage.chars().take(80).collect();
                        #[allow(clippy::collapsible_if)]
                        // nested let-Some/let-Err guards the usage-record path; let-chain would obscure the match above
                        if let Err(e) = store.record_worker_llm_usage(
                            &run_id_for_usage,
                            &task_id,
                            &worker_trace_id,
                            &role_for_usage,
                            &title,
                            usage_payload.clone(),
                        ) {
                            tracing::warn!(error = %e, "delegate_readonly: 记录 token 失败");
                        }
                    }
                    Ok(ToolResult::success(subagent_result.output))
                }
                Err(e) => Ok(ToolResult::error(format!("delegate_readonly 失败: {e}"))),
            }
        })
    }

    fn execute_with_context<'a>(
        &'a self,
        params: ToolParameters,
        ctx: &'a echo_core::tools::ToolContext,
    ) -> BoxFuture<'a, error::Result<ToolResult>> {
        Box::pin(async move {
            super::task_tools::scoped_with_ctx_run_id(ctx, || self.execute(params)).await
        })
    }
}

/// Register `delegate_readonly` tool on an agent via its handle.
/// Call this AFTER the agent is wrapped in an AgentHandle.
///
/// The optional `store` enables the plan-existence interception:
/// when the current run already has a plan with tasks, the tool refuses
/// direct calls and instructs the LLM to use `execute_plan` instead.
pub async fn register_delegate_readonly_on_handle(
    handle: &AgentHandle,
    store: Option<Arc<TaskRuntimeStore>>,
) {
    let mut tool = DelegateReadonlyTool::new(handle.clone());
    if let Some(s) = store {
        tool = tool.with_store(s);
    }
    handle
        .write(|a| {
            a.add_tool(Box::new(tool));
        })
        .await;
    tracing::info!("Registered delegate_readonly tool on agent");
}
