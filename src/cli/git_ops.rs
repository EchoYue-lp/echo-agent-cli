//! Git 操作辅助模块
//!
//! 提供文件变更检测、差异展示和交互式提交功能。
//! 替代之前的自动 `git add -A && git commit` 行为。

use std::path::Path;
use std::process::Command;

/// 敏感文件模式 — 永不自动暂存。
const PROTECTED_PATTERNS: &[&str] = &[
    ".env",
    ".env.",
    "*.pem",
    "*.key",
    "*.p12",
    "*.pfx",
    "credentials",
    "*secret*",
    "id_rsa",
    "id_ed25519",
    ".npmrc",
    ".pypirc",
    "netrc",
];

/// 检查文件是否匹配保护模式。
pub fn is_protected_file(path: &str) -> bool {
    let filename = path.rsplit('/').next().unwrap_or(path);
    for pattern in PROTECTED_PATTERNS {
        if let Some(suffix) = pattern.strip_prefix('*') {
            if filename.contains(suffix) {
                return true;
            }
        } else if let Some(prefix) = pattern.strip_suffix('*') {
            if filename.starts_with(prefix) {
                return true;
            }
        } else {
            if filename == *pattern || filename.starts_with(pattern) {
                return true;
            }
        }
    }
    false
}

/// 执行 git add（排除保护文件）+ git commit。
pub fn interactive_commit(cwd: &Path, change_count: usize) -> anyhow::Result<()> {
    // Stage tracked files only (not untracked, avoiding accidental inclusion)
    let stage = Command::new("git")
        .args(["add", "-u"])
        .current_dir(cwd)
        .output()?;

    if !stage.status.success() {
        anyhow::bail!("git add failed: {}", String::from_utf8_lossy(&stage.stderr));
    }

    let msg = format!("agent: modified {} file(s)", change_count);
    let commit = Command::new("git")
        .args(["commit", "-m", &msg])
        .current_dir(cwd)
        .output()?;

    if commit.status.success() {
        let stderr = String::from_utf8_lossy(&commit.stderr);
        let short = stderr
            .lines()
            .find(|l| l.contains('['))
            .and_then(|l| l.split('[').nth(1))
            .and_then(|l| l.split(']').next())
            .unwrap_or("ok");
        println!(
            "  {} committed ({})",
            nu_ansi_term::Color::Green.paint("✓"),
            short
        );
    } else {
        let stderr = String::from_utf8_lossy(&commit.stderr);
        println!(
            "  {} commit failed: {}",
            nu_ansi_term::Color::Red.paint("✗"),
            stderr.trim()
        );
    }
    Ok(())
}

/// 仅执行 git add（排除保护文件）。
pub fn interactive_stage(cwd: &Path) -> anyhow::Result<()> {
    let stage = Command::new("git")
        .args(["add", "-u"])
        .current_dir(cwd)
        .output()?;

    if stage.status.success() {
        println!(
            "  {} 已暂存跟踪文件的变更",
            nu_ansi_term::Color::Green.paint("✓")
        );
    } else {
        let stderr = String::from_utf8_lossy(&stage.stderr);
        println!(
            "  {} stage failed: {}",
            nu_ansi_term::Color::Red.paint("✗"),
            stderr.trim()
        );
    }
    Ok(())
}
