//! IPC commands for panels not yet migrated from HTTP server.
//!
//! This module provides Tauri IPC commands for functionality that was
//! previously served via Axum HTTP routes. Each section corresponds to
//! a deleted server route module.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use echo_agent_app_core::state::{AuditDecision, PermissionBehavior, PermissionRuleConfig};
use echo_agent_app_core::tasks::task_runtime::compact_context::RUNTIME_RECOVERY_MARKER;
use serde::Serialize;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;
use tauri::Emitter;

// ════════════════════════════════════════════════════════════════════════════
// Permissions
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn get_permissions_mode(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let mode = state.app_state.config.permission_mode.read().await;
    Ok(serde_json::json!({ "mode": mode.clone() }))
}

#[tauri::command]
pub async fn set_permissions_mode(
    state: tauri::State<'_, TauriState>,
    mode: String,
) -> Result<serde_json::Value, IpcError> {
    let normalized = match mode.as_str() {
        "default" => "default",
        "auto-edit" | "autoedit" | "accept-edits" | "auto-approve" => "auto-edit",
        "full-auto" | "fullauto" | "bypass" => "full-auto",
        // Legacy config/UI alias from before interaction mode was separated
        // from approval mode. Auto routing is controlled by InteractionMode;
        // approval mode has only the four user-facing variants below.
        "auto" | "plan" => "default",
        "strict" | "strict-confirm" | "strict-confirmation" => "strict",
        _ => {
            return Err(IpcError::Validation(format!(
                "Invalid permission mode '{}'. Valid: default, auto-edit, full-auto, strict",
                mode
            )));
        }
    };
    let mut mode_lock = state.app_state.config.permission_mode.write().await;
    *mode_lock = normalized.to_string();
    drop(mode_lock);

    state
        .app_state
        .connection
        .primary_agent()
        .write_async(|agent| {
            let mode = normalized.to_string();
            Box::pin(async move {
                agent.set_permission_mode(&mode);
            })
        })
        .await;

    if let Some(pool) = &state.app_state.connection.pool {
        pool.apply_permission_mode(normalized.to_string()).await;
    }

    Ok(serde_json::json!({"success": true, "mode": normalized}))
}

#[tauri::command]
pub async fn list_permission_rules(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let rules = state.app_state.config.permission_rules.read().await;
    let list: Vec<serde_json::Value> = rules
        .iter()
        .map(|r| {
            serde_json::json!({
                "matcher": r.matcher,
                "behavior": r.behavior.to_string(),
                "source": r.source,
            })
        })
        .collect();
    Ok(serde_json::to_value(list).unwrap_or_default())
}

#[tauri::command]
pub async fn add_permission_rule(
    state: tauri::State<'_, TauriState>,
    matcher: String,
    behavior: String,
    source: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    let behavior = match behavior.as_str() {
        "allow" => PermissionBehavior::Allow,
        "deny" => PermissionBehavior::Deny,
        "ask" => PermissionBehavior::Ask,
        _ => {
            return Err(IpcError::Validation(format!(
                "Invalid behavior '{}', valid: allow, deny, ask",
                behavior
            )));
        }
    };

    let rule = PermissionRuleConfig {
        matcher: matcher.clone(),
        behavior,
        source: source.unwrap_or_else(|| "manual".to_string()),
    };

    let mut rules = state.app_state.config.permission_rules.write().await;
    if let Some(existing) = rules.iter_mut().find(|r| r.matcher == matcher) {
        *existing = rule;
    } else {
        rules.push(rule);
    }

    Ok(serde_json::json!({"success": true}))
}

#[tauri::command]
pub async fn remove_permission_rule(
    state: tauri::State<'_, TauriState>,
    matcher: String,
) -> Result<serde_json::Value, IpcError> {
    let mut rules = state.app_state.config.permission_rules.write().await;
    let before = rules.len();
    rules.retain(|r| r.matcher != matcher);
    let removed = before - rules.len();

    if removed == 0 {
        return Err(IpcError::NotFound(format!(
            "Permission rule '{}' not found",
            matcher
        )));
    }

    Ok(serde_json::json!({"success": true, "removed": removed}))
}

// ════════════════════════════════════════════════════════════════════════════
// Audit
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn get_audit_logs(
    state: tauri::State<'_, TauriState>,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<serde_json::Value, IpcError> {
    let offset = offset.unwrap_or(0);
    let limit = limit.unwrap_or(100).min(1000);
    let total = state.app_state.audit_log_count().await;
    let logs = state.app_state.get_audit_logs_paged(offset, limit).await;
    Ok(serde_json::json!({
        "logs": logs,
        "total": total,
        "offset": offset,
        "limit": limit,
    }))
}

#[tauri::command]
pub async fn get_audit_stats(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let logs = state.app_state.get_audit_logs().await;
    let total_entries = logs.len();
    let allow_count = logs
        .iter()
        .filter(|l| l.decision == AuditDecision::Allow)
        .count();
    let deny_count = logs
        .iter()
        .filter(|l| l.decision == AuditDecision::Deny)
        .count();
    let ask_count = logs
        .iter()
        .filter(|l| l.decision == AuditDecision::Ask)
        .count();

    Ok(serde_json::json!({
        "total_entries": total_entries,
        "allow_count": allow_count,
        "deny_count": deny_count,
        "ask_count": ask_count,
    }))
}

#[tauri::command]
pub async fn clear_audit_logs(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let count = state.app_state.clear_audit_entries().await;
    Ok(serde_json::json!({"success": true, "cleared": count}))
}

// ════════════════════════════════════════════════════════════════════════════
// Auto Memory
// ════════════════════════════════════════════════════════════════════════════

async fn current_agent_messages(state: &TauriState) -> Vec<(String, String)> {
    let agent = state.app_state.connection.primary_agent();
    agent
        .read_async(|agent| {
            Box::pin(async move {
                let ctx = agent.context().lock().await;
                ctx.messages()
                    .iter()
                    .map(|m| {
                        (
                            m.role.as_str().to_string(),
                            m.content.as_text().unwrap_or_default().to_string(),
                        )
                    })
                    .collect()
            })
        })
        .await
}

async fn auto_memory_config_status(
    state: &TauriState,
) -> Result<
    (
        echo_agent_app_core::auto_memory::AutoMemoryConfig,
        usize,
        PathBuf,
    ),
    IpcError,
> {
    let config = echo_agent_app_core::auto_memory::AutoMemoryConfig {
        enabled: crate::cli::cmd_impls::all::AUTO_MEMORY_ENABLED
            .load(std::sync::atomic::Ordering::Relaxed),
        ..Default::default()
    };
    let messages = current_agent_messages(state).await;
    let observations = echo_agent_app_core::auto_memory::extract_observations(&messages, &config);
    let root = workspace_project_root(state).await?;
    let memory_path = root.join(".eko").join("project.md");
    Ok((config, observations.len(), memory_path))
}

fn persist_auto_memory_observations(
    memory_path: &Path,
    observations: &[echo_agent_app_core::auto_memory::Observation],
) -> Result<(), IpcError> {
    if observations.is_empty() {
        return Ok(());
    }

    let formatted = echo_agent_app_core::auto_memory::format_observations_for_memory(observations);
    if let Some(parent) = memory_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| IpcError::Internal(format!("Failed to create memory directory: {e}")))?;
    }

    let existing = std::fs::read_to_string(memory_path).unwrap_or_default();
    let new_content = if let Some(marker_pos) = existing.find("## Auto-extracted observations") {
        let before = &existing[..marker_pos];
        format!("{}{}", before.trim_end(), formatted)
    } else if existing.is_empty() {
        formatted
    } else {
        format!("{}\n{}", existing.trim_end(), formatted)
    };

    std::fs::write(memory_path, new_content)
        .map_err(|e| IpcError::Internal(format!("Failed to write project memory: {e}")))?;
    Ok(())
}

async fn write_auto_memory_observations(
    state: &TauriState,
    observations: &[echo_agent_app_core::auto_memory::Observation],
) -> Result<usize, IpcError> {
    if observations.is_empty() {
        return Ok(0);
    }

    let Some(layer_manager) = state
        .app_state
        .connection
        .primary_agent()
        .read(|agent| agent.memory_layer_manager().cloned())
        .await
    else {
        tracing::debug!("auto-memory: primary agent has no shared layer manager");
        return Ok(0);
    };

    echo_agent_app_core::auto_memory::write_observations_to_memory_layer(
        observations,
        &layer_manager,
    )
    .await
    .map_err(IpcError::Internal)
}

#[tauri::command]
pub async fn get_auto_memory_status(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let (config, observation_count, memory_path) = auto_memory_config_status(&state).await?;
    Ok(json!({
        "enabled": config.enabled,
        "observation_count": observation_count,
        "config": config,
        "memory_path": memory_path.display().to_string(),
    }))
}

#[tauri::command]
pub async fn toggle_auto_memory(
    state: tauri::State<'_, TauriState>,
    enabled: bool,
) -> Result<serde_json::Value, IpcError> {
    crate::cli::cmd_impls::all::AUTO_MEMORY_ENABLED
        .store(enabled, std::sync::atomic::Ordering::Relaxed);
    get_auto_memory_status(state).await
}

#[tauri::command]
pub async fn extract_auto_memory(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let config = echo_agent_app_core::auto_memory::AutoMemoryConfig {
        enabled: crate::cli::cmd_impls::all::AUTO_MEMORY_ENABLED
            .load(std::sync::atomic::Ordering::Relaxed),
        ..Default::default()
    };
    if !config.enabled {
        return Ok(json!({
            "success": false,
            "count": 0,
            "observations": [],
            "formatted": "",
            "message": "Auto Memory is disabled",
        }));
    }

    let messages = current_agent_messages(&state).await;
    let observations = echo_agent_app_core::auto_memory::extract_observations(&messages, &config);
    let root = workspace_project_root(&state).await?;
    let memory_path = root.join(".eko").join("project.md");
    persist_auto_memory_observations(&memory_path, &observations)?;
    let typed_written = write_auto_memory_observations(&state, &observations).await?;
    let count = observations.len();
    let formatted = echo_agent_app_core::auto_memory::format_observations_for_memory(&observations);

    Ok(json!({
        "success": true,
        "count": count,
        "typed_written": typed_written,
        "observations": observations,
        "formatted": formatted,
        "memory_path": memory_path.display().to_string(),
    }))
}

#[tauri::command]
pub async fn get_auto_memory_observations(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let config = echo_agent_app_core::auto_memory::AutoMemoryConfig {
        enabled: crate::cli::cmd_impls::all::AUTO_MEMORY_ENABLED
            .load(std::sync::atomic::Ordering::Relaxed),
        ..Default::default()
    };
    let messages = current_agent_messages(&state).await;
    let observations = echo_agent_app_core::auto_memory::extract_observations(&messages, &config);
    let count = observations.len();
    let formatted = echo_agent_app_core::auto_memory::format_observations_for_memory(&observations);
    Ok(json!({
        "observations": observations,
        "count": count,
        "formatted": formatted,
    }))
}

// ════════════════════════════════════════════════════════════════════════════
// Skills
// ════════════════════════════════════════════════════════════════════════════

fn skill_descriptor_json(d: &echo_agent::skills::external::SkillDescriptor) -> serde_json::Value {
    json!({
        "name": d.name,
        "description": d.description,
        "triggers": d.triggers,
        "file": d.location.display().to_string(),
        "loaded": true,
        "source": "runtime",
    })
}

fn hub_skill_json(entry: &echo_agent_app_core::skills_hub::SkillHubEntry) -> serde_json::Value {
    // 下发前端 SkillInfo 所需的全部字段(M4 修复:此前缺 category/is_baseline/
    // is_builtin/upstream_version/source,且 source 被写死成 "hub" 覆盖真实来源)。
    // source: 优先用 entry.source(上游来源 superpowers/anthropic/builtin),
    // 缺失时回退 "hub"(表示由 SkillsHub 发现,与 runtime 相对)。
    let source = entry.source.clone().unwrap_or_else(|| "hub".to_string());
    json!({
        "name": entry.name,
        "description": entry.description,
        "file": entry.path.display().to_string(),
        "loaded": entry.loaded,
        "source": source,
        "category": entry.category,
        "is_baseline": entry.is_baseline,
        "is_builtin": entry.is_builtin,
        "upstream_version": entry.upstream_version,
        "license": entry.license,
        "version": entry.version,
        "author": entry.author,
        "tags": entry.tags,
        "has_sandbox": entry.has_sandbox,
        "depends_on": entry.depends_on,
        // 缺失的系统二进制(scan 时探测 requires-binaries 得出)。
        // 前端 SkillsPanel 据此显示 ⚠️ AlertTriangle + tooltip。
        "missing_dependencies": entry.missing_dependencies,
        // has_updates: 需 upstream registry + git fetch 比对(P6 sync 未实现),
        // 暂返回 false。
        "has_updates": false,
    })
}

/// ~/.echo-agent/enabled-skills.json 路径(B3:enable/disable 同步写此文件,
/// 消除"SkillsHub 内存 / enabled-skills.json / is_baseline 硬编码"三套状态不同步)。
fn enabled_skills_json_path() -> Option<std::path::PathBuf> {
    let home = dirs::home_dir()?;
    Some(home.join(".echo-agent").join("enabled-skills.json"))
}

/// 同步 enabled-skills.json:确保 skill entry 存在(带 category),设置 enabled。
/// 失败仅记日志不阻断(技能已加载进 agent,持久化失败不应让 UI 操作报错)。
fn persist_skill_enabled(name: &str, category: &str, enabled: bool) {
    use echo_agent_app_core::skills_hub::enabled_skills::{EnabledSkillsConfig, SkillEnableEntry};
    let Some(path) = enabled_skills_json_path() else {
        tracing::warn!("无法获取 HOME,enabled-skills.json 未更新");
        return;
    };
    let mut config = match EnabledSkillsConfig::load(&path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "读取 enabled-skills.json 失败,用默认配置");
            EnabledSkillsConfig::default()
        }
    };
    // 确保 entry 存在(set_enabled 只改已有 entry,新启用的技能需先插入)。
    // 仅当 entry 不存在时插入,避免覆盖用户已设的 baseline 标记。
    if !config.skills.contains_key(name) {
        config.skills.insert(
            name.to_string(),
            SkillEnableEntry {
                category: category.to_string(),
                enabled,
                baseline: false, // 新启用默认非 baseline;用户可另行设 baseline
            },
        );
    } else {
        config.set_enabled(name, enabled);
    }
    if let Err(e) = config.save(&path) {
        tracing::warn!(error = %e, "写入 enabled-skills.json 失败");
    }
}

async fn runtime_skill_names(state: &TauriState) -> Vec<String> {
    let agent = state.app_state.connection.primary_agent();
    agent
        .read(|a| {
            a.skill_descriptors()
                .iter()
                .map(|descriptor| descriptor.name.clone())
                .collect::<Vec<_>>()
        })
        .await
}

async fn refresh_skill_hub_loaded_state(state: &TauriState) {
    let names = runtime_skill_names(state).await;
    let mut hub = state.app_state.skills_hub.write().await;
    hub.refresh();
    hub.set_loaded_skills(names);
}

async fn refresh_pool_skill_descriptors(state: &TauriState) {
    let descriptors = state
        .app_state
        .connection
        .primary_agent()
        .read(|a| a.skill_descriptors())
        .await;
    if let Some(pool) = &state.app_state.connection.pool {
        pool.refresh_skill_descriptors(descriptors).await;
    }
}

#[tauri::command]
pub async fn list_skills(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    refresh_skill_hub_loaded_state(&state).await;
    let agent = state.app_state.connection.primary_agent();
    let descriptors = agent.read(|a| a.skill_descriptors()).await;
    let mut skills: Vec<serde_json::Value> = {
        let hub = state.app_state.skills_hub.read().await;
        hub.list().into_iter().map(hub_skill_json).collect()
    };
    for descriptor in descriptors {
        if !skills.iter().any(|skill| {
            skill
                .get("name")
                .and_then(|name| name.as_str())
                .is_some_and(|name| name == descriptor.name)
        }) {
            skills.push(skill_descriptor_json(&descriptor));
        }
    }
    skills.sort_by(|a, b| {
        let a_name = a.get("name").and_then(|v| v.as_str()).unwrap_or_default();
        let b_name = b.get("name").and_then(|v| v.as_str()).unwrap_or_default();
        a_name.cmp(b_name)
    });
    Ok(json!(skills))
}

#[tauri::command]
pub async fn get_skill(
    state: tauri::State<'_, TauriState>,
    name: String,
) -> Result<serde_json::Value, IpcError> {
    let agent = state.app_state.connection.primary_agent();
    let descriptors = agent.read(|a| a.skill_descriptors()).await;
    match descriptors.iter().find(|d| d.name == name) {
        Some(d) => Ok(skill_descriptor_json(d)),
        None => Err(IpcError::NotFound(format!("Skill '{}' not found", name))),
    }
}

#[tauri::command]
pub async fn load_skill(
    state: tauri::State<'_, TauriState>,
    name: String,
) -> Result<serde_json::Value, IpcError> {
    let raw = name.trim();
    if raw.is_empty() {
        return Err(IpcError::Validation("技能目录路径不能为空".into()));
    }

    let path = std::path::PathBuf::from(raw);

    // P1-6: confine the skill directory to an allowed root. Previously any
    // absolute path the frontend supplied was loaded as a skill — a single XSS
    // could point the agent at an attacker-controlled SKILL.md anywhere on disk
    // (e.g. a temp dir) and have it executed with the agent's privileges. We
    // canonicalize the resolved path and require it to live under the current
    // workspace root or the user's home directory (where legitimate skill
    // bundles — project-local `.echo-agent/skills` or the global skills dir —
    // already reside).
    let canonical = path
        .canonicalize()
        .map_err(|_| IpcError::NotFound(format!("技能目录不存在: {}", path.display())))?;
    let allowed_roots = allowed_skill_roots(&state).await;
    if !allowed_roots.iter().any(|root| canonical.starts_with(root)) {
        return Err(IpcError::Validation(format!(
            "技能目录不在允许范围内（须位于当前工作区或用户主目录下）: {}",
            path.display()
        )));
    }

    if !canonical.is_dir() {
        return Err(IpcError::Validation(format!(
            "技能路径不是目录: {}",
            path.display()
        )));
    }

    let agent = state.app_state.connection.primary_agent();
    let loaded = agent
        .write_async(|agent| {
            let path = path.clone();
            Box::pin(async move { agent.load_skills_from_dir(path).await })
        })
        .await
        .map_err(|e| IpcError::Internal(e.to_string()))?;
    let count = loaded.len();
    refresh_skill_hub_loaded_state(&state).await;
    refresh_pool_skill_descriptors(&state).await;

    let skills: Vec<serde_json::Value> = agent
        .read(|a| {
            a.skill_descriptors()
                .iter()
                .map(skill_descriptor_json)
                .collect()
        })
        .await;

    Ok(json!({
        "success": true,
        "loaded": loaded,
        "count": count,
        "skills": skills,
    }))
}

#[tauri::command]
pub async fn enable_skill(
    state: tauri::State<'_, TauriState>,
    name: String,
) -> Result<serde_json::Value, IpcError> {
    let (skill_path, category) = {
        let mut hub = state.app_state.skills_hub.write().await;
        hub.refresh();
        hub.enable_skill(&name).map_err(IpcError::Validation)?;
        let entry = hub
            .get(&name)
            .ok_or_else(|| IpcError::NotFound(format!("Skill '{}' not found", name)))?;
        (entry.path.clone(), entry.category.clone())
    };

    let load_root = skill_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or(skill_path);
    let loaded = state
        .app_state
        .connection
        .primary_agent()
        .write_async(|agent| {
            let path = load_root.clone();
            Box::pin(async move { agent.load_skills_from_dir(path).await })
        })
        .await
        .map_err(|e| IpcError::Internal(e.to_string()))?;
    let count = loaded.len();
    // B3:同步写 enabled-skills.json,消除三套状态不同步。
    persist_skill_enabled(&name, &category, true);
    refresh_skill_hub_loaded_state(&state).await;
    refresh_pool_skill_descriptors(&state).await;

    Ok(json!({
        "success": true,
        "loaded": loaded,
        "count": count,
        "skills": list_skills(state).await?,
    }))
}

#[tauri::command]
pub async fn disable_skill(
    state: tauri::State<'_, TauriState>,
    name: String,
) -> Result<serde_json::Value, IpcError> {
    let category = {
        let mut hub = state.app_state.skills_hub.write().await;
        hub.refresh();
        // 先取 category(disable 前entry 还在),再 disable。
        let category = hub
            .get(&name)
            .map(|e| e.category.clone())
            .unwrap_or_default();
        hub.disable_skill(&name).map_err(IpcError::Validation)?;
        category
    };
    // B3:同步写 enabled-skills.json(标记 enabled=false)。
    // 运行中 agent 已发现的技能不能热卸载,但持久化状态必须更新,
    // 这样下次 bootstrap 不会再加载它,与 UI 显示一致。
    persist_skill_enabled(&name, &category, false);

    Ok(json!({
        "success": true,
        "requires_restart": true,
        "message": "技能已从启用列表移除(enabled-skills.json 已更新);当前运行中的 agent 已发现的技能不能热卸载,新会话或重启后生效。",
        "skills": list_skills(state).await?,
    }))
}

#[tauri::command]
pub async fn upload_skill(
    _state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    Err(IpcError::Validation(
        "Tauri 桌面端不支持浏览器式技能上传；请使用“浏览”选择本地技能目录加载".into(),
    ))
}

// ════════════════════════════════════════════════════════════════════════════
// Workflow
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn list_workflows(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let workflows = state.app_state.history.workflows.read().await;
    let list: Vec<serde_json::Value> = workflows
        .values()
        .map(|w| {
            serde_json::json!({
                "id": w.id,
                "name": w.name,
                "node_count": w.node_count,
                "edge_count": w.edge_count,
                "created_at": w.created_at,
                "updated_at": w.updated_at,
            })
        })
        .collect();
    Ok(serde_json::to_value(list).unwrap_or_default())
}

#[tauri::command]
pub async fn get_workflow(
    state: tauri::State<'_, TauriState>,
    id: String,
) -> Result<serde_json::Value, IpcError> {
    let workflows = state.app_state.history.workflows.read().await;
    workflows
        .get(&id)
        .map(|w| serde_json::to_value(w).unwrap_or_default())
        .ok_or_else(|| IpcError::NotFound(format!("Workflow '{}' not found", id)))
}

#[tauri::command]
pub async fn create_workflow(
    state: tauri::State<'_, TauriState>,
    name: String,
    definition: String,
) -> Result<serde_json::Value, IpcError> {
    use echo_agent_app_core::state::StoredWorkflow;
    let id = uuid::Uuid::new_v4().to_string();
    let now = echo_agent::utils::time::now_local().to_rfc3339();
    let workflow = StoredWorkflow {
        id: id.clone(),
        name,
        definition,
        node_count: 0,
        edge_count: 0,
        created_at: now.clone(),
        updated_at: now,
    };
    let mut workflows = state.app_state.history.workflows.write().await;
    workflows.insert(id.clone(), workflow);
    Ok(serde_json::json!({"success": true, "id": id}))
}

#[tauri::command]
pub async fn delete_workflow(
    state: tauri::State<'_, TauriState>,
    id: String,
) -> Result<serde_json::Value, IpcError> {
    let mut workflows = state.app_state.history.workflows.write().await;
    if workflows.remove(&id).is_some() {
        Ok(serde_json::json!({"success": true}))
    } else {
        Err(IpcError::NotFound(format!("Workflow '{}' not found", id)))
    }
}

#[tauri::command]
pub async fn execute_workflow(
    state: tauri::State<'_, TauriState>,
    id: String,
    input: Option<serde_json::Value>,
) -> Result<serde_json::Value, IpcError> {
    let workflow = {
        let workflows = state.app_state.history.workflows.read().await;
        workflows
            .get(&id)
            .cloned()
            .ok_or_else(|| IpcError::NotFound(format!("Workflow '{}' not found", id)))?
    };

    let definition = workflow.definition.trim();
    let graph = echo_agent::workflow::loader::load_graph_from_yaml_str(definition)
        .or_else(|_| echo_agent::workflow::loader::load_graph_from_json_str(definition))
        .map_err(|e| {
            IpcError::Validation(format!(
                "Workflow '{}' is not a framework Graph workflow definition: {e}",
                workflow.name
            ))
        })?;

    let shared_state = echo_agent::workflow::SharedState::new();
    shared_state
        .set("input", input.unwrap_or_else(|| json!({})))
        .map_err(|e| IpcError::Internal(format!("Failed to initialize workflow state: {e}")))?;
    let result = graph
        .run(shared_state)
        .await
        .map_err(|e| IpcError::Internal(format!("Workflow execution failed: {e}")))?;

    Ok(json!({
        "success": true,
        "workflow_id": workflow.id,
        "path": result.path,
        "steps": result.steps,
        "state": result.state.to_json_value().map_err(|e| IpcError::Internal(format!("Failed to serialize workflow state: {e}")))?,
    }))
}

// ════════════════════════════════════════════════════════════════════════════
// Sandbox
// ════════════════════════════════════════════════════════════════════════════

async fn eko_local_sandbox_available(manager: &echo_agent::sandbox::SandboxManager) -> bool {
    if cfg!(target_os = "windows") {
        false
    } else {
        manager.has_local_sandbox().await
    }
}

fn eko_local_sandbox_backend_label() -> &'static str {
    if cfg!(target_os = "windows") {
        "wsl2-required"
    } else {
        "local"
    }
}

fn eko_local_sandbox_unavailable_message() -> &'static str {
    if cfg!(target_os = "windows") {
        "Native Windows sandbox is not supported by EKO. Run EKO inside WSL2 so it can use Linux bubblewrap (bwrap)."
    } else {
        "Local sandbox backend is unavailable. On macOS this requires sandbox-exec; on Linux/WSL2 this requires bubblewrap (bwrap)."
    }
}

#[tauri::command]
pub async fn get_sandbox_status(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let config = state.app_state.config.sandbox_config.read().await.clone();
    let manager = state
        .app_state
        .connection
        .primary_agent()
        .read(|agent| agent.sandbox_manager().cloned())
        .await;
    let local_available = match manager {
        Some(manager) => eko_local_sandbox_available(&manager).await,
        None => false,
    };
    Ok(serde_json::json!({
        "local_available": local_available,
        "docker_available": false,
        "k8s_available": false,
        "current_backend": eko_local_sandbox_backend_label(),
        "config": config,
    }))
}

#[tauri::command]
pub async fn get_sandbox_config(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let config = state.app_state.config.sandbox_config.read().await;
    Ok(serde_json::to_value(&*config).unwrap_or_default())
}

#[tauri::command]
pub async fn update_sandbox_config(
    state: tauri::State<'_, TauriState>,
    config: serde_json::Value,
) -> Result<serde_json::Value, IpcError> {
    let new_config: echo_agent_app_core::state::SandboxConfigData =
        serde_json::from_value(config).map_err(|e| IpcError::Validation(e.to_string()))?;
    let mut config_lock = state.app_state.config.sandbox_config.write().await;
    *config_lock = new_config;
    Ok(serde_json::json!({"success": true}))
}

#[tauri::command]
pub async fn execute_sandbox(
    state: tauri::State<'_, TauriState>,
    code: String,
    language: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    let lang = language.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let is_shell = matches!(
        lang.map(str::to_ascii_lowercase).as_deref(),
        Some("shell") | Some("sh") | Some("bash") | Some("zsh") | Some("fish")
    );
    let Some(lang) = lang else {
        return Err(IpcError::Validation("language is required".to_string()));
    };
    let lang = lang.to_ascii_lowercase();
    if matches!(lang.as_str(), "zsh" | "fish") {
        return Err(IpcError::Validation(format!(
            "Unsupported sandbox shell language: {lang}"
        )));
    }

    let manager = state
        .app_state
        .connection
        .primary_agent()
        .read(|agent| agent.sandbox_manager().cloned())
        .await;
    let manager = match manager {
        Some(m) => m,
        None => {
            return Err(IpcError::Validation(
                "No sandbox manager configured on the agent; code execution disabled.".to_string(),
            ));
        }
    };
    if !eko_local_sandbox_available(&manager).await {
        return Err(IpcError::Validation(
            eko_local_sandbox_unavailable_message().to_string(),
        ));
    }

    let command = if is_shell {
        echo_agent::sandbox::SandboxCommand::shell(code)
    } else {
        echo_agent::sandbox::SandboxCommand::code(lang, code)
    };
    let config = state.app_state.config.sandbox_config.read().await.clone();
    let memory_bytes = u64::from(config.max_memory_mb)
        .checked_mul(1024)
        .and_then(|v| v.checked_mul(1024))
        .ok_or_else(|| IpcError::Validation("max_memory_mb is too large".to_string()))?;
    let limits = echo_agent::sandbox::ResourceLimits {
        cpu_time_secs: Some(u64::from(config.max_cpu_seconds)),
        memory_bytes: Some(memory_bytes),
        max_output_bytes: Some(1024 * 1024),
        max_processes: Some(64),
        network: config.network_enabled,
        read_only_paths: vec![],
        writable_paths: vec![],
    };
    let result = manager
        .execute_with_limits(command, limits)
        .await
        .map_err(|e| IpcError::Internal(format!("Sandbox execution failed: {e}")))?;
    Ok(json!({
        "success": result.success(),
        "exit_code": result.exit_code,
        "stdout": result.stdout,
        "stderr": result.stderr,
        "duration_ms": result.duration.as_millis(),
        "sandbox_type": result.sandbox_type,
        "timed_out": result.timed_out,
    }))
}

// ════════════════════════════════════════════════════════════════════════════
// Compress
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn compress_context(
    app: tauri::AppHandle,
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let (messages_before, messages_after, tokens_before, tokens_after) = state
        .app_state
        .connection
        .agent
        .write_async(|agent| {
            Box::pin(async move {
                match agent.force_compress_context().await {
                    Ok((stats, _checkpoint)) => (
                        stats.before_count,
                        stats.after_count,
                        stats.before_tokens,
                        stats.after_tokens,
                    ),
                    Err(e) => {
                        tracing::warn!(error = %e, "Manual context compression failed");
                        (0, 0, 0, 0)
                    }
                }
            })
        })
        .await;

    if messages_before == 0 && messages_after == 0 {
        Ok(serde_json::json!({
            "success": true,
            "message": "No messages to compress",
            "messages_before": 0,
            "messages_after": 0,
            "tokens_saved": 0,
        }))
    } else {
        // 手动压缩不走 run_compact，不会发 AgentEvent::ContextCompressed；
        // 显式 emit，与 auto-compact 对齐（前端 Snapshot 置空、Accumulator 保留）。
        let payload = serde_json::json!({
            "type": "context_compressed",
            "before_count": messages_before,
            "after_count": messages_after,
            "before_tokens": tokens_before,
            "after_tokens": tokens_after,
        });
        if let Err(e) = app.emit("chat://event", payload) {
            tracing::warn!(error = %e, "failed to emit context_compressed after manual compress");
        }
        Ok(serde_json::json!({
            "success": true,
            "message": format!("Compressed: {messages_before} → {messages_after} messages"),
            "messages_before": messages_before,
            "messages_after": messages_after,
            "tokens_saved": tokens_before.saturating_sub(tokens_after),
        }))
    }
}

#[tauri::command]
pub async fn get_compression_stats(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let (
        message_count,
        current_tokens,
        token_limit,
        compression_ratio,
        protected_message_count,
        protected_tokens,
        runtime_recovery_active,
    ) = state
        .app_state
        .connection
        .agent
        .read_async(|agent| {
            Box::pin(async move {
                let token_limit = agent.config().get_token_limit();
                let ctx = agent.context().lock().await;
                let ratio = ctx.compression_metrics().compression_ratio();
                (
                    ctx.messages().len(),
                    ctx.token_estimate(),
                    token_limit,
                    ratio,
                    ctx.protected_message_count(),
                    ctx.protected_token_estimate(),
                    ctx.has_projection(RUNTIME_RECOVERY_MARKER),
                )
            })
        })
        .await;
    let needs_compression = token_limit > 0 && current_tokens > token_limit * 3 / 4;

    Ok(serde_json::json!({
        "message_count": message_count,
        "current_tokens": current_tokens,
        "token_limit": token_limit,
        "compression_ratio": compression_ratio,
        "protected_message_count": protected_message_count,
        "protected_tokens": protected_tokens,
        "runtime_recovery_active": runtime_recovery_active,
        "needs_compression": needs_compression,
    }))
}

// ════════════════════════════════════════════════════════════════════════════
// Extract
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn extract_data(
    state: tauri::State<'_, TauriState>,
    input: String,
    schema: serde_json::Value,
    schema_name: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    if !schema.is_object() {
        return Err(IpcError::Validation("Schema must be a JSON object".into()));
    }
    let name = schema_name.unwrap_or_else(|| "extraction".to_string());
    let format = echo_agent::llm::ResponseFormat::json_schema(name, schema);
    let value = state
        .app_state
        .connection
        .primary_agent()
        .read_async(|agent| Box::pin(async move { agent.extract_json(&input, format).await }))
        .await
        .map_err(|e| IpcError::Internal(format!("Extraction failed: {e}")))?;
    Ok(json!({
        "success": true,
        "data": value,
    }))
}

#[tauri::command]
pub async fn validate_schema(
    _state: tauri::State<'_, TauriState>,
    schema: serde_json::Value,
) -> Result<serde_json::Value, IpcError> {
    let mut errors = Vec::new();
    if !schema.is_object() {
        errors.push("Schema must be a JSON object".to_string());
    }
    Ok(json!({
        "valid": errors.is_empty(),
        "errors": errors,
    }))
}

#[tauri::command]
pub async fn get_extract_examples(
    _state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    Ok(json!([
        {
            "name": "person",
            "input": "Zhang San, 28 years old, works as an engineer.",
            "schema": {
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "age": {"type": "integer"},
                    "job": {"type": "string"}
                },
                "required": ["name", "age"],
                "additionalProperties": false
            }
        }
    ]))
}

// ════════════════════════════════════════════════════════════════════════════
// Context
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn get_context_stats(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let (message_count, token_count) = state
        .app_state
        .connection
        .agent
        .read_async(|agent| Box::pin(async move { agent.context_stats().await }))
        .await;
    Ok(serde_json::json!({
        "message_count": message_count,
        "token_count": token_count,
    }))
}

// ════════════════════════════════════════════════════════════════════════════
// History
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn get_history(
    _state: tauri::State<'_, TauriState>,
    _limit: Option<usize>,
) -> Result<serde_json::Value, IpcError> {
    Err(IpcError::NotImplemented("对话历史查询功能尚未实现".into()))
}

#[tauri::command]
pub async fn export_history_markdown(
    _state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    Err(IpcError::NotImplemented(
        "对话历史 Markdown 导出功能尚未实现".into(),
    ))
}

#[tauri::command]
pub async fn export_history_json(
    _state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    Err(IpcError::NotImplemented(
        "对话历史 JSON 导出功能尚未实现".into(),
    ))
}

// ════════════════════════════════════════════════════════════════════════════
// Trace Events
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn list_trace_sessions(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let sessions = state.app_state.trace.collector.list_sessions().await;
    Ok(json!(sessions))
}

#[tauri::command]
pub async fn get_trace_events(
    state: tauri::State<'_, TauriState>,
    session_id: String,
) -> Result<serde_json::Value, IpcError> {
    let events = state
        .app_state
        .trace
        .collector
        .get_events(&session_id)
        .await;
    Ok(json!(events))
}

#[tauri::command]
pub async fn get_trace_summary(
    state: tauri::State<'_, TauriState>,
    session_id: String,
) -> Result<serde_json::Value, IpcError> {
    if let Some(summary) = state
        .app_state
        .trace
        .collector
        .get_summary(&session_id)
        .await
    {
        Ok(json!(summary))
    } else {
        Ok(json!({
            "session_id": session_id,
            "total_duration_ms": 0,
            "llm_calls": 0,
            "total_input_tokens": 0,
            "total_output_tokens": 0,
            "tool_calls": 0,
            "tool_success_rate": 1.0,
            "agent_steps": 0,
            "events": [],
        }))
    }
}

#[tauri::command]
pub async fn clear_trace_session(
    state: tauri::State<'_, TauriState>,
    session_id: String,
) -> Result<serde_json::Value, IpcError> {
    state
        .app_state
        .trace
        .collector
        .clear_session(&session_id)
        .await;
    Ok(json!({"success": true, "cleared": session_id}))
}

// ════════════════════════════════════════════════════════════════════════════
// Papers
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn list_papers(
    _state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    Err(IpcError::NotImplemented("论文列表功能尚未实现".into()))
}

#[tauri::command]
pub async fn get_paper(
    _state: tauri::State<'_, TauriState>,
    _id: String,
) -> Result<serde_json::Value, IpcError> {
    Err(IpcError::NotImplemented("论文查询功能尚未实现".into()))
}

#[tauri::command]
pub async fn create_paper(
    _state: tauri::State<'_, TauriState>,
    _title: String,
    _authors: Option<Vec<String>>,
) -> Result<serde_json::Value, IpcError> {
    Err(IpcError::NotImplemented("论文创建功能尚未实现".into()))
}

#[tauri::command]
pub async fn delete_paper(
    _state: tauri::State<'_, TauriState>,
    _id: String,
) -> Result<serde_json::Value, IpcError> {
    Err(IpcError::NotImplemented("论文删除功能尚未实现".into()))
}

#[tauri::command]
pub async fn update_paper_notes(
    _state: tauri::State<'_, TauriState>,
    _id: String,
    _notes: String,
) -> Result<serde_json::Value, IpcError> {
    Err(IpcError::NotImplemented("论文笔记更新功能尚未实现".into()))
}

#[tauri::command]
pub async fn add_paper_tags(
    _state: tauri::State<'_, TauriState>,
    _id: String,
    _tags: Vec<String>,
) -> Result<serde_json::Value, IpcError> {
    Err(IpcError::NotImplemented("论文标签添加功能尚未实现".into()))
}

// ════════════════════════════════════════════════════════════════════════════
// Evolution
// ════════════════════════════════════════════════════════════════════════════

fn curator_status_json(status: echo_agent::improve::CuratorStatus) -> serde_json::Value {
    json!({
        "total": status.total,
        "candidate": status.candidate,
        "draft": status.draft,
        "active": status.active,
        "stale": status.stale,
        "deprecated": status.deprecated,
        "archived": status.archived,
        "pinned": status.pinned,
        "last_run_at": status.last_run_at,
    })
}

#[tauri::command]
pub async fn get_trajectories(
    _state: tauri::State<'_, TauriState>,
    date: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    let saver = echo_agent::improve::TrajectorySaver::default_dir()
        .map_err(|e| IpcError::Internal(e.to_string()))?;
    let trajectories = saver
        .list(date.as_deref())
        .await
        .map_err(|e| IpcError::Internal(e.to_string()))?;
    Ok(json!({
        "count": trajectories.len(),
        "trajectories": trajectories,
    }))
}

#[tauri::command]
pub async fn get_trajectory_stats(
    _state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let saver = echo_agent::improve::TrajectorySaver::default_dir()
        .map_err(|e| IpcError::Internal(e.to_string()))?;
    let stats = saver
        .stats()
        .await
        .map_err(|e| IpcError::Internal(e.to_string()))?;
    Ok(json!({ "stats": stats }))
}

#[tauri::command]
pub async fn review_trajectory(
    state: tauri::State<'_, TauriState>,
    trajectory_id: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    let agent = state.app_state.connection.primary_agent();
    let (llm_client, memory_store, run_store) = agent
        .read(|a| {
            (
                a.llm_client().cloned(),
                a.store().cloned(),
                a.run_store().cloned(),
            )
        })
        .await;

    let llm_client = llm_client
        .ok_or_else(|| IpcError::Internal("No LLM client available for review".into()))?;
    let run_store =
        run_store.ok_or_else(|| IpcError::Internal("No run store configured".into()))?;

    let run_id = match trajectory_id {
        Some(id) if !id.trim().is_empty() => id,
        _ => {
            let runs = run_store
                .list_all(1)
                .await
                .map_err(|e| IpcError::Internal(e.to_string()))?;
            runs.first()
                .map(|r| r.run_id.clone())
                .ok_or_else(|| IpcError::NotFound("No runs to review".into()))?
        }
    };

    let reviewer = echo_agent::evolution::BackgroundReviewer::new(
        echo_agent::evolution::BackgroundReviewConfig::default(),
        llm_client,
        memory_store.clone(),
        Some(run_store),
    );
    let reviewer = if let Some(store) = memory_store {
        let review_integration = state
            .app_state
            .review_integration
            .clone()
            .unwrap_or_else(|| {
                std::sync::Arc::new(echo_agent_app_core::evolution::ReviewIntegration::new(
                    echo_agent::evolution::ReviewConfig::default(),
                    echo_agent_app_core::evolution::discover_echo_agent_dir(),
                    store,
                ))
            });
        reviewer.with_layer_manager(std::sync::Arc::new(
            review_integration
                .create_layer_manager()
                .with_write_observer(review_integration.clone()),
        ))
    } else {
        reviewer
    };
    let handle = reviewer
        .review_by_run_id(&run_id)
        .map_err(|e| IpcError::Internal(e.to_string()))?;
    let outcome = handle
        .await
        .map_err(|e| IpcError::Internal(format!("Background review task failed to join: {e}")))?;
    Ok(json!({
        "success": outcome.error.is_none(),
        "run_id": outcome.run_id,
        "actions": outcome.actions,
        "nothing_to_save": outcome.nothing_to_save,
        "error": outcome.error,
    }))
}

#[tauri::command]
pub async fn curator_action(
    _state: tauri::State<'_, TauriState>,
    action: String,
    skill_name: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    let curator =
        echo_agent::improve::Curator::default_path(echo_agent::improve::CuratorConfig::default());

    match action.as_str() {
        "status" => Ok(json!({
            "success": true,
            "status": curator_status_json(curator.status()),
        })),
        "run" => {
            let transitions = curator
                .apply_transitions()
                .map_err(|e| IpcError::Internal(e.to_string()))?;
            let transition_values: Vec<serde_json::Value> = transitions
                .iter()
                .map(|(skill, from, to)| {
                    json!({
                        "skill": skill,
                        "from": format!("{from:?}"),
                        "to": format!("{to:?}"),
                    })
                })
                .collect();
            Ok(json!({
                "success": true,
                "transitions": transition_values,
                "count": transitions.len(),
                "status": curator_status_json(curator.status()),
            }))
        }
        "pin" => {
            let name = skill_name
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| IpcError::Validation("skill_name is required for pin".into()))?;
            curator
                .pin_skill(&name)
                .map_err(|e| IpcError::Internal(e.to_string()))?;
            Ok(
                json!({"success": true, "pinned": name, "status": curator_status_json(curator.status())}),
            )
        }
        "unpin" => {
            let name = skill_name
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| IpcError::Validation("skill_name is required for unpin".into()))?;
            curator
                .unpin_skill(&name)
                .map_err(|e| IpcError::Internal(e.to_string()))?;
            Ok(
                json!({"success": true, "unpinned": name, "status": curator_status_json(curator.status())}),
            )
        }
        _ => Err(IpcError::Validation(format!(
            "Unknown curator action '{action}'. Valid: status, run, pin, unpin"
        ))),
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Evolution Dashboard
// ════════════════════════════════════════════════════════════════════════════

/// 返回自进化系统的概览指标:分层记忆统计(按 type/status)、技能健康度、
/// 最近变更活动。复用 app-core 的 `Dashboard`(扫 WARM_NAMESPACE,与写入/召回
/// 同源),让用户第一次能"看见"系统学到了什么。
///
/// 这是阶段 1(Dashboard 接线)的后端入口;前端 `EvolutionPanel` 据此渲染
/// "进化概览"段。构造 pattern 复刻自 `cmd_evolution_dashboard`
/// (src/cli/cmd_impls/evolution.rs:1373)。
#[tauri::command]
pub async fn get_evolution_dashboard(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let agent = state.app_state.connection.primary_agent();
    let store = agent
        .read(|a| a.store().cloned())
        .await
        .ok_or_else(|| IpcError::Internal("No memory store configured".into()))?;

    let change_log = echo_agent::evolution::JsonlChangeLog::new(
        echo_agent_app_core::evolution::discover_echo_agent_dir()
            .join("evolution")
            .join("changelog.jsonl"),
    );

    let dashboard = echo_agent_app_core::evolution::Dashboard::new(store, change_log);
    let metrics = dashboard.generate_metrics().await;

    Ok(json!({ "metrics": metrics }))
}

/// 返回「记忆 → AGENTS.md 规则」的晋升候选列表(高置信 + 满足 age/type 门槛)。
///
/// 这是 review gate 的「展示」侧:用户在 EvolutionPanel 看到候选(置信度/
/// 类型/规则文本),决定是否采纳。采纳走 `promote_rule` —— 只有用户点按钮
/// 才会写 AGENTS.md,agent 不会静默改规则。复刻 CLI 的两步式
/// (src/cli/cmd_impls/evolution.rs:1297 scan)。
#[tauri::command]
pub async fn scan_rule_proposals(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let agent = state.app_state.connection.primary_agent();
    let store = agent
        .read(|a| a.store().cloned())
        .await
        .ok_or_else(|| IpcError::Internal("No memory store configured".into()))?;

    let promoter = echo_agent_app_core::evolution::RulePromoter::new(store);
    let proposals = promoter.scan_for_proposals().await;

    Ok(json!({ "proposals": proposals, "count": proposals.len() }))
}

/// 采纳一条规则候选:按 memory_key 找到候选,写 AGENTS.md `## Rules` 段,
/// 并在原记忆打 `<!-- PROMOTED_TO_RULE -->` 标记防重复。
///
/// review gate 的「执行」侧 —— 由用户在前端点「采纳」触发,不经此路径
/// 不会改 AGENTS.md。复刻 CLI (evolution.rs:1338 promote_rule)。
#[tauri::command]
pub async fn promote_rule(
    state: tauri::State<'_, TauriState>,
    memory_key: String,
) -> Result<serde_json::Value, IpcError> {
    let agent = state.app_state.connection.primary_agent();
    let store = agent
        .read(|a| a.store().cloned())
        .await
        .ok_or_else(|| IpcError::Internal("No memory store configured".into()))?;

    let promoter = echo_agent_app_core::evolution::RulePromoter::new(store);

    // 找到对应候选(scan 已过置信度/age/type 门槛)
    let proposal = promoter
        .scan_for_proposals()
        .await
        .into_iter()
        .find(|p| p.memory_key == memory_key)
        .ok_or_else(|| {
            IpcError::NotFound(format!(
                "Memory '{memory_key}' not found or does not meet promotion criteria"
            ))
        })?;

    let change_log = echo_agent::evolution::JsonlChangeLog::new(
        echo_agent_app_core::evolution::discover_echo_agent_dir()
            .join("evolution")
            .join("changelog.jsonl"),
    );

    promoter
        .promote_rule(&proposal, &change_log)
        .await
        .map_err(|e| IpcError::Internal(format!("Failed to promote rule: {e}")))?;

    // Fire RulePromoted hook so registered hooks are notified.
    echo_agent_app_core::evolution::fire_evolution_hook(
        &agent,
        echo_core::hooks::HookEvent::RulePromoted,
        &memory_key,
    )
    .await;

    Ok(json!({
        "success": true,
        "memory_key": memory_key,
        "rule_text": proposal.rule_text,
    }))
}

// ════════════════════════════════════════════════════════════════════════════
// Skill Candidates(技能自动创建闭环的可见可控出口)
// ════════════════════════════════════════════════════════════════════════════

/// 列出已检测到的技能候选(来自 SkillCandidateDetector,存在 CANDIDATE_NAMESPACE)。
///
/// 闭环已存在:review 时 SkillCandidateDetector 扫描 WorkflowPattern/
/// DebuggingLesson 记忆,超阈值(≥3)的进 CANDIDATE_NAMESPACE。本 command 只是把
/// 它们暴露给前端,让用户看见「系统发现了哪些可复用模式」。
#[tauri::command]
pub async fn scan_skill_candidates(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let agent = state.app_state.connection.primary_agent();
    let store = agent
        .read(|a| a.store().cloned())
        .await
        .ok_or_else(|| IpcError::Internal("No memory store configured".into()))?;

    let typed = echo_agent::memory::TypedMemoryStore::new(store);
    let entries = typed
        .list_typed(
            echo_agent::evolution::candidate::CANDIDATE_NAMESPACE,
            &echo_agent::memory::MemoryFilter::new(),
        )
        .await
        .map_err(|e| IpcError::Internal(format!("Failed to list candidates: {e}")))?;

    let echo_agent_dir = echo_agent_app_core::evolution::discover_echo_agent_dir();
    let candidates: Vec<serde_json::Value> = entries
        .into_iter()
        .map(|e| {
            let name = e.key.clone();
            // 草稿是否已生成(_drafts/<name>/SKILL.md)
            let draft_path = echo_agent_dir
                .join("skills")
                .join("_drafts")
                .join(&name)
                .join("SKILL.md");
            let activated = echo_agent_dir
                .join("skills")
                .join(&name)
                .join("SKILL.md")
                .exists();
            // 尝试解析 content 为 SkillCandidate(JSON),取 description/sample_count;
            // 解析失败则 fallback 到截断的原始 content(UTF-8 安全:chars().take)。
            let (description, sample_count, source_type) =
                serde_json::from_str::<echo_agent::evolution::SkillCandidate>(&e.content)
                    .ok()
                    .map(|c| {
                        (
                            c.description,
                            c.sample_count,
                            format!("{:?}", c.source_type),
                        )
                    })
                    .unwrap_or_else(|| {
                        (
                            e.content.chars().take(80).collect::<String>(),
                            0,
                            "unknown".to_string(),
                        )
                    });
            json!({
                "name": name,
                "description": description,
                "sample_count": sample_count,
                "source_type": source_type,
                "has_draft": draft_path.exists(),
                "activated": activated,
                "confidence": e.meta.confidence,
            })
        })
        .collect();

    Ok(json!({ "candidates": candidates, "count": candidates.len() }))
}

/// 为一个候选生成草稿 SKILL.md(review gate:用户触发才生成)。
///
/// ReviewConfig.auto_generate_drafts 默认 false —— 草稿不会自动生成。本 command
/// 让用户显式触发:点「生成草稿」→ SkillDraftGenerator 写 _drafts/<name>/SKILL.md
/// + curator 状态 Candidate→Draft。
#[tauri::command]
pub async fn generate_skill_draft(
    state: tauri::State<'_, TauriState>,
    name: String,
) -> Result<serde_json::Value, IpcError> {
    let agent = state.app_state.connection.primary_agent();
    let store = agent
        .read(|a| a.store().cloned())
        .await
        .ok_or_else(|| IpcError::Internal("No memory store configured".into()))?;

    let echo_agent_dir = echo_agent_app_core::evolution::discover_echo_agent_dir();
    let change_log = echo_agent::evolution::JsonlChangeLog::new(
        echo_agent_dir.join("evolution").join("changelog.jsonl"),
    );
    let typed = echo_agent::memory::TypedMemoryStore::new(store);

    let generator = echo_agent::evolution::SkillDraftGenerator::new(echo_agent_dir, &change_log);
    let result = generator
        .generate(&name, &typed)
        .await
        .map_err(|e| IpcError::Internal(format!("Failed to generate draft: {e}")))?;

    echo_agent_app_core::evolution::fire_evolution_hook(
        &agent,
        echo_core::hooks::HookEvent::SkillLifecycleTransition,
        &result.name,
    )
    .await;

    Ok(json!({
        "success": true,
        "name": result.name,
        "path": result.skill_md_path.to_string_lossy(),
    }))
}

/// 激活一个草稿技能:复制 _drafts/<name>/ → skills/<name>/,并 curator
/// 状态 Draft→Active。激活后技能进入 DiscoveryScope::Project 扫描路径,下次
/// agent 启动/技能重载时自动加载。review gate:用户显式触发才激活。
#[tauri::command]
pub async fn activate_skill_draft(
    state: tauri::State<'_, TauriState>,
    name: String,
) -> Result<serde_json::Value, IpcError> {
    let agent = state.app_state.connection.primary_agent();
    let echo_agent_dir = echo_agent_app_core::evolution::discover_echo_agent_dir();
    let draft_dir = echo_agent_dir.join("skills").join("_drafts").join(&name);
    let target_dir = echo_agent_dir.join("skills").join(&name);

    if !draft_dir.join("SKILL.md").exists() {
        return Err(IpcError::NotFound(format!(
            "Draft for '{name}' not found at {}",
            draft_dir.display()
        )));
    }

    // 复制 _drafts/<name>/SKILL.md → skills/<name>/SKILL.md
    // (草稿目录目前只含 SKILL.md;若将来有附属资源,扩成递归复制即可)
    std::fs::create_dir_all(&target_dir)
        .map_err(|e| IpcError::Internal(format!("Failed to create target dir: {e}")))?;
    std::fs::copy(draft_dir.join("SKILL.md"), target_dir.join("SKILL.md"))
        .map_err(|e| IpcError::Internal(format!("Failed to copy draft SKILL.md: {e}")))?;

    // curator 状态 Draft→Active(feature gate:improve)
    #[cfg(feature = "improve")]
    {
        let curator = echo_agent::improve::Curator::default_path(
            echo_agent::improve::CuratorConfig::default(),
        );
        if let Err(e) = curator.promote_to_active(&name) {
            tracing::warn!(name = %name, error = %e, "Failed to promote '{}' to active", name);
        }
    }

    echo_agent_app_core::evolution::fire_evolution_hook(
        &agent,
        echo_core::hooks::HookEvent::SkillLifecycleTransition,
        &name,
    )
    .await;

    Ok(json!({
        "success": true,
        "name": name,
        "path": target_dir.to_string_lossy(),
    }))
}

// ════════════════════════════════════════════════════════════════════════════
// Worktree
// ════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize)]
struct WorktreeInfo {
    path: String,
    branch: String,
    managed: bool,
    head: String,
}

async fn workspace_project_root(state: &TauriState) -> Result<PathBuf, IpcError> {
    if let Some(ws) = state.app_state.current_workspace().await {
        Ok(ws.project_root.unwrap_or(ws.root))
    } else {
        std::env::current_dir()
            .map_err(|e| IpcError::Internal(format!("Failed to resolve current directory: {e}")))
    }
}

/// Roots under which a skill directory may legitimately reside (P1-6).
///
/// Returns the current workspace root (best-effort; skipped if unavailable)
/// plus the user's home directory (where the global skills bundle lives).
/// `load_skill` canonicalizes the requested path and requires it to be under
/// one of these, so a compromised page cannot point the agent at an arbitrary
/// on-disk SKILL.md.
pub(crate) async fn allowed_skill_roots(state: &TauriState) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(ws_root) = workspace_project_root(state).await
        && let Ok(c) = ws_root.canonicalize()
    {
        roots.push(c);
    }
    if let Some(home) = std::env::var("HOME").ok().map(PathBuf::from)
        && let Ok(c) = home.canonicalize()
    {
        roots.push(c);
    }
    roots
}

fn run_git(repo: &Path, args: &[&str]) -> Result<String, IpcError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| IpcError::Internal(format!("Failed to run git: {e}")))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(IpcError::Validation(if stderr.is_empty() {
            format!("git {:?} failed", args)
        } else {
            stderr
        }))
    }
}

fn git_repo_root(start: &Path) -> Result<PathBuf, IpcError> {
    let root = run_git(start, &["rev-parse", "--show-toplevel"])?;
    Ok(PathBuf::from(root))
}

fn parse_worktree_list(output: &str, repo_root: &Path) -> Vec<WorktreeInfo> {
    let canonical_repo = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.to_path_buf());
    let mut items = Vec::new();
    let mut current_path: Option<PathBuf> = None;
    let mut current_head = String::new();
    let mut current_branch = String::new();

    let flush = |items: &mut Vec<WorktreeInfo>,
                 path: &mut Option<PathBuf>,
                 head: &mut String,
                 branch: &mut String| {
        let Some(path_buf) = path.take() else {
            return;
        };
        let canonical = path_buf.canonicalize().unwrap_or_else(|_| path_buf.clone());
        let display_branch = if branch.is_empty() {
            "(detached)".to_string()
        } else {
            branch
                .strip_prefix("refs/heads/")
                .unwrap_or(branch.as_str())
                .to_string()
        };
        items.push(WorktreeInfo {
            path: path_buf.to_string_lossy().to_string(),
            branch: display_branch,
            managed: canonical != canonical_repo,
            head: head.chars().take(12).collect(),
        });
        head.clear();
        branch.clear();
    };

    for line in output.lines() {
        if line.trim().is_empty() {
            flush(
                &mut items,
                &mut current_path,
                &mut current_head,
                &mut current_branch,
            );
            continue;
        }
        if let Some(path) = line.strip_prefix("worktree ") {
            flush(
                &mut items,
                &mut current_path,
                &mut current_head,
                &mut current_branch,
            );
            current_path = Some(PathBuf::from(path));
        } else if let Some(head) = line.strip_prefix("HEAD ") {
            current_head = head.to_string();
        } else if let Some(branch) = line.strip_prefix("branch ") {
            current_branch = branch.to_string();
        }
    }
    flush(
        &mut items,
        &mut current_path,
        &mut current_head,
        &mut current_branch,
    );
    items
}

fn validate_branch_name(repo: &Path, branch: &str) -> Result<(), IpcError> {
    if branch.trim().is_empty() {
        return Err(IpcError::Validation("Branch name cannot be empty".into()));
    }
    if branch.starts_with('-') || branch.chars().any(char::is_whitespace) {
        return Err(IpcError::Validation(
            "Branch name cannot start with '-' or contain whitespace".into(),
        ));
    }
    run_git(repo, &["check-ref-format", "--branch", branch]).map(|_| ())
}

fn default_worktree_path(repo_root: &Path, branch: &str) -> Result<PathBuf, IpcError> {
    let parent = repo_root
        .parent()
        .ok_or_else(|| IpcError::Validation("Repository root has no parent directory".into()))?;
    let repo_name = repo_root
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "worktree".to_string());
    let safe_branch: String = branch
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    Ok(parent.join(format!("{repo_name}-{safe_branch}")))
}

fn validate_worktree_target(repo_root: &Path, target: &Path) -> Result<(), IpcError> {
    let repo_parent = repo_root
        .parent()
        .ok_or_else(|| IpcError::Validation("Repository root has no parent directory".into()))?
        .canonicalize()
        .map_err(|e| IpcError::Validation(format!("Cannot resolve repository parent: {e}")))?;
    let target_parent = target
        .parent()
        .ok_or_else(|| IpcError::Validation("Worktree path has no parent directory".into()))?;
    let canonical_parent = target_parent
        .canonicalize()
        .map_err(|e| IpcError::Validation(format!("Cannot resolve worktree parent: {e}")))?;
    if !canonical_parent.starts_with(&repo_parent) {
        return Err(IpcError::Validation(format!(
            "Worktree path must stay under repository parent: {}",
            repo_parent.display()
        )));
    }
    if target.exists() {
        return Err(IpcError::Validation(format!(
            "Worktree path already exists: {}",
            target.display()
        )));
    }
    Ok(())
}

#[tauri::command]
pub async fn list_worktrees(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let start = workspace_project_root(&state).await?;
    let repo_root = git_repo_root(&start)?;
    let output = run_git(&repo_root, &["worktree", "list", "--porcelain"])?;
    Ok(json!(parse_worktree_list(&output, &repo_root)))
}

#[tauri::command]
pub async fn create_worktree(
    state: tauri::State<'_, TauriState>,
    branch: String,
    base: Option<String>,
    path: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    let start = workspace_project_root(&state).await?;
    let repo_root = git_repo_root(&start)?;
    let branch = branch.trim().to_string();
    validate_branch_name(&repo_root, &branch)?;

    let target = path
        .filter(|p| !p.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or(default_worktree_path(&repo_root, &branch)?);
    validate_worktree_target(&repo_root, &target)?;

    let base_ref = base
        .and_then(|s| {
            let trimmed = s.trim().to_string();
            (!trimmed.is_empty()).then_some(trimmed)
        })
        .unwrap_or_else(|| "HEAD".to_string());
    let target_str = target.to_string_lossy().to_string();
    run_git(
        &repo_root,
        &["worktree", "add", "-b", &branch, &target_str, &base_ref],
    )?;

    let output = run_git(&repo_root, &["worktree", "list", "--porcelain"])?;
    parse_worktree_list(&output, &repo_root)
        .into_iter()
        .find(|wt| wt.path == target_str)
        .map(|wt| json!(wt))
        .ok_or_else(|| IpcError::Internal("Created worktree was not found in git output".into()))
}

#[tauri::command]
pub async fn remove_worktree(
    state: tauri::State<'_, TauriState>,
    path: String,
) -> Result<serde_json::Value, IpcError> {
    let start = workspace_project_root(&state).await?;
    let repo_root = git_repo_root(&start)?;
    let target = PathBuf::from(path.trim());
    if target.as_os_str().is_empty() {
        return Err(IpcError::Validation("Worktree path cannot be empty".into()));
    }

    let canonical_repo = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.clone());
    let canonical_target = target
        .canonicalize()
        .map_err(|e| IpcError::Validation(format!("Cannot resolve worktree path: {e}")))?;
    if canonical_target == canonical_repo {
        return Err(IpcError::Validation(
            "Refusing to remove the primary repository worktree".into(),
        ));
    }

    let output = run_git(&repo_root, &["worktree", "list", "--porcelain"])?;
    let known = parse_worktree_list(&output, &repo_root)
        .into_iter()
        .any(|wt| {
            PathBuf::from(wt.path)
                .canonicalize()
                .map(|p| p == canonical_target)
                .unwrap_or(false)
        });
    if !known {
        return Err(IpcError::Validation(
            "Path is not a registered git worktree".into(),
        ));
    }

    let target_str = target.to_string_lossy().to_string();
    run_git(&repo_root, &["worktree", "remove", &target_str])?;
    Ok(json!({"success": true}))
}

#[tauri::command]
pub async fn list_unattended_worktrees(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let repo_root = workspace_project_root(&state).await?;
    // 任务运行时存储在 AppState.tasks.runtime(Option<Arc<TaskRuntimeStore>>);ConnectionState 无此字段。
    let store = state.app_state.tasks.runtime.clone();

    let unattended = echo_agent_app_core::tasks::task_runtime::worktree::list_unattended_worktrees(
        &repo_root,
        store.as_deref(),
    )
    .map_err(|e| IpcError::Internal(format!("Failed to list unattended worktrees: {e}")))?;

    let result: Vec<serde_json::Value> = unattended
        .into_iter()
        .map(|wt| {
            json!({
                "run_id": wt.run_id,
                "branch": wt.branch,
                "path": wt.path.to_string_lossy(),
                "head": wt.head,
                "status": wt.status,
            })
        })
        .collect();

    Ok(json!(result))
}

#[tauri::command]
pub async fn merge_unattended_worktree(
    state: tauri::State<'_, TauriState>,
    run_id: String,
    remove_after_merge: Option<bool>,
) -> Result<serde_json::Value, IpcError> {
    let repo_root = workspace_project_root(&state).await?;
    let remove = remove_after_merge.unwrap_or(true);

    echo_agent_app_core::tasks::task_runtime::worktree::merge_unattended_worktree(
        &repo_root, &run_id, remove,
    )
    .map_err(|e| IpcError::Internal(format!("Failed to merge unattended worktree: {e}")))?;

    Ok(json!({"success": true, "merged": run_id}))
}

#[tauri::command]
pub async fn discard_unattended_worktree(
    state: tauri::State<'_, TauriState>,
    run_id: String,
) -> Result<serde_json::Value, IpcError> {
    let repo_root = workspace_project_root(&state).await?;

    echo_agent_app_core::tasks::task_runtime::worktree::discard_unattended_worktree(
        &repo_root, &run_id,
    )
    .map_err(|e| IpcError::Internal(format!("Failed to discard unattended worktree: {e}")))?;

    Ok(json!({"success": true, "discarded": run_id}))
}

// ════════════════════════════════════════════════════════════════════════════
// MCP (missing method)
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn get_mcp_server(
    state: tauri::State<'_, TauriState>,
    name: String,
) -> Result<serde_json::Value, IpcError> {
    let mcp_config = state.app_state.plugins.mcp_config.read().await;
    let mcp_health = state.app_state.plugins.mcp_health.read().await;

    let exists = state
        .app_state
        .connection
        .agent
        .read(|agent| agent.list_mcp_servers().iter().any(|s| s == &name))
        .await;

    if !exists {
        return Err(IpcError::NotFound(format!(
            "MCP server '{}' not found",
            name
        )));
    }

    let health = mcp_health.get(&name);
    let status = if let Some(h) = health {
        if h.healthy { "connected" } else { "error" }
    } else {
        "disconnected"
    };

    let transport = mcp_config
        .mcp_servers
        .get(&name)
        .map(|e| {
            if e.url.is_some() {
                if e.transport.as_deref() == Some("sse") {
                    "sse"
                } else {
                    "http"
                }
            } else if e.command.is_some() {
                "stdio"
            } else {
                "unknown"
            }
        })
        .unwrap_or("unknown");

    Ok(serde_json::json!({
        "name": name,
        "status": status,
        "transport": transport,
        "tool_count": 0,
        "tools": [],
        "connected_at": null,
        "error": health.and_then(|h| h.error.clone()),
    }))
}
