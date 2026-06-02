# EchoCoWork 快速入门

## 安装

### 1. 安装 Rust 工具链

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

### 2. 克隆仓库

```bash
git clone <repository-url>
cd echo-agent-cli
```

### 3. 安装依赖

```bash
# Rust 依赖
cargo fetch

# 前端依赖（如果需要使用 GUI 模式）
cd web-frontend && npm install && cd ..
```

## 首次配置

### 运行初始化向导

```bash
cargo run --bin echo-agent-cli -- onboard
```

向导将帮助你：
- 创建数据目录 `~/.echo-agent/`
- 配置 LLM API Key
- 选择默认模型
- 选择工作模式（general/coding/research/data/writing）

### 手动配置 API Key

你也可以直接在 `~/.echo-agent/config.yaml` 中配置：

```yaml
model:
  name: "qwen-plus"
  
agent:
  name: "echo-assistant"
  system_prompt: "你是一个智能助手。"
```

设置环境变量：

```bash
export DASHSCOPE_API_KEY="your-api-key"
# 或
export OPENAI_API_KEY="your-api-key"
# 或
export ANTHROPIC_API_KEY="your-api-key"
```

## 基本使用

### 启动 TUI 交互界面

```bash
cargo run --bin echo-agent-cli
```

或者直接运行已安装的版本：

```bash
echo-agent-cli
```

### 启动 GUI 桌面应用

```bash
# 需要先构建前端
cd web-frontend && npm run build && cd ..

# 启动 Tauri 应用
cargo run --bin echo-agent-tauri
```

### 命令行模式

```bash
# 单次对话（run 子命令）
echo-agent-cli run "解释这段代码"

# Headless 模式（适合 CI/CD，非交互式执行）
echo-agent-cli --headless "写一个快速排序并测试"

# 指定模型
echo-agent-cli --model qwen-max run "写一个快速排序"

# 使用指定配置
echo-agent-cli --config ./my-config.yaml run "分析这段代码"

# 继续最近一次会话
echo-agent-cli --continue

# 恢复指定会话
echo-agent-cli --resume <session-id>
```

## 核心功能

### 1. 会话管理

在 TUI 中可使用以下 slash 命令管理会话：

- `/new` - 创建新会话
- `/reset` - 重置当前会话历史
- `/history` - 查看会话历史
- `/stats` - 显示会话统计
- `/status` - 显示当前状态
- `/compact` - 压缩上下文窗口

通过 CLI 子命令管理会话持久化数据：

```bash
echo-agent-cli sessions list          # 列出所有会话
echo-agent-cli sessions show <id>     # 查看会话详情
echo-agent-cli sessions export <id>   # 导出会话
echo-agent-cli sessions delete <id>   # 删除会话
```

### 2. 模式切换

```
/mode general     # 通用模式（默认）
/mode coding      # 编程模式
/mode research    # 研究模式
/mode data        # 数据分析模式
/mode writing     # 写作模式
```

### 3. 工具使用

EchoCoWork 内置了 67+ 工具，包括：

- **文件操作**: read, write, edit, list
- **Shell 执行**: bash, powershell
- **网络请求**: http, websocket
- **数据库**: sqlite, postgresql, mysql
- **浏览器自动化**: playwright
- **版本控制**: git

示例：

```
请读取当前目录下的 README.md 文件
```

### 4. 人机协作

某些高风险操作（如文件写入、命令执行）会请求你的确认：

- **y** - 批准执行（Approve）
- **n** - 拒绝执行（Deny）

### 5. 记忆系统

EchoCoWork 支持长期记忆：

- 自动记录重要信息
- 跨会话保持上下文
- 存储在 `~/.echo-agent/memory/`

## 配置示例

### 完整配置文件

```yaml
# ~/.echo-agent/config.yaml（或 ./echo-agent.yaml）

model:
  name: "qwen-plus"
  # max_tokens: 4096          # 可选
  # temperature: 0.7          # 可选

agent:
  name: "echo-assistant"
  system_prompt: "你是一个智能助手，可以帮助用户回答问题、执行任务。"
  max_iterations: 0            # 0 = 无限制
  enable_tools: true
  enable_memory: true
  enable_human_in_loop: true
  memory_path: "~/.echo-agent/memory"
  # token_limit: 0             # 上下文自动压缩阈值（0 = 禁用）
  # compress_strategy: sliding # 压缩策略: sliding / summary / hybrid
  # compress_window: 20        # 滑动窗口大小

mcp:
  # config_path: "mcp.json"   # MCP 配置文件路径
  # 默认搜索: ./mcp.json → ~/.echo-agent/mcp.json

logging:
  level: "info"               # 日志级别: trace / debug / info / warn / error
```

## 常见问题

### Q: 如何选择模型？

推荐使用：
- **编程任务**: qwen-max, gpt-4, claude-3-5-sonnet
- **通用对话**: qwen-plus, gpt-3.5-turbo
- **本地部署**: ollama/llama3.1

### Q: API Key 在哪里获取？

- **阿里通义**: https://dashscope.console.aliyun.com/
- **OpenAI**: https://platform.openai.com/api-keys
- **Anthropic**: https://console.anthropic.com/

### Q: 如何添加自定义工具？

通过 MCP (Model Context Protocol) 服务器添加工具。在 `mcp.json` 文件中配置 MCP 服务器（默认搜索路径: `./mcp.json` → `~/.echo-agent/mcp.json`），详细格式参见 [配置指南](./configuration.md#mcp-服务器)。

### Q: 数据存储在哪里？

所有数据存储在 `~/.echo-agent/` 目录：
- `config.yaml` - 配置文件
- `memory/` - 记忆存储
- `logs/` - 日志文件
- `workspaces/` - 工作区数据

## 下一步

- 阅读 [配置指南](./configuration.md) 了解详细配置选项
- 查看 [架构说明](./architecture.md) 理解系统设计
- 浏览 [echo-agent 文档](../echo-agent/README.md) 了解底层框架
