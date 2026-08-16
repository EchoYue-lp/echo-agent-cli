# EKO 架构说明

## 整体架构

EKO 采用分层架构设计，分为三个主要层次：

```
┌─────────────────────────────────────────────────────────┐
│                    用户界面层                             │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  │
│  │     TUI      │  │  Tauri GUI   │  │   CLI Mode   │  │
│  │  (ratatui)   │  │   (React)    │  │  (REPL/Run)  │  │
│  └──────┬───────┘  └──────┬───────┘  └──────┬───────┘  │
└─────────┼──────────────────┼──────────────────┼─────────┘
          │                  │                  │
          └──────────────────┼──────────────────┘
                             │
┌────────────────────────────┼────────────────────────────┐
│                    核心应用层                             │
│                             ▼                            │
│              echo-agent-app-core                         │
│  ┌─────────────────────────────────────────────────┐    │
│  │  ┌──────────┐  ┌──────────┐  ┌──────────────┐  │    │
│  │  │AppState  │  │  Agent   │  │  Background  │  │    │
│  │  │ (多子域) │  │  Handle  │  │  TaskService │  │    │
│  │  └──────────┘  └──────────┘  └──────────────┘  │    │
│  │                                                  │    │
│  │  ┌──────────┐  ┌──────────┐  ┌──────────────┐  │    │
│  │  │ Sessions │  │Workspace │  │  Scheduler   │  │    │
│  │  │  Manager │  │ Registry │  │   Runner     │  │    │
│  │  └──────────┘  └──────────┘  └──────────────┘  │    │
│  │                                                  │    │
│  │  ┌──────────┐  ┌──────────┐  ┌──────────────┐  │    │
│  │  │  Skills  │  │  Webhook │  │  Trace/      │  │    │
│  │  │   Hub    │  │ Emitter  │  │  Observ.     │  │    │
│  │  └──────────┘  └──────────┘  └──────────────┘  │    │
│  └─────────────────────────────────────────────────┘    │
└────────────────────────────┬────────────────────────────┘
                             │
                             ▼
┌────────────────────────────────────────────────────────┐
│                    框架层 (echo-agent)                   │
│  ┌──────────┐  ┌──────────┐  ┌──────────────────────┐  │
│  │  React   │  │  Tools   │  │  MCP / LSP / Tasks   │  │
│  │  Agent   │  │  System  │  │  Integration         │  │
│  └──────────┘  └──────────┘  └──────────────────────┘  │
└────────────────────────────────────────────────────────┘
```

## 核心组件

### 1. State Manager (状态管理器)

负责管理应用的全局状态，按功能域拆分为子状态，使用 `Arc<RwLock<>>` 确保线程安全：

```rust
pub struct AppState {
    pub connection: ConnectionState,   // Agent 句柄 + HITL Dispatcher
    pub config: ConfigState,           // 应用 / Web / 沙箱 / 权限配置
    pub session: SessionState,         // 工具状态 + 取消令牌
    pub plugins: PluginState,          // MCP 服务管理
    pub storage: StorageState,         // 对话持久化存储
    pub history: HistoryState,         // 审计日志 + 工作流定义
    pub scheduler: SchedulerState,     // 定时任务调度
    pub tasks: TaskState,              // 后台任务系统
    pub webhook: WebhookState,         // Webhook 事件回调
    pub trace: TraceState,             // Trace 观测分析
    pub workspace: WorkspaceState,     // 工作区管理
    pub skills_hub: Arc<RwLock<SkillsHub>>, // 本地技能市场
}
```

**关键职责：**
- 管理 Agent 实例的生命周期
- 维护配置状态（应用配置、Web 配置、沙箱配置、权限规则）
- 协调各组件之间的通信
- 管理后台任务、定时任务、Webhook 事件

### 2. Agent Handle (Agent 句柄)

封装对 Agent 的并发访问，提供安全的异步接口（替代直接使用 `Arc<RwLock<ReactAgent>>`）：

```rust
pub struct AgentHandle {
    agent: Arc<RwLock<ReactAgent>>,
}

impl AgentHandle {
    // 同步读访问（闭包内不可 await）
    pub fn read<F, R>(&self, f: F) -> R where F: FnOnce(&ReactAgent) -> R;
    
    // 异步读访问（闭包内可 await，持有锁）
    pub async fn read_async<F, Fut, R>(&self, f: F) -> R;
    
    // 同步写访问
    pub fn write<F, R>(&self, f: F) -> R where F: FnOnce(&mut ReactAgent) -> R;
    
    // 异步写访问（闭包内可 await，持有锁）
    pub async fn write_async<F, Fut, R>(&self, f: F) -> R;
    
    // 尝试写访问（非阻塞）
    pub fn try_write<F, R>(&self, f: F) -> Option<R>;
}
```

**流式输出机制：**
- 使用 `tokio::sync::mpsc::unbounded_channel` 实现真正的增量式流式输出
- 后台任务获取 `RwLock<ReactAgent>` 并调用 `execute_stream()` / `chat_stream()`
- 逐事件通过 channel 发送，避免返回时持有锁

### 3. Task Manager (任务管理器)

管理后台任务的执行和调度，使用 `BackgroundTaskKind` 区分不同类型的任务：

```rust
pub enum BackgroundTaskKind {
    AgentChat { prompt: String, session_id: Option<String> },
    Cron { cron_expr: String, prompt: String },
    Workflow { workflow_id: String, input: Value },
    Research { topic: String, max_papers: usize, output_format: ResearchOutputFormat },
    ResearchToWriting { topic: String, max_papers: usize, audience: String, ... },
    DataPipeline { dataset_path: String, objective: Option<String>, max_charts: usize },
    Writing { prompt: String, config: WritingPipelineConfig },
}
```

**任务类型：**
- `AgentChat` — 单次对话
- `Cron` — 定时任务
- `Workflow` — 工作流编排
- `Research` — 学术研究流水线（论文检索 → 抓取 → 综合 → 撰写）
- `ResearchToWriting` — 研究到写作端到端流水线
- `DataPipeline` — 数据处理流水线（加载 → 分析 → 可视化 → 总结）
- `Writing` — 文档写作流水线

### 4. Session Manager (会话管理器)

管理多会话状态，支持会话持久化、分支和版本追踪：

```rust
pub struct Session {
    pub id: String,
    pub name: String,
    pub model: String,
    pub system_prompt: Option<String>,
    pub parent_id: Option<String>,      // 分支来源
    pub branch: Option<String>,         // 分支名称
    pub messages: Vec<SessionMessage>,
    pub tags: Vec<String>,
    pub message_count: usize,
    pub estimated_tokens: usize,
    pub created_at: String,
    pub updated_at: String,
}
```

**功能：**
- 创建/删除会话
- 会话切换和分支
- 会话历史持久化（SQLite）
- 会话全文搜索

### 5. Memory Store (记忆存储)

实现 Agent 的长期记忆能力，包含两个层次：

| 概念 | 来源 | 写入者 | 存储位置 | 用途 |
|------|------|--------|----------|------|
| **Instructions** | 用户维护 | 用户 | `user.md` / `project.md` / `local.md` | 告知 Agent 如何行为 |
| **Memories** | Agent 学习 | Agent (自动) | KV Store | Agent 积累的知识 |

```rust
pub enum InstructionTier {
    User,    // ~/.echo-agent/user.md
    Project, // <project>/.eko/project.md
    Local,   // <cwd>/.eko/local.md
}

pub struct UnifiedMemory { ... }

impl UnifiedMemory {
    pub fn load() -> Self;
    pub fn system_prompt_context(&self) -> MemoryContext;
    pub async fn remember(&self, fact: &str, importance: f64);
    pub async fn recall(&self, query: &str) -> Vec<MemoryEntry>;
}
```

**存储后端：**
- SQLite（默认）
- 向量数据库（可选）

### 6. Scheduler (调度器)

管理定时任务和后台作业：

```rust
pub struct SchedulerRunner {
    agent: AgentHandle,
    cancel: CancellationToken,
    store: TaskStore,
    task_service: Option<Arc<BackgroundTaskService>>,
}

impl SchedulerRunner {
    pub fn spawn(self: Arc<Self>);        // 启动后台调度循环
    pub fn add_cron(&self, expr, prompt); // 添加定时任务
    pub fn remove_cron(&self, id);        // 移除定时任务
}
```

## 工作区架构

工作区数据存储在 `~/.echo-agent/workspaces/` 下：

```
workspaces/
├── {workspace-id}/
│   └── .eko/
│       ├── sessions/         # 会话历史 (JSON)
│       ├── conversations/    # 对话记录
│       ├── memory/           # 记忆存储 (SQLite)
│       ├── tasks/            # 任务状态
│       ├── traces/           # 执行轨迹 (用于调试)
│       ├── logs/             # 日志
│       ├── data/             # 数据文件
│       ├── papers/           # 论文文件
│       ├── artifacts/        # 生成物
│       ├── scratchpad.md     # 共享草稿
│       └── workspace.json    # 工作区清单
```

## TUI 架构

TUI 使用 ratatui 构建，采用事件驱动架构：

```rust
pub enum Event {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Resize(u16, u16),
    Agent(AgentEvent),
    Task(TaskEvent),
    Tick,
}

pub struct App {
    state: AppState,
    ui_state: UIState,
    event_rx: mpsc::Receiver<Event>,
}
```

**状态机模式：**
```rust
pub enum UIState {
    Normal,
    Sidebar(SidebarTab),
    Modal(ModalType),
    Input(InputMode),
}
```

**主要组件：**
- `ChatPanel` — 聊天显示区域
- `InputPanel` — 输入框
- `Sidebar` — 侧边栏（会话列表、工具列表）
- `Modal` — 模态对话框（确认、设置）

## Tauri GUI 架构

GUI 使用 Tauri + React 构建：

```
┌─────────────────────────────────────┐
│         Tauri Application           │
│  ┌───────────────────────────────┐  │
│  │      React Frontend           │  │
│  │  ┌─────────┐  ┌───────────┐  │  │
│  │  │ Chat UI │  │ Sidebar   │  │  │
│  │  └────┬────┘  └─────┬─────┘  │  │
│  │       │              │         │  │
│  │       └──────┬───────┘         │  │
│  │              │                 │  │
│  │         IPC Bridge             │  │
│  └──────────────┼─────────────────┘  │
│                 │                    │
│                 ▼                    │
│         Rust Backend                 │
│    echo-agent-app-core               │
└─────────────────────────────────────┘
```

**IPC 通信：**
```typescript
// 前端调用
const response = await invoke('chat', { message: 'Hello' });

// 后端处理
#[tauri::command]
async fn chat(message: String, state: State<'_, AppState>) -> Result<String> {
    state.agent.chat(message).await
}
```

## 并发模型

### 1. 异步运行时

使用 `tokio` 作为异步运行时：

```rust
#[tokio::main]
async fn main() -> Result<()> {
    // 启动 TUI 或 GUI
}
```

### 2. 锁策略

- **RwLock**: 用于读多写少的场景（Agent、Config）
- **Mutex**: 用于互斥访问（Task 执行）
- **DashMap**: 用于并发 HashMap（Session 存储）

### 3. 消息传递

- **mpsc channel**: 用于事件流（Agent 输出、TUI 事件）
- **broadcast channel**: 用于多订阅者（任务状态更新）

## 工具系统

### 内置工具

EKO 继承 echo-agent 的 67+ 内置工具：

```rust
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn execute(&self, args: Value) -> BoxFuture<Result<Value>>;
}
```

**工具分类：**
- 文件操作: read, write, edit, list, glob
- Shell 执行: bash, powershell
- 网络请求: http, websocket, fetch
- 数据库: sqlite, postgresql, mysql
- 浏览器: playwright
- 版本控制: git
- 系统: process, env

### MCP 集成

通过 MCP (Model Context Protocol) 扩展工具：

```rust
pub struct McpClient {
    servers: Vec<McpServer>,
}

impl McpClient {
    pub async fn connect(&mut self, config: McpConfig) -> Result<()> {
        // 启动 MCP 服务器进程
        // 通过 stdio/HTTP 通信
    }
}
```

## 安全机制

### 1. 人机协作 (Human-in-the-Loop)

高风险操作需要用户确认：

```rust
pub trait HumanLoop: Send + Sync {
    fn request_approval(&self, action: Action) -> BoxFuture<bool>;
}
```

**风险等级：**
- **Low**: 自动执行（读取文件、查询）
- **Medium**: 可选确认（写入文件）
- **High**: 强制确认（执行命令、删除文件）

### 2. 沙箱执行

- Shell 命令在受限环境中执行
- 文件系统访问受白名单控制
- 网络请求受代理和超时限制

### 3. 审计日志

所有操作记录到 traces/ 目录，用于：
- 调试和问题排查
- 合规性审计
- 性能分析

## 性能优化

### 1. 流式输出

- 使用 channel 实现真正的增量式流式输出
- 避免等待完整响应后再返回
- 减少内存占用

### 2. 缓存

- 工具执行结果缓存
- 配置缓存
- 会话历史缓存

### 3. 并发控制

- 使用 Semaphore 限制并发任务数
- 任务队列避免资源耗尽
- 优雅降级处理高负载

## 扩展性

### 1. 插件系统

采用根 `plugin.json` 和固定扁平组件位置。框架负责 `skills/`、`mcp.json`、
`agents/`、`hooks/hooks.yaml` 与 `lsp.yaml`；应用层只转换 `monitors.yaml`、
`themes/` 和 `output-styles/`
中的 monitors、themes 与 output styles。三种交互界面共享同一个
`PluginRuntimeService`，插件启停和 reload 会精确替换其运行时组件。

### 2. 自定义工具

实现 `Tool` trait 即可添加自定义工具：

```rust
pub struct MyTool;

impl Tool for MyTool {
    fn name(&self) -> &str { "my_tool" }
    fn execute(&self, args: Value) -> BoxFuture<Result<Value>> {
        // 实现逻辑
    }
}
```

### 3. 自定义模式

通过配置文件定义新的工作模式，无需修改代码。
