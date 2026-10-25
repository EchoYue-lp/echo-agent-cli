//! EKO-owned immutable domain rules. Publication and task execution keep their
//! existing owners; these values are data carried by those owners, not a service.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use echo_agent::subagent::{SubagentDefinition, SubagentKind};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ts_rs::TS;

use crate::tasks::task_runtime::types::PlanTaskKind;

const SCHEMA_VERSION: u32 = 1;

/// Invocation-local proof identity. Pointer equality is process-local only and
/// never persisted as a plugin version or authorization source.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct DomainInvocationIdentity {
    run_id: String,
    binding: DomainBindingRef,
    pool_scope: usize,
}

impl DomainInvocationIdentity {
    pub(crate) fn new(run_id: &str, binding: DomainBindingRef, pool_scope: usize) -> Self {
        Self {
            run_id: run_id.to_string(),
            binding,
            pool_scope,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DomainPluginIdentity {
    pub plugin_id: String,
    pub version: Option<String>,
    pub content_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DomainBindingRef {
    pub plugin: DomainPluginIdentity,
    pub binding_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DomainRoleBinding {
    pub role_name: String,
    pub provider_kind: String,
    pub provider: Option<DomainPluginIdentity>,
    pub definition_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainInputDigest {
    pub plugin_id: String,
    pub path: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainOutputStyleBinding {
    pub name: String,
    pub provider: DomainPluginIdentity,
    pub instructions: String,
    pub instructions_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainRuleSnapshot {
    pub schema_version: u32,
    pub plugin: DomainPluginIdentity,
    pub dependency_closure: Vec<DomainPluginIdentity>,
    pub method: String,
    pub default_subagent_roles: BTreeMap<String, DomainRoleBinding>,
    pub allowed_tools: Vec<String>,
    pub required_artifacts: Vec<String>,
    pub execution_checks: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub review_checklist: Vec<String>,
    pub inputs: Vec<DomainInputDigest>,
    pub output_style: Option<DomainOutputStyleBinding>,
    pub output_style_closure: Vec<DomainPluginIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainBinding {
    pub binding_sha256: String,
    pub snapshot: DomainRuleSnapshot,
}

impl DomainBinding {
    pub(crate) fn new(snapshot: DomainRuleSnapshot) -> Result<Self, String> {
        let binding_sha256 = content_digest(&snapshot)?;
        let value = Self {
            binding_sha256,
            snapshot,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn reference(&self) -> DomainBindingRef {
        DomainBindingRef {
            plugin: self.snapshot.plugin.clone(),
            binding_sha256: self.binding_sha256.clone(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.snapshot.schema_version != SCHEMA_VERSION
            || self.snapshot.plugin.plugin_id.trim().is_empty()
            || self.snapshot.method.trim().is_empty()
            || content_digest(&self.snapshot)? != self.binding_sha256
        {
            return Err(
                "domain binding has an invalid identity, schema or rule digest".to_string(),
            );
        }
        if !self
            .snapshot
            .dependency_closure
            .contains(&self.snapshot.plugin)
        {
            return Err("domain binding does not contain its root provider".to_string());
        }
        Ok(())
    }

    pub(crate) fn prompt_instructions(&self) -> String {
        let version = self
            .snapshot
            .plugin
            .version
            .as_deref()
            .unwrap_or("undeclared");
        format!(
            "[bound_domain]\nPlugin: {} (version: {}; content: {}; binding: {})\nMethod:\n{}\nRequired artifacts: {:?}\nExecutable checks: {:?}\nSemantic criteria: {:?}\nReview checklist: {:?}\nTaskRun policy and the existing task graph remain authoritative. Materialize formal obligations on real task_create nodes; never claim completion from prose alone.\n[/bound_domain]",
            self.snapshot.plugin.plugin_id,
            version,
            self.snapshot.plugin.content_sha256,
            self.binding_sha256,
            self.snapshot.method,
            self.snapshot.required_artifacts,
            self.snapshot.execution_checks,
            self.snapshot.acceptance_criteria,
            self.snapshot.review_checklist
        )
    }

    pub(crate) fn check_capabilities(
        &self,
        names: &[String],
        disabled: &HashSet<String>,
    ) -> Result<(), String> {
        let available =
            |name: &str| names.iter().any(|actual| actual == name) && !disabled.contains(name);
        if self
            .snapshot
            .allowed_tools
            .iter()
            .any(|name| !available(name))
        {
            return Err(format!(
                "domain '{}' requires a tool unavailable in this Agent's effective catalog",
                self.snapshot.plugin.plugin_id
            ));
        }
        if self.requires_plan()
            && ["task_create", "task_execute"].iter().any(|name| {
                !available(name)
                    || (!self.snapshot.allowed_tools.is_empty()
                        && !self.snapshot.allowed_tools.iter().any(|tool| tool == name))
            })
        {
            return Err(format!(
                "domain '{}' formal obligations require task_create/task_execute within its effective tool scope",
                self.snapshot.plugin.plugin_id
            ));
        }
        Ok(())
    }

    pub(crate) fn verify_task_role(
        &self,
        role: &DomainRoleBinding,
        definitions: &[SubagentDefinition],
    ) -> Result<(), String> {
        if resolve_role(
            &role.role_name,
            &self.snapshot.dependency_closure,
            definitions,
        )? != *role
        {
            return Err(format!(
                "domain Subagent '{}' original provider or definition is unavailable",
                role.role_name
            ));
        }
        Ok(())
    }

    pub fn requires_plan(&self) -> bool {
        !self.snapshot.required_artifacts.is_empty()
            || !self.snapshot.execution_checks.is_empty()
            || !self.snapshot.acceptance_criteria.is_empty()
            || !self.snapshot.review_checklist.is_empty()
    }

    /// Empty remains the host's historical "no additional restriction" value.
    /// An actually restricted empty intersection must never become that value.
    pub(crate) fn effective_allowed_tools(
        &self,
        task_tools: &[String],
    ) -> Result<Option<HashSet<String>>, String> {
        let domain = &self.snapshot.allowed_tools;
        let effective: HashSet<String> = match (domain.is_empty(), task_tools.is_empty()) {
            (true, true) => return Ok(None),
            (true, false) => task_tools.iter().cloned().collect(),
            (false, true) => domain.iter().cloned().collect(),
            (false, false) => task_tools
                .iter()
                .filter(|name| domain.contains(name))
                .cloned()
                .collect(),
        };
        if effective.is_empty() {
            return Err("domain and task tool restrictions have an empty intersection".to_string());
        }
        Ok(Some(effective))
    }

    pub(crate) fn role_for_kind(&self, kind: PlanTaskKind) -> Result<&DomainRoleBinding, String> {
        self.snapshot
            .default_subagent_roles
            .get(kind.as_str())
            .ok_or_else(|| {
                format!(
                    "bound domain has no resolved Subagent for {}",
                    kind.as_str()
                )
            })
    }

    pub(crate) fn review_criteria(&self) -> Vec<String> {
        let mut values = self.snapshot.acceptance_criteria.clone();
        for item in &self.snapshot.review_checklist {
            if !values.contains(item) {
                values.push(item.clone());
            }
        }
        values
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DomainDeclaration {
    pub schema_version: u32,
    pub method: String,
    #[serde(default)]
    pub default_subagent_roles: BTreeMap<String, String>,
    pub allowed_tools: Vec<String>,
    pub required_artifacts: Vec<String>,
    pub execution_checks: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub review_checklist: Vec<String>,
    pub inputs: Vec<String>,
}

impl DomainDeclaration {
    pub(crate) fn contains_sensitive_value(&self, values: &[&String]) -> bool {
        let contains = |text: &str| {
            values
                .iter()
                .any(|value| !value.is_empty() && text.contains(value.as_str()))
        };
        contains(&self.method)
            || self
                .default_subagent_roles
                .iter()
                .any(|(kind, role)| contains(kind) || contains(role))
            || [
                &self.allowed_tools,
                &self.required_artifacts,
                &self.execution_checks,
                &self.acceptance_criteria,
                &self.review_checklist,
                &self.inputs,
            ]
            .into_iter()
            .any(|items| items.iter().any(|item| contains(item)))
    }

    pub(crate) fn parse(contents: &str) -> Result<Self, String> {
        let value: Self = serde_yaml::from_str(contents).map_err(|error| {
            // YAML excerpts may contain private instructions. Report location,
            // never the document or an interpolated configuration value.
            error.location().map_or_else(
                || "domain.yaml is not a valid domain declaration".to_string(),
                |position| {
                    format!(
                        "domain.yaml is invalid at line {}, column {}",
                        position.line(),
                        position.column()
                    )
                },
            )
        })?;
        if value.schema_version != SCHEMA_VERSION || value.method.trim().is_empty() {
            return Err("domain.yaml requires schema_version 1 and a nonempty method".to_string());
        }
        for (kind, role) in &value.default_subagent_roles {
            if PlanTaskKind::from_str(kind).is_none() || role.trim().is_empty() {
                return Err(format!(
                    "domain.yaml has an invalid Subagent mapping for '{kind}'"
                ));
            }
            if (kind == PlanTaskKind::Verification.as_str()) != (role == "primary") {
                return Err(format!(
                    "domain.yaml Subagent mapping for '{kind}' must reserve primary exclusively for Verification"
                ));
            }
        }
        for list in [
            &value.allowed_tools,
            &value.required_artifacts,
            &value.execution_checks,
            &value.acceptance_criteria,
            &value.review_checklist,
            &value.inputs,
        ] {
            if list.iter().any(|item| item.trim().is_empty()) {
                return Err("domain.yaml lists cannot contain empty entries".to_string());
            }
        }
        Ok(value)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedDomainPlugin {
    pub identity: DomainPluginIdentity,
    pub root: PathBuf,
    pub closure: Vec<String>,
    pub declaration: Option<DomainDeclaration>,
    pub inputs: Vec<DomainInputDigest>,
    /// Private diagnostics only; configured credential values are never persisted.
    pub binding_error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct PreparedDomainComponents {
    pub plugins: BTreeMap<String, PreparedDomainPlugin>,
    pub output_styles: BTreeMap<String, (String, String, bool)>,
    prepared_roles: BTreeMap<String, DomainRoleBinding>,
}

impl PreparedDomainComponents {
    pub(crate) fn catalog_prompt(&self) -> Option<String> {
        let entries = self
            .plugins
            .values()
            .filter_map(|plugin| {
                let declaration = plugin.declaration.as_ref()?;
                let version = plugin.identity.version.as_deref().unwrap_or("undeclared");
                let availability = if plugin.binding_error.is_some() {
                    "unavailable: sensitive rule content"
                } else {
                    "selectable"
                };
                Some(format!(
                    "- {} (version: {}; {}): method={}\n",
                    plugin.identity.plugin_id,
                    version,
                    availability,
                    if plugin.binding_error.is_some() {
                        "[not projected]".to_string()
                    } else {
                        declaration.method.chars().take(160).collect()
                    }
                ))
            })
            .collect::<Vec<_>>();
        (!entries.is_empty()).then(|| format!("Available domain plugins for new tasks (domain_plugin selects manifest.name; an omitted selection keeps General):\n{}\nThe actual RunCreated binding fixes content, roles and evidence rules; this catalog is discovery only.", entries.join("")))
    }

    fn closure(&self, root: &str) -> Result<Vec<DomainPluginIdentity>, String> {
        let plugin = self.plugins.get(root).ok_or_else(|| {
            format!("domain source '{root}' is not enabled in the published generation")
        })?;
        plugin
            .closure
            .iter()
            .map(|id| {
                self.plugins
                    .get(id)
                    .map(|value| value.identity.clone())
                    .ok_or_else(|| {
                        format!("domain source '{root}' has unavailable dependency '{id}'")
                    })
            })
            .collect()
    }

    pub(crate) fn bind(
        &self,
        plugin_id: &str,
        active_style: Option<&str>,
        definitions: &[SubagentDefinition],
    ) -> Result<DomainBinding, String> {
        self.bind_with_roles(plugin_id, active_style, definitions, None)
    }

    fn bind_with_roles(
        &self,
        plugin_id: &str,
        active_style: Option<&str>,
        definitions: &[SubagentDefinition],
        fixed_roles: Option<&BTreeMap<String, DomainRoleBinding>>,
    ) -> Result<DomainBinding, String> {
        let plugin = self
            .plugins
            .get(plugin_id)
            .ok_or_else(|| format!("domain plugin '{plugin_id}' is not enabled or published"))?;
        if let Some(error) = &plugin.binding_error {
            return Err(error.clone());
        }
        let declaration = plugin
            .declaration
            .as_ref()
            .ok_or_else(|| format!("plugin '{plugin_id}' does not declare domain.yaml"))?;
        let dependency_closure = self.closure(plugin_id)?;
        let (output_style, output_style_closure) = match active_style {
            None => (None, Vec::new()),
            Some(name) => {
                let (owner, instructions, contains_sensitive_value) =
                    self.output_styles.get(name).ok_or_else(|| {
                        format!("active output style '{name}' has no published provider")
                    })?;
                if *contains_sensitive_value {
                    return Err(format!(
                        "output style '{name}' contains a sensitive configuration expansion and cannot be persisted in a domain binding"
                    ));
                }
                let source = self.plugins.get(owner).ok_or_else(|| {
                    format!("output style '{name}' has unavailable provider '{owner}'")
                })?;
                (
                    Some(DomainOutputStyleBinding {
                        name: name.to_string(),
                        provider: source.identity.clone(),
                        instructions: instructions.clone(),
                        instructions_sha256: format!(
                            "{:x}",
                            Sha256::digest(instructions.as_bytes())
                        ),
                    }),
                    self.closure(owner)?,
                )
            }
        };
        let mut roles = fixed_roles.cloned().unwrap_or_default();
        if fixed_roles.is_none() {
            for kind in [
                PlanTaskKind::ReadOnlyReview,
                PlanTaskKind::Investigation,
                PlanTaskKind::TestPlan,
                PlanTaskKind::Implementation,
                PlanTaskKind::Debugging,
                PlanTaskKind::Review,
                PlanTaskKind::Summary,
                PlanTaskKind::Verification,
            ] {
                let role = declaration
                    .default_subagent_roles
                    .get(kind.as_str())
                    .map(String::as_str)
                    .unwrap_or_else(|| {
                        crate::tasks::task_runtime::profiles::default_subagent_for(kind)
                    });
                roles.insert(
                    kind.as_str().to_string(),
                    resolve_role_for_kind(kind, role, &dependency_closure, definitions)?,
                );
            }
        }
        let mut inputs = Vec::new();
        let mut checked = BTreeSet::new();
        for source in dependency_closure.iter().chain(output_style_closure.iter()) {
            if checked.insert(source.plugin_id.clone()) {
                let package = self
                    .plugins
                    .get(&source.plugin_id)
                    .ok_or_else(|| "domain provider disappeared".to_string())?;
                verify_package_inputs(package)?;
                inputs.extend(package.inputs.clone());
            }
        }
        DomainBinding::new(DomainRuleSnapshot {
            schema_version: SCHEMA_VERSION,
            plugin: plugin.identity.clone(),
            dependency_closure,
            method: declaration.method.clone(),
            default_subagent_roles: roles,
            allowed_tools: declaration.allowed_tools.clone(),
            required_artifacts: declaration.required_artifacts.clone(),
            execution_checks: declaration.execution_checks.clone(),
            acceptance_criteria: declaration.acceptance_criteria.clone(),
            review_checklist: declaration.review_checklist.clone(),
            inputs,
            output_style,
            output_style_closure,
        })
    }

    pub(crate) fn record_prepared_role(
        &mut self,
        definition: &SubagentDefinition,
    ) -> Result<(), String> {
        let SubagentKind::Plugin { source } = &definition.kind else {
            return Ok(());
        };
        if definition.name == "primary" {
            return Ok(());
        }
        let provider = self
            .plugins
            .get(source)
            .ok_or_else(|| "prepared Subagent provider is unavailable".to_string())?;
        let role = resolve_role(
            &definition.name,
            std::slice::from_ref(&provider.identity),
            std::slice::from_ref(definition),
        )?;
        self.prepared_roles.insert(definition.name.clone(), role);
        Ok(())
    }

    pub(crate) fn admit_registration(
        &self,
        binding: &DomainBinding,
        active_style: Option<&str>,
        definition: &SubagentDefinition,
    ) -> Result<(), String> {
        binding.validate()?;
        let actual_sources = self.bind_with_roles(
            &binding.snapshot.plugin.plugin_id,
            active_style,
            &[],
            Some(&binding.snapshot.default_subagent_roles),
        )?;
        if actual_sources != *binding {
            return Err(
                "Subagent admission cannot use changed domain rules, content or output style"
                    .to_string(),
            );
        }
        // The primary Verification route is the original Agent, not a registry
        // role with a coincidentally equal name. General is handled by the caller.
        if definition.name == "primary" {
            return Err(
                "domain primary Verification cannot be dispatched as an adhoc registry role"
                    .to_string(),
            );
        }
        let actual_role = resolve_role(
            &definition.name,
            &binding.snapshot.dependency_closure,
            std::slice::from_ref(definition),
        )?;
        if matches!(definition.kind, SubagentKind::Plugin { .. })
            && self.prepared_roles.get(&definition.name) != Some(&actual_role)
        {
            return Err(format!(
                "domain Subagent '{}' does not match its prepared provider definition",
                definition.name
            ));
        }
        if let Some(expected) = binding
            .snapshot
            .default_subagent_roles
            .values()
            .find(|role| role.role_name == definition.name)
            && expected != &actual_role
        {
            return Err(format!(
                "domain Subagent '{}' changed its fixed provider or definition",
                definition.name
            ));
        }
        Ok(())
    }

    pub(crate) fn verify_binding(
        &self,
        binding: &DomainBinding,
        active_style: Option<&str>,
        definitions: &[SubagentDefinition],
    ) -> Result<(), String> {
        binding.validate()?;
        let actual = self.bind(
            &binding.snapshot.plugin.plugin_id,
            active_style,
            definitions,
        )?;
        if actual != *binding {
            return Err(format!(
                "domain '{}' original content, provider or effective output style is unavailable",
                binding.snapshot.plugin.plugin_id
            ));
        }
        Ok(())
    }
}

pub(crate) fn resolve_role_for_kind(
    kind: PlanTaskKind,
    name: &str,
    closure: &[DomainPluginIdentity],
    definitions: &[SubagentDefinition],
) -> Result<DomainRoleBinding, String> {
    if (kind == PlanTaskKind::Verification) != (name == "primary") {
        return Err(format!(
            "domain {} role '{name}' must reserve primary exclusively for Verification",
            kind.as_str()
        ));
    }
    resolve_role(name, closure, definitions)
}

pub(crate) fn resolve_role(
    name: &str,
    closure: &[DomainPluginIdentity],
    definitions: &[SubagentDefinition],
) -> Result<DomainRoleBinding, String> {
    if name == "primary" {
        if definitions.iter().any(|definition| {
            definition.name == name && matches!(definition.kind, SubagentKind::Plugin { .. })
        }) {
            return Err(
                "domain Verification primary cannot be substituted by a same-name plugin provider"
                    .to_string(),
            );
        }
        return Ok(DomainRoleBinding {
            role_name: name.to_string(),
            provider_kind: "host".to_string(),
            provider: None,
            definition_sha256: None,
        });
    }
    let definition = definitions
        .iter()
        .find(|definition| definition.name == name)
        .ok_or_else(|| format!("domain Subagent role '{name}' is not registered"))?;
    let (provider_kind, provider) = match &definition.kind {
        SubagentKind::BuiltIn => ("builtin", None),
        SubagentKind::Plugin { source } => {
            let provider = closure.iter().find(|plugin| &plugin.plugin_id == source)
                .ok_or_else(|| format!("domain role '{name}' provider '{source}' is outside the declared dependency closure"))?;
            ("plugin", Some(provider.clone()))
        }
        SubagentKind::Custom { .. } => {
            return Err(format!(
                "domain role '{name}' has an unbound custom provider"
            ));
        }
    };
    let digest = content_digest(&serde_json::json!({
        "name": definition.name,
        "description": definition.description,
        "system_prompt": definition.system_prompt,
        "access": format!("{:?}", definition.access_mode),
        "mode": definition.execution_mode.to_string(),
        "model": definition.model,
        "thinking": definition.thinking,
        "tool_filter": definition.tool_filter,
        "token_limit": definition.token_limit,
        "inherit_history": definition.inherit_history,
        "inherit_memory": definition.inherit_memory,
        "timeout_secs": definition.timeout_secs,
        "lightweight": definition.lightweight,
        "team": definition.team,
        "is_background": definition.is_background,
        "can_delegate": definition.can_delegate,
        "isolation": definition.isolation,
        "max_iterations": definition.max_iterations,
        "tags": definition.tags,
    }))?;
    Ok(DomainRoleBinding {
        role_name: name.to_string(),
        provider_kind: provider_kind.to_string(),
        provider,
        definition_sha256: Some(digest),
    })
}

pub(crate) fn content_digest<T: Serialize>(value: &T) -> Result<String, String> {
    echo_agent::utils::canonical_json::canonical_json_bytes(value)
        .map(|bytes| format!("{:x}", Sha256::digest(bytes)))
        .map_err(|error| format!("cannot encode domain identity: {error}"))
}

pub(crate) fn read_declaration(root: &Path) -> Result<Option<(DomainDeclaration, String)>, String> {
    let path = root.join("domain.yaml");
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err("cannot inspect domain.yaml".to_string()),
        Ok(metadata) if !metadata.file_type().is_file() => {
            Err("domain.yaml must be a regular file".to_string())
        }
        Ok(_) => {
            let text = std::fs::read_to_string(path)
                .map_err(|_| "cannot read domain.yaml as UTF-8".to_string())?;
            let declaration = DomainDeclaration::parse(&text)?;
            let digest = format!("{:x}", Sha256::digest(text.as_bytes()));
            Ok(Some((declaration, digest)))
        }
    }
}

pub(crate) fn read_inputs(
    plugin_id: &str,
    root: &Path,
    declaration: Option<&DomainDeclaration>,
) -> Result<Vec<DomainInputDigest>, String> {
    let mut inputs = declaration
        .map(|value| value.inputs.clone())
        .unwrap_or_default();
    inputs.sort();
    inputs.dedup();
    inputs
        .into_iter()
        .map(|path| {
            let absolute = input_path(root, &path)?;
            Ok(DomainInputDigest {
                plugin_id: plugin_id.to_string(),
                path,
                sha256: file_digest(&absolute)?,
            })
        })
        .collect()
}

fn input_path(root: &Path, path: &str) -> Result<PathBuf, String> {
    let relative = Path::new(path);
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(format!(
            "domain input '{path}' must be a normalized package-relative file"
        ));
    }
    let root = root
        .canonicalize()
        .map_err(|_| "domain package root is unavailable".to_string())?;
    let candidate = root
        .join(relative)
        .canonicalize()
        .map_err(|_| format!("domain input '{path}' is unavailable"))?;
    if !candidate.starts_with(&root) || !candidate.is_file() {
        return Err(format!(
            "domain input '{path}' is not a regular file inside its package"
        ));
    }
    Ok(candidate)
}

fn file_digest(path: &Path) -> Result<String, String> {
    let mut file =
        std::fs::File::open(path).map_err(|_| "domain input cannot be opened".to_string())?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|_| "domain input cannot be read".to_string())?;
        if count == 0 {
            break;
        }
        let bytes = buffer
            .get(..count)
            .ok_or_else(|| "invalid domain input read length".to_string())?;
        digest.update(bytes);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn verify_package_inputs(package: &PreparedDomainPlugin) -> Result<(), String> {
    for input in &package.inputs {
        if file_digest(&input_path(&package.root, &input.path)?)? != input.sha256 {
            return Err(format!(
                "domain '{}' input '{}' changed after preparation",
                package.identity.plugin_id, input.path
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn test_binding(formal: bool) -> Result<DomainBinding, String> {
    let plugin = DomainPluginIdentity {
        plugin_id: "evidence-domain".to_string(),
        version: None,
        content_sha256: format!("{:x}", Sha256::digest(b"domain-fixture")),
    };
    let definitions = ["explorer", "reviewer", "summarizer", "implementer"]
        .map(|name| SubagentDefinition::new(name, "fixture host role"));
    let mut roles = BTreeMap::new();
    for kind in [
        PlanTaskKind::ReadOnlyReview,
        PlanTaskKind::Investigation,
        PlanTaskKind::TestPlan,
        PlanTaskKind::Implementation,
        PlanTaskKind::Debugging,
        PlanTaskKind::Review,
        PlanTaskKind::Summary,
        PlanTaskKind::Verification,
    ] {
        let name = crate::tasks::task_runtime::profiles::default_subagent_for(kind);
        roles.insert(
            kind.as_str().to_string(),
            resolve_role(name, std::slice::from_ref(&plugin), &definitions)?,
        );
    }
    DomainBinding::new(DomainRuleSnapshot {
        schema_version: 1,
        plugin: plugin.clone(),
        dependency_closure: vec![plugin],
        method: "Inspect exact inputs; preserve provenance and report uncertainty.".to_string(),
        default_subagent_roles: roles,
        allowed_tools: Vec::new(),
        required_artifacts: if formal {
            vec!["reports/result.md".to_string()]
        } else {
            Vec::new()
        },
        execution_checks: if formal {
            vec!["cargo test --locked".to_string()]
        } else {
            Vec::new()
        },
        acceptance_criteria: if formal {
            vec!["Result cites its evidence".to_string()]
        } else {
            Vec::new()
        },
        review_checklist: Vec::new(),
        inputs: Vec::new(),
        output_style: None,
        output_style_closure: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_declaration_is_strict_and_version_is_not_invented() -> Result<(), String> {
        let text = "schema_version: 1\nmethod: Examine evidence.\nallowed_tools: []\nrequired_artifacts: []\nexecution_checks: []\nacceptance_criteria: []\nreview_checklist: []\ninputs: []\n";
        let declaration = DomainDeclaration::parse(text)?;
        assert_eq!(declaration.method, "Examine evidence.");
        assert!(DomainDeclaration::parse(&format!("{text}unexpected: true\n")).is_err());
        assert!(
            DomainDeclaration::parse(&text.replace("method: Examine evidence.", "method: ''"))
                .is_err()
        );
        let binding = test_binding(false)?;
        assert_eq!(binding.snapshot.plugin.version, None);
        assert!(!binding.requires_plan());
        assert!(test_binding(true)?.requires_plan());
        Ok(())
    }

    #[test]
    fn positive_scope_empty_intersection_never_becomes_unrestricted() -> Result<(), String> {
        let mut snapshot = test_binding(false)?.snapshot;
        assert_eq!(
            DomainBinding::new(snapshot.clone())?.effective_allowed_tools(&[])?,
            None
        );
        snapshot.allowed_tools = vec!["read_file".to_string()];
        let binding = DomainBinding::new(snapshot)?;
        assert_eq!(
            binding.effective_allowed_tools(&[])?,
            Some(HashSet::from(["read_file".to_string()]))
        );
        assert!(
            binding
                .effective_allowed_tools(&["shell".to_string()])
                .is_err()
        );
        assert_eq!(
            binding.effective_allowed_tools(&["shell".to_string(), "read_file".to_string()])?,
            Some(HashSet::from(["read_file".to_string()]))
        );
        Ok(())
    }

    #[test]
    fn role_binding_checks_provider_and_definition_even_with_same_name() -> Result<(), String> {
        let binding = test_binding(false)?;
        let mut definition = SubagentDefinition::new("domain-expert", "Evidence expert");
        definition.kind = SubagentKind::Plugin {
            source: binding.snapshot.plugin.plugin_id.clone(),
        };
        let expected = resolve_role(
            "domain-expert",
            &binding.snapshot.dependency_closure,
            &[definition.clone()],
        )?;
        binding.verify_task_role(&expected, &[definition.clone()])?;
        definition.kind = SubagentKind::Plugin {
            source: "unrelated-provider".to_string(),
        };
        assert!(
            binding
                .verify_task_role(&expected, &[definition.clone()])
                .is_err()
        );
        definition.kind = SubagentKind::Plugin {
            source: binding.snapshot.plugin.plugin_id.clone(),
        };
        definition.system_prompt = Some("Changed method".to_string());
        assert!(binding.verify_task_role(&expected, &[definition]).is_err());
        Ok(())
    }

    #[test]
    fn declared_inputs_stay_inside_package_and_detect_post_prepare_changes() -> Result<(), String> {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::write(dir.path().join("method.txt"), "First method")
            .map_err(|error| error.to_string())?;
        let declaration = DomainDeclaration::parse(
            "schema_version: 1\nmethod: Evidence.\nallowed_tools: []\nrequired_artifacts: []\nexecution_checks: []\nacceptance_criteria: []\nreview_checklist: []\ninputs: [method.txt]\n",
        )?;
        let package = PreparedDomainPlugin {
            identity: test_binding(false)?.snapshot.plugin,
            root: dir.path().to_path_buf(),
            closure: Vec::new(),
            declaration: Some(declaration.clone()),
            inputs: read_inputs("evidence-domain", dir.path(), Some(&declaration))?,
            binding_error: None,
        };
        verify_package_inputs(&package)?;
        std::fs::write(dir.path().join("method.txt"), "Second method")
            .map_err(|error| error.to_string())?;
        assert!(verify_package_inputs(&package).is_err());
        assert!(input_path(dir.path(), "../outside.txt").is_err());
        assert!(input_path(dir.path(), "/absolute.txt").is_err());
        Ok(())
    }

    #[test]
    fn maintained_example_parses_and_reads_its_real_declared_inputs() -> Result<(), String> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples/domain-plugin");
        let (declaration, _) =
            read_declaration(&root)?.ok_or("example domain declaration missing")?;
        let inputs = read_inputs("evidence-method", &root, Some(&declaration))?;
        assert_eq!(inputs.len(), 1);
        assert!(inputs.iter().any(|input| input.path == "method.txt"));
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../../examples/domain-plugin/plugin.json"))
                .map_err(|error| error.to_string())?;
        assert_eq!(
            manifest.get("name").and_then(serde_json::Value::as_str),
            Some("evidence-method")
        );
        assert!(manifest.get("version").is_none());
        Ok(())
    }

    #[test]
    fn domain_primary_route_is_kind_bound_and_cannot_name_a_plugin_as_host() -> Result<(), String> {
        let binding = test_binding(false)?;
        let closure = &binding.snapshot.dependency_closure;
        assert!(
            resolve_role_for_kind(PlanTaskKind::Investigation, "primary", closure, &[]).is_err()
        );
        assert!(
            resolve_role_for_kind(PlanTaskKind::Implementation, "primary", closure, &[]).is_err()
        );
        assert_eq!(
            resolve_role_for_kind(PlanTaskKind::Verification, "primary", closure, &[])?
                .provider_kind,
            "host"
        );
        let mut impostor = SubagentDefinition::new("primary", "Plugin role with reserved name");
        impostor.kind = SubagentKind::Plugin {
            source: binding.snapshot.plugin.plugin_id.clone(),
        };
        assert!(
            resolve_role_for_kind(PlanTaskKind::Verification, "primary", closure, &[impostor])
                .is_err()
        );
        let declaration = "schema_version: 1\nmethod: Inspect evidence.\ndefault_subagent_roles:\n  investigation: primary\nallowed_tools: []\nrequired_artifacts: []\nexecution_checks: []\nacceptance_criteria: []\nreview_checklist: []\ninputs: []\n";
        assert!(DomainDeclaration::parse(declaration).is_err());
        Ok(())
    }
}
