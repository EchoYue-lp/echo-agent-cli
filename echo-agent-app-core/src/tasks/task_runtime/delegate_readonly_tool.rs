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
         - project_explorer: 探索项目结构、配置、文档\n\
         - code_reviewer: 审查代码 bug/架构/边界条件\n\
         - test_planner: 规划测试和验证方案\n\
         - summary_writer: 汇总多个 worker 发现,综合结论\n\
         - data_profiler: 检查数据来源/schema/质量\n\
         - analysis_reviewer: 审查分析方法/统计假设/图表\n\
         - reproducibility_planner: 规划可复现路径和交付物\n\
         - literature_scout: 探索学术资料/检索策略\n\
         - evidence_reviewer: 审查证据质量/引用可靠性\n\
         - synthesis_planner: 规划综述/证据表/报告结构\n\
         - medical_literature_scout: 探索医学指南/系统综述\n\
         - clinical_evidence_reviewer: 审查临床证据等级/适用性\n\
         - safety_reviewer: 审查安全边界/免责声明/过度建议风险"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "agent_role": {
                    "type": "string",
                    "enum": [
                        "project_explorer",
                        "code_reviewer",
                        "test_planner",
                        "summary_writer",
                        "data_profiler",
                        "analysis_reviewer",
                        "reproducibility_planner",
                        "literature_scout",
                        "evidence_reviewer",
                        "synthesis_planner",
                        "medical_literature_scout",
                        "clinical_evidence_reviewer",
                        "safety_reviewer"
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
                Ok(subagent_result) => Ok(ToolResult::success(subagent_result.output)),
                Err(e) => Ok(ToolResult::error(format!("delegate_readonly 失败: {e}"))),
            }
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
