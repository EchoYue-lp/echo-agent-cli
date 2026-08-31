# ADR-0002：Codex 工具能力目录与 EKO 参考设计

> 状态：Proposed
>
> 日期：2026-08-24
>
> 范围：Codex Desktop 当前会话可观察的工具入口、参数边界、触发条件、权限语义和 EKO
> 未来工具层的参考设计。
>
> 证据边界：本文优先记录当前会话工具 schema 与宿主行为。Codex Desktop 的工具集合会随
> App 版本、运行宿主、模式、插件、MCP 和权限策略变化；本文不是永久固定的公共 API。

## 1. 结论摘要

Codex 的“工具”不是一个平坦列表，而是多个能力层叠加：

```text
宿主编排包装器
  ├── functions.exec / functions.wait
  ├── collaboration.*
  └── multi-tool orchestration

直接运行时工具
  ├── Shell / file
  ├── plan / goal
  ├── MCP / Node REPL
  ├── Web
  └── Codex App control plane

扩展能力（不等于直接工具入口）
  ├── Skills
  ├── Plugins
  ├── MCP servers / resources
  └── AGENTS.md / project config / hooks
```

用户提供的历史枚举是 **40 个入口**：

```text
2 个编排入口
+ 6 个子智能体协作入口
+ 32 个直接运行时入口
= 40
```

在 2026-08-24 当前会话重新读取 `ALL_TOOLS` 后，直接运行时入口为 34 个，新增：

- `codex_app__list_archived_threads`
- `codex_app__share_thread`

此外，当前会话还有两个不在 `ALL_TOOLS` 中的直接入口：

- `multi_tool_use.parallel`：并行调用多个开发者工具；
- `image_gen.imagegen`：生成或编辑位图。

所以按“模型当前能够直接触发的全部入口”口径，Default 模式的当前观察值是 **44 个**。
`request_user_input` 已声明但 Default 模式不可调用；切换到 Plan 模式后可视为第 45 个入口。
这个数字差异是能力快照和计数口径差异，不是功能矛盾。

## 2. 计数和可见性规则

### 2.1 四个概念必须分开

| 概念                 | 含义                                        | 是否一定可由当前 Agent 调用        |
| -------------------- | ------------------------------------------- | ---------------------------------- |
| Tool schema entry    | 当前宿主给模型的可调用函数定义              | 只有在当前工具目录中才可调用       |
| Nested/deferred tool | 通过编排工具或宿主动态解析的工具            | 取决于当前 turn 和宿主能力         |
| Skill / Plugin       | 提示词、脚本、资源、工具和配置的工作流包    | 不等于一个函数入口                 |
| MCP capability       | MCP server 暴露的工具、resource 或 template | 需要 server 已配置、连接且获准使用 |

因此不能用“安装了插件”推断“模型一定能调用插件中的每个工具”，也不能用“某个工具在
其他会话出现过”推断它在当前会话可见。

### 2.2 当前会话的实际边界

当前会话可通过 `ALL_TOOLS` 看到的直接运行时入口为 34 个；加上 6 个 collaboration 入口、
`functions.exec` / `functions.wait`、`multi_tool_use.parallel` 和 `image_gen.imagegen`，Default
模式共 44 个。`ALL_TOOLS` 不一定包含所有外层包装器，也不代表所有可延迟解析的 nested
tool。

工具可见性受以下因素影响：

- Desktop App 与 CLI/API/其他宿主的产品形态；
- Default/Plan 等运行模式；
- 当前模型和 reasoning effort；
- 项目、workspace、host 和 worktree；
- 插件、Skills、MCP server 的安装与启用状态；
- 用户、管理员或项目配置的权限策略；
- 当前任务是否允许创建、写入、交接或调度其他任务。

### 2.3 工具名称不是授权

工具 schema 只说明“存在一个可能的操作入口”，不等于当前调用已被授权。每次操作还必须
通过目标、宿主、项目、工作区、用户意图、sandbox、permission profile 和 approval policy
等门禁。

## 3. 编排与子智能体协作

### 3.1 `multi_tool_use.parallel`

并行调用多个互不依赖的开发者工具，减少串行往返延迟。

**参数**：

```json
{
  "tool_uses": [
    {
      "recipient_name": "functions.some_tool",
      "parameters": { "key": "value" }
    }
  ]
}
```

它只能包装开发者消息中声明的工具，不能包装 system tool，也不能因为并行包装而绕过任一
子工具自己的权限和参数校验。存在数据依赖、共享写入或顺序要求时不得并行。

### 3.2 `functions.exec`

`functions.exec` 是宿主编排入口，不是业务工具。它在一个 JavaScript isolate 中编排并调用
开发者工具，可并行或串行执行。主要能力包括：

- 从 `tools` 对象发现和调用嵌套工具；
- `text()`、`image()`、`audio()`、`generatedImage()` 输出结果；
- `store()` / `load()` 在同一执行单元会话中保存中间值；
- `yield_control()` 向模型让出控制权；
- `notify()` 发送额外状态；
- `ALL_TOOLS` 查询当前可发现工具元数据。

它接收自由格式 JavaScript 源码，不是一个带 `source` 字段的普通 JSON 参数对象。可用首行
pragma 控制本次执行的提前 yield 和直接输出预算：

```javascript
// @exec: {"yield_time_ms": 30000, "max_output_tokens": 10000}
const result = await tools.exec_command({ cmd: "git status --short" });
text(result.output);
```

isolate 本身没有 Node、文件系统或网络能力，必须通过 `tools.*` 调用宿主工具。协作工具和
system tool 不会自动出现在这个嵌套对象中。它适合把多个独立的读取、检查或外部工具调用
组合成一个 turn；不应被 EKO Agent 当作业务领域工具暴露给模型，否则模型会获得越过
typed contract 的任意工具编排能力。

### 3.3 `functions.wait`

等待一个已经由 `functions.exec` 异步返回的单元。只有在 `exec` 明确返回运行中的 cell ID
后才触发。

**参数**：

```json
{
  "cell_id": "running-exec-cell-id",
  "yield_time_ms": 10000,
  "max_tokens": 10000,
  "terminate": false
}
```

`terminate: true` 会停止该执行单元，属于有副作用的控制操作。它不是等待 Codex Thread 或
Subagent 的替代品。

### 3.4 `collaboration.spawn_agent`

创建当前任务内部的子智能体。它不是创建用户侧栏里的独立 Codex Thread。

**参数**：

```json
{
  "task_name": "bounded-subtask",
  "message": "明确的子任务和验收标准",
  "fork_turns": "all",
  "model": "optional-model",
  "reasoning_effort": "optional-effort"
}
```

`fork_turns` 可以是 `none`、`all` 或最近 turn 数。子智能体有独立上下文，但仍属于当前
任务的 parent/child 协作树。

### 3.5 其他子智能体入口

| 入口                            | 参数                | 触发/副作用                                         |
| ------------------------------- | ------------------- | --------------------------------------------------- |
| `collaboration.send_message`    | `target`、`message` | 向已存在的运行中子智能体投递消息；不保证启动新 turn |
| `collaboration.followup_task`   | `target`、`message` | 投递追加任务；目标 idle 时触发一个新的 turn         |
| `collaboration.interrupt_agent` | `target`            | 中断精确子智能体；不会删除历史或代码                |
| `collaboration.list_agents`     | 可选 `path_prefix`  | 查看当前 parent 任务树中的子智能体状态              |
| `collaboration.wait_agent`      | 可选 `timeout_ms`   | 等待子智能体消息、结束或用户输入                    |

任务内协作的常见生命周期是：

```text
spawn_agent
  -> list_agents
  -> send_message / followup_task
  -> wait_agent
  -> interrupt_agent（必要时）
```

### 3.6 `request_user_input`（Plan 模式）

该入口在当前 Default 模式不可调用；进入 Plan 模式后，可向用户展示 1–3 个短问题并等待
回答。每个问题包含 `header`、稳定 `id`、问题文本，以及 2–3 个互斥选项。

```json
{
  "questions": [
    {
      "header": "范围",
      "id": "scope",
      "question": "本次要覆盖哪个范围？",
      "options": [
        {
          "label": "当前项目 (Recommended)",
          "description": "只处理当前 workspace。"
        },
        {
          "label": "全部项目",
          "description": "覆盖所有可访问 workspace。"
        }
      ]
    }
  ]
}
```

它用于无法安全推断的关键选择，不应替代 Agent 的正常自主调查，也不能在 Default 模式中
通过普通文本伪造同等的强制交互流程。

## 4. Shell 与文件工具

### 4.1 `apply_patch`

使用 unified diff 创建、修改或删除文件。它是受限文件编辑入口，目标通常由 patch 中的
路径确定。

**触发条件**：用户授权了代码/文档修改，且当前任务需要落盘变更。

**EKO 参考**：保留 typed patch 或事务式文件编辑，不让模型直接拥有任意文件写入 syscall。
编辑前检查路径、worktree、未提交改动和文件所有权；编辑后记录 diff、事件和可回滚边界。

### 4.2 `exec_command`

在 PTY 或普通管道中执行 Shell 命令。

**主要参数**：

```json
{
  "cmd": "command",
  "workdir": "/absolute/workdir",
  "yield_time_ms": 10000,
  "max_output_tokens": 10000,
  "tty": false,
  "shell": "/bin/zsh",
  "login": true,
  "sandbox_permissions": "use_default",
  "justification": "only for an escalated command",
  "prefix_rule": ["git", "pull"]
}
```

当命令超过等待时间时返回 session ID，后续用 `write_stdin` 轮询或写入。用户批准、
sandbox、workdir 和命令风险必须分开判断；终端是用户主动工具时，不应误套 Agent 自动执行
权限门。

### 4.3 `write_stdin`

向已有 `exec_command` session 写入字符，或轮询输出。

**主要参数**：

```json
{
  "session_id": 123,
  "chars": "optional-input",
  "yield_time_ms": 5000,
  "max_output_tokens": 10000
}
```

它只能作用于已存在的命令 session，不能凭空创建新的后台任务。

### 4.4 `view_image`

读取本地图片并返回可视化数据。

**参数**：

```json
{ "path": "/absolute/path/to/image.png" }
```

这是只读工具。路径仍需满足当前 workspace 和文件访问边界。

## 5. 计划与 Goal

### 5.1 `update_plan`

更新当前任务的计划；最多一个步骤为 `in_progress`。

**参数**：

```json
{
  "explanation": "optional-update-reason",
  "plan": [
    { "step": "inspect", "status": "completed" },
    { "step": "implement", "status": "in_progress" },
    { "step": "verify", "status": "pending" }
  ]
}
```

计划是当前任务的控制投影，不替代持久 TaskRun、依赖 DAG 或完成证据。

### 5.2 `create_goal` / `get_goal` / `update_goal`

| 工具          | 参数                                               | 语义                                      |
| ------------- | -------------------------------------------------- | ----------------------------------------- |
| `create_goal` | `objective`；只有用户明确要求时可选 `token_budget` | 创建一个长期 Goal；不能从普通任务自动推断 |
| `get_goal`    | `{}`                                               | 读取当前 Goal、状态、预算和使用量         |
| `update_goal` | `status: complete\|blocked`                        | 只能把真正完成或满足阻塞条件的 Goal 结算  |

`update_goal` 的 `blocked` 不是“暂时困难”或“需要更多时间”；同一阻塞条件需要达到规定
的连续重复阈值且无法继续推进，才应结算为 blocked。

EKO 参考：Goal、Plan、TaskRun、Todo 和 Subagent result 必须有明确层级，不能让计划工具
悄悄创建第二套任务状态机。

## 6. MCP 与 Node REPL

### 6.1 MCP resource 工具

| 工具                          | 参数                    | 作用                             |
| ----------------------------- | ----------------------- | -------------------------------- |
| `list_mcp_resources`          | 可选 `server`、`cursor` | 列出 MCP server 提供的 resources |
| `list_mcp_resource_templates` | 可选 `server`、`cursor` | 列出参数化 resource templates    |
| `read_mcp_resource`           | `server`、`uri`         | 读取已列出的具体 resource        |

`read_mcp_resource` 必须使用 server 返回的合法 URI；不能把任意 URL 当作资源读取。MCP
连接、资源和工具的权限由 server 配置、用户授权和宿主策略共同决定。

### 6.2 `mcp__node_repl__js`

在持久 Node.js REPL 中执行 JavaScript，支持 top-level await。绑定会持续到 reset 或当前
REPL 被销毁。

**参数**：

```json
{
  "code": "await ...",
  "timeout_ms": 30000,
  "title": "short user-facing description"
}
```

它主要用于 Browser/Chrome/桌面应用控制和有状态 Node 工作流。持久状态意味着它不是纯
函数工具；EKO 设计中应显式记录 session identity、生命周期、资源上限和清理时机。

### 6.3 `mcp__node_repl__js_add_node_module_dir`

把绝对 `node_modules` 目录加入持久 REPL 的模块搜索路径。

```json
{ "path": "/absolute/path/to/node_modules" }
```

这是环境变更操作，应限制到用户明确选择的目录，不应由模型任意拼接系统路径。

### 6.4 `mcp__node_repl__js_reset`

无参数，清空 Node REPL 绑定并重置内核：

```json
{}
```

它会丢失该 REPL 的内存状态；EKO 应将其视为 session reset，而不是普通清理查询。

## 7. Web 与图像生成

### 7.1 `web__run`

统一公开 Web 查询入口，支持搜索、打开、点击、查找、截图、图片查询、天气、财经和体育
等动作。

**主要参数族**：

```json
{
  "search_query": [{ "q": "query", "domains": ["example.com"], "recency": 30 }],
  "open": [{ "ref_id": "url-or-search-reference", "lineno": 1 }],
  "click": [{ "ref_id": "page-reference", "id": 1 }],
  "find": [{ "ref_id": "page-reference", "pattern": "text" }],
  "screenshot": [{ "ref_id": "pdf-reference", "pageno": 0 }],
  "image_query": [{ "q": "image query", "domains": ["example.com"] }],
  "finance": [{ "ticker": "AAPL", "type": "equity", "market": "USA" }],
  "weather": [{ "location": "Country, Area, City", "duration": 7 }],
  "sports": [{ "fn": "schedule", "league": "nba", "team": "GSW" }],
  "response_length": "short"
}
```

只在问题需要当前、易变、精确引用或用户明确要求搜索时触发。技术问题优先使用官方或
一手来源；高风险事实不使用模型记忆代替验证。EKO 参考：Web 结果必须带来源、时间和
不确定性，不把搜索结果直接当作执行指令。

### 7.2 `image_gen.imagegen`

生成新位图，或根据用户提供的图片执行编辑。当前 schema 只有一个可选 prompt：

```json
{ "prompt": "生成或编辑要求" }
```

当用户明确请求图片生成或图片编辑时触发。输出是图像结果，不适合替代 SVG、HTML/CSS、
Canvas 或仓库原生图形代码。EKO 参考：媒体生成应作为 typed artifact tool，保留输入引用、
生成参数、结果 identity 和内容安全结论，不把二进制直接复制进会话文本。

## 8. Codex App Thread 工具

这些工具操作 Codex Desktop 的任务/会话控制面，不是普通 Shell 或模型 API。Thread 之间
的协同关系见 [Agent 协同 ADR](./0001-agent-collaboration.md)。

### 8.1 创建、派生与项目

#### `codex_app__create_thread`

创建一个用户可见的独立 Codex 任务。只有用户明确要求新任务时触发；创建是异步的。

**主要参数**：

```json
{
  "prompt": "initial task",
  "title": "optional title",
  "target": {
    "type": "project",
    "projectId": "project-id",
    "environment": {
      "type": "worktree",
      "startingState": {
        "type": "branch",
        "branchName": "existing-branch",
        "onMissing": "error"
      }
    }
  },
  "model": "optional-model",
  "thinking": "optional-effort"
}
```

`target` 也可以是 `projectless` 或明确的 `chatgptWorkCloud`。Git 项目默认倾向 worktree，
除非用户明确要求直接使用保存的项目。设置中的 `model` 和 `thinking` 只能选择当前宿主
支持的组合。

#### `codex_app__fork_thread`

从一个 Thread 的已完成历史派生新任务；省略 `threadId` 表示派生当前任务。

```json
{
  "threadId": "source-thread-id",
  "environment": { "type": "same-directory" }
}
```

可选环境为 `same-directory` 或 `worktree`。运行中的 turn 和未完成响应不会被复制。

#### `codex_app__list_projects`

无参数，列出可用于任务创建的 local、remote 和 ChatGPT 项目，并返回项目是否为 Git 仓库。

```json
{}
```

`create_thread` 使用它返回的 `projectId`；不能凭空编造项目 ID。

### 8.2 Thread 发现、读取和等待

#### `codex_app__list_threads`

```json
{ "limit": 30 }
```

列出 App 范围内的 Thread/Chat 摘要。返回通常包括 `id`、`kind`、`hostId`、`projectId`、
`cwd`、`status`、`updatedAt`、`title` 和 `summary`。它不返回完整 prompt、完整上下文、
隐藏推理或其他任务的内存。

#### `codex_app__read_thread`

```json
{
  "threadId": "01...",
  "hostId": "local",
  "cursor": "older-page-cursor",
  "turnLimit": 4,
  "includeOutputs": false,
  "maxOutputCharsPerItem": 3000
}
```

读取精确 Thread 的近期状态、turn 摘要和可选的截断工具输出。它不是读取目标完整隐藏
上下文的权限提升。

#### `codex_app__wait_threads`

```json
{
  "targets": [
    { "threadId": "01...", "hostId": "local", "afterCursor": "cursor" }
  ],
  "timeoutMs": 120000
}
```

最多等待 8 个目标；游标防止重复交付；完成或需要处理时唤醒；`timeoutMs: 0` 表示即时
快照。拿到稳定 Thread ID 后，优先使用 wait，而不是不停调用 list。

#### `codex_app__list_archived_threads`

分页列出已归档任务：

```json
{
  "hostId": "local",
  "cursor": "next-page-cursor",
  "limit": 10
}
```

归档是目录状态，不是永久删除；归档任务仍可通过 ID 恢复。

### 8.3 Thread 通信与控制

#### `codex_app__send_message_to_thread`

```json
{
  "threadId": "target-thread-id",
  "hostId": "local",
  "prompt": "用户可见的 follow-up",
  "model": "optional-model-override",
  "thinking": "optional-effort-override"
}
```

消息作为目标任务中的用户可见 follow-up 出现。它不是隐形共享内存，也不能绕过目标任务
自己的 sandbox、权限或审批策略。

#### `codex_app__handoff_thread`

把目标 Thread 及关联 Git 状态在 checkout/worktree 或宿主之间迁移。运行中的目标会先被
中断。

```json
{
  "threadId": "target-thread-id",
  "destinationHostId": "local",
  "followUpPrompt": "optional-after-handoff-prompt"
}
```

当前调用方不能 handoff 自己；云任务不支持本地 handoff。

#### `codex_app__get_handoff_status`

读取 handoff 的后台操作状态：

```json
{
  "operationId": "operation-id",
  "afterRevision": 3,
  "waitMs": 30000
}
```

`waitMs` 上限为 60000。推荐在 handoff 后带 `afterRevision` 等待变化，不要高频轮询同一
状态。

### 8.4 Thread UI 与展示生命周期

#### `codex_app__set_thread_archived`

```json
{
  "threadId": "thread-id",
  "hostId": "local",
  "archived": true
}
```

归档/取消归档，不删除历史、Git 分支或文件。

#### `codex_app__set_thread_pinned`

```json
{ "threadId": "thread-id", "pinned": true }
```

只改变任务目录展示顺序和置顶状态。

#### `codex_app__set_thread_title`

```json
{ "threadId": "thread-id", "title": "new title" }
```

只改变任务标题，不改变 goal、历史或权限。

#### `codex_app__navigate_to_codex_page`

```json
{ "threadId": "thread-or-chat-id" }
```

导航最近聚焦的主 App 窗口到指定任务；它是 UI 操作，不是给目标任务发送输入。

#### `codex_app__open_in_codex`

在 Codex 面板中打开文件、浏览器标签、终端或 review。

```json
{
  "placement": "right",
  "threadId": "optional-target-thread",
  "target": {
    "type": "file",
    "path": "/absolute/path/file.rs",
    "line": 42
  }
}
```

`target.type` 还可以是 `browser`、`terminal` 或 `review`。默认打开到当前任务窗口；把
tab 发给隐藏任务可能先排队。该工具只改变 UI，不代替文件、浏览器或 terminal 工具执行
操作。

#### `codex_app__share_thread`

为当前或其他可访问 Thread 生成不可变分享链接：

```json
{
  "threadId": "optional-thread-id",
  "hostId": "local"
}
```

这是用户主动分享操作，不应被协调器自动调用；分享对象的访问范围由宿主决定。

#### `codex_app__read_thread_terminal`

无参数，读取当前 Desktop 任务的 App terminal 输出：

```json
{}
```

它不是读取其他任务 terminal 的通用接口。

### 8.5 自动化与工作区依赖

#### `codex_app__automation_update`

创建、查看、修改或删除 recurring automation、heartbeat、cron、monitor 和 follow-up。
工具 schema 是按 mode 联合的，常见形状包括：

```json
// view
{ "mode": "view", "id": "automation-id" }

// local cron
{
  "mode": "create",
  "kind": "cron",
  "executionEnvironment": "local",
  "projectId": "project-id",
  "name": "daily check",
  "prompt": "检查任务状态并报告",
  "model": "model",
  "reasoningEffort": "low",
  "rrule": "<host-generated schedule>",
  "status": "ACTIVE"
}
```

真实参数随 automation kind/mode 变化。heartbeat 默认附着当前 Thread；cron 可以作为
独立 local project job。自动化 prompt 是用户可见且以后可能被重放的用户消息，不能偷偷
写入隐藏指令或把通知偏好塞进 prompt。

#### `codex_app__load_workspace_dependencies`

无参数，定位当前本地 Desktop 线程可用的 Node、Python、表格、文档、PDF 和演示文稿工作区
运行时依赖：

```json
{}
```

这是只读能力发现，不是安装依赖，也不应修改用户环境。

## 9. Thread 方法、状态和工具层级的对应关系

### 9.1 Thread 生命周期方法

Codex App 工具层的 Thread 方法可以映射为：

```text
create_thread
  -> fork_thread
  -> list_threads / read_thread
  -> send_message_to_thread / wait_threads
  -> handoff_thread / get_handoff_status
  -> set_thread_title / set_thread_pinned / set_thread_archived
  -> share_thread / navigate_to_codex_page / open_in_codex
```

公开 app-server 还存在更底层的 `thread/start`、`thread/resume`、`thread/fork`、
`thread/list`、`thread/read`、turn 控制和 event stream；App tool 名称不能直接当作稳定的
公开 JSON-RPC wire contract。

### 9.2 状态必须分层

不能把下面三层状态合并成一个 `status`：

| 层级            | 典型状态                                             | 含义                                             |
| --------------- | ---------------------------------------------------- | ------------------------------------------------ |
| Thread 目录状态 | `active`、`idle`、`notLoaded`、归档/置顶投影         | Thread 是否正在宿主中运行/加载，以及目录展示状态 |
| Turn 状态       | started、in progress、completed、failed、interrupted | 当前一次用户输入的执行生命周期                   |
| Item 状态       | started、updated、completed、failed                  | turn 内命令、消息、文件修改或工具调用的生命周期  |

`idle` 不表示 Thread 删除；`notLoaded` 不表示 Thread 没有历史；归档不等于删除。
具体正式 ThreadStatus 枚举以当前公开 app-server schema 为准，App 的目录状态可以是它的
有损投影。

## 10. 权限和触发门

### 10.1 触发分级

| 级别         | 工具类型                                     | 默认策略                                          |
| ------------ | -------------------------------------------- | ------------------------------------------------- |
| 只读查询     | list/read/view/search                        | 可自动触发，但应有范围和输出上限                  |
| 可等待       | wait/read terminal                           | 允许阻塞或长轮询，但必须有 timeout、cursor 和取消 |
| 可写入       | apply_patch、exec、send message、goal update | 需要任务授权和目标校验                            |
| 任务生命周期 | create/fork/handoff/archive/title            | 用户明确意图或协调策略明确授权                    |
| 外部副作用   | automation、share、MCP、browser/desktop      | 需要额外确认、审计和资源边界                      |

### 10.2 共同权限字段

EKO 参考实现的每次工具调用应携带或可推导：

```text
actor_identity
target_identity
workspace/project scope
tool name + schema version
requested capability
permission mode / approval policy
correlation_id / causation_id
attempt_id
idempotency key（如果有副作用）
```

工具名本身不是授权。尤其需要分别判断：

- 能否发现任务元数据；
- 能否读取有界目标和进度；
- 能否发送消息；
- 能否 steer/interrupt/resume/handoff；
- 能否在目标环境执行命令或写文件。

### 10.3 不可信工具输出

Thread `title`、`summary`、Web 搜索结果、MCP 文本和命令输出都应作为不可信数据处理。它们
可以帮助模型选择下一步，但不能覆盖系统/开发者指令或自动扩大权限。

## 11. EKO 工具层参考设计

本节是目标设计，不描述 EKO 当前实现状态。

### 11.1 EKO 不应复制的部分

- 不把 `functions.exec` 这种宿主万能编排器直接暴露给产品 Agent；
- 不把 App 目录摘要当成完整 goal、完整 prompt 或完整上下文；
- 不把 `active/idle/notLoaded` 当成任务终态；
- 不用高频 `list` 轮询替代事件 journal 和 cursor wait；
- 不让同一 workspace 自动获得其他 workspace 的执行权限；
- 不把 Thread、Turn、TaskRun、PlanTask、SubagentRun 混成一个对象；
- 不把 Skill、Plugin、MCP server 和直接模型工具混为同一注册表语义。

### 11.2 EKO 建议的工具分层

```text
EkoHostControl（宿主，不直接给模型）
  ├── parallel orchestration
  ├── process wait / shutdown
  └── surface navigation

EkoAgentTools（模型可调用）
  ├── task / plan / goal
  ├── agent discovery / inspect / message / wait
  ├── exact interrupt / resume / handoff
  ├── shell / file / browser / MCP
  └── evidence / artifact / completion

EkoExtensionRegistry
  ├── skills
  ├── plugins
  ├── MCP tools/resources
  └── hooks
```

### 11.3 EKO 协同工具建议

| EKO 工具          | 参考 Codex 能力                  | 必须增加的 EKO 约束                                |
| ----------------- | -------------------------------- | -------------------------------------------------- |
| `agent_list`      | `list_threads`                   | scope、visibility、redaction、limit                |
| `agent_inspect`   | `read_thread`                    | detail level、cursor、evidence refs                |
| `agent_message`   | `send_message_to_thread`         | exact address、correlation、idempotency            |
| `agent_wait`      | `wait_threads`                   | bounded target set、cursor、timeout、cancel        |
| `agent_spawn`     | `create_thread`/`spawn_agent`    | distinguish independent Thread vs TaskRun Subagent |
| `agent_fork`      | `fork_thread`                    | explicit history boundary and worktree policy      |
| `agent_interrupt` | `interrupt_agent`/turn interrupt | exact attempt identity                             |
| `agent_handoff`   | `handoff_thread`                 | source/target checkout and ownership receipt       |
| `agent_group`     | App task directory + team        | explicit membership, visibility and lifecycle      |

### 11.4 EKO 工具注册与动态可见性

EKO 每次创建 Agent invocation 时生成一个 capability snapshot：

```json
{
  "schema_version": 1,
  "agent_address": "scope/conversation",
  "tools": [
    {
      "name": "agent_wait",
      "capability": "coordination.wait",
      "visible": true,
      "reason": "group_member",
      "redactions": []
    }
  ],
  "permission_mode": "default",
  "workspace_scope": "workspace-id"
}
```

工具集合变化必须有事件和 revision；旧 invocation 不能因为新配置热更新而突然获得新能力。
需要新能力时，在 safe point 创建新的 invocation 或显式刷新 capability snapshot。

### 11.5 EKO 等待和长任务

所有长任务工具都应采用：

```text
stable identity
  + durable event / receipt
  + bounded wait
  + cursor
  + cancellation
  + restart reconciliation
```

`wait` 只负责等待，不拥有任务终态；终态来自 TaskRun/Subagent/Tool 的事实事件。模型
摘要只能作为展示，不是完成 authority。

## 12. EKO 验收清单

### 12.1 能力目录

- 工具计数明确区分直接运行时、宿主包装器、协作入口和动态 MCP 能力；
- 每个工具有 schema version、参数校验、触发策略和副作用标记；
- Default/Plan/用户确认模式下的工具可见性有测试；
- 插件、Skill、MCP 和工具注册表不互相伪装；
- 文档中的工具列表可由运行时 snapshot 或生成脚本校验。

### 12.2 协同和 Thread

- 独立 Thread 与 TaskRun 内 Subagent 可明确区分；
- `list`、`inspect`、`message`、`wait`、`interrupt`、`resume` 有精确 identity；
- wait 支持 cursor、timeout、cancel 和 restart；
- title/summary/command output 不会提升权限；
- 归档、fork、handoff 不会误删历史或工作树；
- active、idle、notLoaded、turn terminal 和 item terminal 不混淆。

### 12.3 安全与可靠性

- 写文件、Shell、MCP、Browser、automation 和 share 都有独立副作用门；
- 消息和控制操作有幂等键、correlation/causation 和 receipt；
- 旧 attempt 不能影响新 attempt；
- 大输出使用 artifact/reference/cursor，不复制进每个 prompt；
- 进程和 workspace 资源有界；
- 所有 surface 对同一套工具合同做 typed 投影。

## 13. 参考资料与限制

用户提供的 Claude Code 工具、子智能体与 Skills/插件对照快照见
[Claude Code 能力目录](./0003-claude-code-capability-catalog.md)。

- [OpenAI Codex app-server README](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md)：公开的 Thread/Turn/Item、JSON-RPC、生命周期和事件流。
- [OpenAI Codex exec events](https://github.com/openai/codex/blob/main/codex-rs/exec/src/exec_events.rs)：`thread.started`、`turn.started`、`item.started`、`item.completed` 和终态 usage。
- [OpenAI Codex multi-agents v2](https://github.com/openai/codex/tree/main/codex-rs/core/src/tools/handlers/multi_agents_v2)：任务内 Subagent 的 spawn、消息、等待和中断边界。
- [OpenAI Agents SDK multi-agent orchestration](https://github.com/openai/openai-agents-python/blob/main/docs/multi_agent.md)：manager/tool、handoff 和代码编排的公开取舍。

当前会话的工具 schema 是本机 Desktop 能力快照；官方 Codex Manual 在本次校准时无法通过
官方站点拉取（HTTP 403），因此本文不把未公开的后台实现细节写成确定事实。未来 App、模式、
插件或宿主变化时，应重新导出工具目录、更新版本记录并重新校验本文。
