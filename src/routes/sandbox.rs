//! 沙箱执行 API

use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::error::AppError;
use crate::state::AppState;

// ── 类型定义 ─────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct SandboxStatus {
    pub local_available: bool,
    pub docker_available: bool,
    pub k8s_available: bool,
    pub current_backend: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SandboxConfig {
    pub security_level: String,
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
    State(_state): State<Arc<AppState>>,
) -> Result<Json<SandboxStatus>, AppError> {
    // 检测 Docker 是否可用
    let docker_available = tokio::process::Command::new("docker")
        .arg("--version")
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false);

    Ok(Json(SandboxStatus {
        local_available: true,
        docker_available,
        k8s_available: false,
        current_backend: "local".to_string(),
    }))
}

/// GET /api/sandbox/config
pub async fn get_sandbox_config(
    State(state): State<Arc<AppState>>,
) -> Result<Json<SandboxConfig>, AppError> {
    let config = state.sandbox_config.read().unwrap();
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
    let mut sandbox_config = state.sandbox_config.write().unwrap();
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
    State(_state): State<Arc<AppState>>,
    Json(req): Json<SandboxExecuteRequest>,
) -> Result<Json<SandboxExecuteResult>, AppError> {
    tracing::info!("沙箱执行: {} ({} bytes)", req.language, req.code.len());

    let start = std::time::Instant::now();

    // 根据语言选择执行器
    let result = execute_local(&req.language, &req.code).await;

    let duration_ms = start.elapsed().as_millis() as u64;

    match result {
        Ok(output) => Ok(Json(SandboxExecuteResult {
            success: output.exit_code == Some(0),
            stdout: output.stdout,
            stderr: output.stderr,
            exit_code: output.exit_code,
            duration_ms,
        })),
        Err(e) => Ok(Json(SandboxExecuteResult {
            success: false,
            stdout: String::new(),
            stderr: format!("执行错误: {}", e),
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
) -> Result<LocalExecuteOutput, String> {
    let (cmd, args) = match language.to_lowercase().as_str() {
        "shell" | "bash" | "sh" => ("sh", vec!["-c".to_string(), code.to_string()]),
        "python" | "python3" => ("python3", vec!["-c".to_string(), code.to_string()]),
        "ruby" | "rb" => ("ruby", vec!["-e".to_string(), code.to_string()]),
        "node" | "javascript" | "js" => ("node", vec!["-e".to_string(), code.to_string()]),
        "perl" | "pl" => ("perl", vec!["-e".to_string(), code.to_string()]),
        _ => return Err(format!("不支持的语言: '{}'. 支持: shell/bash, python/python3, ruby, node/javascript, perl", language)),
    };

    let output = tokio::process::Command::new(cmd)
        .args(&args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("执行 '{}' 失败: {}", cmd, e))?;

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
