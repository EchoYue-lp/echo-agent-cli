//! 管道模式
//!
//! 支持从 stdin 读取输入，处理后将结果输出到 stdout。
//! 适用于 Unix 管道组合。

use std::io::{self, BufRead, Read, Write};

use echo_agent::prelude::*;
use crate::agent_handle::AgentHandle;

/// 管道模式配置
#[derive(Debug, Clone)]
pub struct PipeConfig {
    /// 模型名称
    pub model: String,
    /// 系统提示词
    pub system_prompt: Option<String>,
    /// 是否输出原始结果（不做格式化）
    pub raw: bool,
    /// 读取整个 stdin 作为一次对话，还是逐行处理
    pub line_by_line: bool,
    /// 静默模式（不输出前缀）
    pub quiet: bool,
}

impl Default for PipeConfig {
    fn default() -> Self {
        Self {
            model: "qwen-plus".to_string(),
            system_prompt: None,
            raw: false,
            line_by_line: false,
            quiet: true,
        }
    }
}

/// 运行管道模式
///
/// 从 stdin 读取内容，发送给 Agent 处理，结果输出到 stdout。
pub async fn run_pipe(agent: AgentHandle, config: PipeConfig) -> anyhow::Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    if config.line_by_line {
        // 逐行处理模式
        let reader = io::BufReader::new(stdin.lock());
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let response = process_input(&agent, &config, &line).await?;
            if !config.quiet && !config.raw {
                writeln!(stdout, "> {}", line)?;
            }
            writeln!(stdout, "{}", response)?;
        }
    } else {
        // 整体处理模式：读取所有 stdin 内容作为一次对话
        let mut input = String::new();
        stdin.lock().read_to_string(&mut input)?;
        let input = input.trim();
        if input.is_empty() {
            return Ok(());
        }

        let response = process_input(&agent, &config, input).await?;
        if config.raw {
            write!(stdout, "{}", response)?;
        } else {
            writeln!(stdout, "{}", response)?;
        }
    }

    stdout.flush()?;
    Ok(())
}

/// 处理单条输入
async fn process_input(
    agent: &AgentHandle,
    config: &PipeConfig,
    input: &str,
) -> anyhow::Result<String> {
    let input = input.to_string();
    let system_prompt = config.system_prompt.clone();

    // Brief write lock for system prompt change (only when configured)
    if let Some(sp) = system_prompt {
        agent.write_async(|a| Box::pin(async move {
            a.set_system_prompt(sp).await;
        })).await;
    }

    // Read lock for the chat operation
    let result = agent.read_async(|a| Box::pin(async move {
        match a.chat(&input).await {
            Ok(response) => response,
            Err(e) => format!("[Error] {}", e),
        }
    })).await;
    Ok(result)
}

/// 检查 stdin 是否有数据可用（非阻塞）
pub fn stdin_has_data() -> bool {
    use std::io::IsTerminal;
    !std::io::stdin().is_terminal()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pipe_config_default() {
        let config = PipeConfig::default();
        assert!(config.quiet);
        assert!(!config.raw);
        assert!(!config.line_by_line);
    }
}
