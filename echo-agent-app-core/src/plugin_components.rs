//! EKO-owned runtime adapters for framework plugin component paths.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use echo_agent::agent::subagent::{
    AgentFactory, DefaultSubagentPromptCompiler, ExecutionMode, FnAgentFactory, SubagentKind,
};
use echo_agent::agent::{Agent, ReactAgent, ReactAgentBuilder};
use echo_agent::lsp::LspConfig;
use echo_agent::plugin::{PluginRegistry, PluginVariables, ResolvedComponents};
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
    pub lsp_configs: Vec<(String, LspConfig)>,
    pub monitors: Vec<CronTask>,
    pub themes: Vec<PluginThemeDefinition>,
    pub output_styles: Vec<PluginOutputStyle>,
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
    registry: Arc<echo_agent::agent::subagent::SubagentRegistry>,
    sandbox: Option<Arc<echo_agent::sandbox::SandboxManager>>,
    working_dir: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
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
    registry: &mut PluginRegistry,
) -> Result<PreparedApplicationComponents, Vec<String>> {
    let ordered = registry
        .resolve_enabled_dependencies()
        .map_err(|error| vec![error])?;
    let mut prepared = PreparedApplicationComponents::default();
    let mut agent_names = HashSet::new();
    let mut lsp_languages = HashSet::new();
    let mut monitor_ids = HashSet::new();
    let mut theme_names = HashSet::new();
    let mut output_style_names = HashSet::new();
    let mut errors = Vec::new();

    for plugin in ordered {
        let root = match registry.get(&plugin) {
            Some(entry) => entry.root.clone(),
            None => {
                errors.push(format!("Plugin '{plugin}' disappeared during resolution"));
                continue;
            }
        };
        let variables = match registry.variables_for(&plugin) {
            Ok(variables) => variables,
            Err(error) => {
                errors.push(error);
                continue;
            }
        };
        let resolved = match registry.resolve_components(&plugin) {
            Ok(resolved) => resolved,
            Err(error) => {
                errors.push(error);
                continue;
            }
        };
        let eko_components = match resolve_eko_components(&root) {
            Ok(components) => components,
            Err(error) => {
                errors.push(format!("Plugin '{plugin}' EKO components: {error}"));
                continue;
            }
        };
        if let Some(path) = resolved.hooks_file.as_ref() {
            validate_hooks_file(&plugin, path, &variables, &mut errors);
        }
        for path in resolved.agent_files {
            match read_plugin_agent_with_variables(&plugin, &path, Some(&variables)) {
                Ok(agent) => {
                    let name = agent.definition.name.clone();
                    if !agent_names.insert(name.clone()) {
                        errors.push(format!(
                            "Plugin '{plugin}' declares duplicate Subagent '{name}'"
                        ));
                    } else {
                        prepared.agents.push(agent);
                    }
                }
                Err(error) => errors.push(error),
            }
        }

        if let Some(path) = resolved.lsp_config_file {
            match read_component_text(&path, Some(&variables))
                .and_then(|content| LspConfig::from_yaml(&content))
            {
                Ok(config) => {
                    let duplicate = config
                        .servers
                        .keys()
                        .find(|language| lsp_languages.contains(*language));
                    if let Some(language) = duplicate {
                        errors.push(format!(
                            "Plugin '{plugin}' declares duplicate LSP language '{language}'"
                        ));
                    } else {
                        lsp_languages.extend(config.servers.keys().cloned());
                        prepared.lsp_configs.push((plugin.clone(), config));
                    }
                }
                Err(error) => errors.push(format!(
                    "Plugin '{plugin}' LSP config '{}': {error}",
                    path.display()
                )),
            }
        }

        if let Some(path) = eko_components.monitors_file {
            match read_monitors_with_variables(&plugin, &path, Some(&variables)) {
                Ok(monitors) => {
                    for monitor in monitors {
                        if !monitor_ids.insert(monitor.id.clone()) {
                            errors.push(format!(
                                "Plugin '{plugin}' declares duplicate monitor id '{}'",
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
            match read_theme_with_variables(&plugin, &path, Some(&variables)) {
                Ok(theme) => {
                    if !theme_names.insert(theme.name.clone()) {
                        errors.push(format!(
                            "Plugin '{plugin}' declares duplicate theme '{}'",
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
            match read_output_style_with_variables(&plugin, &path, Some(&variables)) {
                Ok(style) => {
                    if !output_style_names.insert(style.name.clone()) {
                        errors.push(format!(
                            "Plugin '{plugin}' declares duplicate output style '{}'",
                            style.name
                        ));
                    } else {
                        prepared.output_styles.push(style);
                    }
                }
                Err(error) => errors.push(error),
            }
        }
    }

    if errors.is_empty() {
        Ok(prepared)
    } else {
        Err(errors)
    }
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
    if let Some(path) = &resolved.lsp_config_file
        && let Err(error) = read_component_text(path, Some(variables))
            .and_then(|content| LspConfig::from_yaml(&content))
    {
        errors.push(format!(
            "Plugin '{plugin}' LSP config '{}': {error}",
            path.display()
        ));
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
            match read_component_text(&path, Some(variables)).and_then(|content| {
                echo_agent::skills::external::parse_skill_md(&content)
                    .map_err(|error| error.to_string())
            }) {
                Ok(descriptor) => {
                    if let Some(definition) = descriptor.hooks {
                        validate_hooks_definition(plugin, &path, definition, errors);
                    }
                }
                Err(error) => errors.push(format!(
                    "Plugin '{plugin}' skill '{}': {error}",
                    path.display()
                )),
            }
        } else if path.file_name().and_then(|name| name.to_str()) == Some("hooks.json") {
            validate_hooks_document(plugin, &path, variables, true, errors);
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
    let mut definition = crate::subagent_loader::parse_subagent_md(&content, None)
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
    Ok(tasks)
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
) -> Result<Vec<String>, String> {
    let resources = PluginAgentResources {
        llm_client: agent.llm_client().cloned(),
        llm_config: agent.llm_config().cloned(),
        parent_model: agent.model_name().to_string(),
        registry: agent.subagent_registry().clone(),
        sandbox: agent.sandbox_manager().cloned(),
        working_dir: agent.working_dir(),
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
                build_plugin_agent(&definition, &resources)
                    .map(|agent| Box::new(agent) as Box<dyn Agent>)
            })
        }));
        agent.register_subagent_with_definition(framework_definition.clone(), Box::new(instance));
        agent.register_subagent_factory(framework_definition, factory);
        registered.push(plugin_agent.definition.name.clone());
    }
    Ok(registered)
}

fn framework_definition(
    plugin_agent: &PreparedPluginAgent,
) -> echo_agent::agent::subagent::SubagentDefinition {
    let definition = &plugin_agent.definition;
    echo_agent::agent::subagent::SubagentDefinition {
        name: definition.name.clone(),
        description: definition.description.clone(),
        kind: SubagentKind::Plugin {
            source: plugin_agent.plugin.clone(),
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
        .enable_memory()
        .working_dir(
            resources
                .working_dir
                .clone()
                .unwrap_or_else(|| PathBuf::from(".")),
        );
    if definition.readonly {
        builder = builder.readonly_tools();
    } else if let Some(sandbox) = resources.sandbox.clone() {
        builder = builder.sandbox_manager(sandbox);
    }
    if let Some(max_iterations) = definition.max_turns {
        builder = builder.max_iterations(max_iterations);
    }
    if definition.can_delegate {
        builder = builder
            .enable_subagent()
            .subagent_registry(resources.registry.clone())
            .subagent_prompt_compiler(Arc::new(DefaultSubagentPromptCompiler))
            .register_agent_dispatch_tool();
    }
    if let Some(client) = resources.llm_client.clone() {
        builder = builder.llm_client(client);
    } else if let Some(config) = resources.llm_config.clone() {
        builder = builder.llm_config(config);
    }
    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
