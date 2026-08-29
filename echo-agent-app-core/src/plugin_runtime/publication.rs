fn load_preferences(path: &Path) -> PluginPreferences {
    match std::fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_else(|error| {
            tracing::warn!(%error, "Ignoring invalid plugin preferences");
            PluginPreferences::default()
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => PluginPreferences::default(),
        Err(error) => {
            tracing::warn!(%error, "Failed to read plugin preferences");
            PluginPreferences::default()
        }
    }
}

fn append_errors(mut primary: String, errors: Vec<String>) -> String {
    if !errors.is_empty() {
        primary.push_str("; ");
        primary.push_str(&errors.join("; "));
    }
    primary
}

fn persist_preferences(path: &Path, preferences: &PluginPreferences) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(preferences)?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, content)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(temporary, path)?;
    Ok(())
}

struct AppliedAgentComponents {
    registry: PluginRegistry,
    wiring: Option<PluginWiringResult>,
    framework_generation: Option<Arc<PreparedPluginSet>>,
    mcp_ownership: PluginMcpOwnership,
    prepared: PreparedApplicationComponents,
    previous_registry: PluginRegistry,
    previous_framework_generation: Option<Arc<PreparedPluginSet>>,
    previous_mcp_declarations: PluginMcpDeclarations,
    previous_prepared: PreparedApplicationComponents,
}

struct FailedAgentComponents {
    error: String,
    registry: PluginRegistry,
    framework_components: HashMap<String, WiredPluginComponents>,
    framework_generation: Option<Arc<PreparedPluginSet>>,
    framework_receipt: Option<PluginWiringResult>,
    mcp_ownership: PluginMcpOwnership,
    prepared: PreparedApplicationComponents,
    candidate_monitors: Vec<CronTask>,
}

fn active_output_style_instructions(state: &PluginRuntimeState) -> Option<String> {
    active_output_style_instructions_for(state.active_output_style.as_deref(), &state.prepared)
}

fn active_output_style_instructions_for(
    selected: Option<&str>,
    prepared: &PreparedApplicationComponents,
) -> Option<String> {
    selected.and_then(|name| {
        prepared
            .output_styles
            .iter()
            .find(|style| style.name == name)
            .map(|style| style.instructions.clone())
    })
}

async fn load_exact_application_skill(
    agent: &mut echo_agent::agent::react::ReactAgent,
    requested: &str,
    load_root: PathBuf,
    requested_source: &str,
) -> anyhow::Result<Vec<String>> {
    let loaded = agent.load_skills_from_dir(load_root).await?;
    for name in &loaded {
        let source = if name == requested {
            requested_source.to_string()
        } else {
            format!("eko:discarded-sibling-skill:{name}")
        };
        agent
            .tag_skills_source(std::slice::from_ref(name), &source)
            .await;
        if name != requested {
            agent.unregister_skills_by_source(&source).await;
        }
    }
    Ok(loaded
        .into_iter()
        .filter(|name| name == requested)
        .collect())
}

fn agent_name(agent: &crate::plugin_components::PreparedPluginAgent) -> String {
    agent.name().to_string()
}

fn plugin_mcp_declarations(
    generation: &PreparedPluginSet,
) -> anyhow::Result<PluginMcpDeclarations> {
    let mut declarations = HashMap::new();
    let mut declared_by = HashMap::<String, String>::new();

    for plugin in generation.plugins() {
        let plugin_id = plugin.id();
        let Some(config) = plugin.mcp() else {
            continue;
        };
        let mut names = config.mcp_servers.keys().cloned().collect::<Vec<_>>();
        names.sort();
        for name in &names {
            if let Some(previous) = declared_by.insert(name.clone(), plugin_id.to_string()) {
                return Err(anyhow::anyhow!(
                    "Plugin MCP server name '{name}' is declared by both '{previous}' and '{plugin_id}'"
                ));
            }
        }
        declarations.insert(plugin_id.to_string(), names);
    }
    Ok(declarations)
}

fn require_applicable_generation(
    generation: Arc<PreparedPluginSet>,
) -> anyhow::Result<Arc<PreparedPluginSet>> {
    if generation.is_applicable() {
        return Ok(generation);
    }
    Err(anyhow::Error::new(PluginPreparationRejected {
        generation: generation.generation(),
        diagnostics: generation.diagnostics().to_vec(),
    }))
}

#[derive(Debug)]
struct PluginPreparationRejected {
    generation: u64,
    diagnostics: Vec<PluginPreparationDiagnostic>,
}

impl std::fmt::Display for PluginPreparationRejected {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let diagnostics = self
            .diagnostics
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ");
        write!(
            formatter,
            "prepared plugin generation {} is not applicable: {}",
            self.generation, diagnostics
        )
    }
}

impl std::error::Error for PluginPreparationRejected {}

fn validate_plugin_mcp_claims(
    guard: &McpNameOwnershipGuard,
    declarations: &PluginMcpDeclarations,
    previous: &PluginMcpOwnership,
) -> Result<(), String> {
    for (plugin_id, names) in declarations {
        for name in names {
            let previous_token = previous
                .get(plugin_id)
                .and_then(|tokens| tokens.get(name))
                .copied();
            guard.validate_plugin_claim(plugin_id, name, previous_token)?;
        }
    }
    Ok(())
}

fn release_plugin_mcp_claims(guard: &mut McpNameOwnershipGuard, ownership: &PluginMcpOwnership) {
    for (plugin_id, tokens) in ownership {
        for (name, token) in tokens {
            guard.release_plugin(plugin_id, name, *token);
        }
    }
}

fn claim_plugin_mcp_names(
    guard: &mut McpNameOwnershipGuard,
    declarations: &PluginMcpDeclarations,
) -> Result<PluginMcpOwnership, String> {
    let mut claimed: PluginMcpOwnership = HashMap::new();
    for (plugin_id, names) in declarations {
        for name in names {
            match guard.claim_plugin(plugin_id, name) {
                Ok(token) => {
                    claimed
                        .entry(plugin_id.clone())
                        .or_default()
                        .insert(name.clone(), token);
                }
                Err(error) => {
                    release_plugin_mcp_claims(guard, &claimed);
                    return Err(error);
                }
            }
        }
    }
    Ok(claimed)
}

#[cfg(test)]
fn exact_plugin_framework_receipts(
    framework: &HashMap<String, WiredPluginComponents>,
    ownership: &PluginMcpOwnership,
    guard: &McpNameOwnershipGuard,
) -> HashMap<String, WiredPluginComponents> {
    framework
        .iter()
        .map(|(plugin_id, components)| {
            let mut exact = components.clone();
            exact.mcp_servers.retain(|name| {
                ownership
                    .get(plugin_id)
                    .and_then(|tokens| tokens.get(name))
                    .is_some_and(|token| guard.owns_plugin(plugin_id, name, *token))
            });
            (plugin_id.clone(), exact)
        })
        .collect()
}

fn workspace_scope_plugin_ids(registry: &PluginRegistry) -> Vec<String> {
    let mut plugin_ids = registry
        .list()
        .into_iter()
        .filter(|entry| matches!(entry.scope, PluginScope::Project | PluginScope::Local))
        .map(|entry| entry.manifest.name.clone())
        .collect::<Vec<_>>();
    plugin_ids.sort();
    plugin_ids.dedup();

    plugin_ids
}

fn retire_plugin_lifecycles(
    lifecycle: &mut PluginLifecycleManager,
    plugin_ids: &[String],
) -> (Vec<String>, Vec<String>) {
    let mut errors = Vec::new();
    let mut failed_plugin_ids = Vec::new();
    for plugin_id in plugin_ids {
        if let Err(error) = lifecycle.unregister(plugin_id) {
            errors.push(error);
            failed_plugin_ids.push(plugin_id.clone());
        }
    }
    (errors, failed_plugin_ids)
}

async fn unload_application_components(
    agent: &mut echo_agent::agent::react::ReactAgent,
    application: &PreparedApplicationComponents,
) {
    for plugin_agent in &application.agents {
        let _ = agent.unregister_subagent(plugin_agent.name()).await;
    }
}

async fn replace_plugin_monitors(
    scheduler: &Arc<SchedulerRunner>,
    previous: &[CronTask],
    candidate: &[CronTask],
) -> anyhow::Result<()> {
    let mut removed = Vec::new();
    for task in previous {
        match scheduler.remove_task_exact(&task.id).await {
            Ok(true) => removed.push(task.clone()),
            Ok(false) => {}
            Err(error) => {
                let rollback_errors = rollback_plugin_monitors(scheduler, &[], &removed).await;
                return Err(monitor_replacement_error(
                    format!("Failed to remove plugin monitor '{}': {error}", task.name),
                    rollback_errors,
                ));
            }
        }
    }
    let mut added: Vec<CronTask> = Vec::new();
    for task in candidate {
        if let Err(error) = scheduler.add_task(task.clone()).await {
            let rollback_errors = rollback_plugin_monitors(scheduler, &added, &removed).await;
            return Err(monitor_replacement_error(
                format!("Failed to register plugin monitor '{}': {error}", task.name),
                rollback_errors,
            ));
        }
        added.push(task.clone());
    }
    Ok(())
}

async fn remove_plugin_monitors_best_effort(
    scheduler: &Arc<SchedulerRunner>,
    monitors: &[CronTask],
) -> Vec<String> {
    let mut errors = Vec::new();
    for monitor in monitors {
        if let Err(error) = scheduler.remove_task_exact(&monitor.id).await {
            errors.push(format!(
                "Failed to remove plugin monitor '{}': {error}",
                monitor.name
            ));
        }
    }
    errors
}

async fn rollback_plugin_monitors(
    scheduler: &Arc<SchedulerRunner>,
    added: &[CronTask],
    removed: &[CronTask],
) -> Vec<String> {
    let mut errors = Vec::new();
    for task in added.iter().rev() {
        if let Err(error) = scheduler.remove_task_exact(&task.id).await {
            errors.push(format!(
                "failed to remove candidate monitor '{}': {error}",
                task.name
            ));
        }
    }
    for task in removed {
        if let Err(error) = scheduler.add_task(task.clone()).await {
            errors.push(format!(
                "failed to restore previous monitor '{}': {error}",
                task.name
            ));
        }
    }
    errors
}

fn monitor_replacement_error(message: String, rollback_errors: Vec<String>) -> anyhow::Error {
    if rollback_errors.is_empty() {
        anyhow::anyhow!(message)
    } else {
        anyhow::anyhow!(
            "{message}; monitor rollback failed: {}",
            rollback_errors.join("; ")
        )
    }
}

async fn fire_plugin_events(
    hook_registry: &Arc<RwLock<echo_agent::skills::hooks::HookRegistry>>,
    event: echo_agent::skills::hooks::HookEvent,
    plugin_names: &[String],
    session_id: &str,
    agent_name: &str,
) {
    for plugin_name in plugin_names {
        let context = echo_agent::skills::hooks::HookContext::for_lifecycle(
            event,
            plugin_name,
            session_id,
            agent_name,
        );
        let _ = hook_registry
            .read()
            .await
            .run_lifecycle_hooks(&context)
            .await;
    }
}

fn validate_plugin_name(name: &str) -> anyhow::Result<()> {
    let length = name.chars().count();
    if !(1..=64).contains(&length)
        || !name
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_alphanumeric())
        || !name
            .chars()
            .last()
            .is_some_and(|character| character.is_ascii_alphanumeric())
        || !name.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || character == '-'
                || character == '.'
        })
        || name.contains("--")
        || name.contains("..")
    {
        return Err(anyhow::anyhow!(
            "Plugin name must follow the Agent Plugins lowercase name format"
        ));
    }
    Ok(())
}

fn write_scaffold(directory: &Path, name: &str) -> anyhow::Result<()> {
    let children = [
        "skills/example",
        "agents",
        "hooks",
        "themes",
        "output-styles",
        "scripts",
    ];
    for child in children {
        std::fs::create_dir_all(directory.join(child))?;
    }
    let manifest = serde_json::json!({
        "$schema": AGENT_PLUGIN_SCHEMA_V1,
        "name": name,
        "version": "0.1.0",
        "description": "EKO plugin",
        "license": "MIT",
        "displayName": name,
        "defaultEnabled": true
    });
    std::fs::write(
        directory.join("plugin.json"),
        serde_json::to_string_pretty(&manifest)?,
    )?;
    std::fs::write(
        directory.join("skills/example/SKILL.md"),
        format!(
            "---\nname: {name}-example\ndescription: Example skill\n---\nUse this skill for {name} tasks.\n"
        ),
    )?;
    std::fs::write(
        directory.join("agents/example.md"),
        format!(
            "---\nname: {name}-specialist\ndescription: Example plugin Subagent\nreadonly: true\n---\nHandle the assigned task carefully and return evidence.\n"
        ),
    )?;
    std::fs::write(directory.join("hooks/hooks.yaml"), "{}\n")?;
    std::fs::write(
        directory.join("mcp.json"),
        "{\n  \"$schema\": \"https://agent-plugins.org/schemas/1.0.0/mcp.schema.json\",\n  \"mcpServers\": {}\n}\n",
    )?;
    std::fs::write(directory.join("lsp.yaml"), "languages: {}\n")?;
    std::fs::write(directory.join("monitors.yaml"), "monitors: []\n")?;
    std::fs::write(
        directory.join("themes/example.json"),
        format!(
            "{{\n  \"name\": \"{name}-dark\",\n  \"display_name\": \"{name} Dark\",\n  \"dark\": true,\n  \"colors\": {{\"accent\": \"#5b8def\"}}\n}}\n"
        ),
    )?;
    std::fs::write(
        directory.join(format!("output-styles/{name}-concise.md")),
        format!(
            "---\nname: {name}-concise\ndescription: Concise answers\n---\nAnswer directly, preserve important evidence, and avoid repetition.\n"
        ),
    )?;
    std::fs::write(
        directory.join("README.md"),
        format!("# {name}\n\nEKO plugin package.\n"),
    )?;
    Ok(())
}

fn component_names(root: &Path, resolved: &echo_agent::plugin::ResolvedComponents) -> Vec<String> {
    let mut names = Vec::new();
    if !resolved.skill_dirs.is_empty() {
        names.push("skills".to_string());
    }
    if !resolved.agent_files.is_empty() {
        names.push("agents".to_string());
    }
    if resolved.hooks_file.is_some() {
        names.push("hooks".to_string());
    }
    if resolved.mcp_config_file.is_some() {
        names.push("mcp_servers".to_string());
    }
    if resolved.lsp_config_file.is_some() {
        names.push("lsp_servers".to_string());
    }
    if let Ok(eko) = crate::plugin_components::resolve_eko_components(root) {
        if eko.monitors_file.is_some() {
            names.push("monitors".to_string());
        }
        if !eko.theme_files.is_empty() {
            names.push("themes".to_string());
        }
        if !eko.output_style_files.is_empty() {
            names.push("output_styles".to_string());
        }
    }
    names
}
