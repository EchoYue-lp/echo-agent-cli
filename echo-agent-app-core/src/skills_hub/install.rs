//! 技能安装器
//!
//! 支持从 Git 仓库或本地目录安装技能到 `~/.echo-agent/skills/`。

use std::path::{Path, PathBuf};

use super::registry::SkillsHub;

/// 安装结果
#[derive(Debug)]
pub struct InstallResult {
    pub name: String,
    pub path: PathBuf,
    pub source: String,
}

/// 从本地目录安装技能（复制或符号链接）
pub fn install_from_local(source: &Path, hub: &mut SkillsHub) -> Result<InstallResult, String> {
    if !source.exists() {
        return Err(format!("源路径不存在: {}", source.display()));
    }
    if !source.is_dir() {
        return Err(format!("源路径不是目录: {}", source.display()));
    }
    if !source.join("SKILL.md").exists() {
        return Err(format!("目录中缺少 SKILL.md: {}", source.display()));
    }

    let skill_name = source
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| "无法获取目录名".to_string())?
        .to_string();

    let dest = hub.root().join(&skill_name);
    if dest.exists() {
        // 如果已存在，先移除
        std::fs::remove_dir_all(&dest).map_err(|e| format!("移除旧技能失败: {e}"))?;
    }

    // 复制目录
    copy_dir_recursive(source, &dest).map_err(|e| format!("复制目录失败: {e}"))?;

    hub.refresh();

    Ok(InstallResult {
        name: skill_name,
        path: dest,
        source: format!("local:{}", source.display()),
    })
}

/// 从 Git 仓库安装技能
pub async fn install_from_git(
    repo_url: &str,
    subdir: Option<&str>,
    hub: &mut SkillsHub,
) -> Result<InstallResult, String> {
    // Security: validate the repo URL — only allow https:// scheme,
    // reject private IP hostnames to prevent SSRF.
    validate_git_url(repo_url)?;

    // 克隆到临时目录
    let temp_dir = std::env::temp_dir().join(format!("echo-agent-skill-{}", std::process::id()));
    if temp_dir.exists() {
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    let output = tokio::process::Command::new("git")
        .args([
            "clone",
            "--depth",
            "1",
            repo_url,
            &temp_dir.to_string_lossy(),
        ])
        .output()
        .await
        .map_err(|e| format!("执行 git clone 失败: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git clone 失败: {stderr}"));
    }

    // 确定源目录
    let source_dir = if let Some(sub) = subdir {
        let p = temp_dir.join(sub);
        if !p.exists() {
            return Err(format!("仓库中不存在子目录: {sub}"));
        }
        p
    } else {
        // 自动检测：如果仓库根有 SKILL.md 则用根，否则找含 SKILL.md 的子目录
        if temp_dir.join("SKILL.md").exists() {
            temp_dir.clone()
        } else {
            let mut found = None;
            if let Ok(entries) = std::fs::read_dir(&temp_dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_dir() && p.join("SKILL.md").exists() {
                        found = Some(p);
                        break;
                    }
                }
            }
            found.ok_or_else(|| "仓库中未找到 SKILL.md".to_string())?
        }
    };

    let result = install_from_local(&source_dir, hub)?;

    // 清理临时目录
    let _ = std::fs::remove_dir_all(&temp_dir);

    Ok(InstallResult {
        source: if let Some(sub) = subdir {
            format!("git:{repo_url}#{sub}")
        } else {
            format!("git:{repo_url}")
        },
        ..result
    })
}

/// 卸载技能
pub fn uninstall(name: &str, hub: &mut SkillsHub) -> Result<(), String> {
    let entry = hub
        .get(name)
        .ok_or_else(|| format!("技能 '{name}' 未找到"))?;

    let path = entry.path.clone();
    if path.exists() {
        std::fs::remove_dir_all(&path).map_err(|e| format!("删除技能目录失败: {e}"))?;
    }

    hub.refresh();
    Ok(())
}

/// 递归复制目录
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// Validate a git repository URL for safe cloning.
///
/// Only allows `https://` scheme. Rejects `file://`, `ssh://`, `git://`,
/// `http://` (plaintext), and URLs pointing to private/reserved IPs.
fn validate_git_url(url: &str) -> Result<(), String> {
    // Basic scheme check without pulling in the `url` crate (app-core
    // doesn't depend on it). This is sufficient for our security needs.
    if !url.starts_with("https://") {
        return Err(format!(
            "Only https:// git URLs are allowed for skill installation. Got: {}",
            url.split("://").next().unwrap_or(url)
        ));
    }

    // Extract hostname for private IP check
    if let Some(rest) = url.strip_prefix("https://") {
        let host = rest.split('/').next().unwrap_or("");
        // Strip port if present
        let host = host.split(':').next().unwrap_or(host);

        if is_private_hostname(host) {
            return Err(
                "Git URLs pointing to private or reserved IP addresses are not allowed".to_string(),
            );
        }
    }

    Ok(())
}

/// Check if a hostname is a private/reserved IP or localhost alias.
fn is_private_hostname(host: &str) -> bool {
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return match ip {
            std::net::IpAddr::V4(v4) => {
                v4.is_loopback()
                    || v4.is_private()
                    || v4.is_link_local()
                    || v4.is_broadcast()
                    || v4.is_unspecified()
            }
            std::net::IpAddr::V6(v6) => v6.is_loopback() || v6.is_unspecified(),
        };
    }
    let lower = host.to_lowercase();
    lower == "localhost" || lower.ends_with(".localhost") || lower == "0.0.0.0"
}
