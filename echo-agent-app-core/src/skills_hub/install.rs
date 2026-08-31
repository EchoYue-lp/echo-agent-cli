//! 技能安装器
//!
//! 支持从 Git 仓库或本地目录安装技能到 `~/.eko/skills/`。

use std::ffi::OsStr;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use chrono::Utc;
use echo_agent::skills::external::validate_skill_dir as validate_standard_skill_dir;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::registry::SkillsHub;

const SOURCE_RECORD_FILE: &str = ".eko-skill-source.json";
const GIT_TIMEOUT: Duration = Duration::from_secs(120);

/// 安装结果
#[derive(Debug)]
pub(crate) struct InstallResult {
    pub name: String,
    pub path: PathBuf,
    pub source: String,
    pub revision: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillSourceRecord {
    pub repo_url: String,
    #[serde(default)]
    pub subdir: Option<String>,
    pub revision: String,
    pub content_hash: String,
    pub installed_at: String,
    pub synced_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SkillUpdateState {
    UpToDate,
    UpdateAvailable,
    LocalChanges,
    Untracked,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillUpdateStatus {
    pub name: String,
    pub state: SkillUpdateState,
    pub current_revision: Option<String>,
    pub remote_revision: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct SkillSyncResult {
    pub(crate) name: String,
    pub(crate) success: bool,
    pub(crate) updated: bool,
    pub(crate) revision: Option<String>,
    pub(crate) message: String,
    pub(crate) retryable: bool,
}

/// 从本地目录安装技能。
pub(crate) fn install_from_local(
    source: &Path,
    hub: &mut SkillsHub,
) -> Result<InstallResult, String> {
    validate_skill_dir(source)?;
    let skill_name = source
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| "无法获取目录名".to_string())?
        .to_string();
    let dest = hub.root().join(&skill_name);
    replace_skill_directory(source, &dest, None)?;
    hub.refresh();

    Ok(InstallResult {
        name: skill_name,
        path: dest,
        source: format!("local:{}", source.display()),
        revision: None,
    })
}

/// 从 Git 仓库安装技能
pub(crate) async fn install_from_git(
    repo_url: &str,
    subdir: Option<&str>,
    hub: &mut SkillsHub,
) -> Result<InstallResult, String> {
    validate_git_url(repo_url)?;
    validate_subdir(subdir)?;
    let checkout = clone_repository(repo_url).await?;
    let revision = git_revision(checkout.path()).await?;
    let source_dir = locate_skill_dir(checkout.path(), subdir)?;
    let skill_name = source_dir
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| "无法获取技能目录名".to_string())?
        .to_string();
    let dest = hub.root().join(&skill_name);
    let now = Utc::now().to_rfc3339();
    let record = SkillSourceRecord {
        repo_url: repo_url.to_string(),
        subdir: subdir.map(str::to_string),
        revision: revision.clone(),
        content_hash: String::new(),
        installed_at: now.clone(),
        synced_at: now,
    };
    replace_skill_directory(&source_dir, &dest, Some(record))?;
    hub.refresh();

    Ok(InstallResult {
        name: skill_name,
        path: dest,
        source: git_source_label(repo_url, subdir),
        revision: Some(revision),
    })
}

/// 卸载技能
pub(crate) fn uninstall(name: &str, hub: &mut SkillsHub) -> Result<bool, String> {
    let Some(entry) = hub.get(name) else {
        return Ok(false);
    };

    let path = entry.path.clone();
    if !path.exists() {
        return Ok(false);
    }
    std::fs::remove_dir_all(&path).map_err(|e| format!("删除技能目录失败: {e}"))?;

    hub.refresh();
    Ok(true)
}

pub fn read_source_record(skill_dir: &Path) -> Result<Option<SkillSourceRecord>, String> {
    let path = skill_dir.join(SOURCE_RECORD_FILE);
    if !path.is_file() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)
        .map_err(|error| format!("读取技能来源记录失败 {}: {error}", path.display()))?;
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|error| format!("解析技能来源记录失败 {}: {error}", path.display()))
}

pub async fn check_updates(
    hub: &SkillsHub,
    target: Option<&str>,
) -> Result<Vec<SkillUpdateStatus>, String> {
    let entries = selected_entries(hub, target)?;
    let mut statuses = Vec::with_capacity(entries.len());
    for (name, path) in entries {
        let record = match read_source_record(&path) {
            Ok(Some(record)) => record,
            Ok(None) => {
                statuses.push(SkillUpdateStatus {
                    name,
                    state: SkillUpdateState::Untracked,
                    current_revision: None,
                    remote_revision: None,
                    message: "技能不是从 Git 安装，无法检查上游更新".to_string(),
                });
                continue;
            }
            Err(error) => {
                statuses.push(SkillUpdateStatus {
                    name,
                    state: SkillUpdateState::Error,
                    current_revision: None,
                    remote_revision: None,
                    message: error,
                });
                continue;
            }
        };
        let current_hash = match hash_skill_dir(&path) {
            Ok(hash) => hash,
            Err(error) => {
                statuses.push(SkillUpdateStatus {
                    name,
                    state: SkillUpdateState::Error,
                    current_revision: Some(record.revision),
                    remote_revision: None,
                    message: error,
                });
                continue;
            }
        };
        if current_hash != record.content_hash {
            statuses.push(SkillUpdateStatus {
                name,
                state: SkillUpdateState::LocalChanges,
                current_revision: Some(record.revision),
                remote_revision: None,
                message: "检测到本地修改；同步前请审阅，或显式使用 --force".to_string(),
            });
            continue;
        }
        match remote_revision(&record.repo_url).await {
            Ok(remote) => {
                let update_available = remote != record.revision;
                statuses.push(SkillUpdateStatus {
                    name,
                    state: if update_available {
                        SkillUpdateState::UpdateAvailable
                    } else {
                        SkillUpdateState::UpToDate
                    },
                    current_revision: Some(record.revision),
                    remote_revision: Some(remote),
                    message: if update_available {
                        "发现上游更新".to_string()
                    } else {
                        "已是最新版本".to_string()
                    },
                });
            }
            Err(error) => statuses.push(SkillUpdateStatus {
                name,
                state: SkillUpdateState::Error,
                current_revision: Some(record.revision),
                remote_revision: None,
                message: error,
            }),
        }
    }
    Ok(statuses)
}

pub(crate) async fn sync_skills(
    hub: &mut SkillsHub,
    target: Option<&str>,
    force: bool,
) -> Result<Vec<SkillSyncResult>, String> {
    let entries = selected_entries(hub, target)?;
    let mut results = Vec::with_capacity(entries.len());
    for (name, path) in entries {
        results.push(sync_one(&name, &path, force).await);
    }
    hub.refresh();
    Ok(results)
}

async fn sync_one(name: &str, path: &Path, force: bool) -> SkillSyncResult {
    let record = match read_source_record(path) {
        Ok(Some(record)) => record,
        Ok(None) => {
            return SkillSyncResult {
                name: name.to_string(),
                success: false,
                updated: false,
                revision: None,
                message: "技能不是从 Git 安装，已跳过".to_string(),
                retryable: false,
            };
        }
        Err(error) => {
            return SkillSyncResult {
                name: name.to_string(),
                success: false,
                updated: false,
                revision: None,
                message: error,
                retryable: false,
            };
        }
    };
    let current_hash = match hash_skill_dir(path) {
        Ok(hash) => hash,
        Err(error) => {
            return SkillSyncResult {
                name: name.to_string(),
                success: false,
                updated: false,
                revision: Some(record.revision),
                message: error,
                retryable: true,
            };
        }
    };
    if !force && current_hash != record.content_hash {
        return SkillSyncResult {
            name: name.to_string(),
            success: false,
            updated: false,
            revision: Some(record.revision),
            message: "检测到本地修改，未覆盖；使用 --force 可显式同步".to_string(),
            retryable: false,
        };
    }
    let remote = match remote_revision(&record.repo_url).await {
        Ok(remote) => remote,
        Err(error) => {
            return SkillSyncResult {
                name: name.to_string(),
                success: false,
                updated: false,
                revision: Some(record.revision),
                message: error,
                retryable: true,
            };
        }
    };
    if !force && remote == record.revision {
        return SkillSyncResult {
            name: name.to_string(),
            success: true,
            updated: false,
            revision: Some(record.revision),
            message: "已是最新版本".to_string(),
            retryable: false,
        };
    }
    let checkout = match clone_repository(&record.repo_url).await {
        Ok(checkout) => checkout,
        Err(error) => {
            return SkillSyncResult {
                name: name.to_string(),
                success: false,
                updated: false,
                revision: Some(record.revision),
                message: error,
                retryable: true,
            };
        }
    };
    let revision = match git_revision(checkout.path()).await {
        Ok(revision) => revision,
        Err(error) => {
            return SkillSyncResult {
                name: name.to_string(),
                success: false,
                updated: false,
                revision: Some(record.revision),
                message: error,
                retryable: true,
            };
        }
    };
    let source_dir = match locate_skill_dir(checkout.path(), record.subdir.as_deref()) {
        Ok(source_dir) => source_dir,
        Err(error) => {
            return SkillSyncResult {
                name: name.to_string(),
                success: false,
                updated: false,
                revision: Some(record.revision),
                message: error,
                retryable: true,
            };
        }
    };
    let updated_record = SkillSourceRecord {
        revision: revision.clone(),
        content_hash: String::new(),
        synced_at: Utc::now().to_rfc3339(),
        ..record
    };
    match replace_skill_directory(&source_dir, path, Some(updated_record)) {
        Ok(()) => SkillSyncResult {
            name: name.to_string(),
            success: true,
            updated: true,
            revision: Some(revision),
            message: "同步完成".to_string(),
            retryable: false,
        },
        Err(error) => SkillSyncResult {
            name: name.to_string(),
            success: false,
            updated: false,
            revision: None,
            message: error,
            retryable: true,
        },
    }
}

fn selected_entries(
    hub: &SkillsHub,
    target: Option<&str>,
) -> Result<Vec<(String, PathBuf)>, String> {
    match target.map(str::trim).filter(|value| !value.is_empty()) {
        Some("all") | None => Ok(hub
            .list()
            .into_iter()
            .map(|entry| (entry.name.clone(), entry.path.clone()))
            .collect()),
        Some(name) => hub
            .get(name)
            .map(|entry| vec![(entry.name.clone(), entry.path.clone())])
            .ok_or_else(|| format!("技能 '{name}' 未找到")),
    }
}

fn validate_skill_dir(source: &Path) -> Result<(), String> {
    if !source.exists() {
        return Err(format!("源路径不存在: {}", source.display()));
    }
    if !source.is_dir() {
        return Err(format!("源路径不是目录: {}", source.display()));
    }
    let report = validate_standard_skill_dir(source);
    if report.is_valid() {
        Ok(())
    } else {
        Err(format!(
            "技能目录 {} 未通过官方 Agent Skills 校验: {}",
            source.display(),
            report.violations.join("; ")
        ))
    }
}

fn replace_skill_directory(
    source: &Path,
    destination: &Path,
    source_record: Option<SkillSourceRecord>,
) -> Result<(), String> {
    validate_skill_dir(source)?;
    let parent = destination
        .parent()
        .ok_or_else(|| format!("技能目标目录没有父目录: {}", destination.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("创建技能根目录失败 {}: {error}", parent.display()))?;
    let name = destination
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| "无法获取技能目标目录名".to_string())?;
    let nonce = uuid::Uuid::new_v4();
    let staging = parent.join(format!(".{name}.staging-{nonce}"));
    let backup = parent.join(format!(".{name}.backup-{nonce}"));

    if let Err(error) = copy_dir_recursive(source, &staging) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!("复制技能到 staging 失败: {error}"));
    }
    if let Some(mut record) = source_record {
        record.content_hash = match hash_skill_dir(&staging) {
            Ok(hash) => hash,
            Err(error) => {
                let _ = std::fs::remove_dir_all(&staging);
                return Err(error);
            }
        };
        if let Err(error) = write_source_record(&staging, &record) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(error);
        }
    }

    if destination.exists() {
        std::fs::rename(destination, &backup)
            .map_err(|error| format!("备份现有技能失败: {error}"))?;
    }
    if let Err(error) = std::fs::rename(&staging, destination) {
        let restore_error = backup
            .exists()
            .then(|| std::fs::rename(&backup, destination).err())
            .flatten();
        let _ = std::fs::remove_dir_all(&staging);
        if let Some(restore_error) = restore_error {
            return Err(format!(
                "安装技能 staging 失败: {error}; 恢复旧技能也失败: {restore_error}; 备份保留在 {}",
                backup.display()
            ));
        }
        return Err(format!("安装技能 staging 失败: {error}"));
    }
    if backup.exists() {
        let _ = std::fs::remove_dir_all(backup);
    }
    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    let mut entries = std::fs::read_dir(src)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name();
        if name == OsStr::new(".git") || name == OsStr::new(SOURCE_RECORD_FILE) {
            continue;
        }
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "skill contains unsupported symbolic link: {}",
                    entry.path().display()
                ),
            ));
        }
        let src_path = entry.path();
        let dst_path = dst.join(name);
        if file_type.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else if file_type.is_file() {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

fn hash_skill_dir(skill_dir: &Path) -> Result<String, String> {
    let mut hasher = Sha256::new();
    hash_directory_entries(skill_dir, skill_dir, &mut hasher)?;
    Ok(format!("{:x}", hasher.finalize()))
}

fn hash_directory_entries(
    base: &Path,
    directory: &Path,
    hasher: &mut Sha256,
) -> Result<(), String> {
    let mut entries = std::fs::read_dir(directory)
        .map_err(|error| format!("读取技能目录失败 {}: {error}", directory.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("读取技能目录项失败: {error}"))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name();
        if name == OsStr::new(".git") || name == OsStr::new(SOURCE_RECORD_FILE) {
            continue;
        }
        let path = entry.path();
        let relative = path
            .strip_prefix(base)
            .map_err(|error| format!("计算技能相对路径失败: {error}"))?;
        hasher.update(relative.to_string_lossy().as_bytes());
        let file_type = entry
            .file_type()
            .map_err(|error| format!("读取技能文件类型失败 {}: {error}", path.display()))?;
        if file_type.is_dir() {
            hasher.update(b"dir");
            hash_directory_entries(base, &path, hasher)?;
        } else if file_type.is_file() {
            hasher.update(b"file");
            let mut file = std::fs::File::open(&path)
                .map_err(|error| format!("打开技能文件失败 {}: {error}", path.display()))?;
            let mut buffer = [0_u8; 8192];
            loop {
                let read = file
                    .read(&mut buffer)
                    .map_err(|error| format!("读取技能文件失败 {}: {error}", path.display()))?;
                if read == 0 {
                    break;
                }
                let chunk = buffer
                    .get(..read)
                    .ok_or_else(|| "技能哈希缓冲区范围无效".to_string())?;
                hasher.update(chunk);
            }
        } else {
            return Err(format!("技能包含不支持的文件类型: {}", path.display()));
        }
    }
    Ok(())
}

fn write_source_record(skill_dir: &Path, record: &SkillSourceRecord) -> Result<(), String> {
    let path = skill_dir.join(SOURCE_RECORD_FILE);
    let content = serde_json::to_vec_pretty(record)
        .map_err(|error| format!("序列化技能来源记录失败: {error}"))?;
    std::fs::write(&path, content)
        .map_err(|error| format!("写入技能来源记录失败 {}: {error}", path.display()))
}

fn validate_git_url(url: &str) -> Result<(), String> {
    if url.starts_with("https://") && url.chars().count() > "https://".chars().count() {
        return Ok(());
    }
    let scheme = url.split("://").next().unwrap_or(url);
    Err(format!(
        "技能 Git 地址必须使用 https://，当前 scheme: {scheme}"
    ))
}

fn validate_subdir(subdir: Option<&str>) -> Result<(), String> {
    let Some(subdir) = subdir else {
        return Ok(());
    };
    if subdir.trim().is_empty() {
        return Err("技能子目录不能为空".to_string());
    }
    let path = Path::new(subdir);
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(format!("技能子目录必须是仓库内相对路径: {subdir}"));
    }
    Ok(())
}

async fn clone_repository(repo_url: &str) -> Result<tempfile::TempDir, String> {
    let checkout = tempfile::Builder::new()
        .prefix("eko-skill-source-")
        .tempdir()
        .map_err(|error| format!("创建技能临时目录失败: {error}"))?;
    let mut command = tokio::process::Command::new("git");
    command
        .arg("clone")
        .arg("--depth")
        .arg("1")
        .arg(repo_url)
        .arg(checkout.path());
    let output = command_output(command, "git clone").await?;
    ensure_command_success(output, "git clone")?;
    Ok(checkout)
}

async fn git_revision(repo_dir: &Path) -> Result<String, String> {
    let mut command = tokio::process::Command::new("git");
    command.arg("-C").arg(repo_dir).arg("rev-parse").arg("HEAD");
    let output = command_output(command, "git rev-parse").await?;
    ensure_command_success(output, "git rev-parse")
}

async fn remote_revision(repo_url: &str) -> Result<String, String> {
    validate_git_url(repo_url)?;
    let mut command = tokio::process::Command::new("git");
    command.arg("ls-remote").arg(repo_url).arg("HEAD");
    let output = command_output(command, "git ls-remote").await?;
    let raw = ensure_command_success(output, "git ls-remote")?;
    raw.split_whitespace()
        .next()
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "git ls-remote 未返回 HEAD revision".to_string())
}

async fn command_output(
    mut command: tokio::process::Command,
    action: &str,
) -> Result<Output, String> {
    tokio::time::timeout(GIT_TIMEOUT, command.output())
        .await
        .map_err(|_| format!("{action} 超时（120 秒）"))?
        .map_err(|error| format!("执行 {action} 失败: {error}"))
}

fn ensure_command_success(output: Output, action: &str) -> Result<String, String> {
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            format!("{action} 失败，退出状态: {}", output.status)
        } else {
            format!("{action} 失败: {stderr}")
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn locate_skill_dir(repo_dir: &Path, subdir: Option<&str>) -> Result<PathBuf, String> {
    validate_subdir(subdir)?;
    if let Some(subdir) = subdir {
        let path = repo_dir.join(subdir);
        validate_skill_dir(&path)?;
        return Ok(path);
    }
    if repo_dir.join("SKILL.md").is_file() {
        return Ok(repo_dir.to_path_buf());
    }
    let mut candidates = std::fs::read_dir(repo_dir)
        .map_err(|error| format!("读取技能仓库失败 {}: {error}", repo_dir.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("读取技能仓库目录项失败: {error}"))?;
    candidates.sort_by_key(|entry| entry.file_name());
    candidates
        .into_iter()
        .map(|entry| entry.path())
        .find(|path| path.is_dir() && path.join("SKILL.md").is_file())
        .ok_or_else(|| "仓库根目录及一级子目录中未找到 SKILL.md".to_string())
}

fn git_source_label(repo_url: &str, subdir: Option<&str>) -> String {
    subdir.map_or_else(
        || format!("git:{repo_url}"),
        |subdir| format!("git:{repo_url}#{subdir}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_record_hash_detects_local_changes() -> Result<(), String> {
        let source = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::write(
            source.path().join("SKILL.md"),
            "---\nname: demo\ndescription: Demo Skill\n---\n\n# Skill\n",
        )
        .map_err(|error| error.to_string())?;
        let destination = root.path().join("demo");
        let now = Utc::now().to_rfc3339();
        replace_skill_directory(
            source.path(),
            &destination,
            Some(SkillSourceRecord {
                repo_url: "https://example.com/demo.git".to_string(),
                subdir: None,
                revision: "abc123".to_string(),
                content_hash: String::new(),
                installed_at: now.clone(),
                synced_at: now,
            }),
        )?;
        let record =
            read_source_record(&destination)?.ok_or_else(|| "source record missing".to_string())?;
        assert_eq!(hash_skill_dir(&destination)?, record.content_hash);
        std::fs::write(destination.join("SKILL.md"), "# Locally edited\n")
            .map_err(|error| error.to_string())?;
        assert_ne!(hash_skill_dir(&destination)?, record.content_hash);
        Ok(())
    }

    #[tokio::test]
    async fn untracked_and_local_change_sync_failures_are_terminal() -> Result<(), String> {
        let untracked = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::write(untracked.path().join("SKILL.md"), "# Untracked\n")
            .map_err(|error| error.to_string())?;
        let untracked_result = sync_one("untracked", untracked.path(), false).await;
        assert!(!untracked_result.success);
        assert!(!untracked_result.retryable);

        let source = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::write(
            source.path().join("SKILL.md"),
            "---\nname: local-change\ndescription: Local change Skill\n---\n\n# Original\n",
        )
        .map_err(|error| error.to_string())?;
        let destination = root.path().join("local-change");
        let now = Utc::now().to_rfc3339();
        replace_skill_directory(
            source.path(),
            &destination,
            Some(SkillSourceRecord {
                repo_url: "https://example.com/local-change.git".to_string(),
                subdir: None,
                revision: "abc123".to_string(),
                content_hash: String::new(),
                installed_at: now.clone(),
                synced_at: now,
            }),
        )?;
        std::fs::write(destination.join("SKILL.md"), "# Local edit\n")
            .map_err(|error| error.to_string())?;
        let local_change = sync_one("local-change", &destination, false).await;
        assert!(!local_change.success);
        assert!(!local_change.retryable);
        Ok(())
    }

    #[test]
    fn git_validation_is_local_app_appropriate() {
        assert!(validate_git_url("https://localhost/skills.git").is_ok());
        assert!(validate_git_url("http://example.com/skills.git").is_err());
        assert!(validate_git_url("file:///tmp/skills").is_err());
    }

    #[test]
    fn subdir_cannot_escape_checkout() {
        assert!(validate_subdir(Some("skills/review")).is_ok());
        assert!(validate_subdir(Some("../review")).is_err());
        assert!(validate_subdir(Some("/absolute/review")).is_err());
    }
}
