use std::io::{self, Write};
use std::path::PathBuf;

pub fn run_onboard() -> anyhow::Result<()> {
    println!();
    println!("╭─────────────────────────────────────────────────────────────╮");
    println!("│            🚀 Echo Agent CLI — 初始化向导                    │");
    println!("╰─────────────────────────────────────────────────────────────╯");
    println!();
    println!("  这个向导将帮你完成首次配置。按 Ctrl+C 可随时退出。");
    println!();

    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let echo_dir = PathBuf::from(&home).join(".echo-agent");

    // Step 1: Create data directory
    if !echo_dir.exists() {
        print!("  📁 创建数据目录 {}? [Y/n] ", echo_dir.display());
        io::stdout().flush()?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        if answer.trim().to_lowercase() != "n" {
            std::fs::create_dir_all(&echo_dir)?;
            println!("  ✅ 已创建 {}", echo_dir.display());
        }
    } else {
        println!("  ✅ 数据目录已存在: {}", echo_dir.display());
    }
    println!();

    // Step 2: API Key
    println!("  🔑 配置 LLM API Key");
    println!("  ─────────────────────────────────────");
    let providers = [
        (
            "DASHSCOPE_API_KEY",
            "阿里通义千问 (Qwen)",
            "https://dashscope.console.aliyun.com/",
        ),
        (
            "OPENAI_API_KEY",
            "OpenAI (GPT)",
            "https://platform.openai.com/api-keys",
        ),
        (
            "ANTHROPIC_API_KEY",
            "Anthropic (Claude)",
            "https://console.anthropic.com/",
        ),
        (
            "DEEPSEEK_API_KEY",
            "DeepSeek",
            "https://platform.deepseek.com/",
        ),
        ("ZHIPU_API_KEY", "智谱 (GLM)", "https://open.bigmodel.cn/"),
        (
            "MOONSHOT_API_KEY",
            "月之暗面 (Kimi)",
            "https://platform.moonshot.cn/",
        ),
    ];

    let mut has_any_key = false;
    for (key, name, _) in &providers {
        if std::env::var(key).is_ok() {
            println!("  ✅ {} 已配置 ({})", name, key);
            has_any_key = true;
        }
    }

    if !has_any_key {
        println!("  未检测到任何 API Key, 请选择一个提供商:");
        for (i, (_, name, url)) in providers.iter().enumerate() {
            println!("    {}. {} — 获取 Key: {}", i + 1, name, url);
        }
        println!();
        print!("  请输入提供商编号 (1-6), 或直接输入 API Key: ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim();

        if let Ok(num) = input.parse::<usize>() {
            if num >= 1 && num <= providers.len() {
                let (key, name, _) = &providers[num - 1];
                print!("  请输入 {} API Key: ", name);
                io::stdout().flush()?;
                let mut api_key = String::new();
                io::stdin().read_line(&mut api_key)?;
                let api_key = api_key.trim();

                if !api_key.is_empty() {
                    let env_path = echo_dir.join(".env");
                    let content = format!("{}={}\n", key, api_key);
                    if env_path.exists() {
                        let existing = std::fs::read_to_string(&env_path)?;
                        if !existing.contains(key) {
                            std::fs::write(&env_path, format!("{}{}", existing, content))?;
                        }
                    } else {
                        std::fs::write(&env_path, content)?;
                    }
                    println!("  ✅ 已保存到 {}", env_path.display());
                    has_any_key = true;
                }
            }
        }
    }

    if !has_any_key {
        println!("  ⚠️  跳过 API Key 配置, 可稍后在 ~/.echo-agent/.env 中设置");
    }
    println!();

    // Step 3: Model selection
    println!("  🤖 选择默认模型");
    println!("  ─────────────────────────────────────");
    let models = [
        ("qwen3-max", "通义千问 Max (推荐)"),
        ("qwen3-plus", "通义千问 Plus (性价比)"),
        ("qwen3-turbo", "通义千问 Turbo (快速)"),
        ("gpt-4o", "OpenAI GPT-4o"),
        ("gpt-4o-mini", "OpenAI GPT-4o Mini"),
        ("claude-sonnet-4-20250514", "Anthropic Claude Sonnet 4"),
        ("deepseek-chat", "DeepSeek Chat"),
        ("glm-4-plus", "智谱 GLM-4 Plus"),
    ];
    for (i, (model, desc)) in models.iter().enumerate() {
        println!("    {}. {} — {}", i + 1, model, desc);
    }
    print!("  请选择 (1-{}), 或直接输入模型名: ", models.len());
    io::stdout().flush()?;

    let mut model_input = String::new();
    io::stdin().read_line(&mut model_input)?;
    let selected_model = if let Ok(num) = model_input.trim().parse::<usize>() {
        if num >= 1 && num <= models.len() {
            models[num - 1].0.to_string()
        } else {
            "qwen3-max".to_string()
        }
    } else if !model_input.trim().is_empty() {
        model_input.trim().to_string()
    } else {
        "qwen3-max".to_string()
    };
    println!("  ✅ 已选择模型: {}", selected_model);
    println!();

    // Step 4: Agent mode
    println!("  🎭 选择默认 Agent 模式");
    println!("  ─────────────────────────────────────");
    let modes = [
        ("general", "通用助手 (默认)"),
        ("coding", "编程助手"),
        ("research", "研究助手"),
        ("data", "数据分析"),
        ("writing", "写作助手"),
    ];
    for (i, (mode, desc)) in modes.iter().enumerate() {
        println!("    {}. {} — {}", i + 1, mode, desc);
    }
    print!("  请选择 (1-5), 默认 1: ");
    io::stdout().flush()?;
    let mut mode_input = String::new();
    io::stdin().read_line(&mut mode_input)?;
    let selected_mode = if let Ok(num) = mode_input.trim().parse::<usize>() {
        if num >= 1 && num <= modes.len() {
            modes[num - 1].0.to_string()
        } else {
            "general".to_string()
        }
    } else {
        "general".to_string()
    };
    println!("  ✅ 已选择模式: {}", selected_mode);
    println!();

    // Step 5: MCP configuration
    println!("  🔌 MCP 服务器配置 (可选)");
    println!("  ─────────────────────────────────────");
    println!("  MCP (Model Context Protocol) 允许 Agent 连接外部工具服务。");
    let mcp_path = echo_dir.join("mcp.json");
    if mcp_path.exists() {
        println!("  ✅ MCP 配置已存在: {}", mcp_path.display());
    } else {
        print!("  是否创建示例 MCP 配置? [y/N] ");
        io::stdout().flush()?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        if answer.trim().to_lowercase() == "y" {
            let example = r#"{
  "mcpServers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
    }
  }
}
"#;
            std::fs::write(&mcp_path, example)?;
            println!("  ✅ 已创建示例 MCP 配置: {}", mcp_path.display());
        }
    }
    println!();

    // Step 6: Generate echo-agent.yaml
    println!("  📄 生成配置文件");
    println!("  ─────────────────────────────────────");
    let config_path = echo_dir.join("echo-agent.yaml");
    if config_path.exists() {
        println!("  ✅ 配置文件已存在: {}", config_path.display());
        print!("  是否覆盖? [y/N] ");
        io::stdout().flush()?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        if answer.trim().to_lowercase() != "y" {
            println!("  ⏭️  保留现有配置");
        } else {
            write_config(&config_path, &selected_model, &selected_mode)?;
        }
    } else {
        write_config(&config_path, &selected_model, &selected_mode)?;
    }
    println!();

    // Done
    println!("╭─────────────────────────────────────────────────────────────╮");
    println!("│                    ✅ 初始化完成!                            │");
    println!("╰─────────────────────────────────────────────────────────────╯");
    println!();
    println!("  接下来，你可以:");
    println!("    echo-agent-cli --cli          # 启动交互式 REPL");
    println!("    echo-agent-cli --web          # 启动 Web 服务");
    println!("    echo-agent-cli run '你好'     # 一次性对话");
    println!("    echo-agent-cli doctor         # 运行诊断检查");
    println!();

    Ok(())
}

fn write_config(path: &std::path::Path, model: &str, mode: &str) -> anyhow::Result<()> {
    let mode_prompt = match mode {
        "coding" => "你是一个专业的编程助手。你可以阅读、编写、调试和重构代码。",
        "research" => "你是一个研究助手。你擅长搜索、分析和总结信息。",
        "data" => "你是一个数据分析助手。你可以读取和分析数据文件。",
        "writing" => "你是一个写作助手。你擅长撰写各类文本内容。",
        _ => "你是一个智能助手，可以回答各种问题并帮助用户完成任务。",
    };

    let yaml = format!(
        r#"model:
  name: "{}"
  temperature: 0.7

agent:
  name: "echo-agent"
  system_prompt: "{}"
  max_iterations: 10
  enable_tools: true
  enable_memory: true
  enable_human_in_loop: false
  tool_timeout_ms: 30000

server:
  host: "0.0.0.0"
  port: 3000

logging:
  level: "info"
"#,
        model, mode_prompt
    );
    std::fs::write(path, yaml)?;
    println!("  ✅ 已创建配置文件: {}", path.display());
    Ok(())
}
