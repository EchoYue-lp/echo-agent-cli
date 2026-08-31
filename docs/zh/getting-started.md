# EKO 快速入门

## 前置条件

- Rust 1.95 或更高版本
- Node.js 20.19+、22.13+ 或 24+（仅 GUI 开发/打包需要）
- macOS/Linux/Windows 对应的 Tauri 2 系统依赖（仅 GUI 需要）
- 至少一个可用 LLM Provider 与模型

## 获取依赖

```bash
cd echo-agent-cli
cargo fetch

cd web-frontend
npm install
```

只使用 TUI 或 JSONL 时可以跳过前端依赖。

## 配置模型

GUI 用户在“设置 -> 模型 Provider”中：

1. 新建 Provider，填写 API 根地址和认证方式。
2. 在 Provider 下添加模型。
3. 为每个模型明确选择 `chat_completions`、`responses` 或 `anthropic`。
4. 选择输入能力并设为默认模型。

TUI/CLI 用户可以准备 `./eko.yaml` 或 `~/.eko/config.yaml`。最小示例：

```yaml
model:
  default_model_id: "my-provider:my-model"

model_providers:
  my-provider:
    name: "My Provider"
    base_url: "https://api.example.com/v1"
    api_key_env: "MY_PROVIDER_API_KEY"
    requires_api_key: true
    default_api_protocol: "responses"

configured_models:
  - id: "my-provider:my-model"
    display_name: "My Model"
    provider: "my-provider"
    model: "my-model"
    api_protocol: "responses"
    input_modalities: ["text"]
    enabled: true
```

```bash
export MY_PROVIDER_API_KEY="..."
```

完整字段见 [配置指南](./configuration.md)。

## 启动 TUI

```bash
cargo run --bin echo-agent-cli
```

常用参数：

```bash
cargo run --bin echo-agent-cli -- --project /path/to/project
cargo run --bin echo-agent-cli -- --model my-provider:my-model
cargo run --bin echo-agent-cli -- --continue
cargo run --bin echo-agent-cli -- --resume <conversation-id>
```

TUI 是完整 Agent surface，支持 TaskRun、Subagent、HITL、MCP、Browser、Plugin、Skill、
Memory 和附件，不是 GUI 的精简版。输入 `/help` 查看当前代码注册的命令，避免依赖
静态命令清单。

## 启动 GUI

```bash
cargo gui-dev
```

等价命令：

```bash
cargo tauri dev -- --no-default-features --features gui --bin echo-agent-tauri
```

生产打包：

```bash
cargo gui-bundle
```

不要把裸 `target/release/echo-agent-tauri` 当作桌面安装包；Tauri bundle 才包含前端
资源、图标和平台元数据。

## 非交互 JSONL

```bash
cargo run --bin echo-agent-cli -- --jsonl "检查当前项目并给出结论"
```

stdout 每行是 canonical chat envelope，日志写到 stderr/文件，适合脚本消费。

## MCP 与 Browser

默认 MCP 文件是 `~/.eko/mcp.json`：

```json
{
  "mcpServers": {
    "example": {
      "command": "npx",
      "args": ["-y", "example-mcp-server"],
      "disabled": false
    }
  }
}
```

优先级为 `--mcp-config`、YAML `mcp.config_path`、`MCP_CONFIG_PATH`、
`~/.eko/mcp.json`。

托管 Browser 默认通过 `@playwright/mcp` 启动，需要可用的 Node/npm/npx。需要现有
Chrome 登录态时启用 Playwright Extension backend；详见[配置指南](./configuration.md)。

## 数据位置

EKO 默认使用 `~/.eko/`，可通过 `EKO_DATA_DIR` 覆盖。每个 workspace 的会话、任务、
记忆、artifact 和 trace 位于 workspace 根的 `.eko/` 下。EKO 不使用 SQLite。

## 下一步

- [功能总览](./features.md)
- [架构说明](./architecture/overview.md)
- [Skill 同步](./operations/skill-sync.md)
