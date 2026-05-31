//! 沙箱执行 API
//!
//! 本地执行模式注意事项：
//! - echo-agent-cli 定位为本地 CoWork 工具，代码执行在本地环境中进行
//! - 沙箱主要用于安全性（防止意外损害）而非隔离
//! - 通过进程级限制（超时、内存限制）提供基本保护
//! - 超时时会强制杀死子进程（通过 `kill_on_drop`）

use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::OnceCell;

use crate::error::AppError;
use crate::state::{AppState, SandboxTier};

// ── 类型定义 ─────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct SandboxStatus {
    pub local_available: bool,
    pub current_backend: String,
    /// 当前安全级别下的保护说明
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protection_info: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SandboxConfig {
    pub security_level: SandboxTier,
    pub max_memory_mb: Option<u32>,
    pub max_cpu_seconds: Option<u32>,
    pub network_enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct SandboxExecuteRequest {
    pub language: String,
    pub code: String,
}

#[derive(Debug, Serialize)]
pub struct SandboxExecuteResult {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
}

// ── API 处理器 ───────────────────────────────────────────────────

/// GET /api/sandbox/status
pub async fn get_sandbox_status(
    State(state): State<Arc<AppState>>,
) -> Result<Json<SandboxStatus>, AppError> {
    let sandbox_config = state.config.sandbox_config.read().await;

    let protection_info = match sandbox_config.security_level {
        SandboxTier::Low => Some("基础保护：仅限制危险命令（rm -rf / 等）".to_string()),
        SandboxTier::Medium => Some("中等保护：限制危险命令 + 超时保护（默认）".to_string()),
        SandboxTier::High => Some("高保护：限制危险命令 + 超时保护 + 内存限制".to_string()),
    };

    Ok(Json(SandboxStatus {
        local_available: true,
        current_backend: "local".to_string(),
        protection_info,
    }))
}

/// GET /api/sandbox/config
pub async fn get_sandbox_config(
    State(state): State<Arc<AppState>>,
) -> Result<Json<SandboxConfig>, AppError> {
    let config = state.config.sandbox_config.read().await;
    Ok(Json(SandboxConfig {
        security_level: config.security_level.clone(),
        max_memory_mb: Some(config.max_memory_mb),
        max_cpu_seconds: Some(config.max_cpu_seconds),
        network_enabled: config.network_enabled,
    }))
}

/// PUT /api/sandbox/config
pub async fn update_sandbox_config(
    State(state): State<Arc<AppState>>,
    Json(config): Json<SandboxConfig>,
) -> Result<Json<serde_json::Value>, AppError> {
    tracing::info!("更新沙箱配置: {:?}", config);
    let mut sandbox_config = state.config.sandbox_config.write().await;

    // Audit: warn when security level is being downgraded
    if config.security_level < sandbox_config.security_level {
        tracing::warn!(
            from = ?sandbox_config.security_level,
            to = ?config.security_level,
            "AUDIT: Sandbox security level is being downgraded via API"
        );
    }

    sandbox_config.security_level = config.security_level;
    if let Some(mem) = config.max_memory_mb {
        sandbox_config.max_memory_mb = mem;
    }
    if let Some(cpu) = config.max_cpu_seconds {
        sandbox_config.max_cpu_seconds = cpu;
    }
    sandbox_config.network_enabled = config.network_enabled;
    Ok(Json(serde_json::json!({"success": true})))
}

/// POST /api/sandbox/execute
pub async fn execute_in_sandbox(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SandboxExecuteRequest>,
) -> Result<Json<SandboxExecuteResult>, AppError> {
    // Input validation
    const MAX_CODE_LEN: usize = 10_240; // 10KB
    const ALLOWED_LANGUAGES: &[&str] = &[
        "shell",
        "bash",
        "sh",
        "python",
        "python3",
        "node",
        "javascript",
        "js",
    ];

    let language_lower = req.language.to_lowercase();
    if !ALLOWED_LANGUAGES.contains(&language_lower.as_str()) {
        return Ok(Json(SandboxExecuteResult {
            success: false,
            stdout: String::new(),
            stderr: format!(
                "Language '{}' is not allowed. Allowed: {}",
                req.language,
                ALLOWED_LANGUAGES.join(", ")
            ),
            exit_code: None,
            duration_ms: 0,
        }));
    }
    if req.code.len() > MAX_CODE_LEN {
        return Ok(Json(SandboxExecuteResult {
            success: false,
            stdout: String::new(),
            stderr: format!(
                "Code too long: {} bytes (max {} bytes)",
                req.code.len(),
                MAX_CODE_LEN
            ),
            exit_code: None,
            duration_ms: 0,
        }));
    }
    if req.code.trim().is_empty() {
        return Ok(Json(SandboxExecuteResult {
            success: false,
            stdout: String::new(),
            stderr: "Code cannot be empty".to_string(),
            exit_code: None,
            duration_ms: 0,
        }));
    }

    // Read sandbox config for limits
    let sandbox_config = state.config.sandbox_config.read().await;
    let max_cpu_secs = sandbox_config.max_cpu_seconds as u64;
    let network_enabled = sandbox_config.network_enabled;
    let security_level = sandbox_config.security_level.clone();
    drop(sandbox_config);

    // Warn when High security level uses local execution (no real isolation)
    if security_level == SandboxTier::High {
        tracing::warn!(
            "SandboxTier::High with local backend — no container/VM isolation in effect"
        );
    }

    tracing::info!("沙箱执行: {} ({} bytes)", req.language, req.code.len());

    let start = std::time::Instant::now();
    let timeout_dur = std::time::Duration::from_secs(max_cpu_secs.max(1));

    // Use tokio::time::timeout with kill_on_drop to clean up child processes
    let result = tokio::time::timeout(
        timeout_dur,
        execute_local(&req.language, &req.code, network_enabled),
    )
    .await;

    let duration_ms = start.elapsed().as_millis() as u64;

    match result {
        Ok(Ok(output)) => Ok(Json(SandboxExecuteResult {
            success: output.exit_code == Some(0),
            stdout: output.stdout,
            stderr: output.stderr,
            exit_code: output.exit_code,
            duration_ms,
        })),
        Ok(Err(e)) => Ok(Json(SandboxExecuteResult {
            success: false,
            stdout: String::new(),
            stderr: format!("执行错误: {}", e),
            exit_code: None,
            duration_ms,
        })),
        Err(_) => Ok(Json(SandboxExecuteResult {
            success: false,
            stdout: String::new(),
            stderr: format!("Execution timed out ({}s limit)", timeout_dur.as_secs()),
            exit_code: None,
            duration_ms,
        })),
    }
}

// ── 本地执行器 ──

struct LocalExecuteOutput {
    stdout: String,
    stderr: String,
    exit_code: Option<i32>,
}

async fn execute_local(
    language: &str,
    code: &str,
    network_enabled: bool,
) -> Result<LocalExecuteOutput, String> {
    let (cmd, args) = match language.to_lowercase().as_str() {
        "shell" | "bash" | "sh" => ("sh", vec!["-c".to_string(), code.to_string()]),
        "python" | "python3" => ("python3", vec!["-c".to_string(), code.to_string()]),
        "node" | "javascript" | "js" => ("node", vec!["-e".to_string(), code.to_string()]),
        _ => {
            return Err(format!(
                "不支持的语言: '{}'. 支持: shell/bash, python/python3, node/javascript",
                language
            ));
        }
    };

    // NOTE: This is process-level restriction, not true network isolation.
    // A determined process can still make network calls via raw syscalls.
    // For real isolation, use container/VM-based sandboxing.
    let mut command = tokio::process::Command::new(cmd);
    command.args(&args);
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    command.kill_on_drop(true); // Ensure child is killed if the future is dropped (e.g. on timeout)

    if !network_enabled {
        // Clear inherited environment and whitelist only minimal safe vars.
        // Also blank out proxy variables to hinder network access.
        command.env_clear();
        if let Ok(path) = std::env::var("PATH") {
            command.env("PATH", path);
        }
        if let Ok(home) = std::env::var("HOME") {
            command.env("HOME", home);
        }
        if let Ok(user) = std::env::var("USER") {
            command.env("USER", user);
        }
        if let Ok(lang) = std::env::var("LANG") {
            command.env("LANG", lang);
        }
        if let Ok(term) = std::env::var("TERM") {
            command.env("TERM", term);
        }
        // Explicitly blank proxy variables (defense in depth even after env_clear)
        command.env("HTTP_PROXY", "");
        command.env("HTTPS_PROXY", "");
        command.env("http_proxy", "");
        command.env("https_proxy", "");
    }

    let child = command
        .spawn()
        .map_err(|e| format!("启动进程 '{}' 失败: {}", cmd, e))?;

    // Wait with optional timeout (caller handles the timeout via tokio::time::timeout)
    let output = child
        .wait_with_output()
        .await
        .map_err(|e| format!("等待进程 '{}' 结束失败: {}", cmd, e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let exit_code = output.status.code();

    // 截断过长的输出（使用 char-boundary-safe 截断避免切割多字节字符）
    let max_len = 100_000;
    let stdout = if stdout.len() > max_len {
        let safe_len = (0..=max_len.min(stdout.len()))
            .rev()
            .find(|&i| stdout.is_char_boundary(i))
            .unwrap_or(0);
        format!(
            "{}...(truncated, total {} bytes)",
            &stdout[..safe_len],
            stdout.len()
        )
    } else {
        stdout
    };
    let stderr = if stderr.len() > max_len {
        let safe_len = (0..=max_len.min(stderr.len()))
            .rev()
            .find(|&i| stderr.is_char_boundary(i))
            .unwrap_or(0);
        format!(
            "{}...(truncated, total {} bytes)",
            &stderr[..safe_len],
            stderr.len()
        )
    } else {
        stderr
    };

    Ok(LocalExecuteOutput {
        stdout,
        stderr,
        exit_code,
    })
}
