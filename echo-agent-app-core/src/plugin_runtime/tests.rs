#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::agent::ReactAgentBuilder;
    use echo_agent::intent::IntentClassifier;
    use echo_agent::testing::MockLlmClient;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[derive(Default)]
    struct LifecycleCounts {
        init: AtomicUsize,
        activate: AtomicUsize,
        deactivate: AtomicUsize,
        shutdown: AtomicUsize,
        fail_next_activation: AtomicBool,
        shutdown_failures_remaining: AtomicUsize,
    }

    struct TestLifecycle(Arc<LifecycleCounts>);

    impl PluginLifecycle for TestLifecycle {
        fn init(&self) -> Result<(), String> {
            self.0.init.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn activate(&self) -> Result<(), String> {
            self.0.activate.fetch_add(1, Ordering::SeqCst);
            if self.0.fail_next_activation.swap(false, Ordering::SeqCst) {
                Err("injected activation failure".to_string())
            } else {
                Ok(())
            }
        }

        fn deactivate(&self) -> Result<(), String> {
            self.0.deactivate.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn shutdown(&self) -> Result<(), String> {
            self.0.shutdown.fetch_add(1, Ordering::SeqCst);
            if self
                .0
                .shutdown_failures_remaining
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
            {
                Err("injected shutdown failure".to_string())
            } else {
                Ok(())
            }
        }
    }

    fn write_fixture(root: &Path) -> Result<PathBuf, String> {
        write_fixture_at(
            root.join(".echo-agent/plugins/runtime-fixture"),
            "runtime-fixture",
        )
    }

    fn write_fixture_at(plugin: PathBuf, name: &str) -> Result<PathBuf, String> {
        PluginRuntimeService::scaffold(&plugin, name).map_err(|error| error.to_string())?;
        std::fs::write(
            plugin.join("skills/example/SKILL.md"),
            format!(
                "---\nname: {name}-example\ndescription: Example skill for route {name} work and related tasks.\n---\nUse this skill for {name} tasks.\n"
            ),
        )
        .map_err(|error| error.to_string())?;
        std::fs::write(
            plugin.join("monitors.yaml"),
            "monitors:\n  - name: daily-review\n    cron: \"0 0 * * * *\"\n    prompt: Review pending work\n",
        )
        .map_err(|error| error.to_string())?;
        #[cfg(unix)]
        write_fake_lsp(&plugin, name)?;
        Ok(plugin)
    }

    #[cfg(unix)]
    fn write_fake_lsp(plugin: &Path, plugin_name: &str) -> Result<(), String> {
        let server = plugin.join("fake-lsp.sh");
        std::fs::write(
            &server,
            r#"#!/bin/sh
while IFS= read -r raw_line; do
  line=$(printf '%s' "$raw_line" | tr -d '\r')
  case "$line" in
    Content-Length:*) length=${line#Content-Length: } ;;
    "")
      request=$(dd bs=1 count="$length" 2>/dev/null)
      id=$(printf '%s' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
      if [ -n "$id" ]; then
        response=$(printf '{"jsonrpc":"2.0","id":%s,"result":{"capabilities":{}}}' "$id")
        printf 'Content-Length: %s\r\n\r\n%s' "${#response}" "$response"
      fi
      ;;
  esac
done
"#,
        )
        .map_err(|error| error.to_string())?;
        let mut permissions = std::fs::metadata(&server)
            .map_err(|error| error.to_string())?
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&server, permissions).map_err(|error| error.to_string())?;

        let language = if plugin_name == "runtime-fixture" {
            "fixture".to_string()
        } else {
            format!("{plugin_name}-fixture")
        };
        let lsp = serde_yaml::to_string(&echo_agent::lsp::LspConfigFile {
            languages: HashMap::from([(
                language.clone(),
                echo_agent::lsp::LspServerConfig {
                    language,
                    command: server.display().to_string(),
                    args: Vec::new(),
                    extensions: vec![".fixture".to_string()],
                    env: HashMap::new(),
                    initialization_options: None,
                    max_restarts: 0,
                },
            )]),
        })
        .map_err(|error| error.to_string())?;
        std::fs::write(plugin.join("lsp.yaml"), lsp).map_err(|error| error.to_string())
    }

    async fn service(root: &Path) -> Result<Arc<PluginRuntimeService>, String> {
        let agent = ReactAgentBuilder::new()
            .llm_client(Arc::new(MockLlmClient::new()))
            .system_prompt("plugin runtime integration test")
            .enable_tools()
            .enable_subagent()
            .register_agent_dispatch_tool()
            .working_dir(root)
            .build()
            .map_err(|error| error.to_string())?;
        PluginRuntimeService::new_for_test(
            AgentHandle::new(agent),
            root.to_path_buf(),
            root.join("registry.json"),
            root.join("plugin-data"),
        )
        .await
        .map_err(|error| error.to_string())
    }

    #[tokio::test]
    async fn constructor_rejects_an_invalid_initial_generation() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let plugin = write_fixture(temp.path())?;
        std::fs::write(plugin.join("agents/example.md"), "not valid frontmatter")
            .map_err(|error| error.to_string())?;
        let agent = ReactAgentBuilder::new()
            .llm_client(Arc::new(MockLlmClient::new()))
            .system_prompt("constructor rejection test")
            .working_dir(temp.path())
            .build()
            .map(AgentHandle::new)
            .map_err(|error| error.to_string())?;

        let result = PluginRuntimeService::new_for_test(
            agent,
            temp.path().to_path_buf(),
            temp.path().join("registry.json"),
            temp.path().join("plugin-data"),
        )
        .await;

        assert!(result.is_err());
        Ok(())
    }

    async fn bind_test_pool(runtime: &Arc<PluginRuntimeService>) -> Result<Arc<AgentPool>, String> {
        let pool = Arc::new(
            AgentPool::new_for_test(runtime.agent_handle.clone(), None, None, 8, false).await,
        );
        runtime
            .bind_agent_pool(Arc::downgrade(&pool))
            .await
            .map_err(|error| error.to_string())?;
        Ok(pool)
    }

    fn write_application_skill(root: &Path, name: &str) -> Result<PathBuf, String> {
        let skill_root = root.join(name);
        std::fs::create_dir_all(&skill_root).map_err(|error| error.to_string())?;
        std::fs::write(
            skill_root.join("SKILL.md"),
            format!(
                "---\nname: {name}\ndescription: Application skill replay fixture for use {name}.\n---\nUse this skill for replay tests.\n"
            ),
        )
        .map_err(|error| error.to_string())?;
        Ok(root.to_path_buf())
    }

    async fn agent_has_application_skill(
        handle: &AgentHandle,
        name: &str,
        source: &str,
        expected: bool,
    ) -> Result<(), String> {
        let matches = handle
            .read(|agent| {
                agent
                    .skill_descriptors()
                    .into_iter()
                    .filter(|descriptor| {
                        descriptor.name == name && descriptor.source.as_deref() == Some(source)
                    })
                    .count()
            })
            .await;
        let exact = if expected { matches == 1 } else { matches == 0 };
        if exact {
            Ok(())
        } else {
            Err(format!(
                "application skill projection mismatch for '{name}' from '{source}': count={matches}, expected_present={expected}"
            ))
        }
    }

    async fn agent_has_plugin_generation(
        handle: &AgentHandle,
        plugin: &str,
        expected: bool,
    ) -> Result<(), String> {
        let skill = format!("{plugin}-example");
        let subagent = format!("{plugin}-specialist");
        let has_skill = handle
            .read(|agent| {
                agent
                    .skill_descriptors()
                    .iter()
                    .any(|descriptor| descriptor.name == skill)
            })
            .await;
        let registry = handle.read(|agent| agent.subagent_registry().clone()).await;
        let has_subagent = registry.contains(&subagent).await;
        let classifier = handle.write(crate::runtime::configure_intent_router).await;
        let routed_skill = match classifier
            .classify(&format!("route {plugin} work"), &[])
            .await
        {
            echo_agent::intent::Intent::SkillRequired { skill_name, .. } => Some(skill_name),
            _ => None,
        };
        let has_route = routed_skill.as_deref() == Some(skill.as_str());
        if has_skill != expected || has_subagent != expected || has_route != expected {
            return Err(format!(
                "agent plugin generation mismatch for {plugin}: skill={has_skill}, subagent={has_subagent}, route={routed_skill:?}, expected={expected}"
            ));
        }
        Ok(())
    }

    async fn agent_has_output_style(handle: &AgentHandle, expected: bool) -> Result<(), String> {
        let messages = handle
            .read_async(|agent| Box::pin(async move { agent.get_messages().await }))
            .await;
        let present = messages.iter().any(|message| {
            message
                .content
                .as_text_ref()
                .is_some_and(|content| content.contains("Answer directly"))
        });
        if present == expected {
            Ok(())
        } else {
            Err(format!(
                "output style projection expected={expected}, actual={present}"
            ))
        }
    }

    async fn default_service(root: &Path) -> Result<Arc<PluginRuntimeService>, String> {
        let agent = ReactAgentBuilder::new()
            .llm_client(Arc::new(MockLlmClient::new()))
            .system_prompt("default plugin runtime integration test")
            .enable_tools()
            .enable_subagent()
            .register_agent_dispatch_tool()
            .working_dir(root)
            .build()
            .map_err(|error| error.to_string())?;
        let manager = Arc::new(RwLock::new(LspManager::new()));
        let lsp = PluginLspRuntime::new(
            manager,
            PluginLspRuntime::config_for_workspace(root),
            root.to_path_buf(),
        );
        PluginRuntimeService::new(
            AgentHandle::new(agent),
            lsp,
            McpNameOwnershipRegistry::new(Vec::<String>::new()),
        )
        .await
        .map_err(|error| error.to_string())
    }

    async fn wait_until_mutation_holds_state(runtime: &PluginRuntimeService) -> Result<(), String> {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if runtime.state.try_lock().is_err() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "plugin mutation did not acquire runtime state".to_string())
    }

    #[tokio::test]
    async fn user_first_plugin_claim_is_rejected_without_a_receipt() -> Result<(), String> {
        let ownership = McpNameOwnershipRegistry::new(["shared".to_string()]);
        let mut guard = ownership.lock().await;

        let error = guard
            .claim_plugin("fixture", "shared")
            .err()
            .ok_or_else(|| "plugin unexpectedly claimed a user MCP name".to_string())?;

        assert!(error.contains("user configuration"));
        Ok(())
    }

    #[tokio::test]
    async fn plugin_first_user_takeover_invalidates_plugin_shutdown_receipt() -> Result<(), String>
    {
        let ownership = McpNameOwnershipRegistry::new(Vec::<String>::new());
        let token = {
            let mut guard = ownership.lock().await;
            guard.claim_plugin("fixture", "shared")?
        };
        let plugin_receipts = HashMap::from([(
            "fixture".to_string(),
            WiredPluginComponents {
                mcp_servers: vec!["shared".to_string()],
                ..Default::default()
            },
        )]);
        let plugin_ownership = HashMap::from([(
            "fixture".to_string(),
            HashMap::from([("shared".to_string(), token)]),
        )]);

        ownership.claim_user_names(["shared".to_string()]).await;
        let guard = ownership.lock().await;
        let shutdown_receipts =
            exact_plugin_framework_receipts(&plugin_receipts, &plugin_ownership, &guard);

        assert!(
            shutdown_receipts
                .get("fixture")
                .is_some_and(|receipt| receipt.mcp_servers.is_empty())
        );
        assert!(!guard.owns_plugin("fixture", "shared", token));
        Ok(())
    }

    #[tokio::test]
    async fn real_plugin_load_disable_and_unload_are_live() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let plugin = write_fixture(temp.path())?;
        let runtime = service(temp.path()).await?;
        let summary = runtime.reload().await.map_err(|error| error.to_string())?;
        assert_eq!(summary.total, 1);
        assert_eq!(summary.agents_loaded, 1);
        #[cfg(unix)]
        assert_eq!(summary.lsp_languages_loaded, 1);
        assert_eq!(summary.monitors_loaded, 1);
        assert_eq!(summary.themes_loaded, 1);
        assert_eq!(summary.output_styles_loaded, 1);
        let registry = runtime
            .agent_handle
            .read(|agent| agent.subagent_registry().clone())
            .await;
        assert!(registry.contains("runtime-fixture-specialist").await);
        assert_eq!(runtime.themes().await.len(), 1);
        assert_eq!(runtime.output_styles().await.len(), 1);
        #[cfg(unix)]
        assert_eq!(
            runtime.lsp.manager.read().await.running_servers(),
            ["fixture"]
        );

        let cron_store =
            crate::scheduler::CronTaskStore::new().with_path(temp.path().join("cron-tasks.json"));
        let fire_fn: echo_agent::scheduler::FireFn =
            Arc::new(|_| Box::pin(async { Ok("fixture monitor fired".to_string()) }));
        let scheduler = Arc::new(
            SchedulerRunner::new(
                cron_store,
                echo_agent::agent::CancellationToken::new(),
                fire_fn,
            )
            .await
            .map_err(|error| error.to_string())?,
        );
        assert_eq!(
            runtime
                .bind_scheduler(scheduler.clone())
                .await
                .map_err(|error| error.to_string())?,
            1
        );
        assert_eq!(scheduler.list_tasks().await.len(), 1);

        runtime
            .activate_output_style(Some("runtime-fixture-concise"))
            .await
            .map_err(|error| error.to_string())?;
        let projected = runtime
            .agent_handle
            .read_async(|agent| Box::pin(async move { agent.get_messages().await }))
            .await;
        assert!(projected.iter().any(|message| {
            message
                .content
                .as_text_ref()
                .is_some_and(|content| content.contains("Answer directly"))
        }));

        runtime
            .disable("runtime-fixture")
            .await
            .map_err(|error| error.to_string())?;
        assert!(!registry.contains("runtime-fixture-specialist").await);
        assert!(runtime.themes().await.is_empty());
        assert!(runtime.output_styles().await.is_empty());
        assert!(scheduler.list_tasks().await.is_empty());
        #[cfg(unix)]
        assert!(
            runtime
                .lsp
                .manager
                .read()
                .await
                .running_servers()
                .is_empty()
        );
        let projected = runtime
            .agent_handle
            .read_async(|agent| Box::pin(async move { agent.get_messages().await }))
            .await;
        assert!(projected.iter().all(|message| {
            message
                .content
                .as_text_ref()
                .is_none_or(|content| !content.contains("Answer directly"))
        }));

        runtime
            .enable("runtime-fixture")
            .await
            .map_err(|error| error.to_string())?;
        assert!(registry.contains("runtime-fixture-specialist").await);
        assert_eq!(scheduler.list_tasks().await.len(), 1);
        runtime
            .uninstall("runtime-fixture", false)
            .await
            .map_err(|error| error.to_string())?;
        assert!(!plugin.exists());
        assert!(runtime.list().await.is_empty());
        assert!(!registry.contains("runtime-fixture-specialist").await);
        assert!(scheduler.list_tasks().await.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn plugin_generation_reaches_primary_existing_and_future_pool_agents()
    -> Result<(), String> {
        let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
        let runtime = service(temporary.path()).await?;
        let pool = bind_test_pool(&runtime).await?;
        let existing_lease = pool
            .acquire("existing-plugin-consumer")
            .await
            .map_err(|error| error.to_string())?;
        let existing = existing_lease.agent();
        drop(existing_lease);
        agent_has_plugin_generation(&runtime.agent_handle, "runtime-fixture", false).await?;
        agent_has_plugin_generation(&existing, "runtime-fixture", false).await?;

        let _plugin = write_fixture(temporary.path())?;
        runtime.reload().await.map_err(|error| error.to_string())?;
        agent_has_plugin_generation(&runtime.agent_handle, "runtime-fixture", true).await?;
        agent_has_plugin_generation(&existing, "runtime-fixture", true).await?;
        runtime
            .activate_output_style(Some("runtime-fixture-concise"))
            .await
            .map_err(|error| error.to_string())?;
        agent_has_output_style(&runtime.agent_handle, true).await?;
        agent_has_output_style(&existing, true).await?;

        let future_lease = pool
            .acquire("future-plugin-consumer")
            .await
            .map_err(|error| error.to_string())?;
        let future = future_lease.agent();
        drop(future_lease);
        agent_has_plugin_generation(&future, "runtime-fixture", true).await?;
        agent_has_output_style(&future, true).await?;
        let committed_revision = pool.plugin_generation_revision_for_test().await;

        runtime
            .disable("runtime-fixture")
            .await
            .map_err(|error| error.to_string())?;
        agent_has_plugin_generation(&runtime.agent_handle, "runtime-fixture", false).await?;
        agent_has_plugin_generation(&existing, "runtime-fixture", false).await?;
        agent_has_plugin_generation(&future, "runtime-fixture", false).await?;
        let after_remove_lease = pool
            .acquire("after-plugin-remove")
            .await
            .map_err(|error| error.to_string())?;
        let after_remove = after_remove_lease.agent();
        drop(after_remove_lease);
        agent_has_plugin_generation(&after_remove, "runtime-fixture", false).await?;
        assert!(pool.plugin_generation_revision_for_test().await > committed_revision);
        Ok(())
    }

    #[tokio::test]
    async fn application_skill_replay_repairs_pool_split_and_future_generation()
    -> Result<(), String> {
        let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
        let runtime = service(temporary.path()).await?;
        let pool = bind_test_pool(&runtime).await?;
        let existing_lease = pool
            .acquire("application-skill-existing")
            .await
            .map_err(|error| error.to_string())?;
        let existing = existing_lease.agent();
        drop(existing_lease);
        let name = "replay-skill";
        let source = "eko:user-skill:replay-skill";
        let skill_root = write_application_skill(temporary.path(), name)?;

        runtime
            .enable_application_skill(name.to_string(), skill_root.clone(), source.to_string())
            .await
            .map_err(|error| error.to_string())?;
        agent_has_application_skill(&runtime.agent_handle, name, source, true).await?;
        agent_has_application_skill(&existing, name, source, true).await?;

        let tampered_source = source.to_string();
        existing
            .write_async(|agent| {
                Box::pin(async move {
                    agent.unregister_skills_by_source(&tampered_source).await;
                    crate::runtime::configure_intent_router(agent);
                })
            })
            .await;
        agent_has_application_skill(&runtime.agent_handle, name, source, true).await?;
        agent_has_application_skill(&existing, name, source, false).await?;

        let before_enable_repair = pool.plugin_generation_revision_for_test().await;
        runtime
            .enable_application_skill(name.to_string(), skill_root.clone(), source.to_string())
            .await
            .map_err(|error| error.to_string())?;
        agent_has_application_skill(&existing, name, source, true).await?;
        assert!(pool.plugin_generation_revision_for_test().await > before_enable_repair);
        let future_enabled_lease = pool
            .acquire("application-skill-future-enabled")
            .await
            .map_err(|error| error.to_string())?;
        let future_enabled = future_enabled_lease.agent();
        drop(future_enabled_lease);
        agent_has_application_skill(&future_enabled, name, source, true).await?;

        let descriptor = runtime
            .agent_handle
            .read(|agent| {
                agent.skill_descriptors().into_iter().find(|descriptor| {
                    descriptor.name == name && descriptor.source.as_deref() == Some(source)
                })
            })
            .await
            .ok_or_else(|| "primary application skill descriptor is missing".to_string())?;
        runtime
            .disable_application_skill(name.to_string(), skill_root.clone(), source.to_string())
            .await
            .map_err(|error| error.to_string())?;
        agent_has_application_skill(&runtime.agent_handle, name, source, false).await?;
        agent_has_application_skill(&existing, name, source, false).await?;
        agent_has_application_skill(&future_enabled, name, source, false).await?;

        existing
            .write(|agent| {
                agent.skill_registry_mut().register_descriptor(descriptor);
                crate::runtime::configure_intent_router(agent);
            })
            .await;
        agent_has_application_skill(&runtime.agent_handle, name, source, false).await?;
        agent_has_application_skill(&existing, name, source, true).await?;

        let before_disable_repair = pool.plugin_generation_revision_for_test().await;
        runtime
            .disable_application_skill(name.to_string(), skill_root, source.to_string())
            .await
            .map_err(|error| error.to_string())?;
        agent_has_application_skill(&existing, name, source, false).await?;
        assert!(pool.plugin_generation_revision_for_test().await > before_disable_repair);
        let future_disabled_lease = pool
            .acquire("application-skill-future-disabled")
            .await
            .map_err(|error| error.to_string())?;
        let future_disabled = future_disabled_lease.agent();
        drop(future_disabled_lease);
        agent_has_application_skill(&future_disabled, name, source, false).await?;
        Ok(())
    }

    #[tokio::test]
    async fn failed_plugin_activation_restores_primary_and_pool_generation() -> Result<(), String> {
        let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
        let _first = write_fixture(temporary.path())?;
        let runtime = service(temporary.path()).await?;
        let pool = bind_test_pool(&runtime).await?;
        let existing_lease = pool
            .acquire("rollback-existing")
            .await
            .map_err(|error| error.to_string())?;
        let existing = existing_lease.agent();
        drop(existing_lease);
        let previous_revision = pool.plugin_generation_revision_for_test().await;
        let lifecycle = Arc::new(LifecycleCounts::default());
        runtime
            .register_lifecycle(
                "runtime-fixture",
                Arc::new(TestLifecycle(Arc::clone(&lifecycle))),
            )
            .await
            .map_err(|error| error.to_string())?;
        let _second = write_fixture_at(
            temporary
                .path()
                .join(".echo-agent/plugins/rollback-candidate"),
            "rollback-candidate",
        )?;
        lifecycle.fail_next_activation.store(true, Ordering::SeqCst);

        let error = runtime
            .reload()
            .await
            .err()
            .ok_or_else(|| "injected activation failure unexpectedly committed".to_string())?;
        if !error.to_string().contains("injected activation failure") {
            return Err(format!(
                "plugin activation failed for an unexpected reason: {error}"
            ));
        }
        agent_has_plugin_generation(&runtime.agent_handle, "runtime-fixture", true).await?;
        agent_has_plugin_generation(&existing, "runtime-fixture", true).await?;
        agent_has_plugin_generation(&runtime.agent_handle, "rollback-candidate", false).await?;
        agent_has_plugin_generation(&existing, "rollback-candidate", false).await?;
        assert_eq!(
            pool.plugin_generation_revision_for_test().await,
            previous_revision
        );

        let future_lease = pool
            .acquire("rollback-future")
            .await
            .map_err(|error| error.to_string())?;
        let future = future_lease.agent();
        drop(future_lease);
        agent_has_plugin_generation(&future, "runtime-fixture", true).await?;
        agent_has_plugin_generation(&future, "rollback-candidate", false).await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn active_primary_execution_blocks_plugin_generation_publication() -> Result<(), String> {
        let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
        let runtime = service(temporary.path()).await?;
        let pool = bind_test_pool(&runtime).await?;
        let previous_revision = pool.plugin_generation_revision_for_test().await;
        let primary_execution = runtime
            .agent_handle
            .read(|agent| Arc::clone(agent.execution_mutex()))
            .await;
        let active_execution = primary_execution.lock_owned().await;
        let _plugin = write_fixture(temporary.path())?;

        let reload_runtime = Arc::clone(&runtime);
        let mut reload = tokio::spawn(async move { reload_runtime.reload().await });
        tokio::task::yield_now().await;
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut reload)
                .await
                .is_err(),
            "plugin publication escaped an already-active primary execution"
        );
        assert_eq!(
            pool.plugin_generation_revision_for_test().await,
            previous_revision
        );
        agent_has_plugin_generation(&runtime.agent_handle, "runtime-fixture", false).await?;

        drop(active_execution);
        reload
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        agent_has_plugin_generation(&runtime.agent_handle, "runtime-fixture", true).await?;
        assert!(pool.plugin_generation_revision_for_test().await > previous_revision);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn lsp_readers_wait_for_plugin_generation_settlement() -> Result<(), String> {
        let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
        let runtime = service(temporary.path()).await?;
        let _plugin = write_fixture(temporary.path())?;
        let ownership = runtime.mcp_ownership.lock().await;

        let reload_runtime = Arc::clone(&runtime);
        let reload = tokio::spawn(async move { reload_runtime.reload().await });
        wait_until_mutation_holds_state(&runtime).await?;

        let reader_runtime = Arc::clone(&runtime);
        let mut reader =
            tokio::spawn(async move { reader_runtime.lsp_configured_languages().await });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut reader)
                .await
                .is_err(),
            "LSP reader observed a plugin generation before settlement"
        );

        drop(ownership);
        reload
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        let languages = reader.await.map_err(|error| error.to_string())?;
        assert!(!languages.is_empty());
        runtime.shutdown().await.map_err(|error| error.to_string())
    }

    #[tokio::test]
    async fn aborted_reload_waiter_does_not_cancel_owned_component_settlement() -> Result<(), String>
    {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let plugin = write_fixture(temp.path())?;
        let runtime = service(temp.path()).await?;
        std::fs::write(
            plugin.join("themes/second.json"),
            "{\n  \"name\": \"runtime-fixture-second\",\n  \"dark\": false,\n  \"colors\": {}\n}\n",
        )
        .map_err(|error| error.to_string())?;

        let ownership = runtime.mcp_ownership.lock().await;
        let runtime_for_waiter = Arc::clone(&runtime);
        let waiter = tokio::spawn(async move { runtime_for_waiter.reload().await });
        wait_until_mutation_holds_state(&runtime).await?;
        waiter.abort();
        let waiter_error = waiter
            .await
            .err()
            .ok_or_else(|| "aborted plugin reload waiter unexpectedly completed".to_string())?;
        assert!(waiter_error.is_cancelled());
        drop(ownership);

        let themes = runtime.themes().await;
        assert!(
            themes
                .iter()
                .any(|theme| theme.name == "runtime-fixture-second")
        );
        runtime.shutdown().await.map_err(|error| error.to_string())
    }

    #[tokio::test]
    async fn owned_plugin_mutations_execute_in_admission_order() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let runtime = service(temp.path()).await?;
        let order = Arc::new(Mutex::new(Vec::new()));
        let (first_started_tx, first_started_rx) = tokio::sync::oneshot::channel();
        let (release_first_tx, release_first_rx) = tokio::sync::oneshot::channel();

        let first_runtime = Arc::clone(&runtime);
        let first_order = Arc::clone(&order);
        let first = tokio::spawn(async move {
            first_runtime
                .run_owned_mutation(move |_| async move {
                    first_order.lock().await.push(1_u8);
                    first_started_tx
                        .send(())
                        .map_err(|_| anyhow::anyhow!("first mutation start waiter closed"))?;
                    release_first_rx
                        .await
                        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
                    Ok(())
                })
                .await
        });
        first_started_rx.await.map_err(|error| error.to_string())?;

        let second_runtime = Arc::clone(&runtime);
        let second_order = Arc::clone(&order);
        let second = tokio::spawn(async move {
            second_runtime
                .run_owned_mutation(move |_| async move {
                    second_order.lock().await.push(2_u8);
                    Ok(())
                })
                .await
        });
        release_first_tx
            .send(())
            .map_err(|_| "first plugin mutation stopped before release".to_string())?;
        first
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        second
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert_eq!(*order.lock().await, vec![1, 2]);
        runtime.shutdown().await.map_err(|error| error.to_string())
    }

    #[tokio::test]
    async fn aborted_rebind_waiter_does_not_cancel_owned_workspace_settlement() -> Result<(), String>
    {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        std::fs::create_dir_all(&first).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&second).map_err(|error| error.to_string())?;
        write_fixture(&first)?;
        write_fixture_at(
            second.join(".echo-agent/plugins/second-fixture"),
            "second-fixture",
        )?;
        let runtime = service(&first).await?;

        let ownership = runtime.mcp_ownership.lock().await;
        let runtime_for_waiter = Arc::clone(&runtime);
        let second_for_waiter = second.clone();
        let waiter =
            tokio::spawn(
                async move { runtime_for_waiter.rebind_workspace(second_for_waiter).await },
            );
        wait_until_mutation_holds_state(&runtime).await?;
        waiter.abort();
        let waiter_error = waiter
            .await
            .err()
            .ok_or_else(|| "aborted plugin rebind waiter unexpectedly completed".to_string())?;
        assert!(waiter_error.is_cancelled());
        drop(ownership);

        let entries = runtime.list().await;
        assert_eq!(runtime.workspace_root().await, second);
        assert!(
            entries
                .iter()
                .any(|entry| entry.manifest.name == "second-fixture")
        );
        assert!(
            entries
                .iter()
                .all(|entry| entry.manifest.name != "runtime-fixture")
        );
        runtime.shutdown().await.map_err(|error| error.to_string())
    }

    #[tokio::test]
    async fn workspace_rebind_replaces_project_plugins_and_lsp_generation() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        std::fs::create_dir_all(&first).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&second).map_err(|error| error.to_string())?;
        write_fixture(&first)?;
        let runtime = service(&first).await?;
        let lifecycle = Arc::new(LifecycleCounts::default());
        runtime
            .register_lifecycle(
                "runtime-fixture",
                Arc::new(TestLifecycle(Arc::clone(&lifecycle))),
            )
            .await
            .map_err(|error| error.to_string())?;
        let registry = runtime
            .agent_handle
            .read(|agent| agent.subagent_registry().clone())
            .await;
        assert!(registry.contains("runtime-fixture-specialist").await);

        let summary = runtime
            .rebind_workspace(second.clone())
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(summary.total, 0);
        assert_eq!(runtime.workspace_root().await, second);
        assert!(!registry.contains("runtime-fixture-specialist").await);
        assert_eq!(lifecycle.deactivate.load(Ordering::SeqCst), 1);
        assert_eq!(lifecycle.shutdown.load(Ordering::SeqCst), 1);
        assert!(
            runtime
                .lsp
                .manager
                .read()
                .await
                .running_servers()
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn failed_project_lifecycle_cleanup_is_quarantined_and_retried() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        std::fs::create_dir_all(&first).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&second).map_err(|error| error.to_string())?;
        write_fixture(&first)?;
        let runtime = service(&first).await?;
        let lifecycle = Arc::new(LifecycleCounts::default());
        lifecycle
            .shutdown_failures_remaining
            .store(1, Ordering::SeqCst);
        runtime
            .register_lifecycle(
                "runtime-fixture",
                Arc::new(TestLifecycle(Arc::clone(&lifecycle))),
            )
            .await
            .map_err(|error| error.to_string())?;

        let first_error = runtime
            .rebind_workspace(second.clone())
            .await
            .err()
            .ok_or_else(|| "lifecycle cleanup failure was not reported".to_string())?;
        assert!(
            first_error
                .to_string()
                .contains("lifecycle retirement failed")
        );
        assert_eq!(runtime.workspace_root().await, second);
        assert_eq!(runtime.cleanup_debt_roots().await, vec![first.clone()]);

        runtime
            .rebind_workspace(first.clone())
            .await
            .map_err(|error| error.to_string())?;
        assert!(runtime.cleanup_debt_roots().await.is_empty());
        assert_eq!(lifecycle.shutdown.load(Ordering::SeqCst), 2);
        runtime.shutdown().await.map_err(|error| error.to_string())
    }

    #[tokio::test]
    async fn shutdown_unwires_plugin_receipts_and_is_idempotent() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        write_fixture(temp.path())?;
        let runtime = service(temp.path()).await?;
        let lifecycle = Arc::new(LifecycleCounts::default());
        runtime
            .register_lifecycle(
                "runtime-fixture",
                Arc::new(TestLifecycle(Arc::clone(&lifecycle))),
            )
            .await
            .map_err(|error| error.to_string())?;
        let registry = runtime
            .agent_handle
            .read(|agent| agent.subagent_registry().clone())
            .await;
        assert!(registry.contains("runtime-fixture-specialist").await);

        runtime
            .shutdown()
            .await
            .map_err(|error| error.to_string())?;
        runtime
            .shutdown()
            .await
            .map_err(|error| error.to_string())?;
        assert!(!registry.contains("runtime-fixture-specialist").await);
        assert_eq!(lifecycle.deactivate.load(Ordering::SeqCst), 1);
        assert_eq!(lifecycle.shutdown.load(Ordering::SeqCst), 1);
        assert!(
            runtime
                .lsp
                .manager
                .read()
                .await
                .running_servers()
                .is_empty()
        );
        assert!(runtime.reload().await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn malformed_target_preserves_user_scope_receipt_and_agent() -> Result<(), String> {
        const CHILD_BASE: &str = "EKO_PLUGIN_USER_SCOPE_TEST_BASE";
        let child_base = std::env::var_os(CHILD_BASE).map(PathBuf::from);
        if child_base.is_none() {
            let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
            let output = std::process::Command::new(
                std::env::current_exe().map_err(|error| error.to_string())?,
            )
            .arg("malformed_target_preserves_user_scope_receipt_and_agent")
            .arg("--test-threads=1")
            .env(CHILD_BASE, temp.path().join("plugin-base"))
            .output()
            .map_err(|error| error.to_string())?;
            if output.status.success() {
                return Ok(());
            }
            return Err(format!(
                "isolated User-scope plugin test failed\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let child_base = child_base.ok_or_else(|| "missing child plugin base".to_string())?;
        crate::data_root::configure(child_base.clone()).map_err(|current| {
            format!(
                "plugin base was initialized before isolated test: {}",
                current.display()
            )
        })?;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        std::fs::create_dir_all(&first).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&second).map_err(|error| error.to_string())?;
        write_fixture_at(child_base.join("plugins/user-fixture"), "user-fixture")?;
        let malformed = write_fixture(&second)?;
        std::fs::write(
            malformed.join("hooks/hooks.yaml"),
            "PreToolUse: [not-a-hook-rule]\n",
        )
        .map_err(|error| error.to_string())?;
        let runtime = default_service(&first).await?;
        let registry = runtime
            .agent_handle
            .read(|agent| agent.subagent_registry().clone())
            .await;
        assert!(registry.contains("user-fixture-specialist").await);
        {
            let state = runtime.state.lock().await;
            assert!(state.framework_components.contains_key("user-fixture"));
            assert!(
                state
                    .prepared
                    .agents
                    .iter()
                    .any(|agent| agent.name() == "user-fixture-specialist")
            );
        }

        let error = runtime
            .rebind_workspace(second.clone())
            .await
            .err()
            .ok_or_else(|| "malformed target plugin unexpectedly committed".to_string())?;
        assert!(error.to_string().contains("User-scope plugin generation"));
        assert_eq!(runtime.workspace_root().await, second);
        assert!(!registry.contains("runtime-fixture-specialist").await);
        assert!(registry.contains("user-fixture-specialist").await);
        let entries = runtime.list().await;
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries.first().map(|entry| entry.scope),
            Some(PluginScope::User)
        );
        {
            let state = runtime.state.lock().await;
            assert!(state.framework_components.contains_key("user-fixture"));
            assert!(
                state
                    .prepared
                    .agents
                    .iter()
                    .any(|agent| agent.name() == "user-fixture-specialist")
            );
        }
        runtime
            .shutdown()
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    #[tokio::test]
    async fn double_rebind_failure_retires_old_generation_at_target_root() -> Result<(), String> {
        const CHILD_BASE: &str = "EKO_PLUGIN_FAIL_CLOSED_TEST_BASE";
        let child_base = std::env::var_os(CHILD_BASE).map(PathBuf::from);
        if child_base.is_none() {
            let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
            let output = std::process::Command::new(
                std::env::current_exe().map_err(|error| error.to_string())?,
            )
            .arg("double_rebind_failure_retires_old_generation_at_target_root")
            .arg("--test-threads=1")
            .env(CHILD_BASE, temp.path().join("plugin-base"))
            .output()
            .map_err(|error| error.to_string())?;
            if output.status.success() {
                return Ok(());
            }
            return Err(format!(
                "isolated fail-closed plugin test failed\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let child_base = child_base.ok_or_else(|| "missing child plugin base".to_string())?;
        crate::data_root::configure(child_base.clone()).map_err(|current| {
            format!(
                "plugin base was initialized before isolated test: {}",
                current.display()
            )
        })?;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        std::fs::create_dir_all(&first).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&second).map_err(|error| error.to_string())?;
        write_fixture(&first)?;
        let target_plugin = write_fixture_at(
            second.join(".echo-agent/plugins/target-fixture"),
            "target-fixture",
        )?;
        let user_plugin = child_base.join("plugins/user-fixture");
        PluginRuntimeService::scaffold(&user_plugin, "user-fixture")
            .map_err(|error| error.to_string())?;

        let runtime = default_service(&first).await?;
        let registry = runtime
            .agent_handle
            .read(|agent| agent.subagent_registry().clone())
            .await;
        assert!(registry.contains("runtime-fixture-specialist").await);
        assert!(registry.contains("user-fixture-specialist").await);
        let lifecycle = Arc::new(LifecycleCounts::default());
        lifecycle
            .shutdown_failures_remaining
            .store(1, Ordering::SeqCst);
        runtime
            .register_lifecycle(
                "runtime-fixture",
                Arc::new(TestLifecycle(Arc::clone(&lifecycle))),
            )
            .await
            .map_err(|error| error.to_string())?;
        let cron_store =
            crate::scheduler::CronTaskStore::new().with_path(temp.path().join("cron-tasks.json"));
        let fire_fn: echo_agent::scheduler::FireFn =
            Arc::new(|_| Box::pin(async { Ok("fixture monitor fired".to_string()) }));
        let scheduler = Arc::new(
            SchedulerRunner::new(
                cron_store,
                echo_agent::agent::CancellationToken::new(),
                fire_fn,
            )
            .await
            .map_err(|error| error.to_string())?,
        );
        runtime
            .bind_scheduler(scheduler.clone())
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(scheduler.list_tasks().await.len(), 1);

        std::fs::write(
            target_plugin.join("monitors.yaml"),
            "monitors: [not-a-monitor-definition]\n",
        )
        .map_err(|error| error.to_string())?;
        std::fs::write(
            user_plugin.join("hooks/hooks.yaml"),
            "PreToolUse: [not-a-hook-rule]\n",
        )
        .map_err(|error| error.to_string())?;

        let error = runtime
            .rebind_workspace(second.clone())
            .await
            .err()
            .ok_or_else(|| "double-failure rebind unexpectedly succeeded".to_string())?;
        assert!(
            error
                .to_string()
                .contains("retired all plugin-owned components")
        );
        assert!(error.to_string().contains("degraded User-scope plugins"));
        assert_eq!(runtime.workspace_root().await, second);
        assert!(!registry.contains("runtime-fixture-specialist").await);
        assert!(!registry.contains("target-fixture-specialist").await);
        assert!(!registry.contains("user-fixture-specialist").await);
        assert!(scheduler.list_tasks().await.is_empty());
        assert!(
            runtime
                .lsp
                .manager
                .read()
                .await
                .running_servers()
                .is_empty()
        );
        assert!(runtime.list().await.is_empty());
        assert_eq!(lifecycle.shutdown.load(Ordering::SeqCst), 1);
        assert_eq!(runtime.cleanup_debt_roots().await, vec![first.clone()]);
        {
            let state = runtime.state.lock().await;
            assert!(state.framework_components.is_empty());
            assert!(state.mcp_ownership.is_empty());
            assert!(state.prepared.agents.is_empty());
            assert!(state.prepared.monitors.is_empty());
        }
        runtime
            .shutdown()
            .await
            .map_err(|error| error.to_string())?;
        assert!(runtime.cleanup_debt_roots().await.is_empty());
        assert_eq!(lifecycle.shutdown.load(Ordering::SeqCst), 2);
        Ok(())
    }

    #[tokio::test]
    async fn failed_real_reload_restores_previous_live_components() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let plugin = write_fixture(temp.path())?;
        let runtime = service(temp.path()).await?;
        runtime.reload().await.map_err(|error| error.to_string())?;
        let registry = runtime
            .agent_handle
            .read(|agent| agent.subagent_registry().clone())
            .await;
        assert!(registry.contains("runtime-fixture-specialist").await);
        let cron_store =
            crate::scheduler::CronTaskStore::new().with_path(temp.path().join("cron-tasks.json"));
        let fire_fn: echo_agent::scheduler::FireFn =
            Arc::new(|_| Box::pin(async { Ok("fixture monitor fired".to_string()) }));
        let scheduler = Arc::new(
            SchedulerRunner::new(
                cron_store,
                echo_agent::agent::CancellationToken::new(),
                fire_fn,
            )
            .await
            .map_err(|error| error.to_string())?,
        );
        runtime
            .bind_scheduler(scheduler.clone())
            .await
            .map_err(|error| error.to_string())?;
        runtime
            .activate_output_style(Some("runtime-fixture-concise"))
            .await
            .map_err(|error| error.to_string())?;

        std::fs::write(
            plugin.join("hooks/hooks.yaml"),
            "PreToolUse: [this is not a hook rule]\n",
        )
        .map_err(|error| error.to_string())?;
        let error = runtime
            .reload()
            .await
            .err()
            .ok_or_else(|| "malformed hook reload unexpectedly succeeded".to_string())?;
        let rejection = error
            .downcast_ref::<PluginPreparationRejected>()
            .ok_or_else(|| format!("reload did not preserve prepared diagnostics: {error}"))?;
        assert!(rejection.diagnostics.iter().any(|diagnostic| {
            diagnostic.plugin_id() == Some("runtime-fixture")
                && diagnostic.component() == "hooks"
                && diagnostic.severity() == echo_agent::plugin::PluginDiagnosticSeverity::Error
                && diagnostic
                    .path()
                    .is_some_and(|path| path.ends_with("hooks/hooks.yaml"))
        }));
        assert!(
            registry.contains("runtime-fixture-specialist").await,
            "reload rollback did not restore the previous Subagent: {error}"
        );
        assert_eq!(runtime.themes().await.len(), 1);
        assert_eq!(runtime.output_styles().await.len(), 1);
        assert_eq!(
            runtime.active_output_style().await.as_deref(),
            Some("runtime-fixture-concise")
        );
        assert_eq!(scheduler.list_tasks().await.len(), 1);
        #[cfg(unix)]
        assert_eq!(
            runtime.lsp.manager.read().await.running_servers(),
            ["fixture"]
        );
        Ok(())
    }

    #[tokio::test]
    async fn native_lifecycle_brackets_reload_configure_and_unregisters_on_uninstall()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let plugin = write_fixture(temp.path())?;
        let manifest_path = plugin.join("plugin.json");
        let manifest_text =
            std::fs::read_to_string(&manifest_path).map_err(|error| error.to_string())?;
        let mut manifest: serde_json::Value =
            serde_json::from_str(&manifest_text).map_err(|error| error.to_string())?;
        let manifest_object = manifest
            .as_object_mut()
            .ok_or_else(|| "fixture plugin manifest is not an object".to_string())?;
        manifest_object.insert(
            "config".to_string(),
            serde_json::json!({
                "label": {
                    "type": "string",
                    "title": "Label",
                    "default": "initial"
                }
            }),
        );
        std::fs::write(
            &manifest_path,
            serde_json::to_string_pretty(&manifest).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let runtime = service(temp.path()).await?;
        let counts = Arc::new(LifecycleCounts::default());
        runtime
            .register_lifecycle(
                "runtime-fixture",
                Arc::new(TestLifecycle(Arc::clone(&counts))),
            )
            .await
            .map_err(|error| error.to_string())?;

        runtime.reload().await.map_err(|error| error.to_string())?;
        runtime
            .configure(
                "runtime-fixture",
                HashMap::from([("label".to_string(), serde_json::json!("updated"))]),
            )
            .await
            .map_err(|error| error.to_string())?;

        assert_eq!(counts.init.load(Ordering::SeqCst), 1);
        assert_eq!(counts.activate.load(Ordering::SeqCst), 3);
        assert_eq!(counts.deactivate.load(Ordering::SeqCst), 2);

        runtime
            .uninstall("runtime-fixture", false)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(counts.deactivate.load(Ordering::SeqCst), 3);
        assert_eq!(counts.shutdown.load(Ordering::SeqCst), 1);

        write_fixture(temp.path())?;
        runtime.reload().await.map_err(|error| error.to_string())?;
        runtime
            .register_lifecycle(
                "runtime-fixture",
                Arc::new(TestLifecycle(Arc::new(LifecycleCounts::default()))),
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    #[tokio::test]
    async fn failed_native_lifecycle_registration_shuts_down_and_can_retry() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        write_fixture(temp.path())?;
        let runtime = service(temp.path()).await?;
        let counts = Arc::new(LifecycleCounts::default());
        counts.fail_next_activation.store(true, Ordering::SeqCst);

        let error = runtime
            .register_lifecycle(
                "runtime-fixture",
                Arc::new(TestLifecycle(Arc::clone(&counts))),
            )
            .await
            .err()
            .ok_or_else(|| "failing lifecycle registration unexpectedly succeeded".to_string())?;
        assert!(error.to_string().contains("injected activation failure"));
        assert_eq!(counts.init.load(Ordering::SeqCst), 1);
        assert_eq!(counts.activate.load(Ordering::SeqCst), 1);
        assert_eq!(counts.shutdown.load(Ordering::SeqCst), 1);

        runtime
            .register_lifecycle(
                "runtime-fixture",
                Arc::new(TestLifecycle(Arc::new(LifecycleCounts::default()))),
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    #[tokio::test]
    async fn activation_failure_restores_previous_components_and_lifecycle() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let plugin = write_fixture(temp.path())?;
        let runtime = service(temp.path()).await?;
        let counts = Arc::new(LifecycleCounts::default());
        runtime
            .register_lifecycle(
                "runtime-fixture",
                Arc::new(TestLifecycle(Arc::clone(&counts))),
            )
            .await
            .map_err(|error| error.to_string())?;

        std::fs::write(
            plugin.join("themes/example.json"),
            "{\n  \"name\": \"runtime-fixture-dark\",\n  \"dark\": true,\n  \"colors\": {\"accent\": \"#000000\"}\n}\n",
        )
        .map_err(|error| error.to_string())?;
        counts.fail_next_activation.store(true, Ordering::SeqCst);

        let error = runtime
            .reload()
            .await
            .err()
            .ok_or_else(|| "lifecycle activation failure unexpectedly succeeded".to_string())?;
        assert!(error.to_string().contains("injected activation failure"));
        let themes = runtime.themes().await;
        let accent = themes
            .first()
            .and_then(|theme| theme.colors.get("accent"))
            .map(String::as_str);
        assert_eq!(accent, Some("#5b8def"));
        assert_eq!(counts.init.load(Ordering::SeqCst), 1);
        assert_eq!(counts.activate.load(Ordering::SeqCst), 3);
        assert_eq!(counts.deactivate.load(Ordering::SeqCst), 1);
        #[cfg(unix)]
        assert_eq!(
            runtime.lsp.manager.read().await.running_servers(),
            ["fixture"]
        );
        Ok(())
    }

    #[tokio::test]
    async fn scheduler_binding_uses_the_same_lock_order_as_reload() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let runtime = service(temp.path()).await?;
        let cron_store =
            crate::scheduler::CronTaskStore::new().with_path(temp.path().join("cron-tasks.json"));
        let fire_fn: echo_agent::scheduler::FireFn =
            Arc::new(|_| Box::pin(async { Ok("fixture monitor fired".to_string()) }));
        let scheduler = Arc::new(
            SchedulerRunner::new(
                cron_store,
                echo_agent::agent::CancellationToken::new(),
                fire_fn,
            )
            .await
            .map_err(|error| error.to_string())?,
        );

        let state_guard = runtime.state.lock().await;
        let (started, bind_started) = tokio::sync::oneshot::channel();
        let runtime_for_bind = runtime.clone();
        let bind = tokio::spawn(async move {
            let _ = started.send(());
            runtime_for_bind
                .bind_scheduler(scheduler)
                .await
                .map_err(|error| error.to_string())
        });
        bind_started
            .await
            .map_err(|_| "scheduler bind task stopped before starting".to_string())?;
        tokio::task::yield_now().await;

        let scheduler_guard = tokio::time::timeout(
            std::time::Duration::from_millis(250),
            runtime.scheduler.read(),
        )
        .await
        .map_err(|_| "scheduler lock was acquired before plugin state lock".to_string())?;
        drop(scheduler_guard);
        drop(state_guard);

        let monitor_count = tokio::time::timeout(std::time::Duration::from_secs(2), bind)
            .await
            .map_err(|_| "scheduler binding deadlocked".to_string())?
            .map_err(|error| error.to_string())??;
        assert_eq!(monitor_count, 0);
        Ok(())
    }

    #[tokio::test]
    async fn theme_and_output_style_preferences_survive_runtime_restart() -> Result<(), String> {
        let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
        write_fixture(temporary.path())?;
        let runtime = service(temporary.path()).await?;
        runtime
            .activate_theme(Some("runtime-fixture-dark"))
            .await
            .map_err(|error| error.to_string())?;
        runtime
            .activate_output_style(Some("runtime-fixture-concise"))
            .await
            .map_err(|error| error.to_string())?;
        drop(runtime);

        let restored = service(temporary.path()).await?;

        assert_eq!(
            restored.active_theme().await.as_deref(),
            Some("runtime-fixture-dark")
        );
        assert_eq!(
            restored.active_output_style().await.as_deref(),
            Some("runtime-fixture-concise")
        );
        let messages = restored
            .agent_handle
            .read_async(|agent| Box::pin(async move { agent.get_messages().await }))
            .await;
        assert!(messages.iter().any(|message| {
            message
                .content
                .as_text_ref()
                .is_some_and(|content| content.contains("Answer directly"))
        }));
        Ok(())
    }

    #[test]
    fn scaffold_and_validate_cover_application_components() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let plugin = temp.path().join("scaffolded");
        PluginRuntimeService::scaffold(&plugin, "scaffolded").map_err(|error| error.to_string())?;
        for expected_file in [
            "plugin.json",
            "skills/example/SKILL.md",
            "agents/example.md",
            "hooks/hooks.yaml",
            "mcp.json",
            "lsp.yaml",
            "monitors.yaml",
            "themes/example.json",
            "output-styles/scaffolded-concise.md",
            "README.md",
        ] {
            assert!(
                plugin.join(expected_file).is_file(),
                "missing scaffold file {expected_file}"
            );
        }
        assert!(plugin.join("scripts").is_dir());
        assert!(!plugin.join(".echo-plugin").exists());
        assert!(!plugin.join(".mcp.json").exists());
        let report = PluginRuntimeService::validate(&plugin);
        assert!(report.valid, "{}", report.errors.join("; "));
        for expected in [
            "skills",
            "agents",
            "hooks",
            "mcp_servers",
            "lsp_servers",
            "monitors",
            "themes",
            "output_styles",
        ] {
            assert!(
                report
                    .components
                    .iter()
                    .any(|component| component == expected)
            );
        }
        Ok(())
    }

    #[test]
    fn validate_rejects_malformed_runtime_components() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let plugin = temp.path().join("invalid-components");
        PluginRuntimeService::scaffold(&plugin, "invalid-components")
            .map_err(|error| error.to_string())?;
        std::fs::write(
            plugin.join("hooks/hooks.yaml"),
            "PreToolUse:\n  - matcher: '*'\n    hooks:\n      - type: command\n        command: ''\n",
        )
        .map_err(|error| error.to_string())?;
        std::fs::write(
            plugin.join("mcp.json"),
            r#"{"$schema":"invalid","mcpServers":{}}"#,
        )
        .map_err(|error| error.to_string())?;
        std::fs::write(
            plugin.join("skills/example/SKILL.md"),
            "This file has no frontmatter.\n",
        )
        .map_err(|error| error.to_string())?;

        let report = PluginRuntimeService::validate(&plugin);

        assert!(!report.valid);
        assert!(
            report
                .errors
                .iter()
                .any(|error| error.contains("empty command"))
        );
        assert!(
            report
                .errors
                .iter()
                .any(|error| error.contains("MCP config"))
        );
        assert!(
            report
                .errors
                .iter()
                .any(|error| error.contains("must begin with YAML frontmatter"))
        );
        Ok(())
    }
}
