//! Project-local Skill loading.
//!
//! Project Skills are external application content under
//! `<project-root>/.eko/skills/`. They are distinct from the user Skill hub:
//! the user enablement file never owns their lifecycle or uninstall path.

use std::path::Path;

pub(crate) const PROJECT_SKILL_SOURCE_PREFIX: &str = "eko:project-skill:";

/// Load project-local Skills into one Agent and tag only descriptors that came
/// from the requested project root. The caller's product policy still applies
/// during discovery; project-local roots are admitted by `ReviewIntegration`.
pub(crate) async fn load_project_skills(
    agent: &crate::agent_handle::AgentHandle,
    project_root: Option<&Path>,
) -> Result<Vec<String>, String> {
    let Some(project_root) = project_root else {
        return Ok(Vec::new());
    };
    let skills_root = project_root.join(".eko").join("skills");
    if !skills_root.is_dir() {
        return Ok(Vec::new());
    }
    let source_root = skills_root
        .canonicalize()
        .unwrap_or_else(|_| skills_root.clone());
    let source_root_for_agent = source_root.clone();
    agent
        .write_async(|agent| {
            Box::pin(async move {
                let loaded = agent
                    .load_skills_from_dir(source_root_for_agent.clone())
                    .await?;
                let mut project_names = Vec::new();
                for name in loaded {
                    let is_project_skill = agent.skill_descriptors().iter().any(|descriptor| {
                        descriptor.name == name && descriptor.location.starts_with(&source_root)
                    });
                    if !is_project_skill {
                        continue;
                    }
                    let source = format!("{PROJECT_SKILL_SOURCE_PREFIX}{name}");
                    agent
                        .tag_skills_source(std::slice::from_ref(&name), &source)
                        .await;
                    project_names.push(name);
                }
                crate::runtime::configure_intent_router(agent);
                Ok::<_, echo_agent::error::ReactError>(project_names)
            })
        })
        .await
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn loads_and_tags_project_local_skill() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let project_root = temp.path().join("project");
        let skill_dir = project_root.join(".eko/skills/project-review");
        std::fs::create_dir_all(&skill_dir).map_err(|error| error.to_string())?;
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: project-review\ndescription: Review this project\n---\n# Review",
        )
        .map_err(|error| error.to_string())?;
        let agent = crate::agent_handle::AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("test-model")
                .system_prompt("test")
                .build()
                .map_err(|error| error.to_string())?,
        );

        let loaded = load_project_skills(&agent, Some(&project_root)).await?;
        assert_eq!(loaded, vec!["project-review"]);
        let source = agent
            .read(|agent| {
                agent
                    .skill_descriptors()
                    .into_iter()
                    .find(|descriptor| descriptor.name == "project-review")
                    .and_then(|descriptor| descriptor.source)
            })
            .await;
        assert_eq!(source.as_deref(), Some("eko:project-skill:project-review"));
        Ok(())
    }
}
