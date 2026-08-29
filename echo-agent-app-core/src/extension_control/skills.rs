#[cfg(test)]
async fn skill_source_present(
    agent: &crate::agent_handle::AgentHandle,
    name: &str,
    source: &str,
) -> bool {
    agent
        .read(|agent| {
            agent.skill_descriptors().iter().any(|descriptor| {
                descriptor.name == name && descriptor.source.as_deref() == Some(source)
            })
        })
        .await
}

async fn skill_entry(state: &AppState, name: &str) -> anyhow::Result<(PathBuf, String)> {
    let mut hub = state.skills_hub.write().await;
    hub.refresh();
    let entry = hub
        .get(name)
        .ok_or_else(|| anyhow::anyhow!("Skill '{name}' not found"))?;
    let load_root = entry
        .path
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| entry.path.clone());
    Ok((load_root, entry.category.clone()))
}

fn skill_business_failure(
    outcome: &Result<SkillSyncReceipt, SkillMutationError>,
) -> Option<String> {
    match outcome {
        Ok(receipt) if receipt.status == SkillSettlementStatus::Degraded => Some(format!(
            "skill generation {} committed but runtime settlement is degraded",
            receipt.desired_generation
        )),
        Ok(_) => None,
        Err(error) => Some(error.to_string()),
    }
}

fn skill_toggle_command_identity(name: &str, enabled: bool) -> String {
    let mut digest = Sha256::new();
    digest.update(b"set-skill-enabled\0");
    digest.update(name.as_bytes());
    digest.update([u8::from(enabled)]);
    format!("sha256_{:x}", digest.finalize())
}

fn skill_artifact_command_identity(action: &str, value: &str, force: bool) -> String {
    let mut digest = Sha256::new();
    digest.update(action.as_bytes());
    digest.update(b"\0");
    digest.update(value.as_bytes());
    digest.update([u8::from(force)]);
    format!("sha256_{:x}", digest.finalize())
}

async fn replay_skill_artifact_debt(
    state: &Arc<AppState>,
    config: &mut EnabledSkillsConfig,
) -> (
    bool,
    Vec<SkillTargetSettlementReceipt>,
    Vec<SkillTargetSettlementReceipt>,
) {
    let Some(mut debt) = config.repair_debt.clone() else {
        return (false, Vec::new(), Vec::new());
    };
    let mut changed = false;
    let mut receipts = Vec::new();
    let mut terminal_receipts = Vec::new();
    let mut pending_enablements = Vec::new();
    for name in std::mem::take(&mut debt.artifact_enablements) {
        match skill_entry(state, &name).await {
            Ok((_, category)) => {
                config.skills.insert(
                    name.clone(),
                    SkillEnableEntry {
                        category,
                        enabled: true,
                        baseline: false,
                    },
                );
                changed = true;
                receipts.push(SkillTargetSettlementReceipt {
                    target: format!("skill-artifact-enable:{name}"),
                    workspace_generation: "global".to_string(),
                    specialist_generation: config.desired_generation,
                    status: SkillTargetSettlementStatus::Settled,
                    changed_entries: vec![name],
                    error: None,
                });
            }
            Err(error) => {
                pending_enablements.push(name.clone());
                receipts.push(SkillTargetSettlementReceipt {
                    target: format!("skill-artifact-enable:{name}"),
                    workspace_generation: "global".to_string(),
                    specialist_generation: config.desired_generation,
                    status: SkillTargetSettlementStatus::Degraded,
                    changed_entries: Vec::new(),
                    error: Some(error.to_string()),
                });
            }
        }
    }
    debt.artifact_enablements = pending_enablements;

    let skill_root = state.skills_hub.read().await.root().to_path_buf();
    let mut pending_syncs = Vec::new();
    for pending in std::mem::take(&mut debt.artifact_syncs) {
        let target = format!("skill-artifact-sync:{}", pending.name);
        debt.target_failures
            .retain(|failure| failure.target != target || failure.component != "artifact_sync");
        let mut hub = SkillsHub::with_root(skill_root.clone());
        let result =
            crate::skills_hub::sync_skills(&mut hub, Some(pending.name.as_str()), pending.force)
                .await;
        let (receipt, terminal) = match result {
            Ok(results) if results.iter().all(|result| result.success) => {
                changed = true;
                (
                    SkillTargetSettlementReceipt {
                        target,
                        workspace_generation: "global".to_string(),
                        specialist_generation: config.desired_generation,
                        status: SkillTargetSettlementStatus::Settled,
                        changed_entries: results.into_iter().map(|result| result.name).collect(),
                        error: None,
                    },
                    false,
                )
            }
            Ok(results) => {
                let message = results
                    .iter()
                    .filter(|result| !result.success)
                    .map(|result| format!("{}: {}", result.name, result.message))
                    .collect::<Vec<_>>()
                    .join("; ");
                let retryable = results
                    .iter()
                    .filter(|result| !result.success)
                    .any(|result| result.retryable);
                if retryable {
                    pending_syncs.push(pending.clone());
                } else {
                    changed = true;
                }
                (
                    SkillTargetSettlementReceipt {
                        target,
                        workspace_generation: "global".to_string(),
                        specialist_generation: config.desired_generation,
                        status: SkillTargetSettlementStatus::Degraded,
                        changed_entries: Vec::new(),
                        error: Some(message),
                    },
                    !retryable,
                )
            }
            Err(error) => {
                pending_syncs.push(pending.clone());
                (
                    SkillTargetSettlementReceipt {
                        target,
                        workspace_generation: "global".to_string(),
                        specialist_generation: config.desired_generation,
                        status: SkillTargetSettlementStatus::Degraded,
                        changed_entries: Vec::new(),
                        error: Some(error),
                    },
                    false,
                )
            }
        };
        if terminal {
            terminal_receipts.push(receipt);
        } else {
            receipts.push(receipt);
        }
    }
    debt.artifact_syncs = pending_syncs;
    config.set_repair_debt(debt);
    (changed, receipts, terminal_receipts)
}

fn repair_component(target: &str) -> &'static str {
    if target.starts_with("skill-artifact-sync:") {
        "artifact_sync"
    } else if target.starts_with("skill-artifact-enable:") {
        "artifact_enablement"
    } else if target.starts_with("skill-artifact:") {
        "artifact"
    } else {
        match target {
            "enabled-skills.json" => "durable_file",
            "skill-catalog" => "catalog",
            "workspace-generations" => "workspace_identity",
            "runtime-targets" => "runtime_targets",
            _ => "runtime_fanout",
        }
    }
}

fn repair_target_debt(
    receipt: &SkillTargetSettlementReceipt,
    expected_generation: u64,
) -> Option<SkillRepairTargetDebt> {
    if receipt.status != SkillTargetSettlementStatus::Degraded {
        return None;
    }
    let reason = receipt
        .error
        .clone()
        .unwrap_or_else(|| "target reported degraded settlement without a reason".to_string());
    Some(SkillRepairTargetDebt {
        target: receipt.target.clone(),
        component: repair_component(&receipt.target).to_string(),
        expected_generation,
        observed_generation: (receipt.specialist_generation != expected_generation)
            .then_some(receipt.specialist_generation),
        retryable: !reason.contains("newer desired generation superseded"),
        reason,
    })
}

fn repair_debt_from_target_receipts(
    generation: u64,
    content_identity: String,
    attempts: u32,
    target_receipts: &[SkillTargetSettlementReceipt],
) -> SkillRepairDebt {
    SkillRepairDebt {
        generation,
        content_identity,
        attempts,
        target_failures: target_receipts
            .iter()
            .filter_map(|receipt| repair_target_debt(receipt, generation))
            .collect(),
        artifact_removals: target_receipts
            .iter()
            .filter(|receipt| receipt.status == SkillTargetSettlementStatus::Degraded)
            .filter_map(|receipt| receipt.target.strip_prefix("skill-artifact:"))
            .map(str::to_string)
            .collect(),
        artifact_syncs: Vec::new(),
        artifact_enablements: Vec::new(),
    }
}

fn preserve_artifact_repair_actions(
    debt: &mut SkillRepairDebt,
    existing: Option<&SkillRepairDebt>,
) {
    let Some(existing) = existing else {
        return;
    };
    debt.artifact_removals
        .extend(existing.artifact_removals.iter().cloned());
    debt.artifact_syncs
        .extend(existing.artifact_syncs.iter().cloned());
    debt.artifact_enablements
        .extend(existing.artifact_enablements.iter().cloned());
}

fn remove_skill_artifact(skill_root: PathBuf, name: &str) -> Result<bool, String> {
    let mut hub = SkillsHub::with_root(skill_root);
    crate::skills_hub::install::uninstall(name, &mut hub)
}

async fn record_artifact_repair_debt(
    flow: &crate::product_data_io::ProductDataIoFlow,
    path: PathBuf,
    receipt: &SkillSyncReceipt,
    name: &str,
    failure: &str,
) -> Result<SkillRepairDebt, SkillMutationError> {
    let mut config = read_enabled_skills_config(flow, path.clone()).await?;
    if config.desired_generation != receipt.desired_generation
        || config.content_identity != receipt.content_identity
    {
        return Err(SkillMutationError::BeforeCommit(
            "a newer skill generation superseded artifact repair debt".to_string(),
        ));
    }
    let mut debt = config
        .repair_debt
        .take()
        .unwrap_or_else(|| SkillRepairDebt {
            generation: config.desired_generation,
            content_identity: config.content_identity.clone(),
            attempts: 0,
            target_failures: Vec::new(),
            artifact_removals: Vec::new(),
            artifact_syncs: Vec::new(),
            artifact_enablements: Vec::new(),
        });
    debt.attempts = debt.attempts.saturating_add(1);
    let target_receipt = SkillTargetSettlementReceipt {
        target: format!("skill-artifact:{name}"),
        workspace_generation: "global".to_string(),
        specialist_generation: receipt.desired_generation,
        status: SkillTargetSettlementStatus::Degraded,
        changed_entries: Vec::new(),
        error: Some(failure.to_string()),
    };
    if let Some(target_debt) = repair_target_debt(&target_receipt, receipt.desired_generation) {
        debt.target_failures.push(target_debt);
    }
    if !debt
        .artifact_removals
        .iter()
        .any(|candidate| candidate == name)
    {
        debt.artifact_removals.push(name.to_string());
    }
    config.set_repair_debt(debt.clone());
    write_enabled_skills_config(flow, path, config).await?;
    Ok(debt)
}

async fn record_install_repair_debt(
    state: &Arc<AppState>,
    flow: &crate::product_data_io::ProductDataIoFlow,
    path: PathBuf,
    name: &str,
    failure: &str,
) -> Result<SkillRepairDebt, SkillMutationError> {
    let skill_root = state.skills_hub.read().await.root().to_path_buf();
    let mut config = read_enabled_skills_config(flow, path.clone()).await?;
    normalize_skill_content_identity(flow, &mut config, skill_root).await?;
    let mut debt = config
        .repair_debt
        .take()
        .unwrap_or_else(|| empty_skill_repair_debt(&config));
    debt.attempts = debt.attempts.saturating_add(1);
    debt.target_failures.push(SkillRepairTargetDebt {
        target: format!("skill-artifact-enable:{name}"),
        component: "artifact_enablement".to_string(),
        expected_generation: config.desired_generation,
        observed_generation: None,
        reason: failure.to_string(),
        retryable: true,
    });
    debt.artifact_enablements.push(name.to_string());
    config.set_repair_debt(debt.clone());
    write_enabled_skills_config(flow, path, config).await?;
    Ok(debt)
}

async fn record_artifact_sync_repair_debt(
    flow: &crate::product_data_io::ProductDataIoFlow,
    path: PathBuf,
    receipt: &SkillSyncReceipt,
    failures: &[(String, String)],
    force: bool,
) -> Result<SkillRepairDebt, SkillMutationError> {
    let mut config = read_enabled_skills_config(flow, path.clone()).await?;
    if config.desired_generation != receipt.desired_generation
        || config.content_identity != receipt.content_identity
    {
        return Err(SkillMutationError::BeforeCommit(
            "a newer skill generation superseded artifact sync repair debt".to_string(),
        ));
    }
    let mut debt = config
        .repair_debt
        .take()
        .unwrap_or_else(|| empty_skill_repair_debt(&config));
    debt.attempts = debt.attempts.saturating_add(1);
    for (name, failure) in failures {
        debt.target_failures.push(SkillRepairTargetDebt {
            target: format!("skill-artifact-sync:{name}"),
            component: "artifact_sync".to_string(),
            expected_generation: config.desired_generation,
            observed_generation: None,
            reason: failure.clone(),
            retryable: true,
        });
        debt.artifact_syncs.push(SkillArtifactSyncDebt {
            name: name.clone(),
            force,
        });
    }
    config.set_repair_debt(debt.clone());
    write_enabled_skills_config(flow, path, config).await?;
    Ok(debt)
}

fn empty_skill_repair_debt(config: &EnabledSkillsConfig) -> SkillRepairDebt {
    SkillRepairDebt {
        generation: config.desired_generation,
        content_identity: config.content_identity.clone(),
        attempts: 0,
        target_failures: Vec::new(),
        artifact_removals: Vec::new(),
        artifact_syncs: Vec::new(),
        artifact_enablements: Vec::new(),
    }
}

async fn read_enabled_skills_config(
    flow: &crate::product_data_io::ProductDataIoFlow,
    path: PathBuf,
) -> Result<EnabledSkillsConfig, SkillMutationError> {
    flow.run("read enabled skills desired state", move || {
        EnabledSkillsConfig::load(&path)
    })
    .await
    .map_err(|error| SkillMutationError::BeforeCommit(error.to_string()))?
    .map_err(|error| SkillMutationError::BeforeCommit(error.to_string()))
}

async fn skill_commit_is_current(
    flow: &crate::product_data_io::ProductDataIoFlow,
    path: PathBuf,
    committed: &EnabledSkillsConfig,
) -> Result<bool, SkillMutationError> {
    let latest = read_enabled_skills_config(flow, path).await?;
    Ok(latest.desired_generation == committed.desired_generation
        && latest.content_identity == committed.content_identity)
}

async fn admitted_skill_operation(
    flow: &crate::product_data_io::ProductDataIoFlow,
    path: PathBuf,
    operation_id: &str,
    command_identity: &str,
) -> Result<Option<SkillOperationIdentity>, SkillMutationError> {
    let config = read_enabled_skills_config(flow, path).await?;
    let Some(committed) = config.operation(operation_id) else {
        return Ok(None);
    };
    if committed.command_identity == command_identity {
        return Ok(Some(committed.clone()));
    }
    Err(SkillMutationError::OperationConflict {
        operation_id: operation_id.to_string(),
        committed_content_identity: committed.content_identity.clone(),
    })
}

async fn record_skill_operation_identity(
    flow: &crate::product_data_io::ProductDataIoFlow,
    path: PathBuf,
    receipt: &SkillSyncReceipt,
    operation_id: String,
    command_identity: String,
    artifact_name: Option<String>,
) -> Result<(), SkillMutationError> {
    let mut config = read_enabled_skills_config(flow, path.clone()).await?;
    if config.desired_generation != receipt.desired_generation
        || config.content_identity != receipt.content_identity
    {
        return Err(SkillMutationError::BeforeCommit(
            "a newer skill generation superseded operation identity commit".to_string(),
        ));
    }
    config.record_operation(SkillOperationIdentity {
        operation_id,
        command_identity,
        artifact_name,
        content_identity: receipt.content_identity.clone(),
        generation: receipt.desired_generation,
    });
    write_enabled_skills_config(flow, path, config).await
}

fn stale_skill_generation_receipt(committed: &EnabledSkillsConfig) -> SkillTargetSettlementReceipt {
    SkillTargetSettlementReceipt {
        target: "enabled-skills.json".to_string(),
        workspace_generation: "global".to_string(),
        specialist_generation: committed.desired_generation,
        status: SkillTargetSettlementStatus::Degraded,
        changed_entries: Vec::new(),
        error: Some(
            "a newer desired generation superseded this settlement before runtime fanout"
                .to_string(),
        ),
    }
}

async fn write_enabled_skills_config(
    flow: &crate::product_data_io::ProductDataIoFlow,
    path: PathBuf,
    config: EnabledSkillsConfig,
) -> Result<(), SkillMutationError> {
    flow.run("commit enabled skills desired state", move || {
        config.save(&path)
    })
    .await
    .map_err(|error| SkillMutationError::BeforeCommit(error.to_string()))?
    .map_err(|error| SkillMutationError::BeforeCommit(error.to_string()))
}

async fn compute_skill_content_identity(
    flow: &crate::product_data_io::ProductDataIoFlow,
    skills: HashMap<String, SkillEnableEntry>,
    skill_root: PathBuf,
) -> Result<String, SkillMutationError> {
    flow.run("hash enabled skill desired content", move || {
        skill_content_identity(&skills, skill_root)
    })
    .await
    .map_err(|error| SkillMutationError::BeforeCommit(error.to_string()))?
    .map_err(SkillMutationError::BeforeCommit)
}

fn skill_content_identity(
    skills: &HashMap<String, SkillEnableEntry>,
    skill_root: PathBuf,
) -> Result<String, String> {
    let hub = SkillsHub::with_root(skill_root);
    let skill_paths = hub
        .list()
        .into_iter()
        .map(|entry| (entry.name.clone(), entry.path.join("SKILL.md")))
        .collect::<BTreeMap<_, _>>();
    let mut canonical = BTreeMap::new();
    for (name, entry) in skills {
        let body_identity = if entry.enabled {
            match skill_paths.get(name) {
                Some(path) => {
                    let bytes = std::fs::read(path).map_err(|error| {
                        format!("failed to hash enabled skill '{}': {error}", path.display())
                    })?;
                    format!("sha256_{:x}", Sha256::digest(bytes))
                }
                None => "missing".to_string(),
            }
        } else {
            "disabled".to_string()
        };
        canonical.insert(
            name.clone(),
            (
                entry.category.clone(),
                entry.enabled,
                entry.baseline,
                body_identity,
            ),
        );
    }
    let encoded = serde_json::to_vec(&canonical).map_err(|error| error.to_string())?;
    Ok(format!("sha256_{:x}", Sha256::digest(encoded)))
}

async fn normalize_skill_content_identity(
    flow: &crate::product_data_io::ProductDataIoFlow,
    config: &mut EnabledSkillsConfig,
    skill_root: PathBuf,
) -> Result<bool, SkillMutationError> {
    if config.settled_generation > config.desired_generation {
        return Err(SkillMutationError::BeforeCommit(format!(
            "settled generation {} exceeds desired generation {}",
            config.settled_generation, config.desired_generation
        )));
    }
    let mut changed = false;
    let existing_debt = config.repair_debt.clone();
    if config.version < 2 {
        config.version = 2;
        changed = true;
    }
    let overflow = config
        .operation_identities
        .len()
        .saturating_sub(crate::skills_hub::enabled_skills::MAX_OPERATION_IDENTITIES);
    if overflow > 0 {
        config.operation_identities.drain(..overflow);
        changed = true;
    }
    let identity = compute_skill_content_identity(flow, config.skills.clone(), skill_root).await?;
    if config.content_identity.is_empty() {
        config.content_identity = identity.clone();
        changed = true;
    } else if config.content_identity != identity {
        config.desired_generation = config.desired_generation.checked_add(1).ok_or_else(|| {
            SkillMutationError::BeforeCommit(
                "enabled skill desired generation is exhausted".to_string(),
            )
        })?;
        config.content_identity = identity.clone();
        let mut debt = SkillRepairDebt {
            generation: config.desired_generation,
            content_identity: identity.clone(),
            attempts: 0,
            target_failures: Vec::new(),
            artifact_removals: Vec::new(),
            artifact_syncs: Vec::new(),
            artifact_enablements: Vec::new(),
        };
        preserve_artifact_repair_actions(&mut debt, existing_debt.as_ref());
        config.set_repair_debt(debt);
        changed = true;
    }
    if config.settled_generation < config.desired_generation && config.repair_debt.is_none() {
        let mut debt = SkillRepairDebt {
            generation: config.desired_generation,
            content_identity: identity,
            attempts: 0,
            target_failures: Vec::new(),
            artifact_removals: Vec::new(),
            artifact_syncs: Vec::new(),
            artifact_enablements: Vec::new(),
        };
        preserve_artifact_repair_actions(&mut debt, existing_debt.as_ref());
        config.set_repair_debt(debt);
        changed = true;
    }
    Ok(changed)
}

async fn settle_skill_generation(
    flow: &crate::product_data_io::ProductDataIoFlow,
    path: PathBuf,
    committed: EnabledSkillsConfig,
    operation_id: String,
    idempotent: bool,
    durable_committed: bool,
    mut target_receipts: Vec<SkillTargetSettlementReceipt>,
) -> Result<SkillSyncReceipt, SkillMutationError> {
    let mut latest = match read_enabled_skills_config(flow, path.clone()).await {
        Ok(config) => config,
        Err(error) => {
            target_receipts.push(SkillTargetSettlementReceipt {
                target: "enabled-skills.json".to_string(),
                workspace_generation: "global".to_string(),
                specialist_generation: committed.desired_generation,
                status: SkillTargetSettlementStatus::Degraded,
                changed_entries: Vec::new(),
                error: Some(error.to_string()),
            });
            return Ok(degraded_skill_receipt(
                path,
                committed,
                operation_id,
                idempotent,
                durable_committed,
                target_receipts,
            ));
        }
    };
    if latest.desired_generation != committed.desired_generation
        || latest.content_identity != committed.content_identity
    {
        target_receipts.push(SkillTargetSettlementReceipt {
            target: "enabled-skills.json".to_string(),
            workspace_generation: "global".to_string(),
            specialist_generation: committed.desired_generation,
            status: SkillTargetSettlementStatus::Degraded,
            changed_entries: Vec::new(),
            error: Some("a newer desired generation superseded this settlement".to_string()),
        });
        let repair_debt = repair_debt_from_target_receipts(
            committed.desired_generation,
            committed.content_identity.clone(),
            1,
            &target_receipts,
        );
        return Ok(SkillSyncReceipt {
            operation_id,
            committed_file_path: path,
            content_identity: committed.content_identity.clone(),
            desired_generation: committed.desired_generation,
            settled_generation: latest.settled_generation,
            durable_committed,
            idempotent,
            status: SkillSettlementStatus::Degraded,
            target_receipts,
            repair_debt: Some(repair_debt),
        });
    }

    let has_failures = target_receipts
        .iter()
        .any(|receipt| receipt.status == SkillTargetSettlementStatus::Degraded);
    if !has_failures {
        if latest.settled_generation != latest.desired_generation || latest.repair_debt.is_some() {
            latest.settled_generation = latest.desired_generation;
            latest.repair_debt = None;
            if let Err(error) =
                write_enabled_skills_config(flow, path.clone(), latest.clone()).await
            {
                target_receipts.push(SkillTargetSettlementReceipt {
                    target: "enabled-skills.json".to_string(),
                    workspace_generation: "global".to_string(),
                    specialist_generation: committed.desired_generation,
                    status: SkillTargetSettlementStatus::Degraded,
                    changed_entries: Vec::new(),
                    error: Some(format!(
                        "runtime settled but generation CAS failed: {error}"
                    )),
                });
                return Ok(degraded_skill_receipt(
                    path,
                    committed,
                    operation_id,
                    idempotent,
                    durable_committed,
                    target_receipts,
                ));
            }
        }
        return Ok(SkillSyncReceipt {
            operation_id,
            committed_file_path: path,
            content_identity: latest.content_identity,
            desired_generation: latest.desired_generation,
            settled_generation: latest.settled_generation,
            durable_committed,
            idempotent,
            status: SkillSettlementStatus::Settled,
            target_receipts,
            repair_debt: None,
        });
    }

    let attempts = latest
        .repair_debt
        .as_ref()
        .map_or(1, |debt| debt.attempts.saturating_add(1));
    let mut debt = repair_debt_from_target_receipts(
        latest.desired_generation,
        latest.content_identity.clone(),
        attempts,
        &target_receipts,
    );
    preserve_artifact_repair_actions(&mut debt, latest.repair_debt.as_ref());
    latest.set_repair_debt(debt.clone());
    if let Err(error) = write_enabled_skills_config(flow, path.clone(), latest.clone()).await {
        target_receipts.push(SkillTargetSettlementReceipt {
            target: "enabled-skills.json".to_string(),
            workspace_generation: "global".to_string(),
            specialist_generation: committed.desired_generation,
            status: SkillTargetSettlementStatus::Degraded,
            changed_entries: Vec::new(),
            error: Some(format!("repair debt update failed: {error}")),
        });
        debt = repair_debt_from_target_receipts(
            latest.desired_generation,
            latest.content_identity.clone(),
            attempts,
            &target_receipts,
        );
        preserve_artifact_repair_actions(&mut debt, latest.repair_debt.as_ref());
    }
    Ok(SkillSyncReceipt {
        operation_id,
        committed_file_path: path,
        content_identity: latest.content_identity,
        desired_generation: latest.desired_generation,
        settled_generation: latest.settled_generation,
        durable_committed,
        idempotent,
        status: SkillSettlementStatus::Degraded,
        target_receipts,
        repair_debt: Some(debt),
    })
}

fn degraded_skill_receipt(
    committed_file_path: PathBuf,
    committed: EnabledSkillsConfig,
    operation_id: String,
    idempotent: bool,
    durable_committed: bool,
    target_receipts: Vec<SkillTargetSettlementReceipt>,
) -> SkillSyncReceipt {
    let attempts = committed
        .repair_debt
        .as_ref()
        .map_or(1, |debt| debt.attempts.saturating_add(1));
    let repair_debt = repair_debt_from_target_receipts(
        committed.desired_generation,
        committed.content_identity.clone(),
        attempts,
        &target_receipts,
    );
    SkillSyncReceipt {
        operation_id,
        committed_file_path,
        content_identity: committed.content_identity,
        desired_generation: committed.desired_generation,
        settled_generation: committed.settled_generation,
        durable_committed,
        idempotent,
        status: SkillSettlementStatus::Degraded,
        target_receipts,
        repair_debt: Some(repair_debt),
    }
}

fn desired_skill_entries(
    config: &EnabledSkillsConfig,
    skill_root: PathBuf,
) -> Vec<(String, PathBuf)> {
    let hub = SkillsHub::with_root(skill_root);
    let mut selected = hub
        .list()
        .into_iter()
        .filter(|entry| {
            config
                .skills
                .get(&entry.name)
                .is_some_and(|state| state.enabled)
        })
        .map(|entry| {
            let load_root = entry
                .path
                .parent()
                .map(PathBuf::from)
                .unwrap_or_else(|| entry.path.clone());
            (entry.name.clone(), load_root)
        })
        .collect::<Vec<_>>();
    selected.sort_by(|left, right| left.0.cmp(&right.0));
    selected
}

async fn reconcile_target_skills(
    target: &crate::state::ExtensionRuntimeTarget,
    desired: &[(String, PathBuf)],
    skill_root: &std::path::Path,
) -> anyhow::Result<Vec<String>> {
    let mut current = target
        .primary_agent()
        .read(|agent| {
            agent
                .skill_descriptors()
                .iter()
                .filter_map(|descriptor| {
                    let source = descriptor.source.as_deref()?;
                    source
                        .starts_with(USER_SKILL_SOURCE_PREFIX)
                        .then(|| (descriptor.name.clone(), source.to_string()))
                })
                .collect::<Vec<_>>()
        })
        .await;
    current.sort();
    current.dedup();
    for (name, source) in current {
        let load_root = desired
            .iter()
            .find(|(candidate, _)| candidate == &name)
            .map(|(_, root)| root.clone())
            .unwrap_or_else(|| skill_root.to_path_buf());
        target
            .plugin_runtime()
            .disable_application_skill(name, load_root, source)
            .await?;
    }
    let mut loaded = Vec::new();
    for (name, load_root) in desired {
        loaded.extend(
            target
                .plugin_runtime()
                .enable_application_skill(name.clone(), load_root.clone(), user_skill_source(name))
                .await?,
        );
    }
    Ok(loaded)
}

#[cfg(test)]
async fn load_exact_user_skill(
    agent: &crate::agent_handle::AgentHandle,
    requested: &str,
    load_root: PathBuf,
    requested_source: String,
) -> anyhow::Result<Vec<String>> {
    let requested = requested.to_string();
    agent
        .write_async(|agent| {
            Box::pin(async move {
                let loaded = agent.load_skills_from_dir(load_root).await?;
                for name in &loaded {
                    let source = if name == &requested {
                        requested_source.clone()
                    } else {
                        format!("eko:discarded-sibling-skill:{name}")
                    };
                    agent
                        .tag_skills_source(std::slice::from_ref(name), &source)
                        .await;
                    if name != &requested {
                        agent.unregister_skills_by_source(&source).await;
                    }
                }
                Ok::<_, echo_agent::error::ReactError>(
                    loaded
                        .into_iter()
                        .filter(|name| name == &requested)
                        .collect(),
                )
            })
        })
        .await
        .map_err(anyhow::Error::new)
}

fn ensure_hook_load_succeeded(loaded: &HooksLoadResult) -> anyhow::Result<()> {
    if loaded.errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(loaded.errors.join("; ")))
    }
}

fn mcp_transport(entry: &echo_agent::mcp::McpServerEntry) -> &'static str {
    if entry.url.is_some() {
        if entry.transport.as_deref() == Some("sse") {
            "sse"
        } else {
            "http"
        }
    } else if entry.command.is_some() {
        "stdio"
    } else {
        "unknown"
    }
}

fn mcp_health_scope_key(runtime: &ScopedChatRuntime) -> anyhow::Result<String> {
    serde_json::to_string(&(
        runtime.execution_scope().workspace_id(),
        runtime.workspace_host_generation(),
    ))
    .map_err(|error| anyhow::anyhow!("failed to encode MCP health scope: {error}"))
}
