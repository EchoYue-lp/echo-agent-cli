async fn replace_agent_plugin_generation(
    handle: &AgentHandle,
    previous: &AgentPluginGeneration,
    candidate: &AgentPluginGeneration,
    application_skill_repair: Option<&ApplicationSkillProjectionRepair>,
) -> Result<(), String> {
    let previous = previous.clone();
    let candidate = candidate.clone();
    let application_skill_repair = application_skill_repair.cloned();
    handle
        .write_async(|agent| {
            Box::pin(async move {
                remove_agent_plugin_generation(agent, &previous).await;
                if let Some(repair) = application_skill_repair.as_ref() {
                    agent.unregister_skills_by_source(&repair.source).await;
                }
                for descriptor in &candidate.skill_descriptors {
                    if !crate::skills_hub::is_builtin_skill_path(&descriptor.location) {
                        agent
                            .skill_registry_mut()
                            .register_descriptor(descriptor.clone());
                    }
                }
                if let Err(error) = register_plugin_agents(agent, &candidate.plugin_agents).await {
                    remove_agent_plugin_generation(agent, &candidate).await;
                    if let Some(repair) = application_skill_repair.as_ref() {
                        agent.unregister_skills_by_source(&repair.source).await;
                    }
                    for descriptor in &previous.skill_descriptors {
                        if !crate::skills_hub::is_builtin_skill_path(&descriptor.location) {
                            agent
                                .skill_registry_mut()
                                .register_descriptor(descriptor.clone());
                        }
                    }
                    let restore_error = register_plugin_agents(agent, &previous.plugin_agents)
                        .await
                        .err();
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

pub(crate) async fn remove_agent_plugin_generation(
    agent: &mut echo_agent::agent::ReactAgent,
    generation: &AgentPluginGeneration,
) {
    for plugin_agent in &generation.plugin_agents {
        let _ = agent.unregister_subagent(plugin_agent.name()).await;
    }
    for descriptor in &generation.skill_descriptors {
        if !crate::skills_hub::is_builtin_skill_path(&descriptor.location) {
            agent
                .skill_registry_mut()
                .remove_descriptor(&descriptor.name);
        }
    }
}

impl Drop for AgentPool {
    fn drop(&mut self) {
        self.cleanup_cancel.cancel();
    }
}

// ── Tests ─────────────────────────────────────────────────────────────
