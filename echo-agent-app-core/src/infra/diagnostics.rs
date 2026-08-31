/// 诊断结果
pub struct DoctorResult {
    pub issues: Vec<String>,
    pub checks: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoctorConnectivity {
    Skip,
    Probe,
}

/// Send a minimal chat request to verify the model is reachable and responding.
async fn probe_model_connectivity(model: &str) -> echo_agent::error::Result<()> {
    use echo_agent::error::ReactError;
    let mut app_config = crate::config::load_config(None);
    crate::config::apply_env_overrides(&mut app_config);
    let runtime = model_config::resolve_runtime_model(&app_config, Some(model))
        .map_err(|error| ReactError::Other(error.to_string()))?;
    let prepared = prepare_runtime_llm(&runtime).map_err(ReactError::Other)?;
    let response = prepared
        .client
        .chat(echo_agent::llm::ChatRequest {
            messages: vec![echo_agent::prelude::Message::user("hi".to_string())],
            temperature: Some(0.0),
            max_tokens: Some(1),
            ..Default::default()
        })
        .await?;

    if response
        .content()
        .map(|content| content.is_empty())
        .unwrap_or(true)
    {
        return Err(ReactError::Other(
            "Model returned empty response".to_string(),
        ));
    }

    Ok(())
}

/// 执行基础环境诊断（API Key、配置文件、数据目录等）
pub fn run_base_doctor() -> DoctorResult {
    let mut config = crate::config::load_config(None);
    crate::config::apply_env_overrides(&mut config);
    let model = model_config::resolve_runtime_model(&config, None)
        .map(|runtime| runtime.model)
        .unwrap_or_else(|_| "not configured".to_string());
    run_base_doctor_for_model(&model)
}

/// 执行基础环境诊断（API Key、配置文件、数据目录等）
pub fn run_base_doctor_for_model(model: &str) -> DoctorResult {
    run_base_doctor_for_model_with_connectivity(model, DoctorConnectivity::Skip)
}

/// 执行基础环境诊断（API Key、配置文件、数据目录等）
pub fn run_base_doctor_for_model_with_connectivity(
    model: &str,
    connectivity: DoctorConnectivity,
) -> DoctorResult {
    let mut issues: Vec<String> = Vec::new();
    let mut checks: Vec<String> = Vec::new();

    let base = crate::data_root::user_data_dir();
    let base_display = base.display();

    checks.push(format!("ℹ️  当前模型: {model}"));

    if connectivity == DoctorConnectivity::Probe {
        // block_in_place is required when called from within a tokio task
        // context (e.g. from a Tauri command handler).
        let probe_result = match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                tokio::task::block_in_place(|| handle.block_on(probe_model_connectivity(model)))
            }
            Err(_) => Err(echo_agent::error::ReactError::Other(
                "Not running in a tokio runtime".to_string(),
            )),
        };
        match probe_result {
            Ok(()) => checks.push(format!("✅ 模型连通性: {} 可用", model)),
            Err(e) => issues.push(format!("❌ 模型连通性检查失败: {}", e)),
        }
    }

    let config_path = base.join("config.yaml");
    if config_path.exists() {
        checks.push(format!("✅ 配置文件: {}/config.yaml", base_display));
    } else {
        issues.push(format!(
            "⚠️  未找到配置文件 {}/config.yaml (使用默认配置)",
            base_display
        ));
    }

    let mcp_path = base.join("mcp.json");
    if mcp_path.exists() {
        checks.push(format!("✅ MCP 配置: {}/mcp.json", base_display));
    } else {
        checks.push(format!(
            "ℹ️  未找到 MCP 配置 (如需工具扩展可创建 {}/mcp.json)",
            base_display
        ));
    }

    if base.exists() {
        checks.push(format!("✅ 数据目录: {}/", base_display));
    } else {
        issues.push(format!(
            "⚠️  数据目录 {}/ 不存在 (运行 echo-agent-cli onboard 初始化)",
            base_display
        ));
    }

    let conv_dir = base.join("conversations");
    if conv_dir.exists() {
        checks.push(format!("✅ 对话存储目录: {}/conversations/", base_display));
    } else {
        checks.push("ℹ️  对话存储目录尚未创建 (首次对话后自动创建)".to_string());
    }

    if let Some(root) = crate::project::context::discover_project_root(None) {
        // Instruction files are loaded by InstructionProvider (the single
        // authority); ProjectContext now carries only structural context.
        let provider = crate::instruction_provider::InstructionProvider::load_for(Some(&root));
        let count = [
            provider.user_level.as_ref(),
            provider.repository_level.as_ref(),
            provider.project_level.as_ref(),
            provider.agents_level.as_ref(),
            provider.local_level.as_ref(),
            provider.hot_memory.as_ref(),
        ]
        .iter()
        .filter(|opt| opt.is_some())
        .count();
        if count == 0 {
            checks.push(
                "ℹ️  项目目录已检测到, 但未找到指令文件 (AGENTS.md / user.md / project.md / learned-rules.md 等)"
                    .to_string(),
            );
        } else {
            checks.push(format!("✅ 项目指令: {count} 个文件已加载"));
        }
    } else {
        checks.push("ℹ️  未检测到项目目录 (可在项目根目录创建 .eko/project.md)".to_string());
    }

    DoctorResult { issues, checks }
}

/// Load user hooks from **all** user-config sources into the agent's hook
/// registry, as a single merged `HooksDefinition`.
///
/// 这是 P0-1 修复后的**唯一** user hook 注册入口(bootstrap 路径)。
/// 它通过 [`crate::hook_config_loader::HookConfigLoader::load_merged`] 把
/// 三个来源(eko.yaml 内嵌 + ~/.eko/hooks.yaml + .eko/hooks.yaml)
/// 按固定顺序合并成单个 `HooksDefinition`,然后**一次性**
/// `clear_user_hooks()` + `register_user_hooks(merged)`。
///
/// **重要**:调用方在调用本函数后,不应再单独加载或注册文件 hooks。
/// 文件 hooks 已包含在本函数的合并结果里。
/// `project_root` 必须来自 Agent/workspace execution scope；不得回退到进程 cwd，
/// 否则 GUI focus 与 headless `--project` 会加载错误项目的 hooks。
///
/// 旧的实现只 register `app_config.hooks`(内嵌),把文件来源留给
/// `runtime.rs::bootstrap` 单独 register —— 但 `register_user_hooks`
/// 内部会覆盖 `UserConfig` 单槽位,导致文件来源 clear 掉内嵌来源。
pub async fn load_user_hooks(
    agent: &AgentHandle,
    app_config: &EkoConfig,
    project_root: Option<&std::path::Path>,
) {
    let load_result = crate::hook_config_loader::HookConfigLoader::load_merged_for_workspace(
        app_config,
        project_root,
    );
    for error in &load_result.errors {
        tracing::warn!(%error, "User hook source was not loaded");
    }
    let hooks_def = load_result.definition;
    if hooks_def.is_empty() {
        return;
    }
    let rule_count: usize = hooks_def.rules.values().map(Vec::len).sum();
    agent
        .write_async(|a| {
            Box::pin(async move {
                let mut registry = a.hook_registry().write().await;
                // 一次性 clear + register 合并后的完整 user hook 集。
                // 这里 clear 是为了支持 config reload(避免重复注册);
                // 因为我们已把三源合并,clear 不会丢任何来源。
                registry.clear_user_hooks();
                registry.register_user_hooks(hooks_def);
            })
        })
        .await;
    tracing::info!(
        count = rule_count,
        files = ?load_result.loaded_from,
        "User hooks loaded (merged: inline eko.yaml + hooks.yaml files)"
    );
}

/// Fire SessionStart("startup") hook after hooks are loaded.
///
/// This is called once when the agent first starts up, after all hooks
/// (both skill hooks and user hooks) have been registered, so that
/// registered hooks can react to the startup event.
pub async fn fire_startup_hook(agent: &AgentHandle) {
    agent.read_async(|a| Box::pin(async move {
        let result = a.fire_lifecycle_hook(
            echo_agent::skills::hooks::HookEvent::SessionStart,
            Some("startup"),
        ).await;
        if result.block {
            tracing::warn!(reason = ?result.block_reason, "SessionStart hook blocked agent startup");
        }
    })).await;
    tracing::info!("SessionStart(\"startup\") hook fired");
}

/// 打印诊断结果
pub fn print_doctor_result(result: &DoctorResult) {
    println!();
    println!("╭─────────────────────────────────────────────────────────────╮");
    println!("│                    🏥 EKO 诊断                        │");
    println!("╰─────────────────────────────────────────────────────────────╯");

    if !result.issues.is_empty() {
        println!("\n  ⚠️  问题:");
        for issue in &result.issues {
            println!("    {}", issue);
        }
    }

    println!("\n  检查项:");
    for check in &result.checks {
        println!("    {}", check);
    }

    if result.issues.is_empty() {
        println!("\n  ✅ 所有检查通过, Agent 运行正常");
    } else {
        println!("\n  发现 {} 个问题需要关注", result.issues.len());
    }
    println!();
}
