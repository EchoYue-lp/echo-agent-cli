//! Agent-facing adapters for authoring EKO Skills and plugins.
//!
//! These tools intentionally delegate to the existing framework Skill
//! validator and application PluginRuntime. They expose the established
//! authorities to the model without creating another format or lifecycle.

use std::path::{Path, PathBuf};

use echo_agent::error::Result;
use echo_agent::tools::permission::ToolPermission;
use echo_agent::tools::{Tool, ToolContext, ToolParameters, ToolResult, ToolRiskLevel};
use futures::future::BoxFuture;
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy)]
enum CreatorOperation {
    SkillValidate,
    PluginScaffold,
    PluginValidate,
}

struct CreatorTool {
    operation: CreatorOperation,
}

impl CreatorTool {
    fn new(operation: CreatorOperation) -> Self {
        Self { operation }
    }

    fn required_path(
        parameters: &ToolParameters,
        key: &str,
    ) -> std::result::Result<PathBuf, String> {
        parameters
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| format!("{key} must be a non-empty path string"))
    }

    fn required_text<'a>(
        parameters: &'a ToolParameters,
        key: &str,
    ) -> std::result::Result<&'a str, String> {
        parameters
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("{key} must be a non-empty string"))
    }

    fn resolved_path(context: &ToolContext, path: &Path) -> PathBuf {
        context.resolve_path(path).into_owned()
    }
}

impl Tool for CreatorTool {
    fn name(&self) -> &str {
        match self.operation {
            CreatorOperation::SkillValidate => "skill_validate",
            CreatorOperation::PluginScaffold => "plugin_scaffold",
            CreatorOperation::PluginValidate => "plugin_validate",
        }
    }

    fn description(&self) -> &str {
        match self.operation {
            CreatorOperation::SkillValidate => {
                "Validate one EKO Agent Skill directory with the framework's Agent Skills validator."
            }
            CreatorOperation::PluginScaffold => {
                "Create a new EKO Agent Plugins 1.0 scaffold at a non-existing directory."
            }
            CreatorOperation::PluginValidate => {
                "Validate an EKO plugin directory with the application PluginRuntime authority."
            }
        }
    }

    fn parameters(&self) -> Value {
        match self.operation {
            CreatorOperation::SkillValidate | CreatorOperation::PluginValidate => json!({
                "type": "object",
                "properties": {
                    "directory": {
                        "type": "string",
                        "description": "Absolute path or path relative to the current workspace."
                    }
                },
                "required": ["directory"],
                "additionalProperties": false
            }),
            CreatorOperation::PluginScaffold => json!({
                "type": "object",
                "properties": {
                    "directory": {
                        "type": "string",
                        "description": "New plugin directory, absolute or relative to the current workspace."
                    },
                    "name": {
                        "type": "string",
                        "description": "Agent Plugins lowercase identifier."
                    }
                },
                "required": ["directory", "name"],
                "additionalProperties": false
            }),
        }
    }

    fn permissions(&self) -> Vec<ToolPermission> {
        match self.operation {
            CreatorOperation::PluginScaffold => vec![ToolPermission::Write],
            CreatorOperation::SkillValidate | CreatorOperation::PluginValidate => {
                vec![ToolPermission::Read]
            }
        }
    }

    fn risk_level(&self) -> ToolRiskLevel {
        match self.operation {
            CreatorOperation::PluginScaffold => ToolRiskLevel::Standard,
            CreatorOperation::SkillValidate | CreatorOperation::PluginValidate => {
                ToolRiskLevel::ReadOnly
            }
        }
    }

    fn execute_with_context<'a>(
        &'a self,
        parameters: ToolParameters,
        context: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolResult>> {
        Box::pin(async move {
            let directory = match Self::required_path(&parameters, "directory") {
                Ok(path) => Self::resolved_path(context, &path),
                Err(error) => return Ok(ToolResult::invalid_arguments(error)),
            };
            match self.operation {
                CreatorOperation::SkillValidate => {
                    let report = echo_agent::skills::external::validate_skill_dir(&directory);
                    Ok(ToolResult::success_json(json!({
                        "valid": report.is_valid(),
                        "path": report.path,
                        "violations": report.violations,
                        "warnings": report.warnings,
                    })))
                }
                CreatorOperation::PluginScaffold => {
                    let name = match Self::required_text(&parameters, "name") {
                        Ok(name) => name,
                        Err(error) => return Ok(ToolResult::invalid_arguments(error)),
                    };
                    match crate::plugin_runtime::PluginRuntimeService::scaffold(&directory, name) {
                        Ok(scaffold) => Ok(ToolResult::success_json(json!({
                            "path": scaffold.path,
                            "name": scaffold.name,
                        }))),
                        Err(error) => Ok(ToolResult::error(error.to_string())),
                    }
                }
                CreatorOperation::PluginValidate => {
                    let report = crate::plugin_runtime::PluginRuntimeService::validate(&directory);
                    Ok(ToolResult::success_json(json!({
                        "valid": report.valid,
                        "name": report.name,
                        "components": report.components,
                        "errors": report.errors,
                    })))
                }
            }
        })
    }
}

pub(crate) fn install_creator_tools(agent: &mut echo_agent::agent::ReactAgent) {
    for operation in [
        CreatorOperation::SkillValidate,
        CreatorOperation::PluginScaffold,
        CreatorOperation::PluginValidate,
    ] {
        agent.add_tool(Box::new(CreatorTool::new(operation)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::agent::ReactAgentBuilder;

    fn parameter(key: &str, value: &Path) -> ToolParameters {
        ToolParameters::from([(key.to_string(), Value::String(value.display().to_string()))])
    }

    #[tokio::test]
    async fn creator_tools_use_authoritative_skill_and_plugin_validators() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let skill = temp.path().join("sample-skill");
        std::fs::create_dir_all(&skill)?;
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: sample-skill\ndescription: Validate a sample Skill.\n---\n\n# Sample\n",
        )?;

        let skill_result = CreatorTool::new(CreatorOperation::SkillValidate)
            .execute(parameter("directory", &skill))
            .await?;
        assert!(skill_result.success);
        assert_eq!(
            skill_result
                .data
                .as_ref()
                .and_then(|data| data.get("valid"))
                .and_then(Value::as_bool),
            Some(true)
        );

        let plugin = temp.path().join("sample-plugin");
        let scaffold_result = CreatorTool::new(CreatorOperation::PluginScaffold)
            .execute(ToolParameters::from([
                (
                    "directory".to_string(),
                    Value::String(plugin.display().to_string()),
                ),
                ("name".to_string(), Value::String("sample-plugin".into())),
            ]))
            .await?;
        assert!(scaffold_result.success);
        assert!(plugin.join("plugin.json").is_file());
        assert!(!plugin.join(".codex-plugin/plugin.json").exists());

        let validation_result = CreatorTool::new(CreatorOperation::PluginValidate)
            .execute(parameter("directory", &plugin))
            .await?;
        assert!(validation_result.success);
        assert_eq!(
            validation_result
                .data
                .as_ref()
                .and_then(|data| data.get("valid"))
                .and_then(Value::as_bool),
            Some(true)
        );
        Ok(())
    }

    #[tokio::test]
    async fn activated_creator_skills_can_discover_and_promote_creator_tools() -> Result<()> {
        let mut agent = ReactAgentBuilder::new()
            .model("test-model")
            .name("creator-test")
            .system_prompt("test")
            .enable_tools()
            .build()?;
        install_creator_tools(&mut agent);
        agent
            .load_skills_from_dir(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../skills")
                    .as_path(),
            )
            .await?;

        agent.activate_skill("plugin-creator").await?;
        let allowed = agent
            .skill_registry()
            .active_skill_allowed_tools()
            .ok_or_else(|| {
                echo_agent::error::ReactError::Other("creator allow-list missing".into())
            })?;
        let registered = agent.tool_names();
        let initial = crate::tool_exposure::initial_visible_tools(&registered);
        let runtime =
            echo_agent::agent::snapshot::ToolRuntime::from_agent(&agent, None, Some(&initial));
        let visibility = runtime.visibility.as_ref().ok_or_else(|| {
            echo_agent::error::ReactError::Other("deferred tool visibility missing".into())
        })?;
        assert!(
            runtime
                .tools_for_llm()
                .iter()
                .any(|tool| tool.function.name == "tool_search")
        );
        assert!(visibility.is_eligible("activate_skill"));
        visibility.activate([
            "activate_skill".to_string(),
            "plugin_scaffold".to_string(),
            "plugin_validate".to_string(),
        ]);
        for name in ["plugin_scaffold", "plugin_validate"] {
            assert!(agent.tool_names().iter().any(|tool| tool == name));
            assert!(allowed.contains(name));
            assert!(visibility.is_visible(name));
        }
        assert!(agent.skill_registry().is_activated("plugin-creator"));

        let activate_skill = agent
            .tool_manager()
            .get_tool("activate_skill")
            .ok_or_else(|| {
                echo_agent::error::ReactError::Other("activate_skill tool missing".into())
            })?;
        let context = ToolContext {
            tool_visibility: Some(visibility.clone()),
            ..ToolContext::default()
        };
        let activation = activate_skill
            .execute_with_context(
                ToolParameters::from([(
                    "name".to_string(),
                    Value::String("skill-creator".to_string()),
                )]),
                &context,
            )
            .await?;
        assert!(activation.success);
        assert_eq!(
            activation.kind,
            echo_agent::tools::ToolResultKind::SkillActivation {
                name: "skill-creator".to_string()
            }
        );
        assert!(visibility.is_visible("skill_validate"));
        Ok(())
    }
}
