# ADR-0003：Claude Code 工具、子智能体与 Skills/插件能力目录

> 状态：Reference Snapshot（历史快照，不随 catalog 演进回填；EKO 自身的 Skill catalog
> 现状见 [ADR 0033](0033-skill-catalog-contraction-and-official-frontmatter.md)）
>
> 日期：2026-08-24
>
> 来源：用户提供的 Claude Code 当前会话能力枚举。
>
> 范围：保存该会话报告的 30 个内置工具、6 种子智能体类型、25 个 Skills/插件条目，供
> EKO 产品和工具层设计参考。
>
> 证据边界：本文是环境快照，不把数量、名称或行为声明为所有 Claude Code 版本、账户、
> 项目和机器都长期固定的公共 API。部分能力可能来自实验功能、插件、项目配置或灰度版本。

## 1. 快照摘要

用户提供的当前 Claude Code 会话共有三类能力：

| 类别            | 数量 | 说明                                                           |
| --------------- | ---: | -------------------------------------------------------------- |
| 内置工具入口    |   30 | 文件、Shell、Web、协作、任务、计划、worktree、调度、监控和交互 |
| 子智能体类型    |    6 | 通用、探索、计划、产品指南和状态栏配置等角色                   |
| Skills/插件条目 |   25 | `superpowers` 14 个，独立 Skills 11 个                         |
| 外部 MCP server |    0 | 该会话没有连接外部 MCP server                                  |

这个分类不是一张平坦的工具表：

```text
Claude Code session
  ├── Tool entry：模型可以直接调用的操作
  ├── Subagent type：独立上下文的角色配置和工具集合
  ├── Skill / Plugin：按触发条件加载的工作流说明、脚本或扩展
  └── MCP：外部 server 动态提供的工具、资源或模板
```

工具、子智能体、Skill 和 MCP 必须分别统计。Skill 被加载后可能指导模型调用工具，但 Skill
本身不一定等于一个底层执行函数；子智能体类型也不是一种新模型。

## 2. 内置工具目录（30 个）

### 2.1 完整枚举

|   # | 类别          | 工具              | 当前会话报告的用途                          |
| --: | ------------- | ----------------- | ------------------------------------------- |
|   1 | 文件操作      | `Read`            | 读取文件，包括图片、PDF 和 Jupyter notebook |
|   2 | 文件操作      | `Write`           | 创建、写入或覆盖文件                        |
|   3 | 文件操作      | `Edit`            | 通过精确字符串替换编辑文件                  |
|   4 | 文件操作      | `NotebookEdit`    | 编辑 Jupyter notebook 单元格                |
|   5 | 执行与搜索    | `Bash`            | 执行 Shell 命令，支持后台运行               |
|   6 | 执行与搜索    | `LSP`             | 跳转定义、查找引用、hover 等语言服务        |
|   7 | 网络          | `WebSearch`       | 搜索公开网络信息                            |
|   8 | 网络          | `WebFetch`        | 获取 URL 内容并围绕指定问题处理内容         |
|   9 | 智能体协作    | `Agent`           | 派生子智能体和后台并行任务                  |
|  10 | 智能体协作    | `SendMessage`     | 向其他智能体发送消息                        |
|  11 | 智能体协作    | `Workflow`        | 执行确定性的多智能体编排脚本                |
|  12 | 智能体协作    | `Skill`           | 加载或调用 Skill                            |
|  13 | 任务管理      | `TaskCreate`      | 创建任务清单节点                            |
|  14 | 任务管理      | `TaskGet`         | 读取一个任务                                |
|  15 | 任务管理      | `TaskList`        | 列出任务                                    |
|  16 | 任务管理      | `TaskUpdate`      | 更新任务内容、关系或状态                    |
|  17 | 任务管理      | `TaskOutput`      | 查看后台任务输出                            |
|  18 | 任务管理      | `TaskStop`        | 停止后台任务                                |
|  19 | 计划模式      | `EnterPlanMode`   | 进入规划模式                                |
|  20 | 计划模式      | `ExitPlanMode`    | 退出规划模式并进入后续执行/交互             |
|  21 | Worktree 隔离 | `EnterWorktree`   | 创建并切换到独立 Git worktree               |
|  22 | Worktree 隔离 | `ExitWorktree`    | 退出 worktree 并进入保留/合并/清理流程      |
|  23 | 定时调度      | `CronCreate`      | 创建定时或循环任务                          |
|  24 | 定时调度      | `CronDelete`      | 删除定时任务                                |
|  25 | 定时调度      | `CronList`        | 列出定时任务                                |
|  26 | 定时调度      | `ScheduleWakeup`  | 以自定义节奏延迟唤醒，配合 `/loop` 使用     |
|  27 | 监控          | `Monitor`         | 后台监控日志、进程或 PR，并通过事件流通知   |
|  28 | 交互          | `AskUserQuestion` | 向用户展示结构化问题或选项                  |
|  29 | 交互          | `ReportFindings`  | 以结构化列表报告代码审查发现                |
|  30 | 设计系统      | `DesignSync`      | 与 `claude.ai/design` 设计系统项目同步      |

### 2.2 文件操作

Claude Code 把通用文件能力拆为四个入口：

```text
Read         读取普通文件和富媒体/结构化文件
Write        写入完整文件
Edit         进行精确局部替换
NotebookEdit 修改 notebook cell
```

这种拆分使宿主能够针对“读取”“覆盖”“局部编辑”“notebook 结构编辑”应用不同的参数校验、
权限、审计和 UI 展示。

EKO 可借鉴：

- 读取、覆盖和事务式 patch 不应只有一个任意文件 API；
- notebook 应保留 cell identity、cell type 和输出结构，不能退化为普通 JSON 字符串替换；
- 覆盖文件比局部 edit 风险高，应有更清晰的 diff、未提交改动和恢复门；
- 图片/PDF 等富媒体读取应返回 typed observation 或 artifact reference。

### 2.3 Shell、LSP 和 Web

`Bash` 拥有命令执行和后台运行能力；`LSP` 提供结构化代码导航；`WebSearch` 负责发现公开
网页，`WebFetch` 负责读取一个已知 URL。它们对应四种不同的事实来源：

| 工具        | 事实来源             | 主要边界                                |
| ----------- | -------------------- | --------------------------------------- |
| `Bash`      | 当前机器和 workspace | 命令、副作用、workdir、权限、后台 owner |
| `LSP`       | 语言服务索引         | server 可用性、文件版本、诊断 freshness |
| `WebSearch` | 搜索索引             | 时效性、来源质量、搜索摘要不等于原文    |
| `WebFetch`  | 指定页面             | URL 权限、内容截断、重定向、引用        |

EKO 可借鉴：搜索和抓取应分开；LSP 应优先于文本 grep 回答符号关系；后台命令必须有稳定
identity、wait、cancel 和 terminal receipt。

### 2.4 智能体协作

该会话报告了四个协作入口：

- `Agent`：创建子智能体或后台并行任务；
- `SendMessage`：向其他智能体发送消息；
- `Workflow`：执行确定性多智能体编排；
- `Skill`：加载/调用一个 Skill 工作流。

这反映两类编排方式：

```text
模型驱动
  Agent + SendMessage
  -> 模型决定拆分、协商、等待和汇总

代码/脚本驱动
  Workflow
  -> 预先定义步骤、分支、并行和终止条件
```

两者可以组合，但不能各自拥有一套互不兼容的任务状态和结果权威。

### 2.5 任务管理

`TaskCreate`、`TaskGet`、`TaskList` 和 `TaskUpdate` 组成任务 CRUD；`TaskOutput` 和
`TaskStop` 控制后台执行。这说明“任务关系”和“后台进程句柄”是不同概念：

```text
TaskCreate/Get/List/Update
  -> 任务清单、依赖或状态投影

TaskOutput/TaskStop
  -> 一个正在运行的后台操作
```

EKO 可借鉴：Task CRUD 必须绑定唯一 revisioned TaskRun graph；后台 command/process 的
输出和停止句柄不能成为第二套 Task store。

### 2.6 Plan Mode

`EnterPlanMode` / `ExitPlanMode` 是交互和行为模式工具。Plan Mode 的核心价值是改变模型
当下允许的行为和输出，而不是给产品运行时增加大量 Planning/AwaitingApproval 状态。

EKO 可借鉴：Plan 是可编辑 artifact；批准由 prompt、permission 或 HITL 驱动。TaskRun
只记录真实执行生命周期，不把计划编辑过程强行编码成主状态机。

### 2.7 Worktree 隔离

`EnterWorktree` / `ExitWorktree` 把并行编码任务放入独立 Git worktree。退出时应明确选择
保留、合并、交付或清理，不能把 worktree 生命周期和会话历史删除绑定。

EKO 可借鉴：

- worktree 是文件写入隔离，不是上下文隔离的替代品；
- 每个写入任务有 branch/base/owner；
- 合并前检查主线变化、未提交文件、依赖路径和生成文件；
- 删除 worktree 不应删除 Thread、TaskRun 或完成证据。

### 2.8 定时、唤醒和监控

| 能力                     | 用途                          | 关键差异                      |
| ------------------------ | ----------------------------- | ----------------------------- |
| `CronCreate/Delete/List` | 持久定时或循环调度            | schedule 是持久配置           |
| `ScheduleWakeup`         | 当前任务按自定义节奏再次唤醒  | 更接近 continuation/heartbeat |
| `Monitor`                | 监听日志、进程、PR 等外部状态 | 事件变化驱动，不等同固定 cron |

EKO 可借鉴：cron、heartbeat 和 event monitor 必须分开。它们可以触发同一个 TaskRun/turn
入口，但不应各自复制任务状态机。

### 2.9 用户交互、审查和设计同步

- `AskUserQuestion`：在关键选择无法安全推断时请求结构化输入；
- `ReportFindings`：输出带位置、优先级和说明的结构化 review findings；
- `DesignSync`：连接设计系统项目和代码/资产工作流。

EKO 可借鉴：交互式问题、审查发现和设计同步都应使用 typed DTO，而不是从自由文本重新
解析状态；审查发现只是 review artifact，不应自动变成已确认缺陷或代码修改授权。

## 3. 子智能体类型（6 种）

### 3.1 完整枚举

| 类型                | 当前会话报告的用途                             | 工具边界                 |
| ------------------- | ---------------------------------------------- | ------------------------ |
| `claude`            | 万能兜底，不匹配其他类型时使用                 | 全工具                   |
| `general-purpose`   | 通用研究、搜索代码和多步任务                   | 全工具                   |
| `Explore`           | 大范围只读搜索文件并形成结论                   | 只读，不修改代码         |
| `Plan`              | 架构设计、实施方案和关键文件识别               | 规划优先，不承担生产写入 |
| `claude-code-guide` | 回答 Claude Code、Agent SDK 和 Claude API 问题 | 文档/产品知识导向        |
| `statusline-setup`  | 配置 Claude Code 状态栏                        | 狭窄配置任务             |

### 3.2 角色不是模型

子智能体类型应理解为：

```text
role prompt
  + allowed tools
  + model/reasoning defaults
  + context inheritance policy
  + output contract
```

它不代表一种独立基础模型。一个角色可以换模型，一个模型也可以承载多个角色。角色数量
可能被用户、项目或插件扩展，不能长期写死为 6。

### 3.3 `claude` 与 `general-purpose`

附件把两者都描述为全工具通用角色，但语义仍可不同：

- `claude`：默认兜底或主 Agent 风格；
- `general-purpose`：明确作为可委派的通用多步子任务角色。

具体 prompt、上下文继承和结果回传策略未在附件中给出，本文不进一步推断。

### 3.4 `Explore` 与 `Plan`

这是非常有价值的最小权限角色拆分：

- `Explore` 只读，适合并行扫描、依赖追踪和证据收集；
- `Plan` 负责设计、关键路径和风险分析，不默认写生产代码。

EKO 可借鉴：只读 explorer 不需要获得写文件、执行破坏性命令或 merge 权限；规划角色不应
因为提出了方案就成为运行时状态机的审批 owner。

### 3.5 专项配置角色

`claude-code-guide` 和 `statusline-setup` 表明，子智能体不一定都是大型编码执行者，也可以
是高度聚焦的产品支持或配置角色。EKO 的专业 Subagent 可以采用相同模式，但应由动态注册
和 capability 组合产生，而不是不断扩充硬编码 enum。

## 4. Skills/插件目录（25 个）

### 4.1 `superpowers` 插件（14 个）

|   # | Skill                                        | 当前会话报告的用途                     |
| --: | -------------------------------------------- | -------------------------------------- |
|   1 | `superpowers:using-superpowers`              | Skill 系统使用说明，会话启动时自动加载 |
|   2 | `superpowers:brainstorming`                  | 创造性工作前的需求和设计讨论           |
|   3 | `superpowers:dispatching-parallel-agents`    | 并行派发互相独立的任务                 |
|   4 | `superpowers:executing-plans`                | 在独立会话中执行书面实现计划           |
|   5 | `superpowers:subagent-driven-development`    | 在当前会话内用子智能体执行计划         |
|   6 | `superpowers:writing-plans`                  | 为多步骤任务编写实现计划               |
|   7 | `superpowers:test-driven-development`        | 测试驱动开发流程                       |
|   8 | `superpowers:systematic-debugging`           | 系统化诊断和调试流程                   |
|   9 | `superpowers:verification-before-completion` | 声称完成前运行并核对验证命令           |
|  10 | `superpowers:requesting-code-review`         | 发起代码审查                           |
|  11 | `superpowers:receiving-code-review`          | 接收和处理审查意见                     |
|  12 | `superpowers:finishing-a-development-branch` | 分支完成后的合并、PR 和清理决策        |
|  13 | `superpowers:using-git-worktrees`            | Git worktree 使用规范                  |
|  14 | `superpowers:writing-skills`                 | 编写新 Skill                           |

### 4.2 `superpowers` 工作流链

这些 Skills 大致形成一条软件开发工作流：

```text
using-superpowers
  -> brainstorming
  -> writing-plans
  -> dispatching-parallel-agents / subagent-driven-development / executing-plans
  -> test-driven-development / systematic-debugging
  -> verification-before-completion
  -> requesting-code-review / receiving-code-review
  -> finishing-a-development-branch
```

`using-git-worktrees` 是并行写入隔离策略；`writing-skills` 用于扩展工作流本身。

EKO 可借鉴：Skill 应提供行为指导和可复用流程，但 task graph、测试结果、Git 状态和完成
结论仍由真实工具和运行时事实决定，不能只因为 Skill prompt 声称完成就结算任务。

### 4.3 独立 Skills（11 个）

|   # | Skill                       | 当前会话报告的用途                              |
| --: | --------------------------- | ----------------------------------------------- |
|   1 | `dataviz`                   | 图表和数据可视化设计规范                        |
|   2 | `update-config`             | 配置 `settings.json` 中的权限、hooks 和环境变量 |
|   3 | `keybindings-help`          | 自定义键盘快捷键                                |
|   4 | `simplify`                  | 审查改动并进行简化、复用性清理                  |
|   5 | `fewer-permission-prompts`  | 分析历史记录并生成权限白名单建议                |
|   6 | `loop`                      | 按间隔循环执行某个 prompt 或命令                |
|   7 | `claude-api`                | Claude API/SDK 的模型 ID、定价和参数参考        |
|   8 | `run`                       | 启动并驱动当前项目，验证改动效果                |
|   9 | `init`                      | 初始化项目 `CLAUDE.md`                          |
|  10 | `security-review`           | 执行安全审查                                    |
|  11 | `gitlab-mr-troubleshooting` | GitLab MR、推送选项和 GPG 签名排障              |

### 4.4 Skill 触发和权限

Skill 可能由用户显式调用、匹配任务描述后自动加载，或在会话启动时加载。无论采用哪种
触发方式：

- Skill 只能指导当前 Agent 使用已经获准的工具；
- Skill 不能自己扩大 sandbox、文件或网络权限；
- 修改配置、生成权限白名单、创建循环任务等 Skills 仍需用户意图和宿主门禁；
- Skill 内容可能来自插件或项目，应作为指令资源校验来源和优先级；
- Skill 更新需要版本、来源、启用状态和缓存失效策略。

## 5. MCP 状态

附件明确说明：该 Claude Code 会话没有连接任何外部 MCP server。

因此当前快照中的 30 个工具不包括外部 MCP server 动态提供的工具。未来连接 MCP 后，工具
数量和能力可能增加；还可能出现 resources、resource templates 或 server-specific actions。

EKO 可借鉴：

- 区分“配置了 server”“连接成功”“工具已注册”“当前 invocation 可见”；
- MCP 工具和内置工具使用统一 capability/permission 投影；
- 用户配置的扩展默认由用户负责，保留明显错误输入和密钥日志保护；
- server 断开或热重载不能让旧 invocation 悄悄获得不同工具集合。

## 6. 与 Codex 能力目录对照

Codex 当前工具快照见
[Codex 工具能力目录](./0002-codex-tool-capability-catalog.md)。两者存在相似的能力主题，
但不应按名字机械一一映射：

| 能力主题      | Claude Code 快照                        | Codex 当前快照                                      |
| ------------- | --------------------------------------- | --------------------------------------------------- |
| 文件读取/编辑 | `Read`、`Write`、`Edit`、`NotebookEdit` | `apply_patch`、`view_image`、Shell/文档 Skills      |
| Shell         | `Bash`                                  | `exec_command`、`write_stdin`                       |
| Web           | `WebSearch`、`WebFetch`                 | `web__run`                                          |
| 子智能体      | `Agent`、`SendMessage`                  | `collaboration.spawn_agent`、message/follow-up/wait |
| 独立会话      | 附件未单独列出 Thread 目录 API          | Codex App `create/list/read/wait/send Thread`       |
| 任务          | `TaskCreate/Get/List/Update`            | plan/goal + Codex App Thread +应用任务能力          |
| Plan          | `EnterPlanMode`、`ExitPlanMode`         | Plan mode 与 `request_user_input`、`update_plan`    |
| Worktree      | `EnterWorktree`、`ExitWorktree`         | `create_thread` worktree、fork、handoff             |
| 调度/监控     | Cron、Wakeup、Monitor                   | automation heartbeat/cron、Thread wakeup            |
| Skills        | `Skill` + 25 条目                       | Skills/Plugins 作为独立工作流层                     |

这里的“缺少”只表示附件没有枚举，不等于 Claude Code 产品不存在该能力。

## 7. EKO 参考设计

本节是目标设计，不描述 EKO 当前实现状态。

### 7.1 保留四层模型

```text
工具执行层
  -> file / shell / browser / web / MCP

任务与协同层
  -> TaskRun / PlanTask / SubagentRun / Agent messaging

行为工作流层
  -> Skills / Plugins / system and project instructions

宿主控制层
  -> plan mode / worktree / cron / monitor / UI navigation
```

EKO 不应把所有能力都注册成同一种 Tool。宿主控制、模型工具、Skill 和动态扩展需要不同的
生命周期、权限和可见性。

### 7.2 文件工具

- 保留只读、局部 patch、完整覆盖和结构化 notebook/editor 的差异；
- 默认优先事务式 patch，完整覆盖需更高风险提示；
- 所有写入带 workspace、conversation、turn、tool call 和 attempt identity；
- 并行写入使用文件所有权或 worktree 隔离；
- 长内容和富媒体使用 artifact/reference，不进入首屏完整上下文。

### 7.3 协同和任务

- 单 Task、Todo、依赖 DAG 共用同一 revisioned TaskRun graph；
- Subagent 是任务内执行角色，独立 Thread/会话是用户可见的长期协作对象；
- `SendMessage` 类能力使用 exact address、correlation、causation 和 delivery receipt；
- `Workflow` 只编译确定性意图，不拥有第二套任务 store 或 executor；
- TaskOutput/TaskStop 类后台句柄不替代 PlanTask/Subagent 状态。

### 7.4 Plan、Worktree 和完成门

- Plan Mode 通过 prompt、工具可见性和 permission/HITL 驱动；
- Plan 是 artifact，不增加冗长主状态机；
- Worktree 是应用层文件隔离和交付策略；
- “完成”必须由测试、artifact、task terminal 和 review evidence 证明；
- verification Skill 只能推动验证，不能自己伪造成功结果。

### 7.5 定时和监控

EKO 应区分：

```text
Cron       固定时间规则
Heartbeat  当前会话定期唤醒
Monitor    外部事件变化触发
Wait       等待一个已有任务或命令
```

四者可以触发统一 turn/TaskRun driver，但拥有不同的持久配置、取消、恢复和通知合同。

### 7.6 动态角色和 Skills

- Subagent role 由 prompt、tool capability、model profile 和 output contract 组合；
- 除稳定内置角色外，专业角色通过配置/插件注册，不持续扩充硬编码 enum；
- Skill 有来源、版本、优先级、启用状态和触发记录；
- 自动加载 Skill 仍不能绕过用户、项目或工具权限；
- Skills、MCP、Plugin 热更新只影响新 invocation 或显式 safe point。

## 8. 验收清单

### 8.1 能力目录

- 能区分内置工具、子智能体类型、Skill/Plugin 和 MCP；
- 数量和名称标记来源、版本和当前会话范围；
- 每个工具有参数 schema、触发条件、副作用和权限标签；
- 每个角色有上下文继承、工具集合和输出合同；
- Skill 有来源、版本、启用和触发信息；
- 运行时 snapshot 可以验证文档，没有把历史数量写死为产品常量。

### 8.2 工具可靠性

- 文件覆盖、局部编辑、notebook 编辑有独立失败和恢复合同；
- 后台 Bash 有 stable ID、wait、output、cancel 和 terminal receipt；
- WebSearch/WebFetch 返回来源和 freshness；
- LSP 结果带文件版本或 stale 标识；
- Cron/Wakeup/Monitor 重启后可恢复且不会重复触发副作用；
- AskUserQuestion、ReportFindings 和 DesignSync 使用 typed surface projection。

### 8.3 协同和扩展

- Agent/SendMessage/Workflow 不产生平行 Task 状态机；
- Explore/Plan 等最小权限角色不可越权写入；
- worktree 删除与会话/任务历史删除分离；
- Skill 不能自行扩大 sandbox 或 permission；
- MCP server 的配置、连接、注册和 invocation 可见性可分别诊断；
- GUI、TUI、CLI/JSONL 和 channel 对核心能力保持对等。

## 9. 参考和维护方式

- 本文原始事实来源是用户提供的 Claude Code 当前会话枚举，不是官方全版本能力保证。
- 与 Codex 的对照以 [Codex 工具能力目录](./0002-codex-tool-capability-catalog.md) 为准。
- Agent/Thread 协同设计以 [Agent 协同 ADR](./0001-agent-collaboration.md) 为准。
- 如果后续获得 Claude Code 新会话枚举，应先比较新增、删除和重命名项，再更新日期和数量。
- 如果需要把某项能力写成 EKO 实施规格，必须重新核实当时的官方文档、真实产品行为和 EKO
  最新代码，不能直接把本快照当作实现合同。
