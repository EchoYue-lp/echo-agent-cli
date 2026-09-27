async fn replace_agent_plugin_generation(
    handle: &AgentHandle,
    previous: &AgentPluginGeneration,
    candidate: &AgentPluginGeneration,
    application_skill_repair: Option<&ApplicationSkillProjectionRepair>,
    skill_policy: Option<Arc<crate::evolution::ReviewIntegration>>,
) -> Result<(), String> {
    let previous = previous.clone();
    let candidate = candidate.clone();
    let application_skill_repair = application_skill_repair.cloned();
    handle
        .write_async(|agent| {
            Box::pin(async move {
                remove_agent_plugin_generation_inner(agent, &previous, true).await;
                if let Some(repair) = application_skill_repair.as_ref() {
                    agent.unregister_skills_by_source(&repair.source).await;
                }
                let registration = register_agent_skill_descriptors(
                    agent,
                    &candidate.skill_descriptors,
                    skill_policy.as_deref(),
                )
                .await;
                let plugin_registration = match registration {
                    Ok(()) => register_plugin_agents(agent, &candidate.plugin_agents).await,
                    Err(error) => Err(error),
                };
                if let Err(error) = plugin_registration {
                    remove_agent_plugin_generation_inner(agent, &candidate, true).await;
                    if let Some(repair) = application_skill_repair.as_ref() {
                        agent.unregister_skills_by_source(&repair.source).await;
                    }
                    let descriptor_restore = register_agent_skill_descriptors(
                        agent,
                        &previous.skill_descriptors,
                        skill_policy.as_deref(),
                    )
                    .await;
                    let restore_error = match descriptor_restore {
                        Ok(()) => register_plugin_agents(agent, &previous.plugin_agents).await.err(),
                        Err(restore_error) => Some(restore_error),
                    };
                    crate::runtime::configure_intent_router(agent);
                    return Err(match restore_error {
                        Some(restore_error) => {
                            format!("{error}; previous generation restore failed: {restore_error}")
                        }
                        None => error,
                    });
                }
                agent
                    .replace_system_context_projection(
                        crate::plugin_runtime::OUTPUT_STYLE_PROJECTION,
                        candidate.output_style.clone(),
                    )
                    .await;
                crate::runtime::configure_intent_router(agent);
                Ok(())
            })
        })
        .await
}

async fn register_agent_skill_descriptors(
    agent: &mut echo_agent::agent::ReactAgent,
    descriptors: &[echo_agent::skills::external::SkillDescriptor],
    skill_policy: Option<&crate::evolution::ReviewIntegration>,
) -> Result<(), String> {
    for descriptor in descriptors {
        if let Some(policy) = skill_policy
            && !echo_agent::skills::external::SkillLoadPolicy::allows(policy, descriptor).await
        {
            continue;
        }
        agent
            .register_skill_descriptor(descriptor.clone())
            .await
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

pub(crate) async fn remove_agent_plugin_generation(
    agent: &mut echo_agent::agent::ReactAgent,
    generation: &AgentPluginGeneration,
) {
    // Workspace fork setup calls this full cleanup before loading the target
    // project's own `.eko/skills/`; plugin replacement uses the inner helper
    // with preservation enabled so project Skills survive plugin reloads.
    remove_agent_plugin_generation_inner(agent, generation, false).await;
}

async fn remove_agent_plugin_generation_inner(
    agent: &mut echo_agent::agent::ReactAgent,
    generation: &AgentPluginGeneration,
    preserve_project_skills: bool,
) {
    for plugin_agent in &generation.plugin_agents {
        let _ = agent.unregister_subagent(plugin_agent.name()).await;
    }
    let removable = generation
        .skill_descriptors
        .iter()
        .filter(|descriptor| {
            let is_project_skill = descriptor.source.as_deref().is_some_and(|source| {
                source.starts_with(crate::skills_hub::project::PROJECT_SKILL_SOURCE_PREFIX)
            });
            !preserve_project_skills || !is_project_skill
        })
        .map(|descriptor| descriptor.name.clone())
        .collect::<Vec<_>>();
    agent.unregister_skill_names(&removable).await;
}

impl Drop for AgentPool {
    fn drop(&mut self) {
        self.cleanup_cancel.cancel();
    }
}

// ── Tests ─────────────────────────────────────────────────────────────
