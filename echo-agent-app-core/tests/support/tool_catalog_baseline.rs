// Original EKO factory and policy, inside the existing tool-exposure test seam.
mod harness {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/support/performance_baseline.rs"
    ));
}
use echo_agent::agent::AgentInvocationContext;
use echo_agent::agent::snapshot::AgentRunSnapshot;
use echo_agent::tools::{ToolContext, ToolManager, ToolParameters, ToolResult};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

async fn product_agent(root: &Path) -> harness::HarnessResult<crate::agent_handle::AgentHandle> {
    assert_eq!(crate::data_root::user_data_dir(), root.join("global-data"));
    let store = Arc::new(
        crate::tasks::task_runtime::TaskRuntimeStore::open_for_workspace(
            root.join("tasks"),
            "global",
        )?,
    );
    let config = crate::config::EkoConfig::default();
    let created = crate::infra::create_agent_with_diagnostics(
        &crate::infra::AgentCreateParams {
            working_dir: Some(root.to_path_buf()),
            session_id: Some("baseline-session".to_string()),
            conversation_id: Some("tools-conversation".to_string()),
            task_runtime_store: Some(store.clone()),
            product_data_io: Some(crate::product_data_io::ProductDataIoService::new()),
            ..Default::default()
        },
        &config,
    )
    .await?;
    if created.runtime_model.is_some() {
        return Err("offline catalog profile must not configure a real model".into());
    }
    let catalog = created.model_consumers.subagent_catalog();
    let agent = crate::agent_handle::AgentHandle::new(created.agent);
    // This is the same post-bootstrap registration used by the application,
    // including task_execute and complex-task tools; no synthetic registry.
    crate::tasks::task_runtime::register_task_tools_on_agent(&agent, store, catalog).await;
    Ok(agent)
}

fn request_facts(
    agent: &echo_agent::agent::ReactAgent,
    profile: &str,
) -> harness::HarnessResult<Value> {
    let mut registered = agent.tool_names();
    registered.sort();
    let selected = match profile {
        "chat" => initial_visible_tools(&registered),
        "task" => initial_visible_tools_for_task_run(&registered),
        _ => return Err("unknown invocation profile".into()),
    };
    let snapshot = AgentRunSnapshot::from_agent_with_invocation(
        agent,
        &AgentInvocationContext {
            visible_tools: Some(selected),
            disabled_tools: Some(disabled_tools()),
            ..Default::default()
        },
    );
    let visibility = snapshot
        .tools
        .visibility
        .as_ref()
        .ok_or("invocation visibility missing")?;
    let eligible = registered
        .iter()
        .filter(|name| visibility.is_eligible(name))
        .cloned()
        .collect::<Vec<_>>();
    let definitions = snapshot.tools.tools_for_llm();
    let visible = definitions
        .iter()
        .map(|item| item.function.name.clone())
        .collect::<Vec<_>>();
    let stats = ToolManager::schema_stats_for(&definitions)?;
    // The original source's schema regression budgets are retained verbatim.
    if profile == "chat" || profile == "task" {
        assert!(stats.schema_bytes <= 16_000);
        assert!(stats.estimated_tokens <= 4_000);
    }
    let complete = agent.tool_manager().get_openai_tools();
    let all = ToolManager::schema_stats_for(&complete)?;
    Ok(
        json!({"registered":registered,"eligible":eligible,"visible":visible,
        "request_definitions":definitions,"catalog_sha256":hash(&serde_json::to_vec(&complete)?),
        "registered_schema_bytes":all.schema_bytes,"registered_estimated_tokens":all.estimated_tokens,
        "visible_schema_bytes":stats.schema_bytes,"visible_estimated_tokens":stats.estimated_tokens,
        "input_modalities":snapshot.config.input_modalities,"model_requests":0,
        "profile":profile,"managed_browser_bound":false,"external_mcp_bound":false,
        "subagent_input":"actual factory home/project discovery; exact resulting schema fingerprint frozen"}),
    )
}

// 当前产品只提供真实 chat/task 初始工具面；旧专业 Profile 不再授予工具。
// 所有夹具消费入口共用同一检查，旧组、缺组、乱序都不能混进新测量。
fn checked_profiles(facts: &Value) -> harness::HarnessResult<&[Value]> {
    let profiles = facts
        .get("profiles")
        .and_then(Value::as_array)
        .ok_or("frozen catalog profiles missing")?;
    if profiles.len() != 2
        || profiles
            .iter()
            .zip(["chat", "task"])
            .any(|(profile, name)| profile.get("profile").and_then(Value::as_str) != Some(name))
    {
        return Err("tool profiles changed; regenerate the current-source fixture".into());
    }
    Ok(profiles.as_slice())
}

#[test]
fn current_tool_profiles_reject_legacy_extra_missing_and_reordered_fixtures()
-> harness::HarnessResult {
    let current = json!({"profiles":[{"profile":"chat"},{"profile":"task"}]});
    assert_eq!(checked_profiles(&current)?.len(), 2);
    for invalid in [
        json!({"profiles":[{"profile":"chat"},{"profile":"task"},{"profile":"academic"}]}),
        json!({"profiles":[{"profile":"task"},{"profile":"chat"}]}),
        json!({"profiles":[{"profile":"chat"}]}),
        json!({"profiles":[{"profile":"chat"},{}]}),
        json!({}),
    ] {
        assert!(checked_profiles(&invalid).is_err());
    }
    Ok(())
}

fn payload(bytes: usize) -> String {
    let unit = "中文🧪\n";
    let mut text = unit.repeat(bytes / unit.as_bytes().len());
    text.push_str(&"a".repeat(bytes % unit.as_bytes().len()));
    text
}
fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}
fn tool_context(
    root: &Path,
    config: echo_agent::tools::artifact::ToolOutputArtifactConfig,
    call_id: &str,
) -> ToolContext {
    ToolContext {
        working_dir: Some(root.to_path_buf()),
        conversation_id: Some("tools-conversation".to_string()),
        call_id: Some(call_id.to_string()),
        output_artifacts: Some(config),
        ..Default::default()
    }
}
async fn cat(
    manager: &ToolManager,
    root: &Path,
    config: echo_agent::tools::artifact::ToolOutputArtifactConfig,
    group: &str,
    call: &str,
) -> harness::HarnessResult<ToolResult> {
    let mut parameters = ToolParameters::new();
    parameters.insert(
        "command".to_string(),
        json!(format!(
            "cat {}",
            shell_quote(&root.join("input").join(format!("{group}.txt")))
        )),
    );
    let result = manager
        .execute_tool_with_context("shell", parameters, &tool_context(root, config, call))
        .await?;
    if !result.success {
        return Err(format!(
            "actual EKO shell rejected synthetic read: {:?}",
            result.error
        )
        .into());
    }
    Ok(result)
}

async fn seed_tools(root: &Path) -> harness::HarnessResult {
    harness::empty_seed_root(root)?;
    let agent = product_agent(root).await?;
    let (manager, config, max_tokens) = agent
        .read(|agent| {
            let snapshot = AgentRunSnapshot::from_agent(agent);
            (
                agent.tool_manager().clone(),
                agent.tool_output_artifacts(),
                snapshot.config.max_tool_output_tokens,
            )
        })
        .await;
    let config = config.ok_or("actual EKO artifact config missing")?;
    assert!(config.root_dir.starts_with(root));
    std::fs::create_dir_all(root.join("input"))?;
    std::fs::create_dir_all(root.join("tool-raw"))?;
    let mut cases = Vec::new();
    for (group, bytes) in [
        ("small", 4096),
        ("near", config.threshold_bytes.saturating_sub(1)),
        ("over", config.threshold_bytes.saturating_mul(2)),
    ] {
        let text = payload(bytes);
        assert_eq!(text.as_bytes().len(), bytes);
        std::fs::write(
            root.join("input").join(format!("{group}.txt")),
            text.as_bytes(),
        )?;
        let result = cat(
            &manager,
            root,
            config.clone(),
            group,
            &format!("seed-{group}"),
        )
        .await?;
        let raw = format!("tool-raw/{group}.json");
        harness::write_json(&root.join(&raw), &result)?;
        let expected_artifact = if let Some(reference) = &result.artifact {
            let content = std::fs::read(&reference.path)?;
            assert_eq!(hash(&content), reference.sha256);
            // Original shell artifact preserves the stdout channel header;
            // payload bytes alone are not the complete artifact bytes.
            assert_eq!(content, format!("\n[stdout]\n{text}").as_bytes());
            let path = format!("tool-raw/{group}.complete.txt");
            std::fs::write(root.join(&path), content)?;
            Some(path)
        } else {
            if !result.truncated {
                assert_eq!(result.output, text);
            }
            None
        };
        cases.push(json!({"group":group,"input_bytes":bytes,"input_sha256":hash(text.as_bytes()),"raw_result":raw,"call_id":format!("seed-{group}"),
            "upstream_complete":expected_artifact,"raw_truncated":result.truncated,"producer":"actual registered EKO shell/cat"}));
    }
    // A real read_file refusal is a separate diagnostic, not a fast success.
    std::fs::write(
        root.join("input/oversized-line.txt"),
        "中文🧪".repeat(20_000),
    )?;
    let mut profiles = Vec::new();
    for profile in ["chat", "task"] {
        profiles.push(agent.read(|agent| request_facts(agent, profile)).await?);
    }
    harness::write_json(
        &root.join("fixture.json"),
        &json!({"case":"tool-catalog","fixed_root":root,
        "objects":1,"payloads":cases,"profiles":profiles,"artifact_config":{"root_dir":config.root_dir,"retention":config.retention,
        "threshold_bytes":config.threshold_bytes,"max_age_secs":config.max_age_secs},"max_tool_output_tokens":max_tokens,
        "payload_groups":"small/near/over byte threshold; token budget remains active and may spill earlier"}),
    )
}

fn config_from_facts(
    facts: &Value,
) -> harness::HarnessResult<echo_agent::tools::artifact::ToolOutputArtifactConfig> {
    let value = facts
        .get("artifact_config")
        .ok_or("artifact config facts missing")?;
    Ok(echo_agent::tools::artifact::ToolOutputArtifactConfig::new(
        value
            .get("root_dir")
            .and_then(Value::as_str)
            .ok_or("artifact root missing")?,
        value
            .get("retention")
            .and_then(Value::as_str)
            .ok_or("retention missing")?,
    )
    .threshold_bytes(usize::try_from(
        value
            .get("threshold_bytes")
            .and_then(Value::as_u64)
            .ok_or("threshold missing")?,
    )?)
    .max_age_secs(value.get("max_age_secs").and_then(Value::as_u64)))
}

async fn schema_samples(root: &Path, facts: &Value) -> harness::HarnessResult {
    let frozen = checked_profiles(facts)?;
    let agent = product_agent(root).await?;
    for (position, profile) in ["chat", "task"].into_iter().enumerate() {
        for ordinal in 0_usize..35 {
            let started = Instant::now();
            let current = agent.read(|agent| request_facts(agent, profile)).await?;
            let elapsed = started.elapsed().as_nanos();
            assert_eq!(
                &current,
                frozen.get(position).ok_or("frozen profile missing")?
            );
            let (phase, index) = if ordinal < 5 {
                ("warmup", ordinal)
            } else {
                ("measurement", ordinal - 5)
            };
            harness::emit(
                &format!("tool/actual-EKO-request-schema/{profile}"),
                "hot",
                phase,
                index,
                elapsed,
                "success",
                json!({"ok":true,"profile":current,"timing_includes":"actual visibility, request schemas, stats and catalog hash; equality check after timer","provider_actual_tokens":"not measured"}),
            )?;
        }
    }
    Ok(())
}

async fn raw_samples(root: &Path, facts: &Value) -> harness::HarnessResult {
    let expected_profile = checked_profiles(facts)?.first().ok_or("profile missing")?;
    let agent = product_agent(root).await?;
    let manager = agent.read(|agent| agent.tool_manager().clone()).await;
    let config = config_from_facts(facts)?;
    assert_eq!(
        &agent.read(|agent| request_facts(agent, "chat")).await?,
        expected_profile
    );
    for group in ["small", "near", "over"] {
        for ordinal in 0_usize..35 {
            let started = Instant::now();
            let result = cat(
                &manager,
                root,
                config.clone(),
                group,
                &format!("sample-{group}-{ordinal}"),
            )
            .await?;
            let elapsed = started.elapsed().as_nanos();
            let expected = std::fs::read(root.join("input").join(format!("{group}.txt")))?;
            let complete = if let Some(reference) = &result.artifact {
                let content = std::fs::read(&reference.path)?;
                assert_eq!(hash(&content), reference.sha256);
                let mut complete_bytes = b"\n[stdout]\n".to_vec();
                complete_bytes.extend_from_slice(&expected);
                assert_eq!(content, complete_bytes);
                true
            } else {
                if !result.truncated {
                    assert_eq!(result.output.as_bytes(), expected);
                }
                !result.truncated
            };
            let (phase, index) = if ordinal < 5 {
                ("warmup", ordinal)
            } else {
                ("measurement", ordinal - 5)
            };
            harness::emit(
                &format!("tool/actual-EKO-manager-shell/{group}"),
                "hot",
                phase,
                index,
                elapsed,
                if complete { "success" } else { "preview_only" },
                json!({"ok":true,"raw_success":result.success,"raw_preview_bytes":result.output.as_bytes().len(),"truncated":result.truncated,
                    "artifact":result.artifact,"scope":"raw registered ToolManager + original shell producer; final policy measured separately"}),
            )?;
        }
    }
    let mut params = ToolParameters::new();
    params.insert(
        "path".to_string(),
        json!(root.join("input/oversized-line.txt")),
    );
    params.insert("limit".to_string(), json!(1));
    let started = Instant::now();
    let rejected = manager
        .execute_tool_with_context(
            "read_file",
            params,
            &tool_context(root, config, "read-budget-refusal"),
        )
        .await;
    let elapsed = started.elapsed().as_nanos();
    let reason = match rejected {
        Err(error) => error.to_string(),
        Ok(result) if !result.success => result.error.unwrap_or_default(),
        _ => return Err("read_file budget control unexpectedly succeeded".into()),
    };
    assert!(reason.to_lowercase().contains("budget"));
    harness::emit(
        "tool/read_file-budget-refusal",
        "diagnostic",
        "measurement",
        0,
        elapsed,
        "expected_refusal",
        json!({"ok":true,"reason":reason,"not_complete_output":true}),
    )
}

fn wire_samples(root: &Path, facts: &Value, prepare: bool) -> harness::HarnessResult {
    checked_profiles(facts)?;
    use crate::tool_execution::{ToolExecutionOwner, ToolExecutionRepository};
    let repository = ToolExecutionRepository::open(root.join("wire-projection"))?;
    repository.register_artifact_config(config_from_facts(facts)?);
    for group in ["small", "near", "over"] {
        let result: ToolResult =
            harness::read_json(&root.join(format!("tool-final/{group}.json")))?;
        let final_facts: Value =
            harness::read_json(&root.join(format!("tool-final/{group}.facts.json")))?;
        let complete_origin = final_facts
            .get("complete_origin")
            .and_then(Value::as_bool)
            .ok_or("final origin qualification missing")?;
        for ordinal in 0_usize..if prepare { 1 } else { 35 } {
            let call = format!(
                "{}-{group}-{ordinal}",
                if prepare { "seed-wire" } else { "sample-wire" }
            );
            let command = format!(
                "cat {}",
                shell_quote(&root.join("input").join(format!("{group}.txt")))
            );
            let invocation = echo_agent::agent::ToolInvocation {
                requested_name: "shell".to_string(),
                requested_args: json!({"command":command}),
                name: "shell".to_string(),
                args: json!({"command":command}),
                rewrites: Vec::new(),
            };
            let owner = ToolExecutionOwner::Chat {
                message_id: "tool-message".to_string(),
            };
            let started = Instant::now();
            let projected = repository.project_start(
                "global",
                owner.clone(),
                Some("tools-conversation"),
                None,
                &call,
                &invocation,
            )?;
            repository.project_finish("global", &owner, &call, &result)?;
            let detail = repository.detail_manifest("global", &projected.summary.detail_ref)?;
            let wire = serde_json::to_vec(&detail)?;
            let elapsed = started.elapsed().as_nanos();
            assert_eq!(
                detail.result.as_ref().map(|result| result.success),
                Some(true)
            );
            assert_eq!(
                repository.verified_artifact_reference("global", &detail.id)?,
                result.artifact
            );
            if !prepare {
                let (phase, index) = if ordinal < 5 {
                    ("warmup", ordinal)
                } else {
                    ("measurement", ordinal - 5)
                };
                harness::emit(
                    &format!("tool/EKO-GUI-projection+detail-json/{group}"),
                    "hot",
                    phase,
                    index,
                    elapsed,
                    if complete_origin {
                        "success"
                    } else {
                        "preview_only"
                    },
                    json!({"ok":true,"wire_bytes":wire.len(),"complete_origin":complete_origin,
                        "projection":"original ToolExecutionRepository","network_transport":"not measured","artifact_validation":"after timer"}),
                )?;
            }
        }
    }
    Ok(())
}

#[tokio::test]
#[ignore = "actual EKO catalog/producer: explicit source-qualified seed/measurement window"]
async fn performance_baseline_tool_catalog() -> harness::HarnessResult {
    harness::check_release()?;
    let root = harness::root()?;
    match harness::required("EKO_PERF_ACTION")?.as_str() {
        "seed" => seed_tools(&root).await,
        "validate" => {
            let facts: Value = harness::read_json(&root.join("fixture.json"))?;
            assert_eq!(
                facts.get("fixed_root").and_then(Value::as_str),
                Some(root.to_string_lossy().as_ref())
            );
            let profiles = checked_profiles(&facts)?;
            let agent = product_agent(&root).await?;
            for (expected, profile) in profiles.iter().zip(["chat", "task"]) {
                let current = agent.read(|agent| request_facts(agent, profile)).await?;
                assert_eq!(expected, &current);
            }
            Ok(())
        }
        "prepare-wire" => wire_samples(
            &root,
            &harness::read_json(&root.join("fixture.json"))?,
            true,
        ),
        "sample" => {
            let facts: Value = harness::read_json(&root.join("fixture.json"))?;
            let (phase, index) = harness::phase_index()?;
            assert_eq!(phase, "measurement");
            assert_eq!(index, 0);
            match harness::required("EKO_PERF_MODE")?.as_str() {
                "tool-schema" => schema_samples(&root, &facts).await,
                "tool-output" => raw_samples(&root, &facts).await,
                "tool-wire" => wire_samples(&root, &facts, false),
                _ => Err("unknown tool sample mode".into()),
            }
        }
        _ => Err("unknown tool action".into()),
    }
}
