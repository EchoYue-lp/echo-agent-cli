// Skill 启停的运行时 reconcile 辅助。
//
// 2026-09 简化(取代 ADR 0032):durable 状态机(operation identity /
// content identity CAS / repair debt 重放)已整体移除,本文件只保留
// "解析期望集合 → 同步到各运行时目标"所需的辅助函数。

#[cfg(test)]
async fn skill_source_present(
    agent: &crate::agent_handle::AgentHandle,
    name: &str,
    source: &str,
) -> bool {
    agent
        .read(|agent| {
            agent.skill_descriptors().iter().any(|descriptor| {
                descriptor.name == name && descriptor.source.as_deref() == Some(source)
            })
        })
        .await
}

async fn skill_entry(state: &AppState, name: &str) -> anyhow::Result<(PathBuf, String)> {
    let mut hub = state.skills_hub.write().await;
    hub.refresh();
    if let Some(entry) = hub.get(name) {
        let load_root = entry
            .path
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| entry.path.clone());
        return Ok((load_root, entry.category.clone()));
    }
    drop(hub);

    let builtin_hub = crate::skills_hub::SkillsHub::with_root(
        crate::skills_hub::builtin_skills_root(),
    );
    let entry = builtin_hub
        .get(name)
        .ok_or_else(|| anyhow::anyhow!("Skill '{name}' not found"))?;
    let load_root = entry
        .path
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| entry.path.clone());
    Ok((load_root, entry.category.clone()))
}

async fn read_enabled_skills_config(
    flow: &crate::product_data_io::ProductDataIoFlow,
    path: PathBuf,
) -> Result<EnabledSkillsConfig, SkillMutationError> {
    flow.run("read enabled skills state", move || {
        EnabledSkillsConfig::load(&path)
    })
    .await
    .map_err(|error| SkillMutationError::BeforeCommit(error.to_string()))?
    .map_err(|error| SkillMutationError::BeforeCommit(error.to_string()))
}

async fn write_enabled_skills_config(
    flow: &crate::product_data_io::ProductDataIoFlow,
    path: PathBuf,
    config: EnabledSkillsConfig,
) -> Result<(), SkillMutationError> {
    flow.run("commit enabled skills state", move || {
        config.save(&path)
    })
    .await
    .map_err(|error| SkillMutationError::BeforeCommit(error.to_string()))?
    .map_err(|error| SkillMutationError::BeforeCommit(error.to_string()))
}

fn remove_skill_artifact(skill_root: PathBuf, name: &str) -> Result<bool, String> {
    let mut hub = SkillsHub::with_root(skill_root);
    crate::skills_hub::install::uninstall(name, &mut hub)
}

fn desired_skill_entries(
    config: &EnabledSkillsConfig,
    skill_root: PathBuf,
) -> Vec<(String, PathBuf)> {
    let hub = SkillsHub::with_root(skill_root);
    let mut selected = hub
        .list()
        .into_iter()
        .filter(|entry| {
            config
                .skills
                .get(&entry.name)
                .is_some_and(|state| state.enabled)
        })
        .map(|entry| {
            let load_root = entry
                .path
                .parent()
                .map(PathBuf::from)
                .unwrap_or_else(|| entry.path.clone());
            (entry.name.clone(), load_root)
        })
        .collect::<Vec<_>>();
    selected.sort_by(|left, right| left.0.cmp(&right.0));
    selected
}

async fn reconcile_target_skills(
    target: &crate::state::ExtensionRuntimeTarget,
    desired: &[(String, PathBuf)],
    skill_root: &std::path::Path,
) -> anyhow::Result<Vec<String>> {
    let mut current = target
        .primary_agent()
        .read(|agent| {
            agent
                .skill_descriptors()
                .iter()
                .filter_map(|descriptor| {
                    let source = descriptor.source.as_deref()?;
                    source
                        .starts_with(USER_SKILL_SOURCE_PREFIX)
                        .then(|| (descriptor.name.clone(), source.to_string()))
                })
                .collect::<Vec<_>>()
        })
        .await;
    current.sort();
    current.dedup();
    for (name, source) in current {
        let load_root = desired
            .iter()
            .find(|(candidate, _)| candidate == &name)
            .map(|(_, root)| root.clone())
            .unwrap_or_else(|| skill_root.to_path_buf());
        target
            .plugin_runtime()
            .disable_application_skill(name, load_root, source)
            .await?;
    }
    let mut loaded = Vec::new();
    for (name, load_root) in desired {
        loaded.extend(
            target
                .plugin_runtime()
                .enable_application_skill(name.clone(), load_root.clone(), user_skill_source(name))
                .await?,
        );
    }
    Ok(loaded)
}

#[cfg(test)]
async fn load_exact_user_skill(
    agent: &crate::agent_handle::AgentHandle,
    requested: &str,
    load_root: PathBuf,
    requested_source: String,
) -> anyhow::Result<Vec<String>> {
    let requested = requested.to_string();
    agent
        .write_async(|agent| {
            Box::pin(async move {
                let loaded = agent.load_skills_from_dir(load_root).await?;
                for name in &loaded {
                    let source = if name == &requested {
                        requested_source.clone()
                    } else {
                        format!("eko:discarded-sibling-skill:{name}")
                    };
                    agent
                        .tag_skills_source(std::slice::from_ref(name), &source)
                        .await;
                    if name != &requested {
                        agent.unregister_skills_by_source(&source).await;
                    }
                }
                Ok::<_, echo_agent::error::ReactError>(
                    loaded
                        .into_iter()
                        .filter(|name| name == &requested)
                        .collect(),
                )
            })
        })
        .await
        .map_err(anyhow::Error::new)
}

fn ensure_hook_load_succeeded(loaded: &HooksLoadResult) -> anyhow::Result<()> {
    if loaded.errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(loaded.errors.join("; ")))
    }
}

fn mcp_transport(entry: &echo_agent::mcp::McpServerEntry) -> &'static str {
    if entry.url.is_some() {
        if entry.transport.as_deref() == Some("sse") {
            "sse"
        } else {
            "http"
        }
    } else if entry.command.is_some() {
        "stdio"
    } else {
        "unknown"
    }
}

fn mcp_health_scope_key(runtime: &ScopedChatRuntime) -> anyhow::Result<String> {
    serde_json::to_string(&(
        runtime.execution_scope().workspace_id(),
        runtime.workspace_host_generation(),
    ))
    .map_err(|error| anyhow::anyhow!("failed to encode MCP health scope: {error}"))
}
