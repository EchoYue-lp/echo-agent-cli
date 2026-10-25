//! EKO-owned runtime adapters for framework plugin component paths.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use echo_agent::agent::{Agent, ReactAgent, ReactAgentBuilder};
use echo_agent::plugin::{PluginRegistry, PluginVariables, PreparedPluginSet, ResolvedComponents};
use echo_agent::subagent::{
    AgentFactory, ExecutionMode, FnAgentFactory, SubagentAccessMode, SubagentExecutionBoundary,
    SubagentKind, SubagentPromptCompiler, SubagentSystemPromptInput, ToolCapabilitySnapshot,
};
use serde::{Deserialize, Serialize};

use crate::scheduler::{CronTask, CronTaskStatus};

#[derive(Debug, Clone, Default)]
pub(crate) struct ResolvedEkoComponents {
    pub(crate) monitors_file: Option<PathBuf>,
    pub(crate) theme_files: Vec<PathBuf>,
    pub(crate) output_style_files: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginThemeDefinition {
    pub name: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default = "default_true")]
    pub dark: bool,
    #[serde(default)]
    pub colors: HashMap<String, String>,
    #[serde(default, skip_deserializing)]
    pub plugin: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PluginOutputStyle {
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub plugin: String,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct PreparedApplicationComponents {
    pub agents: Vec<PreparedPluginAgent>,
    pub monitors: Vec<CronTask>,
    pub themes: Vec<PluginThemeDefinition>,
    pub output_styles: Vec<PluginOutputStyle>,
    pub domains: crate::domain_plugin::PreparedDomainComponents,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedPluginAgent {
    plugin: String,
    definition: crate::subagent_loader::SubagentDefinition,
}

impl PreparedPluginAgent {
    pub(crate) fn name(&self) -> &str {
        &self.definition.name
    }
}

#[derive(Clone)]
struct PluginAgentResources {
    llm_client: Option<Arc<dyn echo_agent::llm::LlmClient>>,
    llm_config: Option<echo_agent::llm::LlmConfig>,
    parent_model: String,
    registry: Arc<echo_agent::subagent::SubagentRegistry>,
    sandbox: Option<Arc<dyn echo_agent::sandbox::SandboxExecutor>>,
    working_dir: Option<PathBuf>,
    tool_visibility: echo_agent::agent::ToolVisibilityPolicy,
    files: Arc<dyn echo_agent::tools::filesystem::FileSystem>,
    local_state_dir: Option<PathBuf>,
    local_skill_source: echo_agent::skills::external::loader::SkillFileSource,
}

#[derive(Debug, Serialize, Deserialize)]
struct MonitorDefinition {
    name: String,
    cron: String,
    prompt: String,
    #[serde(default = "default_true")]
    enabled: bool,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum MonitorDocument {
    List(Vec<MonitorDefinition>),
    Wrapped { monitors: Vec<MonitorDefinition> },
}

#[derive(Debug, Deserialize)]
struct OutputStyleFrontmatter {
    name: String,
    #[serde(default)]
    description: String,
}

fn default_true() -> bool {
    true
}

pub(crate) fn prepare_application_components(
    generation: &PreparedPluginSet,
    target_scope: &str,
    registry: &PluginRegistry,
) -> Result<PreparedApplicationComponents, Vec<String>> {
    let mut prepared = PreparedApplicationComponents::default();
    let mut agent_names = HashSet::new();
    let mut monitor_ids = HashSet::new();
    let mut theme_names = HashSet::new();
    let mut output_style_names = HashSet::new();
    let mut errors = Vec::new();

    for plugin in generation.plugins() {
        let plugin_id = plugin.id();
        let root = plugin.root();
        let variables = plugin.variables();
        let Some(entry) = registry.get(plugin_id) else {
            errors.push(format!("Plugin '{plugin_id}' has no registry manifest"));
            continue;
        };
        let sensitive_values = entry
            .manifest
            .config
            .iter()
            .filter(|(_, config)| config.sensitive)
            .filter_map(|(key, _)| variables.user_config.get(key))
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>();
        let declaration = match crate::domain_plugin::read_declaration(root) {
            Ok(value) => value,
            Err(error) => {
                errors.push(format!("Plugin '{plugin_id}': {error}"));
                continue;
            }
        };
        let inputs = match crate::domain_plugin::read_inputs(
            plugin_id,
            root,
            declaration.as_ref().map(|(value, _)| value),
        ) {
            Ok(value) => value,
            Err(error) => {
                errors.push(format!("Plugin '{plugin_id}': {error}"));
                continue;
            }
        };
        let closure = match generation.dependency_closure(plugin_id) {
            Ok(value) => value.iter().map(|member| member.id().to_string()).collect(),
            Err(error) => {
                errors.push(error);
                continue;
            }
        };
        // Compare decoded semantic fields. JSON escaping must not hide a
        // configured credential containing quotes, backslashes or newlines.
        let declaration_sensitive = declaration
            .as_ref()
            .is_some_and(|(value, _)| value.contains_sensitive_value(&sensitive_values));
        let mut monitor_identity: Option<String> = None;
        let mut style_identity = Vec::new();
        let eko_components = match resolve_eko_components(root) {
            Ok(components) => components,
            Err(error) => {
                errors.push(format!("Plugin '{plugin_id}' EKO components: {error}"));
                continue;
            }
        };
        for document in plugin.subagent_documents() {
            match read_plugin_agent_document(plugin_id, document.source_path(), document.contents())
            {
                Ok(agent) => {
                    let name = agent.definition.name.clone();
                    if !agent_names.insert(name.clone()) {
                        errors.push(format!(
                            "Plugin '{plugin_id}' declares duplicate Subagent '{name}'"
                        ));
                    } else {
                        prepared.agents.push(agent);
                    }
                }
                Err(error) => errors.push(error),
            }
        }

        if let Some(path) = eko_components.monitors_file {
            match prepare_monitors_with_variables(plugin_id, &path, Some(variables)) {
                Ok((monitors, identity)) => {
                    monitor_identity = Some(identity);
                    for monitor in monitors {
                        let monitor = scope_monitor(target_scope, monitor);
                        if !monitor_ids.insert(monitor.id.clone()) {
                            errors.push(format!(
                                "Plugin '{plugin_id}' declares duplicate monitor id '{}'",
                                monitor.id
                            ));
                        } else {
                            prepared.monitors.push(monitor);
                        }
                    }
                }
                Err(error) => errors.push(error),
            }
        }

        for path in eko_components.theme_files {
            match read_theme_with_variables(plugin_id, &path, Some(variables)) {
                Ok(theme) => {
                    if !theme_names.insert(theme.name.clone()) {
                        errors.push(format!(
                            "Plugin '{plugin_id}' declares duplicate theme '{}'",
                            theme.name
                        ));
                    } else {
                        prepared.themes.push(theme);
                    }
                }
                Err(error) => errors.push(error),
            }
        }

        for path in eko_components.output_style_files {
            match read_output_style_with_variables(plugin_id, &path, Some(variables)) {
                Ok(style) => {
                    if !output_style_names.insert(style.name.clone()) {
                        errors.push(format!(
                            "Plugin '{plugin_id}' declares duplicate output style '{}'",
                            style.name
                        ));
                    } else {
                        let sensitive = sensitive_values
                            .iter()
                            .any(|secret| style.instructions.contains(secret.as_str()));
                        prepared.domains.output_styles.insert(
                            style.name.clone(),
                            (plugin_id.to_string(), style.instructions.clone(), sensitive),
                        );
                        style_identity.push(style.clone());
                        prepared.output_styles.push(style);
                    }
                }
                Err(error) => errors.push(error),
            }
        }
        // Hash exactly the immutable values published above. GUI theme colors
        // are presentation-only and never contribute to automated task rules.
        let identity = crate::domain_plugin::content_digest(&serde_json::json!({
            "framework": plugin.content_identity(),
            "declaration": declaration.as_ref().map(|(_, digest)| digest),
            "inputs": inputs,
            "monitors": monitor_identity,
            "output_styles": style_identity,
        }));
        match identity {
            Ok(content_sha256) => {
                prepared.domains.plugins.insert(plugin_id.to_string(), crate::domain_plugin::PreparedDomainPlugin {
                    identity: crate::domain_plugin::DomainPluginIdentity {
                        plugin_id: plugin_id.to_string(), version: entry.manifest.version.clone(), content_sha256,
                    },
                    root: root.to_path_buf(), closure,
                    declaration: declaration.map(|(value, _)| value), inputs,
                    binding_error: declaration_sensitive.then(|| format!("domain '{plugin_id}' contains sensitive configuration content and cannot be persisted")),
                });
            }
            Err(error) => errors.push(error),
        }
    }

    if errors.is_empty() {
        for plugin_agent in &prepared.agents {
            if let Err(error) = prepared
                .domains
                .record_prepared_role(&framework_definition(plugin_agent))
            {
                errors.push(error);
            }
        }
    }
    if errors.is_empty() {
        Ok(prepared)
    } else {
        Err(errors)
    }
}

fn scope_monitor(target_scope: &str, mut monitor: CronTask) -> CronTask {
    monitor.id = format!("{target_scope}:{}", monitor.id);
    monitor
}

pub(crate) fn validate_application_component_files(
    plugin: &str,
    root: &Path,
    resolved: &ResolvedComponents,
    variables: &PluginVariables,
) -> Vec<String> {
    let mut errors = resolved.diagnostics.clone();
    for directory in &resolved.skill_dirs {
        validate_skill_directory(plugin, directory, variables, &mut errors);
    }
    if let Some(path) = &resolved.hooks_file {
        validate_hooks_file(plugin, path, variables, &mut errors);
    }
    if let Some(path) = &resolved.mcp_config_file {
        validate_mcp_file(plugin, path, variables, &mut errors);
    }
    for path in &resolved.agent_files {
        if let Err(error) = read_plugin_agent_with_variables(plugin, path, Some(variables)) {
            errors.push(error);
        }
    }

    let eko_components = match resolve_eko_components(root) {
        Ok(components) => components,
        Err(error) => {
            errors.push(format!("Plugin '{plugin}' EKO components: {error}"));
            return errors;
        }
    };
    if let Some(path) = &eko_components.monitors_file
        && let Err(error) = read_monitors_with_variables(plugin, path, Some(variables))
    {
        errors.push(error);
    }
    for path in &eko_components.theme_files {
        if let Err(error) = read_theme_with_variables(plugin, path, Some(variables)) {
            errors.push(error);
        }
    }
    for path in &eko_components.output_style_files {
        if let Err(error) = read_output_style_with_variables(plugin, path, Some(variables)) {
            errors.push(error);
        }
    }
    errors
}

fn validate_skill_directory(
    plugin: &str,
    directory: &Path,
    variables: &PluginVariables,
    errors: &mut Vec<String>,
) {
    const MAX_DEPTH: usize = 4;
    validate_skill_directory_at_depth(plugin, directory, variables, errors, 0, MAX_DEPTH);
}

fn validate_skill_directory_at_depth(
    plugin: &str,
    directory: &Path,
    variables: &PluginVariables,
    errors: &mut Vec<String>,
    depth: usize,
    max_depth: usize,
) {
    if depth > max_depth {
        return;
    }
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) => {
            errors.push(format!(
                "Plugin '{plugin}' skills directory '{}': failed to scan: {error}",
                directory.display()
            ));
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                errors.push(format!(
                    "Plugin '{plugin}' skills directory '{}': failed to read entry: {error}",
                    directory.display()
                ));
                continue;
            }
        };
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                errors.push(format!(
                    "Plugin '{plugin}' skill path '{}': failed to inspect: {error}",
                    path.display()
                ));
                continue;
            }
        };
        if file_type.is_dir() {
            validate_skill_directory_at_depth(
                plugin,
                &path,
                variables,
                errors,
                depth.saturating_add(1),
                max_depth,
            );
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        if path.file_name().and_then(|name| name.to_str()) == Some("SKILL.md") {
            match read_component_text(&path, Some(variables)) {
                Ok(content) => {
                    let dir_name = path
                        .parent()
                        .and_then(|dir| dir.file_name())
                        .and_then(|name| name.to_str())
                        .unwrap_or_default();
                    let report =
                        echo_agent::skills::external::validate_skill_markdown(&content, dir_name);
                    if !report.is_valid() {
                        errors.push(format!(
                            "Plugin '{plugin}' skill '{}': {}",
                            path.display(),
                            report.violations.join("; ")
                        ));
                    }
                    if let Some(skill_dir) = path.parent()
                        && skill_dir.join("hooks.json").is_file()
                    {
                        errors.push(format!(
                            "Plugin '{plugin}' skill '{}': hooks.json is not an official Skill file",
                            skill_dir.display()
                        ));
                    }
                }
                Err(error) => errors.push(format!(
                    "Plugin '{plugin}' skill '{}': {error}",
                    path.display()
                )),
            }
        }
    }
}

fn validate_hooks_file(
    plugin: &str,
    path: &Path,
    variables: &PluginVariables,
    errors: &mut Vec<String>,
) {
    validate_hooks_document(plugin, path, variables, false, errors);
}

fn validate_hooks_document(
    plugin: &str,
    path: &Path,
    variables: &PluginVariables,
    json: bool,
    errors: &mut Vec<String>,
) {
    let content = match read_component_text(path, Some(variables)) {
        Ok(content) => content,
        Err(error) => {
            errors.push(format!(
                "Plugin '{plugin}' hooks '{}': failed to read: {error}",
                path.display()
            ));
            return;
        }
    };
    let definition = if json {
        serde_json::from_str::<echo_agent::skills::hooks::HooksDefinition>(&content)
            .map_err(|error| error.to_string())
    } else {
        serde_yaml::from_str::<echo_agent::skills::hooks::HooksDefinition>(&content)
            .map_err(|error| error.to_string())
    };
    let definition = match definition {
        Ok(definition) => definition,
        Err(error) => {
            let format = if json { "JSON" } else { "YAML" };
            errors.push(format!(
                "Plugin '{plugin}' hooks {format} parse '{}': {error}",
                path.display()
            ));
            return;
        }
    };
    validate_hooks_definition(plugin, path, definition, errors);
}

fn validate_hooks_definition(
    plugin: &str,
    path: &Path,
    definition: echo_agent::skills::hooks::HooksDefinition,
    errors: &mut Vec<String>,
) {
    for (event, rules) in definition.rules {
        for rule in rules {
            for action in rule.hooks {
                if let Err(error) = action.validate() {
                    errors.push(format!(
                        "Plugin '{plugin}' hooks '{}' event {} action {}: {error}",
                        path.display(),
                        event.as_str(),
                        action.kind()
                    ));
                }
            }
        }
    }
}

fn validate_mcp_file(
    plugin: &str,
    path: &Path,
    variables: &PluginVariables,
    errors: &mut Vec<String>,
) {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) => {
            errors.push(format!(
                "Plugin '{plugin}' MCP config '{}': failed to read: {error}",
                path.display()
            ));
            return;
        }
    };
    if let Err(error) = echo_agent::mcp::McpConfigFile::parse_agent_plugin(
        &content,
        &variables.plugin_root,
        &variables.plugin_data,
    ) {
        errors.push(format!(
            "Plugin '{plugin}' MCP config '{}': {error}",
            path.display()
        ));
    }
}

pub(crate) fn resolve_eko_components(root: &Path) -> Result<ResolvedEkoComponents, String> {
    let mut resolved = ResolvedEkoComponents::default();
    let monitors = root.join("monitors.yaml");
    if monitors.is_file() {
        resolved.monitors_file = Some(monitors);
    } else if monitors.exists() {
        return Err(format!(
            "monitors path '{}' is not a regular file",
            monitors.display()
        ));
    }
    let themes = root.join("themes");
    if themes.is_dir() {
        resolved.theme_files = resolve_eko_files(&themes, "json", "themes")?;
    } else if themes.exists() {
        return Err(format!(
            "themes path '{}' is not a directory",
            themes.display()
        ));
    }
    let output_styles = root.join("output-styles");
    if output_styles.is_dir() {
        resolved.output_style_files = resolve_eko_files(&output_styles, "md", "output styles")?;
    } else if output_styles.exists() {
        return Err(format!(
            "output styles path '{}' is not a directory",
            output_styles.display()
        ));
    }
    Ok(resolved)
}

fn resolve_eko_files(
    directory: &Path,
    suffix: &str,
    component: &str,
) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    let entries = std::fs::read_dir(directory).map_err(|error| {
        format!(
            "failed to scan {component} directory '{}': {error}",
            directory.display()
        )
    })?;
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("failed to scan '{}': {error}", directory.display()))?;
        let candidate = entry.path();
        if candidate.is_file()
            && candidate.extension().and_then(|value| value.to_str()) == Some(suffix)
        {
            files.push(candidate);
        }
    }
    files.sort();
    files.dedup();
    Ok(files)
}

fn read_plugin_agent_with_variables(
    plugin: &str,
    path: &Path,
    variables: Option<&PluginVariables>,
) -> Result<PreparedPluginAgent, String> {
    let content = read_component_text(path, variables).map_err(|error| {
        format!(
            "Plugin '{plugin}' Subagent '{}': failed to read: {error}",
            path.display()
        )
    })?;
    read_plugin_agent_document(plugin, path, &content)
}

fn read_plugin_agent_document(
    plugin: &str,
    path: &Path,
    content: &str,
) -> Result<PreparedPluginAgent, String> {
    let mut definition = crate::subagent_loader::parse_subagent_md(content, None)
        .map_err(|error| format!("Plugin '{plugin}' Subagent '{}': {error}", path.display()))?;
    definition.source = format!("plugin:{plugin}:{}", path.display());
    Ok(PreparedPluginAgent {
        plugin: plugin.to_string(),
        definition,
    })
}

fn read_monitors_with_variables(
    plugin: &str,
    path: &Path,
    variables: Option<&PluginVariables>,
) -> Result<Vec<CronTask>, String> {
    prepare_monitors_with_variables(plugin, path, variables).map(|(tasks, _)| tasks)
}

fn prepare_monitors_with_variables(
    plugin: &str,
    path: &Path,
    variables: Option<&PluginVariables>,
) -> Result<(Vec<CronTask>, String), String> {
    let content = read_component_text(path, variables).map_err(|error| {
        format!(
            "Plugin '{plugin}' monitors '{}': failed to read: {error}",
            path.display()
        )
    })?;
    let document: MonitorDocument = serde_yaml::from_str(&content).map_err(|error| {
        format!(
            "Plugin '{plugin}' monitors '{}': parse failed: {error}",
            path.display()
        )
    })?;
    let definitions = match document {
        MonitorDocument::List(definitions) => definitions,
        MonitorDocument::Wrapped { monitors } => monitors,
    };
    let identity = crate::domain_plugin::content_digest(&definitions)?;
    let mut tasks = Vec::with_capacity(definitions.len());
    for definition in definitions {
        let name = definition.name.trim();
        if name.is_empty() || definition.prompt.trim().is_empty() {
            return Err(format!(
                "Plugin '{plugin}' monitor names and prompts must not be empty"
            ));
        }
        let stable_name = name
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                    character
                } else {
                    '-'
                }
            })
            .collect::<String>();
        let mut task = CronTask::new(
            &format!("{plugin}: {name}"),
            definition.cron.trim(),
            definition.prompt.trim(),
        );
        task.id = format!("plugin-{plugin}-{stable_name}");
        task.status = if definition.enabled {
            CronTaskStatus::Enabled
        } else {
            CronTaskStatus::Disabled
        };
        if !task.validate_cron() {
            return Err(format!(
                "Plugin '{plugin}' monitor '{name}' has invalid cron expression '{}'",
                definition.cron
            ));
        }
        tasks.push(task);
    }
    Ok((tasks, identity))
}

fn read_theme_with_variables(
    plugin: &str,
    path: &Path,
    variables: Option<&PluginVariables>,
) -> Result<PluginThemeDefinition, String> {
    let content = read_component_text(path, variables).map_err(|error| {
        format!(
            "Plugin '{plugin}' theme '{}': failed to read: {error}",
            path.display()
        )
    })?;
    let mut theme: PluginThemeDefinition = serde_json::from_str(&content).map_err(|error| {
        format!(
            "Plugin '{plugin}' theme '{}': JSON parse failed: {error}",
            path.display()
        )
    })?;
    theme.name = theme.name.trim().to_string();
    if theme.name.is_empty() {
        return Err(format!(
            "Plugin '{plugin}' theme '{}' has an empty name",
            path.display()
        ));
    }
    for (key, value) in &theme.colors {
        if key.trim().is_empty() || value.trim().is_empty() {
            return Err(format!(
                "Plugin '{plugin}' theme '{}' contains an empty color key or value",
                theme.name
            ));
        }
    }
    theme.plugin = plugin.to_string();
    Ok(theme)
}

fn read_output_style_with_variables(
    plugin: &str,
    path: &Path,
    variables: Option<&PluginVariables>,
) -> Result<PluginOutputStyle, String> {
    let content = read_component_text(path, variables).map_err(|error| {
        format!(
            "Plugin '{plugin}' output style '{}': failed to read: {error}",
            path.display()
        )
    })?;
    let normalized = content.replace("\r\n", "\n");
    let after_open = normalized.strip_prefix("---\n").ok_or_else(|| {
        format!("Plugin '{plugin}' output style must start with YAML frontmatter")
    })?;
    let (frontmatter, instructions) = after_open.split_once("\n---\n").ok_or_else(|| {
        format!(
            "Plugin '{plugin}' output style '{}' has no closing frontmatter",
            path.display()
        )
    })?;
    let metadata: OutputStyleFrontmatter = serde_yaml::from_str(frontmatter).map_err(|error| {
        format!(
            "Plugin '{plugin}' output style '{}': frontmatter parse failed: {error}",
            path.display()
        )
    })?;
    let name = metadata.name.trim().to_string();
    let instructions = instructions.trim().to_string();
    if name.is_empty() || instructions.is_empty() {
        return Err(format!(
            "Plugin '{plugin}' output style '{}' requires a name and instructions",
            path.display()
        ));
    }
    Ok(PluginOutputStyle {
        name,
        description: metadata.description.trim().to_string(),
        instructions,
        plugin: plugin.to_string(),
    })
}

fn read_component_text(path: &Path, variables: Option<&PluginVariables>) -> Result<String, String> {
    let content = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    Ok(match variables {
        Some(variables) => variables.substitute(&content),
        None => content,
    })
}

pub(crate) async fn register_plugin_agents(
    agent: &mut ReactAgent,
    prepared: &[PreparedPluginAgent],
    local_state_dir: Option<PathBuf>,
) -> Result<Vec<String>, String> {
    let resources = PluginAgentResources {
        llm_client: agent.llm_client().cloned(),
        llm_config: agent.llm_config().cloned(),
        parent_model: agent.model_name().to_string(),
        registry: agent.subagent_registry().clone(),
        sandbox: agent.sandbox_manager().cloned(),
        working_dir: agent.working_dir(),
        tool_visibility: agent.tool_visibility_policy(),
        files: agent
            .file_system()
            .unwrap_or_else(|| Arc::new(echo_agent::tools::files::LocalFileSystem)),
        local_state_dir,
        local_skill_source: agent.default_skill_source(),
    };
    let mut registered = Vec::with_capacity(prepared.len());
    for plugin_agent in prepared {
        let framework_definition = framework_definition(plugin_agent);
        let instance = build_plugin_agent(&plugin_agent.definition, &resources)
            .map_err(|error| error.to_string())?;
        let factory_definition = plugin_agent.definition.clone();
        let factory_resources = resources.clone();
        let factory: Arc<dyn AgentFactory> = Arc::new(FnAgentFactory::new(move || {
            let definition = factory_definition.clone();
            let resources = factory_resources.clone();
            Box::pin(async move {
                let agent = build_plugin_agent(&definition, &resources)?;
                box_project_plugin_agent(agent, &resources).await
            })
        }));
        let instance = box_project_plugin_agent(instance, &resources)
            .await
            .map_err(|error| error.to_string())?;
        agent.register_subagent_with_definition(framework_definition.clone(), instance);
        agent.register_subagent_factory(framework_definition, factory);
        registered.push(plugin_agent.definition.name.clone());
    }
    let disabled_tools = agent.disabled_tool_names();
    crate::subagent_prompt::refresh_primary_system_prompt(agent, &disabled_tools);
    Ok(registered)
}

async fn box_project_plugin_agent(
    agent: ReactAgent,
    resources: &PluginAgentResources,
) -> echo_agent::error::Result<Box<dyn Agent>> {
    let executor = resources
        .sandbox
        .clone()
        .or_else(|| agent.sandbox_manager().cloned())
        .ok_or_else(|| {
            echo_agent::error::ReactError::Other(
                "Plugin Subagent project executor is unavailable".to_string(),
            )
        })?;
    Ok(crate::project::agent_adapter::boxed_project_agent(
        crate::agent_handle::AgentHandle::new(agent),
        resources.files.clone(),
        executor,
        resources.local_state_dir.clone(),
        resources.working_dir.clone(),
    )
    .await)
}

fn framework_definition(
    plugin_agent: &PreparedPluginAgent,
) -> echo_agent::subagent::SubagentDefinition {
    let definition = &plugin_agent.definition;
    echo_agent::subagent::SubagentDefinition {
        name: definition.name.clone(),
        description: definition.description.clone(),
        kind: SubagentKind::Plugin {
            source: plugin_agent.plugin.clone(),
        },
        access_mode: if definition.readonly {
            SubagentAccessMode::ReadOnly
        } else {
            SubagentAccessMode::Write
        },
        execution_mode: if definition.team.is_some() {
            ExecutionMode::Team
        } else {
            ExecutionMode::Fork
        },
        model: definition.model.clone(),
        thinking: None,
        system_prompt: Some(definition.system_prompt.clone()),
        tool_filter: None,
        max_iterations: definition.max_turns,
        token_limit: None,
        inherit_history: Some(2),
        inherit_memory: true,
        timeout_secs: 0,
        can_delegate: definition.can_delegate,
        tags: definition.tags.clone(),
        lightweight: false,
        isolation: if definition.isolate_worktree {
            Some("worktree".to_string())
        } else if definition.isolate_workspace {
            Some("workspace".to_string())
        } else {
            None
        },
        team: definition.team.clone(),
        is_background: definition.is_background,
    }
}

fn build_plugin_agent(
    definition: &crate::subagent_loader::SubagentDefinition,
    resources: &PluginAgentResources,
) -> echo_agent::error::Result<ReactAgent> {
    if definition
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty() && *model != "inherit")
        .is_some_and(|model| model != resources.parent_model)
    {
        tracing::warn!(
            subagent = definition.name,
            requested_model = ?definition.model,
            "Plugin Subagent model override has no configured profile resolver; using the complete parent generation"
        );
    }
    let model = resources.parent_model.clone();
    let mut builder = ReactAgentBuilder::new()
        .name(&definition.name)
        .model(&model)
        .system_prompt(&definition.system_prompt)
        .enable_tools()
        .file_system(resources.files.clone())
        .auto_project_rules(false)
        .enable_memory()
        .working_dir(
            resources
                .working_dir
                .clone()
                .unwrap_or_else(|| PathBuf::from(".")),
        );
    if definition.readonly {
        builder = builder.readonly_tools();
    }
    if let Some(max_iterations) = definition.max_turns {
        builder = builder.max_iterations(max_iterations);
    }
    if definition.can_delegate {
        builder = builder
            .enable_subagent()
            .subagent_registry(resources.registry.clone())
            .subagent_prompt_compiler(Arc::new(crate::subagent_prompt::EkoSubagentPromptCompiler))
            .register_agent_dispatch_tool();
    }
    if let Some(client) = resources.llm_client.clone() {
        builder = builder.llm_client(client);
    } else if let Some(config) = resources.llm_config.clone() {
        builder = builder.llm_config(config);
    }
    let mut agent = builder.build()?;
    crate::tasks::task_runtime::compact_context::install_domain_context_projection(&mut agent);
    if let Some(sandbox) = resources.sandbox.clone() {
        agent.set_sandbox_executor(sandbox);
    }
    agent.set_default_skill_source(resources.local_skill_source.clone());
    agent.use_tool_visibility_policy(resources.tool_visibility.clone());
    let disabled_tools = resources.tool_visibility.disabled_names();
    let capabilities =
        ToolCapabilitySnapshot::from_definitions(&agent.tool_definitions(), &disabled_tools);
    let compiled_system = crate::subagent_prompt::EkoSubagentPromptCompiler.compile_system(
        &SubagentSystemPromptInput {
            actor: echo_agent::subagent::PromptActor::Subagent,
            name: &definition.name,
            description: &definition.description,
            role_prompt: &definition.system_prompt,
            capabilities: &capabilities,
            boundary: SubagentExecutionBoundary {
                access: if definition.readonly {
                    SubagentAccessMode::ReadOnly
                } else {
                    SubagentAccessMode::Write
                },
                isolation: crate::subagent_loader::subagent_isolation(definition),
                can_delegate: definition.can_delegate,
            },
        },
    );
    agent.replace_system_prompt(compiled_system.system_prompt);
    Ok(agent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monitor_binding_keys_are_qualified_by_target_scope() {
        let monitor = CronTask::new("daily", "0 0 * * * *", "review");
        let global = scope_monitor("global", monitor.clone());
        let workspace = scope_monitor("workspace-a", monitor);

        assert_ne!(global.id, workspace.id);
        assert!(global.id.starts_with("global:"));
        assert!(workspace.id.starts_with("workspace-a:"));
    }

    #[test]
    fn output_style_frontmatter_is_utf8_safe() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let path = temp.path().join("concise.md");
        std::fs::write(
            &path,
            "---\nname: concise\ndescription: concise replies\n---\nUse clear Chinese: 你好。\n",
        )
        .map_err(|error| error.to_string())?;
        let style = read_output_style_with_variables("test", &path, None)?;
        assert_eq!(style.name, "concise");
        assert!(style.instructions.contains("你好"));
        Ok(())
    }

    #[test]
    fn application_components_expand_plugin_and_manifest_config_variables() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let theme_path = temp.path().join("theme.json");
        let style_path = temp.path().join("style.md");
        std::fs::write(
            &theme_path,
            r##"{"name":"configured","dark":true,"colors":{"accent":"${user_config.accent}"}}"##,
        )
        .map_err(|error| error.to_string())?;
        std::fs::write(
            &style_path,
            "---\nname: configured\n---\nWrite artifacts under ${ECHO_PLUGIN_DATA}.\n",
        )
        .map_err(|error| error.to_string())?;
        let data_dir = temp.path().join("data");
        let variables =
            PluginVariables::new(temp.path().into(), data_dir.clone(), temp.path().into())
                .with_user_config(HashMap::from([(
                    "accent".to_string(),
                    "#123456".to_string(),
                )]));

        let theme = read_theme_with_variables("configured", &theme_path, Some(&variables))?;
        let style = read_output_style_with_variables("configured", &style_path, Some(&variables))?;

        assert_eq!(
            theme.colors.get("accent").map(String::as_str),
            Some("#123456")
        );
        assert!(
            style
                .instructions
                .contains(&data_dir.to_string_lossy().to_string())
        );
        Ok(())
    }

    #[tokio::test]
    async fn actual_agent_tool_plugin_factory_projects_persisted_domain_without_prompt_payload()
    -> Result<(), String> {
        use crate::tasks::task_runtime::compact_context::{
            DOMAIN_METHOD_MARKER, task_runtime_projection_registry,
        };
        use crate::tasks::task_runtime::{
            AttendedMode, DomainProfile, RunPlanPolicy, TaskRunExecutionProfile, TaskRuntimeStore,
        };
        // agent_tool constructs prompt_payload=None. The actual registered
        // plugin factory must still read the original RunCreated projection.
        let client = Arc::new(
            echo_agent::testing::MockLlmClient::new()
                .with_response("Bound evidence inspected")
                .with_response("General follow-up complete"),
        );
        let project =
            crate::tasks::task_runtime::project_environment_tests::EnvironmentFixture::new()?;
        // Model a real target repository boundary inside this routing fixture;
        // VCS discovery must not need ancestors outside its bounded namespace.
        crate::tasks::task_runtime::worktree::run_git(
            &project.target_root,
            &["init", "-b", "main"],
        )
        .map_err(|error| error.to_string())?;
        for root in [&project.visible_root, &project.target_root] {
            std::fs::write(root.join("Cargo.toml"), "fixture marker")
                .map_err(|error| error.to_string())?;
        }
        std::fs::write(
            project.visible_root.join("AGENTS.md"),
            "WRONG_LOCAL_PLUGIN_RULE",
        )
        .map_err(|error| error.to_string())?;
        std::fs::write(
            project.target_root.join("AGENTS.md"),
            "ACTUAL_PLUGIN_PROJECT_RULE",
        )
        .map_err(|error| error.to_string())?;
        let mut primary = ReactAgentBuilder::new()
            .name("domain-parent")
            .llm_client(client.clone())
            .enable_tools()
            .enable_subagent()
            .file_system(project.files.clone())
            .working_dir(project.visible_root.clone())
            .register_agent_dispatch_tool()
            .build()
            .map_err(|error| error.to_string())?;
        primary.set_sandbox_executor(project.executor.clone());
        let prepared = PreparedPluginAgent {
            plugin: "evidence-domain".to_string(),
            definition: crate::subagent_loader::parse_subagent_md(
                "---\nname: projected-domain-role\ndescription: Inspect domain evidence\nreadonly: true\n---\nInspect the supplied evidence.\n",
                None,
            )?,
        };
        register_plugin_agents(&mut primary, &[prepared], None).await?;
        let tool = primary
            .tool_manager()
            .get_tool("agent_tool")
            .ok_or("agent_tool missing")?;
        let binding = crate::domain_plugin::test_binding(false)?;
        let run_id = uuid::Uuid::new_v4().to_string();
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        store
            .create_run_for_active_workspace_with_domain(
                &run_id,
                "domain-projection-conversation",
                "root",
                DomainProfile::General,
                "inspect",
                "task",
                AttendedMode::Attended,
                TaskRunExecutionProfile::orchestrated(RunPlanPolicy::AllowDirect),
                Some(&binding),
            )
            .map_err(|error| error.to_string())?;
        let projection =
            task_runtime_projection_registry().retain_for_invocation(&run_id, store)?;
        let parameters = std::collections::HashMap::from([
            (
                "agent_name".to_string(),
                serde_json::json!("projected-domain-role"),
            ),
            ("task".to_string(), serde_json::json!("inspect evidence")),
            ("mode".to_string(), serde_json::json!("sync")),
        ]);
        let context = echo_agent::tools::ToolContext {
            run_id: Some(run_id),
            working_dir: Some(project.visible_root.clone()),
            ..Default::default()
        };
        let result = tool
            .execute_with_context(parameters.clone(), &context)
            .await
            .map_err(|error| error.to_string())?;
        assert!(result.success);
        let bound_calls = client.all_calls();
        let bound_messages = bound_calls
            .first()
            .ok_or("bound plugin did not call the model")?;
        assert!(
            bound_messages
                .iter()
                .filter_map(|message| message.content.as_text())
                .any(|text| text.contains(DOMAIN_METHOD_MARKER)
                    && text.contains(&binding.snapshot.method))
        );
        let bound_text = bound_messages
            .iter()
            .filter_map(|message| message.content.as_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(bound_text.matches("ACTUAL_PLUGIN_PROJECT_RULE").count(), 1);
        assert!(!bound_text.contains("WRONG_LOCAL_PLUGIN_RULE"));
        drop(projection);
        let result = tool
            .execute_with_context(
                parameters,
                &echo_agent::tools::ToolContext {
                    working_dir: Some(project.visible_root.clone()),
                    ..Default::default()
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        assert!(result.success);
        let calls = client.all_calls();
        let general_messages = calls
            .last()
            .ok_or("General plugin did not call the model")?;
        assert!(
            general_messages
                .iter()
                .filter_map(|message| message.content.as_text())
                .all(|text| !text.contains(DOMAIN_METHOD_MARKER))
        );
        assert!(
            general_messages
                .iter()
                .filter_map(|message| message.content.as_text())
                .any(|text| text.contains("ACTUAL_PLUGIN_PROJECT_RULE"))
        );
        Ok(())
    }
}
