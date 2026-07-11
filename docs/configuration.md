# EKO 配置指南

## 配置文件位置

EKO 按以下优先级查找配置文件：

1. 命令行参数: `--config <path>`
2. 当前目录: `./echo-agent.yaml`
3. 项目目录: `./.echo-agent/echo-agent.yaml`
4. 用户目录: `~/.echo-agent/config.yaml`

## 快速配置

GUI 用户不需要手写 YAML。打开 **设置 → 模型供应商**：

1. 选择厂商。
2. 填写 API Key、模型名和可选 API 地址。
3. 点击“保存并使用”。
4. 在聊天输入框底部的“默认模型”下拉框中切换已配置模型。

用户填写的 API Key 优先级高于系统环境变量。CLI/TUI 会读取同一份已配置默认模型。

## 完整配置文件（echo-agent.yaml）

将以下内容复制到 `~/.echo-agent/config.yaml` 或项目根目录的 `echo-agent.yaml`：

```yaml
# ── 模型配置 ─────────────────────────────────────────────────────
# 推荐通过 GUI 的“模型供应商”页面维护。YAML 适合自动化部署。
model:
  default_model_id: "deepseek:deepseek-v4-flash"
  provider: "deepseek"        # 运行时镜像字段，由“模型供应商”保存默认模型时同步
  name: "deepseek-v4-flash"   # 运行时镜像字段，不建议手动编辑
  max_tokens: 8192
  temperature: 0.7

model_providers:
  deepseek:
    auth_token: "your-api-key"
    base_url: "https://api.deepseek.com/chat/completions"

configured_models:
  - id: "deepseek:deepseek-v4-flash"
    display_name: "Deepseek V4 Flash"
    provider: "deepseek"
    model: "deepseek-v4-flash"
    enabled: true
    max_tokens: 8192
    temperature: 0.7

# ── Agent 配置 ─────────────────────────────────────────────────────
agent:
  name: "echo-assistant"                              # Agent 名称
  system_prompt: "你是 EKO，一个面向真实项目协作的 AI 编程与研究代理。你帮助用户理解需求、检查项目、修改代码、运行验证、整理结论，并在需要时使用记忆和自进化能力沉淀长期经验。"  # 系统提示词
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

# ── 研究与医学 API 配置 ─────────────────────────────────────────
# 学术研究和医学模式使用的工具 — 全部免费，无需 API Key 即可使用
#
# 学术研究工具（Research 模式）:
#   - arxiv_search            → 免费，搜索 ArXiv 预印本论文
#   - semantic_scholar_search  → 免费，搜索 Semantic Scholar 学术数据库
#   - pdf_fetch               → 免费，下载和解析 PDF 论文全文
#   - web_search              → 免费（DuckDuckGo），可选 API Key 提升质量
#   - web_fetch               → 免费，抓取网页内容
#   - bibtex_generate         → 本地工具，生成 BibTeX 引用
#
# 医学研究工具（Medical 模式）:
#   - pubmed_search           → 免费，搜索 PubMed 医学文献（NCBI）
#   - clinical_trials_search  → 免费，搜索 ClinicalTrials.gov 临床试验
#   - pdf_fetch               → 免费（部分论文需机构网络访问）
#   - web_search              → 免费（DuckDuckGo）
#   - web_fetch               → 免费
#   - bibtex_generate         → 本地工具
#
# 可选 API Key（提升搜索质量，非必需）:
#
# web_search 可选升级（优先级：Tavily > Brave > DuckDuckGo）:
#   export TAVILY_API_KEY="your-key"         # 推荐，AI 优化搜索，免费 1000 次/月
#                                          # 申请：https://tavily.com/
#   export BRAVE_SEARCH_API_KEY="your-key"   # 备选，免费 2000 次/月
#                                          # 申请：https://brave.com/search/api/

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
      "args": ["-y", "@playwright/mcp@latest"],
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

### 托管浏览器

EKO 默认在应用启动后后台预热 `@playwright/mcp@latest`，无需把 Playwright
写入 `mcp.json`。需要 Node.js 18 或更高版本以及可用的 `npm`/`npx`。浏览器
Profile 和输出默认位于 `~/.echo-agent/browser/`。

可用环境变量：

```bash
EKO_BROWSER_ENABLED=true
EKO_BROWSER_HEADLESS=false
EKO_BROWSER_PROFILE_DIR=~/.echo-agent/browser/profiles/managed
EKO_BROWSER_OUTPUT_DIR=~/.echo-agent/browser/output
EKO_BROWSER_STARTUP_TIMEOUT_SECS=60
EKO_CHROME_ENABLED=true
# Optional: pin the desktop bridge to the unpacked/Web Store extension id.
EKO_CHROME_EXTENSION_ID=abcdefghijklmnopabcdefghijklmnop
```

保留手动 MCP 配置兼容路径。希望完全使用自定义 Playwright MCP 配置时，可设
`EKO_BROWSER_ENABLED=false`，避免同时启动托管 sidecar。

### Chrome 登录态模式

需要现有 Chrome 登录态时，可加载仓库 `chrome-extension/` 下的 Manifest V3
扩展，并通过桌面命令 `chrome_install_native_host` 注册 Native Messaging Host。
扩展 popup 必须由用户授权当前站点和标签页；普通公开网页与 localhost 仍默认
使用托管 Chromium。Chrome 模式不会读取 Cookie、密码、浏览历史或 Profile
文件。可用 `EKO_CHROME_ENABLED=false` 单独关闭扩展 bridge，而不影响托管浏览器。

## 模型配置详解

### 支持的模型提供商

| 提供商 | 模型名称示例 | 环境变量 |
|--------|-------------|---------|
| DeepSeek | deepseek-v4-flash, deepseek-v4-pro | DEEPSEEK_API_KEY |
| OpenAI | gpt-5.5 | OPENAI_API_KEY |
| Anthropic | claude-opus-4-8, claude-opus-4-7 | ANTHROPIC_API_KEY |
| 阿里通义 | qwen3.7-max, qwen3.6-plus | DASHSCOPE_API_KEY |
| Moonshot | kimi-k2.6 | MOONSHOT_API_KEY |
| 智谱 | glm-5.1 | ZHIPU_API_KEY |
| Gemini | gemini-3.5-flash | GEMINI_API_KEY |
| Ollama (本地) | llama3.1, codellama, mistral | 无需 API Key |

### 切换模型示例

### 模型供应商配置

推荐在 GUI 中配置。需要手写 YAML 时，按下面结构添加到 `model_providers` 和 `configured_models`。

**DeepSeek：**
```yaml
model:
  default_model_id: "deepseek:deepseek-v4-flash"

model_providers:
  deepseek:
    auth_token: "your-api-key"
    base_url: "https://api.deepseek.com/chat/completions"

configured_models:
  - id: "deepseek:deepseek-v4-flash"
    display_name: "Deepseek V4 Flash"
    provider: "deepseek"
    model: "deepseek-v4-flash"
    enabled: true
```

**OpenAI：**
```yaml
model:
  default_model_id: "openai:gpt-5.5"

model_providers:
  openai:
    auth_token: "your-api-key"
    base_url: "https://api.openai.com/v1/chat/completions"

configured_models:
  - id: "openai:gpt-5.5"
    display_name: "Gpt 5.5"
    provider: "openai"
    model: "gpt-5.5"
    enabled: true
```

**Anthropic Claude：**
```yaml
model:
  default_model_id: "anthropic:claude-opus-4-8"

model_providers:
  anthropic:
    auth_token: "your-api-key"
    base_url: "https://api.anthropic.com/v1/messages"

configured_models:
  - id: "anthropic:claude-opus-4-8"
    display_name: "Claude Opus 4 8"
    provider: "anthropic"
    model: "claude-opus-4-8"
    enabled: true
```

**自定义 Provider：**
```yaml
model:
  default_model_id: "custom:your-model-name"

model_providers:
  custom:
    auth_token: "your-api-key"
    base_url: "https://your-api-endpoint.com/v1/chat/completions"

configured_models:
  - id: "custom:your-model-name"
    display_name: "Your Model Name"
    provider: "custom"
    model: "your-model-name"
    enabled: true
```

### Ollama 本地部署

```yaml
model:
  default_model_id: "ollama:llama3.1"

configured_models:
  - id: "ollama:llama3.1"
    display_name: "Llama3.1"
    provider: "ollama"
    model: "llama3.1"
    enabled: true
```

Ollama 使用默认地址 `http://localhost:11434/api/chat`，无需 API Key。

确保 Ollama 已启动：

```bash
ollama serve
ollama pull llama3.1
```

## 学术研究与医学模式

### 快速入门

学术研究和医学模式的工具**全部免费**，无需任何 API Key 即可使用。启动后即可直接检索文献：

```bash
# 启动 TUI（默认 coding 模式）
echo-agent-cli

# 切换到研究模式
/mode research

# 或切换到医学模式
/mode medical
```

### 工具一览

| 工具 | 模式 | 数据来源 | API Key | 说明 |
|------|------|---------|---------|------|
| `arxiv_search` | Research | ArXiv.org | 不需要 | 搜索预印本论文，返回标题、作者、摘要、PDF 链接 |
| `semantic_scholar_search` | Research | SemanticScholar.org | 不需要 | 搜索学术论文，返回引用数、研究领域、DOI |
| `pubmed_search` | Medical | PubMed (NCBI) | 不需要 | 搜索医学文献，返回 PMID、MeSH 词、摘要 |
| `clinical_trials_search` | Medical | ClinicalTrials.gov | 不需要 | 搜索临床试验，返回 NCT ID、状态、阶段、结局 |
| `pdf_fetch` | 两者 | 各学术网站 | 不需要 | 下载并解析 PDF 全文（部分论文需机构网络） |
| `web_search` | 两者 | DuckDuckGo | 不需要 | 网络搜索（可升级为 Tavily/Brave 提升质量） |
| `web_fetch` | 两者 | 各网站 | 不需要 | 抓取网页内容并提取正文 |
| `bibtex_generate` | 两者 | 本地生成 | 不需要 | 根据论文信息生成 BibTeX 引用格式 |

### 可选：提升搜索质量

`web_search` 默认使用 DuckDuckGo（免费、无需配置），可通过环境变量升级为更高质量的搜索引擎：

```bash
# 方式 1（推荐）：Tavily — AI 优化搜索，免费 1000 次/月
# 申请：https://tavily.com/
export TAVILY_API_KEY="tvly-your-key-here"

# 方式 2：Brave Search — 免费 2000 次/月
# 申请：https://brave.com/search/api/
export BRAVE_SEARCH_API_KEY="BSAyour-key-here"
```

优先级：Tavily > Brave > DuckDuckGo（自动选择可用的最佳引擎）。

### 使用示例

**学术研究模式：**

```
> /mode research

> 搜索 2024 年大语言模型在医学领域的最新研究
Agent: 调用 arxiv_search + semantic_scholar_search 交叉检索...

> 下载第一篇论文的全文
Agent: 调用 pdf_fetch 下载并解析...

> 生成这些论文的 BibTeX 引用
Agent: 调用 bibtex_generate...
```

**医学研究模式：**

```
> /mode medical

> 搜索 CRISPR 基因治疗在肿瘤免疫中的最新临床试验
Agent: 调用 pubmed_search + clinical_trials_search 交叉检索...

> 搜索 2023-2025 年发表的 PD-1 抑制剂 meta-analysis
Agent: 调用 pubmed_search(query='PD-1 inhibitor meta-analysis', min_date='2023/01/01')...

> 下载 PMID 为 38245678 的论文全文
Agent: 调用 pdf_fetch...
```

## 工作模式

通过 TUI 内的 `/mode` 命令切换：

| 模式 | 说明 |
|------|------|
| `general` | 通用助手（TUI 默认），适合日常问答和混合任务 |
| `coding` | 编程模式，专注代码阅读、生成、重构、调试 |
| `research` | 研究模式，适合信息检索、文档阅读、报告生成 |
| `medical` | 医学研究模式，PubMed 文献检索、临床试验、循证医学分析 |
| `data` | 数据分析模式，处理数据读取、统计、可视化 |
| `writing` | 写作模式，专注文章撰写、内容编辑、翻译 |

## 环境变量汇总

```bash
# ── Provider 专属 API Key（兜底） ──
# GUI/YAML 中保存的 API Key 优先级更高；环境变量适合 CI 或无 GUI 环境。
export DEEPSEEK_API_KEY="..."          # DeepSeek
export OPENAI_API_KEY="..."            # OpenAI
export ANTHROPIC_API_KEY="..."         # Anthropic
export DASHSCOPE_API_KEY="..."         # 阿里通义
export QWEN_API_KEY="..."              # 阿里通义别名
export GEMINI_API_KEY="..."            # Gemini

# ── 其他 ──
export MCP_CONFIG_PATH="~/my-mcp-config.json"  # MCP 配置文件路径
export MODEL_NAME="deepseek-v4-flash"          # 模型名称（覆盖配置文件）
export TAVILY_API_KEY="..."                    # Web Search（可选，AI 优化搜索）
export BRAVE_SEARCH_API_KEY="..."              # Web Search（可选，备选引擎）
export HTTP_PROXY="http://proxy:8080"          # HTTP 代理
export HTTPS_PROXY="http://proxy:8080"         # HTTPS 代理
export RUST_LOG="info"                         # 日志级别
```
