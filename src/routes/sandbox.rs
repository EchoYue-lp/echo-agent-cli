//! 沙箱执行 API
//!
//! 安全注意事项：
//! - 本地执行模式 (`execute_local`) 直接运行在宿主进程环境中，不提供真正的隔离。
//! - `SecurityLevel::High` 场景应使用 Docker/K8s 后端，但目前仅实现了本地执行。
//! - 超时时会强制杀死子进程（通过 `kill_on_drop`）。

use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::OnceCell;

use crate::error::AppError;
use crate::state::{AppState, SecurityLevel};

// ── 类型定义 ─────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct SandboxStatus {
    pub local_available: bool,
    pub docker_available: bool,
    pub k8s_available: bool,
    pub current_backend: String,
    /// 当前安全级别下的隔离警告
    #[serde(skip_serializing_if = "Option::is_none")]
    pub isolation_warning: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SandboxConfig {
    pub security_level: SecurityLevel,
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

// ── 缓存的 Docker 可用性检查 ─────────────────────────────────────

static DOCKER_AVAILABLE: OnceCell<bool> = OnceCell::const_new();

async fn check_docker_cached() -> bool {
    *DOCKER_AVAILABLE
        .get_or_init(|| async {
            tokio::process::Command::new("docker")
                .arg("--version")
                .output()
                .await
                .map(|o| o.status.success())
                .unwrap_or(false)
        })
        .await
}

// ── API 处理器 ───────────────────────────────────────────────────

/// GET /api/sandbox/status
pub async fn get_sandbox_status(
    State(state): State<Arc<AppState>>,
) -> Result<Json<SandboxStatus>, AppError> {
    let docker_available = check_docker_cached().await;
    let sandbox_config = state.config.sandbox_config.read().await;

    let isolation_warning = if sandbox_config.security_level == SecurityLevel::High {
        Some("SecurityLevel is set to High but execution is local — no container/VM isolation is in effect. Consider using Docker or K8s backend.".to_string())
    } else {
        None
    };

    Ok(Json(SandboxStatus {
        local_available: true,
        docker_available,
        k8s_available: false,
        current_backend: "local".to_string(),
        isolation_warning,
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
    const ALLOWED_LANGUAGES: &[&str] = &["shell", "bash", "sh", "python", "python3", "node", "javascript", "js"];

    let language_lower = req.language.to_lowercase();
    if !ALLOWED_LANGUAGES.contains(&language_lower.as_str()) {
        return Ok(Json(SandboxExecuteResult {
            success: false,
            stdout: String::new(),
            stderr: format!("Language '{}' is not allowed. Allowed: {}", req.language, ALLOWED_LANGUAGES.join(", ")),
            exit_code: None,
            duration_ms: 0,
        }));
    }
    if req.code.len() > MAX_CODE_LEN {
        return Ok(Json(SandboxExecuteResult {
            success: false,
            stdout: String::new(),
            stderr: format!("Code too long: {} bytes (max {} bytes)", req.code.len(), MAX_CODE_LEN),
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
    if security_level == SecurityLevel::High {
        tracing::warn!(
            "SecurityLevel::High with local backend — no container/VM isolation in effect"
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
    _network_enabled: bool,
) -> Result<LocalExecuteOutput, String> {
    let (cmd, args) = match language.to_lowercase().as_str() {
        "shell" | "bash" | "sh" => ("sh", vec!["-c".to_string(), code.to_string()]),
        "python" | "python3" => ("python3", vec!["-c".to_string(), code.to_string()]),
        "node" | "javascript" | "js" => ("node", vec!["-e".to_string(), code.to_string()]),
        _ => return Err(format!("不支持的语言: '{}'. 支持: shell/bash, python/python3, node/javascript", language)),
    };

    let child = tokio::process::Command::new(cmd)
        .args(&args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true) // Ensure child is killed if the future is dropped (e.g. on timeout)
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

    // 截断过长的输出
    let max_len = 100_000;
    let stdout = if stdout.len() > max_len {
        format!("{}...(truncated, total {} bytes)", &stdout[..max_len], stdout.len())
    } else {
        stdout
    };
    let stderr = if stderr.len() > max_len {
        format!("{}...(truncated, total {} bytes)", &stderr[..max_len], stderr.len())
    } else {
        stderr
    };

    Ok(LocalExecuteOutput {
        stdout,
        stderr,
        exit_code,
    })
}
