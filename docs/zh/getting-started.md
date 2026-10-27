# EKO 快速入门

EKO 推荐从 GUI 开始，继续保留 IM 收发和 cron 后台触发。独立 TUI、REPL、CLI/JSONL
入口目前仍在源码中，已进入退役范围；本页保留其现有命令供迁移期间使用，不再承诺长期
入口能力对等。GUI 内置终端、Agent shell 和桌面构建命令继续保留。

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

迁移期间只使用现存 TUI 或 JSONL 入口时可以跳过前端依赖。

## 配置模型

GUI 用户在“设置 -> 模型 Provider”中：

1. 新建 Provider，填写 API 根地址和认证方式。
2. 在 Provider 下添加模型。
3. 为每个模型明确选择 `chat_completions`、`responses` 或 `anthropic`。
4. 选择输入能力并设为默认模型。

迁移期间仍使用 TUI/CLI 的用户可以准备 `./eko.yaml` 或 `~/.eko/config.yaml`。最小示例：

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

## 启动 GUI

这是当前推荐的产品入口：

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

GUI 中的工作空间就是项目。“新建任务”在所选工作空间打开会话草稿，不创建目录。
工作空间菜单支持选择已有本地文件夹、新建文件夹，或打开 SSH 设备与远程项目目录。

首页以“把想法变成成果”为主标题，保留原输入区，不再展示“继续上次对话”卡片或自动查询
最后会话。多任务并行时，从侧栏选择明确的任务继续；普通会话记录、按所选会话恢复和
工作空间切换仍使用原入口，不由首页替用户选择“上次”。普通对话分叉从已完成回复下方
进入，顶部不再重复提供“新建支线对话”按钮。首页与侧栏的品牌图标沿用同一
Eclip 素材。源码更新需要后续前端/桌面构建加载，不表示正在运行的旧窗口已采用新界面。

对话运行时仍可追加输入。“已收到”表示输入已保存，输入框会清除本次仍未改动的文字
和附件，不必等待模型或工具；它不表示输入已经执行或任务已经完成。准备或准入失败
会保留原稿，切换任务或编辑后的迟到结果不会清掉新稿。停止仍使用独立的停止操作。

## 启动 TUI

以下命令对应仍存在、待退役的独立终端入口：

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

现存 TUI 仍复用 TaskRun、Subagent、HITL、MCP、Browser、Plugin、Skill、Memory 和附件
权威；这些共享能力继续保留，独立终端入口的长期扩张与对等目标已由
[GUI 产品收敛边界](./adr/0050-gui-product-convergence.md)替代。输入 `/help` 可查看现有注册命令。

## 非交互 JSONL

该独立产品入口同样待退役，尚未从实现中删除：

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
