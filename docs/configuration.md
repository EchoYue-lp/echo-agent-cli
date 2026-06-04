# EchoCoWork 配置指南

## 配置文件位置

EchoCoWork 按以下优先级查找配置文件：

1. 命令行参数: `--config <path>`
2. 当前目录: `./echo-agent.yaml`
3. 项目目录: `./.echo-agent/echo-agent.yaml`
4. 用户目录: `~/.echo-agent/config.yaml`

## 快速配置

只需设置三个环境变量即可使用：

```bash
export ECHOCOWORK_AUTH_TOKEN="your-api-key"
export ECHOCOWORK_BASE_URL="https://api.deepseek.com/v1"
export ECHOCOWORK_MODEL="deepseek-v4-flash"
```

## 完整配置文件（echo-agent.yaml）

将以下内容复制到 `~/.echo-agent/config.yaml` 或项目根目录的 `echo-agent.yaml`：

```yaml
# ── 模型配置 ─────────────────────────────────────────────────────
# 也可以通过环境变量设置（优先级更高）：
# - ECHOCOWORK_AUTH_TOKEN: API 密钥
# - ECHOCOWORK_BASE_URL: API 基础 URL
# - ECHOCOWORK_MODEL: 模型名称
model:
  provider: "deepseek"        # 模型 Provider（deepseek/openai/anthropic/qwen）
  name: "deepseek-v4-flash"   # 模型名称
  auth_token: ""              # API 密钥（可选，优先从环境变量 ECHOCOWORK_AUTH_TOKEN 读取）
  base_url: ""                # API 基础 URL（可选，优先从环境变量 ECHOCOWORK_BASE_URL 读取）
  # max_tokens: 4096          # 最大输出 token 数（可选）
  # temperature: 0.7          # 温度参数（可选）

# ── Agent 配置 ─────────────────────────────────────────────────────
agent:
  name: "echo-assistant"                              # Agent 名称
  system_prompt: "你是一个智能助手，可以帮助用户回答问题、执行任务。"  # 系统提示词
  max_iterations: 0            # ReAct 最大迭代次数（0 = 无限制，直到任务完成或用户取消）
  enable_tools: true          # 启用工具调用
  enable_memory: true         # 启用记忆
  enable_human_in_loop: true  # 启用人工介入
  memory_path: "~/.echo-agent/memory"  # 记忆存储路径
  tool_timeout_ms: 120000     # 工具执行超时（毫秒）
  token_limit: 0              # 上下文自动压缩阈值（0 = 禁用）
  compress_strategy: "sliding" # 压缩策略: sliding / summary / hybrid
  compress_window: 20         # 滑动窗口保留消息数

# ── MCP 配置 ─────────────────────────────────────────────────────
mcp:
  # MCP 配置文件路径（支持 mcp.json）
  # 如果不指定，会按顺序搜索：
  #   ./mcp.json → ./.echo-agent/mcp.json → ~/.echo-agent/mcp.json
  # config_path: "mcp.json"

# ── IM 通道配置 ─────────────────────────────────────────────────────
channels:
  # QQ Bot 通道
  qq:
    enabled: false             # 是否启用
    app_id: ""                 # QQ Bot App ID
    client_secret: ""          # QQ Bot Client Secret

  # 飞书通道
  feishu:
    enabled: false             # 是否启用
    app_id: ""                 # 飞书 App ID
    app_secret: ""             # 飞书 App Secret
    mode: "long_poll"          # 连接模式: long_poll | webhook

  # 会话配置
  session:
    timeout_minutes: 60                    # 会话超时（分钟）
    reset_keywords:                         # 触发重置的关键词
      - "重置对话"
      - "新对话"
      - "清除记忆"
    reset_commands:                         # 触发重置的命令
      - "/reset"
      - "/clear"
      - "/new"

# ── Webhook 配置 ──────────────────────────────────────────────────
webhooks:
  endpoints: []                        # Webhook 回调端点列表

# ── 用户钩子配置 ──────────────────────────────────────────────────
hooks: {}                              # 生命周期钩子（详见 hooks.yaml）

# ── 服务配置 ─────────────────────────────────────────────────────
server:
  host: "127.0.0.1"             # 监听地址（默认绑定 localhost，安全起见不绑定 0.0.0.0）
  port: 3000                   # 监听端口
  max_body_bytes: 1048576      # 请求体最大大小（字节）

# ── 日志配置 ─────────────────────────────────────────────────────
logging:
  level: "info"                # 日志级别: trace | debug | info | warn | error

# ── TUI 配置 ─────────────────────────────────────────────────────
tui:
  max_display_chars: 20000     # 聊天区域最大保留字符数（超出后自动裁剪旧消息）
```

## MCP 服务器配置（mcp.json）

将以下内容复制到 `~/.echo-agent/mcp.json` 或项目根目录的 `mcp.json`：

```json
{
  "mcpServers": {
    "playwright": {
      "command": "npx",
      "args": ["-y", "@anthropic-ai/mcp-server-playwright"],
      "disabled": false,
      "description": "Playwright MCP Server - 浏览器自动化"
    },
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@anthropic-ai/mcp-server-filesystem", "/workspace"],
      "disabled": true,
      "description": "文件系统 MCP Server - 文件读写操作"
    },
    "github": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-github"],
      "disabled": true,
      "env": {
        "GITHUB_TOKEN": "${GITHUB_TOKEN}"
      },
      "description": "GitHub MCP Server - GitHub API 操作"
    },
    "postgres": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-postgres"],
      "disabled": true,
      "env": {
        "DATABASE_URL": "postgresql://user:pass@localhost:5432/db"
      },
      "description": "PostgreSQL MCP Server - 数据库查询"
    }
  }
}
```

MCP 配置文件搜索路径（按优先级）：
1. `./mcp.json`（项目根目录）
2. `./.echo-agent/mcp.json`
3. `~/.echo-agent/mcp.json`

## 模型配置详解

### 支持的模型提供商

| 提供商 | 模型名称示例 | 环境变量 |
|--------|-------------|---------|
| DeepSeek | deepseek-v4-flash, deepseek-chat, deepseek-coder | DEEPSEEK_API_KEY |
| OpenAI | gpt-4o, gpt-4, gpt-3.5-turbo | OPENAI_API_KEY |
| Anthropic | claude-3.5-sonnet, claude-3-opus | ANTHROPIC_API_KEY |
| 阿里通义 | qwen-plus, qwen-max, qwen-turbo | DASHSCOPE_API_KEY |
| Ollama (本地) | llama3.1, codellama, mistral | 无需 API Key |

### 切换模型示例

**DeepSeek（默认）：**
```yaml
model:
  provider: "deepseek"
  name: "deepseek-v4-flash"
```

**OpenAI GPT-4o：**
```yaml
model:
  provider: "openai"
  name: "gpt-4o"
```

**Anthropic Claude：**
```yaml
model:
  provider: "anthropic"
  name: "claude-3.5-sonnet"
```

**自定义 Provider：**
```yaml
model:
  provider: "custom"
  name: "your-model-name"
  auth_token: "your-api-key"
  base_url: "https://your-api-endpoint.com/v1"
```

### Ollama 本地部署

```yaml
model:
  provider: "ollama"
  name: "llama3.1"
```

Ollama 使用默认地址 `http://localhost:11434/v1`，无需 API Key。如需自定义地址：

```bash
export OLLAMA_BASE_URL="http://localhost:11434/v1"
```

确保 Ollama 已启动：

```bash
ollama serve
ollama pull llama3.1
```

## 工作模式

通过 TUI 内的 `/mode` 命令切换：

| 模式 | 说明 |
|------|------|
| `general` | 通用助手（默认），适合日常问答和混合任务 |
| `coding` | 编程模式，专注代码阅读、生成、重构、调试 |
| `research` | 研究模式，适合信息检索、文档阅读、报告生成 |
| `data` | 数据分析模式，处理数据读取、统计、可视化 |
| `writing` | 写作模式，专注文章撰写、内容编辑、翻译 |

## 环境变量汇总

```bash
# ── 核心配置（优先级最高） ──
export ECHOCOWORK_AUTH_TOKEN="..."     # API 密钥
export ECHOCOWORK_BASE_URL="..."       # API 基础 URL
export ECHOCOWORK_MODEL="..."          # 模型名称

# ── Provider 专属 API Key ──
export DEEPSEEK_API_KEY="..."          # DeepSeek
export OPENAI_API_KEY="..."            # OpenAI
export ANTHROPIC_API_KEY="..."         # Anthropic
export DASHSCOPE_API_KEY="..."         # 阿里通义

# ── 其他 ──
export MCP_CONFIG_PATH="~/my-mcp-config.json"  # MCP 配置文件路径
export MODEL_NAME="deepseek-v4-flash"          # 模型名称（覆盖配置文件）
export HTTP_PROXY="http://proxy:8080"          # HTTP 代理
export HTTPS_PROXY="http://proxy:8080"         # HTTPS 代理
export RUST_LOG="info"                         # 日志级别
```
