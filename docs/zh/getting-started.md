# EKO 快速入门

EKO 以 GUI 为产品主入口，继续保留 IM 收发和 cron 后台触发。独立 TUI、REPL、CLI/JSONL
及 standalone channels 产品入口已退出；GUI 内置终端、Agent shell、桌面最小参数和原共享
任务/会话恢复继续保留。包名、identifier和数据路径不随入口或品牌改名。

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

桌面开发与打包需要前端依赖；裸后端不能替代包含资源的原生安装包。

## 配置模型

GUI 用户在“设置 -> 模型 Provider”中：

1. 新建 Provider，填写 API 根地址和认证方式。
2. 在 Provider 下添加模型。
3. 为每个模型明确选择 `chat_completions`、`responses` 或 `anthropic`。
4. 选择输入能力并设为默认模型。

也可在原配置位置准备 `./eko.yaml` 或 `~/.eko/config.yaml`。最小示例：

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

Rust `tauri` 与前端 `@tauri-apps/api` 必须保持同一 minor 版本；当前两个锁文件中的
版本均为 `2.12.4`，前端同时固定该精确版本。安装依赖时保留 `package-lock.json`，
不要绕过 Tauri 的版本匹配检查。规则依据见[官方依赖更新说明](https://v2.tauri.app/develop/updating-dependencies/)。

打包工具固定使用 Tauri CLI `2.12.4`。Cargo 的 `cargo tauri` 与官方 npm 的 `tauri`
是两个独立安装的入口，构建前须核对实际入口的 `--version`。旧 `2.11.2` 桌面构建会
注入已废弃的 `STATIC_VCRUNTIME`；新 CLI 使用官方配置和默认值，无须修改全局环境。

Cargo默认运行项与唯一产品binary声明均为`echo-agent-tauri`；独立`echo-agent-cli`
binary已移除，Rust package名称仍保留。构建和运行桌面继续显式使用`gui`及原alias。
入口配置修正不等于新包已验收，实际 App 还须核对主入口、内嵌前端并完成原生运行验证。

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

## 桌面参数与保留后台能力

`echo-agent-tauri` 只接受 `--model`（含 `-m`／`MODEL_NAME`）、`--project`、`--config` 和
`--mcp-config`；Finder的`-psn_`元数据继续过滤。旧console交互、JSONL、resume、权限和渲染
参数不作为桌面模式入口。GUI、IM、cron继续使用原Task/Subagent、HITL、MCP、Browser、
Plugin、Skill、Memory、附件与共享恢复；隐藏窗口不停止这些后台owner，真正退出等待原结算。

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
