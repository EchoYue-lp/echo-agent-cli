# EchoCoWork 配置指南

## 配置文件位置

EchoCoWork 按以下优先级查找配置文件：

1. 命令行参数: `--config <path>`
2. 当前目录: `./echo-agent.yaml`
3. 用户目录: `~/.echo-agent/config.yaml`

## 完整配置参考

```yaml
# ── 模型配置 ──────────────────────────────────────────────────
model:
  name: "qwen-plus"                    # 模型名称
  max_tokens: null                     # 最大输出 token 数（null = 使用模型默认值）
  temperature: null                    # 温度参数（null = 使用模型默认值）

# ── Agent 配置 ────────────────────────────────────────────────
agent:
  name: "echo-assistant"               # Agent 名称
  system_prompt: |                     # 自定义系统提示词
    你是一个智能助手，可以帮助用户回答问题、执行任务。
  max_iterations: 0                    # 单次任务最大迭代次数（0 = 无限制）
  enable_tools: true                   # 启用工具调用
  enable_memory: true                  # 启用跨会话记忆
  enable_human_in_loop: true           # 启用人工介入审批
  memory_path: "~/.echo-agent/memory"  # 记忆存储路径
  tool_timeout_ms: 120000              # 工具执行超时（毫秒）
  token_limit: 0                       # 上下文自动压缩阈值（0 = 禁用）
  compress_strategy: "sliding"         # 压缩策略: sliding / summary / hybrid
  compress_window: 20                  # 滑动窗口保留消息数

# ── MCP 配置 ────────────────────────────────────────────────
mcp:
  # MCP 配置文件路径（支持 mcp.json 格式）
  # 不指定时按顺序搜索: ./mcp.json → ~/.echo-agent/mcp.json
  # config_path: "mcp.json"

# ── IM 通道配置 ──────────────────────────────────────────────
channels:
  qq:
    enabled: false                     # 是否启用 QQ Bot
    app_id: "${QQ_APP_ID}"
    client_secret: "${QQ_CLIENT_SECRET}"
  feishu:
    enabled: false                     # 是否启用飞书
    app_id: "${FEISHU_APP_ID}"
    app_secret: "${FEISHU_APP_SECRET}"
    mode: "long_poll"                  # 连接模式: long_poll | webhook
  session:
    timeout_minutes: 60                # 会话超时（分钟）
    reset_keywords:                    # 触发重置的关键词
      - "重置对话"
      - "新对话"
    reset_commands:                    # 触发重置的命令
      - "/reset"
      - "/clear"
      - "/new"

# ── Webhook 配置 ──────────────────────────────────────────────
webhooks:
  endpoints: []                        # Webhook 回调端点列表

# ── 用户钩子配置 ──────────────────────────────────────────────
hooks: {}                              # 生命周期钩子（详见 hooks.yaml）

# ── 服务配置 ──────────────────────────────────────────────────
server:
  host: "127.0.0.1"                    # 监听地址
  port: 3000                           # 监听端口
  max_body_bytes: 1048576              # 请求体最大大小（字节）

# ── 日志配置 ──────────────────────────────────────────────────
logging:
  level: "info"                        # 日志级别: trace / debug / info / warn / error
```

## 模型配置详解

### 支持的模型提供商

| 提供商 | 模型名称示例 | 环境变量 |
|--------|-------------|---------|
| 阿里通义 | qwen-plus, qwen-max, qwen-turbo | DASHSCOPE_API_KEY |
| OpenAI | gpt-4, gpt-3.5-turbo | OPENAI_API_KEY |
| Anthropic | claude-3-5-sonnet, claude-3-opus | ANTHROPIC_API_KEY |
| DeepSeek | deepseek-chat, deepseek-coder | DEEPSEEK_API_KEY |
| Ollama (本地) | llama3.1, codellama, mistral | 无需 API Key |

### Ollama 本地部署

```yaml
model:
  name: "ollama/llama3.1"
```

Ollama 使用默认地址 `http://localhost:11434/v1`，无需 API Key。如需自定义地址，可通过环境变量设置：

```bash
export OLLAMA_BASE_URL="http://localhost:11434/v1"
```

确保 Ollama 已启动：

```bash
ollama serve
ollama pull llama3.1
```

## 工作模式

### general（通用模式，默认）

通用助手模式，适合日常问答和混合任务：
- 通用对话和问题解答
- 简单文件操作
- 基础编程帮助
- 信息查询

### coding（编程模式）

专注于代码相关任务：
- 代码阅读和理解
- 代码生成和重构
- Bug 调试和修复
- 测试编写
- 文档生成

### research（研究模式）

适合信息检索和分析：
- 网络搜索
- 文档阅读
- 信息整理
- 报告生成

### data（数据分析模式）

处理数据相关任务：
- 数据读取和解析
- 统计分析
- 数据可视化
- 报告生成

### writing（写作模式）

专注于文本创作：
- 文章撰写
- 内容编辑
- 翻译
- 校对

## MCP 服务器

### 什么是 MCP？

MCP (Model Context Protocol) 是一个标准协议，允许 Agent 连接外部工具和数据源。

### 配置 MCP 服务器

MCP 服务器通过独立的 `mcp.json` 文件配置（标准 MCP 格式）。在 `echo-agent.yaml` 中指定配置文件路径：

```yaml
mcp:
  config_path: "mcp.json"    # 或直接使用默认搜索路径
```

默认搜索路径（按优先级）：
1. `./mcp.json`（项目根目录）
2. `./.echo-agent/mcp.json`
3. `~/.echo-agent/mcp.json`

### mcp.json 格式示例

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/workspace"]
    },
    "github": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-github"],
      "env": {
        "GITHUB_TOKEN": "${GITHUB_TOKEN}"
      }
    },
    "postgres": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-postgres"],
      "env": {
        "DATABASE_URL": "postgresql://user:pass@localhost:5432/db"
      }
    }
  }
}
```

## 环境变量

API Key 通过环境变量配置，Agent 会根据模型名称自动选择对应的 Provider：

### 常用环境变量

```bash
# API Keys（根据使用的模型提供商设置）
export DASHSCOPE_API_KEY="..."       # 阿里通义
export OPENAI_API_KEY="..."          # OpenAI
export ANTHROPIC_API_KEY="..."       # Anthropic
export DEEPSEEK_API_KEY="..."        # DeepSeek

# MCP 配置文件路径（覆盖默认搜索路径）
export MCP_CONFIG_PATH="~/my-mcp-config.json"

# 模型名称（覆盖配置文件中的值）
export MODEL_NAME="qwen-max"

# 代理设置
export HTTP_PROXY="http://proxy:8080"
export HTTPS_PROXY="http://proxy:8080"

# 日志级别
export RUST_LOG="info"
```

## 配置验证

运行诊断检查配置是否正确：

```bash
echo-agent-cli doctor
```

这将检查：
- 配置文件是否存在
- API Key 是否有效
- 模型是否可用
- 工具是否正常工作
- MCP 服务器是否可连接
