//! Shell 补全脚本生成
//!
//! 通过 clap_complete 为 bash、zsh、fish 生成补全脚本。

use clap_complete::{generate, Shell};
use std::io;

/// 支持的 Shell 类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellType {
    Bash,
    Zsh,
    Fish,
    Elvish,
    PowerShell,
}

impl ShellType {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "bash" => Some(ShellType::Bash),
            "zsh" => Some(ShellType::Zsh),
            "fish" => Some(ShellType::Fish),
            "elvish" => Some(ShellType::Elvish),
            "powershell" | "pwsh" => Some(ShellType::PowerShell),
            _ => None,
        }
    }

    pub fn all() -> &'static [ShellType] {
        &[
            ShellType::Bash,
            ShellType::Zsh,
            ShellType::Fish,
            ShellType::Elvish,
            ShellType::PowerShell,
        ]
    }

    pub fn name(&self) -> &str {
        match self {
            ShellType::Bash => "bash",
            ShellType::Zsh => "zsh",
            ShellType::Fish => "fish",
            ShellType::Elvish => "elvish",
            ShellType::PowerShell => "powershell",
        }
    }
}

/// 构建用于补全生成的命令行定义
pub fn build_clap_command() -> clap::Command {
    use clap::Command;
    Command::new("echo-agent-cli")
        .version("1.0.0")
        .about("AI Agent 命令行与 Web 服务")
        .arg(clap::arg!(--web "启动 Web 服务"))
        .arg(clap::arg!(-i --cli "启动命令行交互"))
        .arg(clap::arg!(-p --port <PORT> "Web 服务端口").default_value("3000"))
        .arg(clap::arg!(--host <HOST> "Web 服务地址").default_value("0.0.0.0"))
        .arg(clap::arg!(-m --model <MODEL> "模型名称").default_value("qwen-plus"))
        .arg(clap::arg!(-s --system_prompt <PROMPT> "系统提示词"))
        .arg(clap::arg!(--mcp_config <PATH> "MCP 配置文件路径"))
        .arg(clap::arg!(--config <PATH> "配置文件路径"))
        .arg(clap::arg!(--no_color "禁用彩色输出"))
        .arg(clap::arg!(--channels "启用 IM 通道模式"))
        .arg(clap::arg!(--tui "启动终端 UI 模式"))
        .arg(clap::arg!(-o --output <FORMAT> "输出格式").default_value("text"))
        .arg(clap::arg!(-v --verbose "详细输出模式"))
        .subcommand(
            clap::Command::new("profiles")
                .about("管理配置档案")
                .subcommand(clap::Command::new("list").about("列出所有档案"))
                .subcommand(clap::Command::new("show").arg(clap::arg!(<NAME> "档案名称")).about("查看档案详情"))
                .subcommand(clap::Command::new("create").arg(clap::arg!(<NAME> "档案名称")).about("创建新档案"))
                .subcommand(clap::Command::new("use").arg(clap::arg!(<NAME> "档案名称")).about("激活档案"))
                .subcommand(clap::Command::new("delete").arg(clap::arg!(<NAME> "档案名称")).about("删除档案"))
        )
        .subcommand(
            clap::Command::new("sessions")
                .about("管理会话")
                .subcommand(clap::Command::new("list").about("列出所有会话"))
                .subcommand(clap::Command::new("show").arg(clap::arg!(<ID> "会话 ID")).about("查看会话详情"))
                .subcommand(clap::Command::new("branch").arg(clap::arg!(<PARENT_ID> "父会话 ID")).arg(clap::arg!(<BRANCH_NAME> "分支名称")).about("创建分支"))
                .subcommand(clap::Command::new("diff").arg(clap::arg!(<ID_A> "会话A的ID")).arg(clap::arg!(<ID_B> "会话B的ID")).about("对比两个会话"))
                .subcommand(clap::Command::new("export").arg(clap::arg!(<ID> "会话 ID")).arg(clap::arg!(-f --format <FORMAT> "导出格式")).arg(clap::arg!(-o --output <PATH> "输出路径")).about("导出会话"))
                .subcommand(clap::Command::new("delete").arg(clap::arg!(<ID> "会话 ID")).about("删除会话"))
        )
        .subcommand(
            clap::Command::new("completions")
                .about("生成 Shell 补全脚本")
                .arg(clap::arg!(<SHELL> "Shell 类型 (bash, zsh, fish, elvish, powershell)"))
                .arg(clap::arg!(--all "生成所有 Shell 的补全"))
        )
        .subcommand(
            clap::Command::new("run")
                .about("一次性对话")
                .arg(clap::arg!(<MESSAGE> ... "用户消息"))
                .arg(clap::arg!(--pipe "从 stdin 读取"))
        )
        .subcommand(clap::Command::new("tui").about("启动终端 UI 模式"))
}

/// 生成指定 Shell 的补全脚本
pub fn generate_completion(shell: ShellType) -> String {
    let mut cmd = build_clap_command();
    let name = cmd.get_name().to_string();
    let mut output = Vec::new();

    match shell {
        ShellType::Bash => generate(Shell::Bash, &mut cmd, &name, &mut output),
        ShellType::Zsh => generate(Shell::Zsh, &mut cmd, &name, &mut output),
        ShellType::Fish => generate(Shell::Fish, &mut cmd, &name, &mut output),
        ShellType::Elvish => generate(Shell::Elvish, &mut cmd, &name, &mut output),
        ShellType::PowerShell => generate(Shell::PowerShell, &mut cmd, &name, &mut output),
    }

    String::from_utf8(output).unwrap_or_default()
}

/// 生成所有 Shell 的补全脚本并写入指定目录
pub fn generate_all(dir: &std::path::Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;

    for shell in ShellType::all() {
        let script = generate_completion(*shell);
        let ext = match shell {
            ShellType::Bash => "bash",
            ShellType::Zsh => "zsh",
            ShellType::Fish => "fish",
            ShellType::Elvish => "elvish",
            ShellType::PowerShell => "ps1",
        };
        let path = dir.join(format!("echo-agent-cli.{}", ext));
        std::fs::write(&path, script)?;
    }
    Ok(())
}

/// 打印补全安装说明
pub fn print_install_hint(shell: ShellType) {
    let home = std::env::var("HOME").unwrap_or_else(|_| "$HOME".to_string());
    match shell {
        ShellType::Bash => {
            println!("# 将以下内容添加到 ~/.bashrc 或 ~/.bash_profile:");
            println!("source {}/.echo-agent/completions/echo-agent-cli.bash", home);
        }
        ShellType::Zsh => {
            println!("# 将以下内容添加到 ~/.zshrc:");
            println!("fpath=({}/.echo-agent/completions $fpath)", home);
            println!("compinit");
        }
        ShellType::Fish => {
            println!("# 将以下文件复制到 Fish 补全目录:");
            println!("cp {}/.echo-agent/completions/echo-agent-cli.fish ~/.config/fish/completions/", home);
        }
        _ => {
            println!("# 补全脚本已生成，请根据你的 Shell 文档配置加载路径。");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_bash_completion() {
        let script = generate_completion(ShellType::Bash);
        assert!(script.contains("echo-agent-cli"));
    }

    #[test]
    fn test_generate_zsh_completion() {
        let script = generate_completion(ShellType::Zsh);
        assert!(!script.is_empty());
    }

    #[test]
    fn test_shell_type_from_str() {
        assert_eq!(ShellType::from_str("bash"), Some(ShellType::Bash));
        assert_eq!(ShellType::from_str("zsh"), Some(ShellType::Zsh));
        assert_eq!(ShellType::from_str("fish"), Some(ShellType::Fish));
        assert_eq!(ShellType::from_str("unknown"), None);
    }
}
