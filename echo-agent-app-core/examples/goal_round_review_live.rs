//! 真实模型轮次总结验证；所有 Goal、任务事实和产物均为专属目录中的验收样本。
//! 应用正常加载配置并构造客户端，配置正文、密钥和密钥摘要均不进入回执。
//! 此入口验证 ReactAgent + 原 TaskRuntime projector；不冒充 GUI/IM/自动续接验收。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow, ensure};
use echo_agent::agent::{
    Agent, AgentEvent, AgentInvocationContext, CancellationToken, ExecutionStatusKind,
    ReactAgentBuilder,
};
use echo_agent::compression::{ContextProjection, PreModelContextProjector, ProjectionContext};
use echo_agent::llm::ChatRequest;
use echo_agent::llm::types::Message;
use echo_agent_app_core::api::tasks::task_runtime::compact_context::{
    TaskRuntimeContextProjector, TaskRuntimeProjectionRegistry,
};
use echo_agent_app_core::api::tasks::task_runtime::store::{
    RunTurnClaimOutcome, RunTurnCompletion,
};
use echo_agent_app_core::api::tasks::task_runtime::{
    AttendedMode, DomainProfile, RunTurnOrigin, RunTurnStatus, TaskRunStatus, TaskRuntimeStore,
    TurnVisibility, task_goal_sha256,
};
use echo_agent_app_core::api::{config, data_root, infra, model_config};
use futures::{StreamExt, future::BoxFuture};
use serde_json::{Value, json};

struct RequestCapture {
    inner: TaskRuntimeContextProjector,
    requests: Mutex<Vec<Value>>,
}

impl PreModelContextProjector for RequestCapture {
    fn project<'a>(
        &'a self,
        context: &'a ProjectionContext,
    ) -> BoxFuture<'a, echo_agent::error::Result<Vec<ContextProjection>>> {
        self.inner.project(context)
    }

    fn is_required(&self) -> bool {
        true
    }

    fn validate_model_request<'a>(
        &'a self,
        context: &'a ProjectionContext,
        request: &'a ChatRequest,
        artifacts: Option<&'a echo_agent::tools::artifact::ToolOutputArtifactConfig>,
    ) -> BoxFuture<'a, echo_agent::error::Result<()>> {
        Box::pin(async move {
            self.inner
                .validate_model_request(context, request, artifacts)
                .await?;
            let mut requests = self.requests.lock().map_err(|_| {
                echo_agent::error::ReactError::Other("sample request capture poisoned".into())
            })?;
            // ChatRequest 正文不包含客户端认证头；不序列化 runtime/config/client。
            requests.push(json!({"messages": request.messages}));
            Ok(())
        })
    }
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    std::fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}

fn redact(text: &str, token: Option<&str>) -> String {
    match token.filter(|token| !token.is_empty()) {
        Some(token) => text.replace(token, "[redacted]"),
        None => text.to_owned(),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let config_path = args.next().ok_or_else(|| anyhow!("需要配置路径参数"))?;
    let output = PathBuf::from(
        args.next()
            .ok_or_else(|| anyhow!("需要新的样本输出目录参数"))?,
    );
    ensure!(args.next().is_none(), "用法：配置路径 新输出目录");
    ensure!(
        output.is_absolute() && !output.exists(),
        "输出目录必须为尚不存在的绝对路径"
    );
    std::fs::create_dir_all(&output)?;
    data_root::configure(output.join("data")).map_err(|_| anyhow!("隔离数据根设置失败"))?;
    // 仅经应用原加载/解析路径使用认证；不打印 serde 配置错误中的输入片段。
    let app = config::load_config(Some(&config_path))
        .map_err(|_| anyhow!("应用配置加载失败；未输出配置内容"))?;
    let requested_provider = |id: &str| {
        id.eq_ignore_ascii_case("ls-ds")
            || app
                .model_providers
                .get(id)
                .is_some_and(|provider| provider.name.eq_ignore_ascii_case("ls-ds"))
    };
    let available = app
        .configured_models
        .iter()
        .filter(|model| requested_provider(&model.provider) && model.enabled)
        .map(|model| {
            json!({
                "configured_id":model.id, "display_name":model.display_name,
                "model":model.model, "api_protocol":model.api_protocol,
            })
        })
        .collect::<Vec<_>>();
    let providers = app
        .model_providers
        .iter()
        .map(|(id, provider)| json!({"id":id, "name":provider.name}))
        .collect::<Vec<_>>();
    write_json(
        &output.join("model-selection.json"),
        &json!({
            "providers":providers, "matching_enabled_models":available,
        }),
    )?;
    let matching = app
        .configured_models
        .iter()
        .filter(|model| {
            let label = format!("{} {}", model.display_name, model.model).to_ascii_lowercase();
            let parts = label
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '.'))
                .collect::<Vec<_>>();
            model.enabled
                && requested_provider(&model.provider)
                && parts.contains(&"deepseek")
                && parts.contains(&"flash")
                && parts.contains(&"v4.1")
        })
        .collect::<Vec<_>>();
    ensure!(
        matching.len() == 1,
        "ls-ds 下应有且仅有一个启用的 DeepSeek v4.1 Flash；实际匹配数 {}，未调用模型",
        matching.len()
    );
    let selected = matching.first().ok_or_else(|| anyhow!("模型选择缺失"))?;
    let runtime = model_config::resolve_runtime_model(&app, Some(&selected.id))?;
    let prepared = infra::prepare_runtime_llm(&runtime)
        .map_err(|_| anyhow!("应用模型客户端准备失败；未输出认证内容"))?;
    let identity = json!({
        "provider": runtime.provider, "model": runtime.model,
        "display_name": runtime.display_name, "configured_id": runtime.id,
        "api_protocol": runtime.api_protocol,
        "endpoint_origin": runtime.base_url.as_deref()
            .and_then(|url| reqwest::Url::parse(url).ok())
            .map(|url| url.origin().ascii_serialization()),
        "scope": "live provider + ReactAgent + TaskRuntime projector; fixture work observations; no GUI/IM/continuation launcher",
    });
    write_json(&output.join("identity.json"), &identity)?;
    println!("{}", identity);

    let cases = [
        (
            "partial",
            "交付 A 数据导出、B 回归验收、C 发布说明；三项均完成才能结束整个任务。",
            vec![
                "本轮只推进 A。验收宿主已在样本目录生成 data.csv；B 没有回归结果，C 没有发布说明。宿主观察见下文。请处理本轮工作并正常回复。",
                "下一轮改为推进 B。宿主已检查 B 的输入要求，但所需原始数据缺失，尚未运行回归；A 的文件仍在，C 仍未编写。请处理本轮工作并正常回复。",
            ],
        ),
        (
            "organization-only",
            "完成数据校验和真实回归验收，并交付可核对的证据。",
            vec![
                "本轮宿主只整理了材料索引 index.md；没有执行数据校验、没有运行测试、没有原生操作或发布。请依据这些已观察事实正常回复，不虚构其它操作。",
            ],
        ),
        (
            "blocked-format",
            "完成 A 数据转换、B 验证、C 交付；每轮报告使用‘进展’与‘未完成及下一步’两个标题。",
            vec![
                "本轮尝试 A 时，验收宿主观察到原始输入数据不存在，因此未执行转换；B/C 均未完成。不能取得缺失输入，也没有新增通过证据。请正常回复。",
            ],
        ),
    ];

    for (case, goal, turns) in cases {
        let directory = output.join(case);
        std::fs::create_dir_all(&directory)?;
        std::fs::write(directory.join("data.csv"), "id,value\n1,42\n")?;
        std::fs::write(
            directory.join("index.md"),
            "# 样本材料索引\n尚无数据校验或回归验收结果。\n",
        )?;
        let store = Arc::new(TaskRuntimeStore::new_in_memory_with_shadow_root(
            directory.join("tasks"),
        )?);
        let run_id = format!("live-round-{case}");
        store.create_run(
            &run_id,
            &store.active_workspace_id(),
            case,
            "root",
            DomainProfile::General,
            goal,
            "agent_task_plan",
            AttendedMode::Attended,
        )?;
        store.configure_run_continuation(&run_id, true, false, Some(32_000), Some(600))?;
        store.transition_run(&run_id, TaskRunStatus::Running)?;
        let registry = Arc::new(TaskRuntimeProjectionRegistry::new());
        let _registration = registry.register(&run_id, store.clone());
        let capture = Arc::new(RequestCapture {
            inner: TaskRuntimeContextProjector::new(registry),
            requests: Mutex::new(Vec::new()),
        });
        let agent = ReactAgentBuilder::new()
            .model(&runtime.model)
            .llm_client(prepared.client.clone())
            .system_prompt(
                "你是 Eclip。完成已授权工作，事实以本轮宿主观察为准；未验证内容保持未验证。",
            )
            .auto_project_rules(false)
            .max_iterations(6)
            .token_limit(16_000)
            .build()?;
        agent.set_pre_model_context_projector(Some(capture.clone()));
        let mut history = Vec::new();
        for (index, instruction) in turns.into_iter().enumerate() {
            let turn_id = format!("{case}-{}", index.saturating_add(1));
            ensure!(
                matches!(
                    store.claim_run_turn(
                        &run_id,
                        &turn_id,
                        if index == 0 {
                            RunTurnOrigin::User
                        } else {
                            RunTurnOrigin::Continuation
                        },
                        TurnVisibility::Visible
                    )?,
                    RunTurnClaimOutcome::Started(_)
                ),
                "样本轮次未接纳"
            );
            let actual_observation = if case == "partial" && index == 0 {
                format!(
                    "\n实际样本文件：{}；宿主回读：{}",
                    directory.join("data.csv").display(),
                    std::fs::read_to_string(directory.join("data.csv"))?
                )
            } else {
                String::new()
            };
            let input = format!("{instruction}{actual_observation}");
            let cancel = CancellationToken::new();
            let invocation = AgentInvocationContext {
                history: Some(history.clone()),
                runtime: Some(echo_agent::tools::ExternalRunContext {
                    run_id: Some(run_id.clone()),
                    turn_id: Some(turn_id.clone()),
                    conversation_id: Some(case.to_owned()),
                    execution_id: Some(turn_id.clone()),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let started = std::time::Instant::now();
            let mut events = agent
                .execute_stream_message_with_invocation_context(
                    Message::user(input.clone()),
                    cancel.clone(),
                    invocation,
                )
                .await
                .map_err(|_| anyhow!("模型流启动失败；未输出可能包含认证信息的错误内容"))?;
            let mut answer = None;
            let mut failure = None;
            let mut settled = false;
            let mut terminal_statuses = Vec::new();
            let consume = async {
                while let Some(event) = events.next().await {
                    match event {
                        Ok(AgentEvent::FinalAnswer(text)) => answer = Some(text),
                        Ok(AgentEvent::ExecutionStatus(status)) => {
                            settled |= status.status == ExecutionStatusKind::Settled;
                            if matches!(
                                status.status,
                                ExecutionStatusKind::Settled
                                    | ExecutionStatusKind::Failed
                                    | ExecutionStatusKind::Cancelled
                            ) {
                                terminal_statuses.push(json!({
                                    "status":status.status, "execution_id":status.execution_id,
                                    "sequence":status.sequence, "request_id":status.request_id,
                                    "reason":status.reason.as_deref()
                                        .map(|text| redact(text, runtime.auth_token.as_deref())),
                                }));
                                if status.status != ExecutionStatusKind::Settled {
                                    failure = Some("runtime did not settle successfully".into());
                                }
                            }
                        }
                        Ok(AgentEvent::Cancelled) => failure = Some("runtime cancelled".into()),
                        Ok(AgentEvent::Error { message, .. }) => failure = Some(message),
                        Err(error) => failure = Some(error.to_string()),
                        _ => {}
                    }
                }
            };
            if tokio::time::timeout(std::time::Duration::from_secs(180), consume)
                .await
                .is_err()
            {
                cancel.cancel();
                failure = Some("live sample timeout; no success claimed".into());
            }
            drop(events);
            let finished = failure.is_none() && answer.is_some() && settled;
            store.finish_run_turn(
                &run_id,
                RunTurnCompletion {
                    turn_id: &turn_id,
                    status: if finished {
                        RunTurnStatus::Ended
                    } else {
                        RunTurnStatus::Failed
                    },
                    elapsed_seconds: started.elapsed().as_secs(),
                    final_message_id: None,
                    error_fingerprint: failure.as_deref().map(|_| "live-provider-failure"),
                },
            )?;
            let current = store
                .get_run(&run_id)?
                .ok_or_else(|| anyhow!("样本Run缺失"))?;
            ensure!(
                current.goal == goal
                    && current.goal_revision == 1
                    && current.goal_sha256 == task_goal_sha256(goal),
                "真实调用后总Goal身份发生变化"
            );
            let requests = capture
                .requests
                .lock()
                .map_err(|_| anyhow!("capture poisoned"))?
                .clone();
            let receipt = json!({
                "case":case, "turn_id":turn_id, "goal":goal, "input":input,
                "answer":answer, "failure":failure.as_deref().map(|s| redact(s, runtime.auth_token.as_deref())),
                "requests_so_far":requests, "goal_revision":current.goal_revision,
                "goal_sha256":current.goal_sha256, "run_status":current.status,
                "runtime_terminals":terminal_statuses,
                "completion_gate":store.completion_gate_report(&run_id)?,
                "provider_turn_finished":finished, "behavior_assessment":"pending_manual_review",
            });
            write_json(
                &directory.join(format!("turn-{}.json", index.saturating_add(1))),
                &receipt,
            )?;
            ensure!(finished, "真实模型调用失败；已保存脱敏回执，停止后续样本");
            let text = answer.context("模型没有最终回答")?;
            println!("{case}/{}: {}", index.saturating_add(1), text);
            history.push(Message::user(input));
            history.push(Message::assistant(text));
        }
        store.request_pause(&run_id)?;
    }
    Ok(())
}
