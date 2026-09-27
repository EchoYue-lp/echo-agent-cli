fn browser_specialist_action(
    action: &str,
    args: &[&str],
) -> anyhow::Result<(
    crate::browser::BrowserAction,
    echo_agent::prelude::ToolParameters,
)> {
    match action {
        "managed" | "chrome" => Ok((
            crate::browser::BrowserAction::Backend,
            HashMap::from([(
                "backend".to_string(),
                serde_json::Value::String(action.to_string()),
            )]),
        )),
        "navigate" => {
            let url = args
                .get(1)
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| anyhow::anyhow!("Usage: /browser navigate <url>"))?;
            Ok((
                crate::browser::BrowserAction::Navigate,
                HashMap::from([(
                    "url".to_string(),
                    serde_json::Value::String((*url).to_string()),
                )]),
            ))
        }
        "back" => Ok((crate::browser::BrowserAction::Back, HashMap::new())),
        "reload" => Ok((crate::browser::BrowserAction::Reload, HashMap::new())),
        "screenshot" => Ok((crate::browser::BrowserAction::Screenshot, HashMap::new())),
        "click" => Ok((
            crate::browser::BrowserAction::ClickAt,
            HashMap::from([
                (
                    "x".to_string(),
                    serde_json::json!(browser_number(args, 1, "x")?),
                ),
                (
                    "y".to_string(),
                    serde_json::json!(browser_number(args, 2, "y")?),
                ),
                (
                    "effect".to_string(),
                    serde_json::Value::String("none".to_string()),
                ),
            ]),
        )),
        "scroll" => Ok((
            crate::browser::BrowserAction::Scroll,
            HashMap::from([
                (
                    "deltaX".to_string(),
                    serde_json::json!(browser_number(args, 1, "delta-x")?),
                ),
                (
                    "deltaY".to_string(),
                    serde_json::json!(browser_number(args, 2, "delta-y")?),
                ),
            ]),
        )),
        "tabs" => {
            let tab_action = args.get(1).copied().unwrap_or("list");
            let mut parameters = HashMap::from([(
                "action".to_string(),
                serde_json::Value::String(tab_action.to_string()),
            )]);
            match tab_action {
                "list" => {}
                "select" | "close" => {
                    let index = args
                        .get(2)
                        .ok_or_else(|| anyhow::anyhow!("browser tabs {tab_action} requires index"))?
                        .parse::<u64>()
                        .map_err(|error| anyhow::anyhow!("invalid browser tab index: {error}"))?;
                    parameters.insert("index".to_string(), serde_json::Value::Number(index.into()));
                }
                "new" => {
                    if let Some(url) = args.get(2).filter(|value| !value.trim().is_empty()) {
                        parameters.insert(
                            "url".to_string(),
                            serde_json::Value::String((*url).to_string()),
                        );
                    }
                }
                _ => anyhow::bail!("browser tabs action must be list, select, new, or close"),
            }
            Ok((crate::browser::BrowserAction::Tabs, parameters))
        }
        _ => anyhow::bail!(
            "Usage: /browser [status|managed|chrome|navigate <url>|back|reload|screenshot|click <x> <y>|scroll <delta-x> <delta-y>|tabs <action>|stop]"
        ),
    }
}

fn browser_number(args: &[&str], index: usize, name: &str) -> anyhow::Result<f64> {
    args.get(index)
        .ok_or_else(|| anyhow::anyhow!("browser {name} is required"))?
        .parse::<f64>()
        .map_err(|error| anyhow::anyhow!("invalid browser {name}: {error}"))
}

async fn plugin_mutation_receipt(
    authority_scope: String,
    authority: &Arc<crate::plugin_runtime::PluginRuntimeService>,
    plugin_id: Option<String>,
    entry: Option<echo_agent::plugin::PluginEntry>,
    summary: crate::plugin_runtime::ReloadSummary,
    target_receipts: Vec<PluginTargetGenerationReceipt>,
) -> PluginMutationReceipt {
    let active_theme = authority.active_theme().await;
    let themes = authority.themes().await;
    let active_output_style = authority.active_output_style().await;
    let styles = authority.output_styles().await;
    let status = if !summary.errors.is_empty()
        || target_receipts
            .iter()
            .any(|target| target.status == PluginTargetSettlementStatus::Degraded)
    {
        PluginSettlementStatus::Degraded
    } else {
        PluginSettlementStatus::Settled
    };
    PluginMutationReceipt {
        theme: PluginThemeSnapshot {
            authority_scope: authority_scope.clone(),
            active: active_theme,
            themes,
        },
        output_style: PluginOutputStyleSnapshot {
            authority_scope: authority_scope.clone(),
            active: active_output_style,
            styles,
        },
        authority_scope,
        status,
        plugin_id,
        entry,
        summary,
        target_receipts,
    }
}

fn captured_targets_include_authority(
    targets: &ExtensionRuntimeTargets,
    authority: &Arc<crate::plugin_runtime::PluginRuntimeService>,
) -> bool {
    targets
        .iter()
        .any(|target| Arc::ptr_eq(&target.plugin_runtime(), authority))
}

async fn settle_captured_plugin_targets(
    targets: &ExtensionRuntimeTargets,
    authority: &Arc<crate::plugin_runtime::PluginRuntimeService>,
    summary: &mut crate::plugin_runtime::ReloadSummary,
) -> Vec<PluginTargetGenerationReceipt> {
    let mut receipts = Vec::new();
    for target in targets.iter() {
        let runtime = target.plugin_runtime();
        let is_authority = Arc::ptr_eq(authority, &runtime);
        let settlement = if is_authority {
            Ok(summary.errors.clone())
        } else {
            runtime.reload().await.map(|follower| follower.errors)
        };
        let previous = target.prepared_generation_identity().to_string();
        let candidate = runtime.prepared_generation_identity().await;
        let (committed, diagnostics) = match settlement {
            Ok(diagnostics) => (true, diagnostics),
            Err(error) => (false, vec![error.to_string()]),
        };
        let status = if committed && diagnostics.is_empty() {
            PluginTargetSettlementStatus::Settled
        } else {
            if !is_authority {
                summary.errors.extend(
                    diagnostics
                        .iter()
                        .map(|error| format!("plugin host {}: {error}", target.scope())),
                );
            }
            PluginTargetSettlementStatus::Degraded
        };
        receipts.push(PluginTargetGenerationReceipt {
            target: target.scope().to_string(),
            workspace_generation: target.workspace_generation().to_string(),
            previous_prepared_generation: previous,
            candidate_prepared_generation: Some(candidate),
            status,
            diagnostics,
        });
    }
    receipts
}

async fn promote_curated_skill_artifact(
    echo_agent_dir: PathBuf,
    name: &str,
) -> Result<CuratedSkillArtifactCommit, String> {
    let name_path = std::path::Path::new(name);
    if name.trim().is_empty()
        || name_path
            .file_name()
            .is_none_or(|component| component != std::ffi::OsStr::new(name))
    {
        return Err("curated Skill name must be one non-empty path component".to_string());
    }

    let curator = crate::evolution::workspace_curator(&echo_agent_dir);
    let state = curator.load_state().map_err(|error| error.to_string())?;
    let lifecycle = state
        .skills
        .get(name)
        .map(|metadata| metadata.lifecycle)
        .ok_or_else(|| format!("Skill '{name}' was not found in curator state"))?;
    let draft_path = echo_agent_dir
        .join("skills")
        .join("_drafts")
        .join(name)
        .join("SKILL.md");
    let active_path = echo_agent_dir.join("skills").join(name).join("SKILL.md");
    let load_root = echo_agent_dir.join("skills");

    if lifecycle == echo_agent::evolution::SkillLifecycle::Active {
        if !active_path.is_file() {
            return Err(format!(
                "Skill '{name}' is Active but its artifact is missing at {}",
                active_path.display()
            ));
        }
        return Ok(CuratedSkillArtifactCommit {
            active_path,
            load_root,
            idempotent: true,
        });
    }
    if lifecycle != echo_agent::evolution::SkillLifecycle::Draft {
        return Err(format!(
            "Skill '{name}' is in {lifecycle:?} state and cannot be promoted"
        ));
    }

    let draft = std::fs::read(&draft_path).map_err(|error| {
        format!(
            "failed to read curated Skill draft '{}': {error}",
            draft_path.display()
        )
    })?;
    let existing = match std::fs::read(&active_path) {
        Ok(existing) if existing == draft => Some(existing),
        Ok(_) => {
            return Err(format!(
                "refusing to overwrite a different active Skill artifact at {}",
                active_path.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(format!(
                "failed to inspect active Skill artifact '{}': {error}",
                active_path.display()
            ));
        }
    };
    let authority = crate::evolution::review_integration::skill_mutation_authority(
        &echo_agent_dir,
    )
    .map_err(|error| format!("failed to open Skill mutation authority: {error}"))?;
    let mut after = state.clone();
    let metadata = after
        .skills
        .get_mut(name)
        .ok_or_else(|| format!("Skill '{name}' disappeared from curator state"))?;
    metadata.lifecycle = echo_agent::evolution::SkillLifecycle::Active;
    metadata.path = Some(
        echo_agent::evolution::SkillFileMutation::canonical_path(&active_path)
            .map_err(|error| format!("invalid active Skill path: {error}"))?,
    );
    metadata.last_modified_at = chrono::Utc::now();
    let files = if existing.is_some() {
        Vec::new()
    } else {
        vec![echo_agent::evolution::SkillFileMutation::new(
            &active_path,
            None,
            Some(draft),
        )
        .map_err(|error| format!("failed to prepare Skill artifact: {error}"))?]
    };
    let request = echo_agent::evolution::SkillMutationRequest {
        request_id: uuid::Uuid::new_v4().to_string(),
        entity_key: name.to_string(),
        kind: echo_agent::evolution::SkillMutationKind::Promote,
        reason: "user requested curated Skill publication".to_string(),
        files,
        curator_before: state,
        curator_after: after,
        rollback_of: None,
    };
    let preview = authority
        .preview(&request)
        .map_err(|error| format!("failed to preview Skill promotion: {error}"))?;
    let approval = echo_agent::evolution::SkillApprovalArtifact::new(
        uuid::Uuid::new_v4().to_string(),
        preview.operation_digest,
        "eko:user-publish-curated-skill",
        chrono::Utc::now(),
    );
    match authority.apply(request, approval).await.map_err(|error| error.to_string())? {
        echo_agent::evolution::SkillMutationOutcome::Applied(_) => Ok(CuratedSkillArtifactCommit {
            active_path,
            load_root,
            idempotent: false,
        }),
        echo_agent::evolution::SkillMutationOutcome::AlreadyApplied(_) => Ok(CuratedSkillArtifactCommit {
            active_path,
            load_root,
            idempotent: true,
        }),
        outcome => Err(format!("Skill '{name}' promotion did not settle: {outcome:?}")),
    }
}

fn user_skill_source(name: &str) -> String {
    format!("{USER_SKILL_SOURCE_PREFIX}{name}")
}
