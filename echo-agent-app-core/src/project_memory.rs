//! 层级化项目记忆系统
//!
//! 支持三级记忆文件：
//! - project.md: 项目级（项目根目录 .echo-agent/）
//! - user.md: 用户级（~/.echo-agent/）
//! - local.md: 目录级（当前工作目录 .echo-agent/）

use std::path::PathBuf;

/// 项目记忆管理器
pub struct ProjectMemory {
    pub project_level: Option<String>,
    pub user_level: Option<String>,
    pub local_level: Option<String>,
}

impl ProjectMemory {
    /// 加载所有层级的记忆
    pub fn load() -> Self {
        let project_level = Self::load_project_memory();
        let user_level = Self::load_user_memory();
        let local_level = Self::load_local_memory();

        Self {
            project_level,
            user_level,
            local_level,
        }
    }

    /// 获取合并后的 system prompt 后缀
    pub fn get_system_prompt_suffix(&self) -> String {
        let mut parts = Vec::new();

        if let Some(ref user) = self.user_level {
            parts.push(format!("## User-level instructions\n{}", user));
        }
        if let Some(ref project) = self.project_level {
            parts.push(format!("## Project-level instructions\n{}", project));
        }
        if let Some(ref local) = self.local_level {
            parts.push(format!("## Local directory instructions\n{}", local));
        }

        if parts.is_empty() {
            String::new()
        } else {
            format!("\n\n{}", parts.join("\n\n"))
        }
    }

    /// 加载项目级记忆
    fn load_project_memory() -> Option<String> {
        std::env::current_dir()
            .ok()
            .and_then(|pwd| Self::find_project_root(&pwd))
            .map(|root| root.join(".echo-agent").join("project.md"))
            .filter(|path| path.exists())
            .and_then(|path| std::fs::read_to_string(path).ok())
    }

    /// 加载用户级记忆
    fn load_user_memory() -> Option<String> {
        dirs::home_dir()
            .map(|home| home.join(".echo-agent").join("user.md"))
            .filter(|path| path.exists())
            .and_then(|path| std::fs::read_to_string(path).ok())
    }

    /// 加载目录级记忆
    fn load_local_memory() -> Option<String> {
        std::env::current_dir()
            .ok()
            .map(|pwd| pwd.join(".echo-agent").join("local.md"))
            .filter(|path| path.exists())
            .and_then(|path| std::fs::read_to_string(path).ok())
    }

    /// 查找项目根目录（包含 .git 或 .echo-agent 的目录）
    fn find_project_root(start: &std::path::Path) -> Option<PathBuf> {
        let mut current = Some(start);
        while let Some(dir) = current {
            if dir.join(".git").exists() || dir.join(".echo-agent").exists() {
                return Some(dir.to_path_buf());
            }
            current = dir.parent();
        }
        None
    }

    /// 保存项目级记忆
    pub fn save_project_memory(content: &str) -> std::io::Result<()> {
        let path = Self::project_memory_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, content)
    }

    /// 保存用户级记忆
    pub fn save_user_memory(content: &str) -> std::io::Result<()> {
        let path = Self::user_memory_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, content)
    }

    /// 项目记忆文件路径
    fn project_memory_path() -> PathBuf {
        std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .join(".echo-agent")
            .join("project.md")
    }

    /// 用户记忆文件路径
    fn user_memory_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".echo-agent")
            .join("user.md")
    }
}

impl Default for ProjectMemory {
    fn default() -> Self {
        Self::load()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_empty_memory() {
        // This test assumes no memory files exist
        let memory = ProjectMemory {
            project_level: None,
            user_level: None,
            local_level: None,
        };
        assert!(memory.get_system_prompt_suffix().is_empty());
    }
}
