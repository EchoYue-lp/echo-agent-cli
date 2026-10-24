# ADR 0050：EKO 收敛为 GUI 产品主入口

## 状态

Accepted：2026-10-24 已批准产品方向；运行时退役尚待实现。

## 背景

EKO 是本地个人助理，不是线上多用户服务。已批准方向以 GUI 主入口替代旧的长期 TUI/GUI
能力对等要求，同时保留 IM 和 cron 后台触发。完整能力基线仍实现独立 TUI、REPL、CLI/JSONL
和 LSP。本 ADR 固化后续实现合同，不删除现有路径。

终端目录名不等于职责归属。桌面仍解析 CLI Args，GUI 面板读取 CLI 记忆开关，IM handler
及连接生命周期仍在 `src/cli`。LSP 在 bootstrap 和 pooled Agent 注册，并参与插件代、配置和
扩展控制。只删菜单、改默认 feature 或整包删目录，会留下可达实现或误删保留能力。
详见配对的[运行时处置清单](../architecture/runtime.md#产品收敛处置清单)。

## 候选方案

1. 继续维护多个完整产品入口。保留既有终端工作流，但继续承担平行交互、配置和验收义务，
   不符合已批准的 GUI 产品范围。
2. 仅隐藏或禁用终端/A2A/LSP 入口。降低可发现性但保留代码、依赖、注册和生命周期义务，
   不构成真实退役，也可能留下其它可达分派路径。
3. 先归位共享 owner、让协议消费者解除依赖，再物理退役具名入口与能力（采用）。需要有序
   验收与恢复锚点，但最终形成清晰的支持边界。

## 决策

- EKO 只保留桌面 GUI 产品主入口；IM 收发和 cron 仍通过同一 app-core runtime 后台触发。
  GUI PTY 终端、Agent shell、桌面配置参数和构建命令继续保留，均不等于独立会话 CLI 产品。
- 消费者核对后退役独立 TUI、REPL、CLI、JSONL 适配及专属 rendering、completion、HITL
  provider、帮助、配置、依赖和测试/示例。先迁移桌面 Args、共享记忆开关和 IM 连接/session/
  input-pump owner；普通 conversation、Task/Subagent、MCP、HITL、附件和恢复语义由保留
  owner 承接，不另建替代终端控制器。
- GUI Side Conversation 导航与并行面板继续由 GUI/Tauri 独有。IM 保留普通 conversation、
  Subagent 和已有 fork 行为，不新增 Side Conversation 专用 wire。历史 console `/fork` 随
  adapter 退役盘点，不构成永久保留终端入口的理由。
- 框架、SDK、EKO 一同退役 A2A/LSP 的生产 feature 请求、实现、注册、配置、plugin LSP
  组件、生成消费者和宣传。其它合法插件能力及 preparation/publication/rollback owner 保留。
  普通 AgentRouter 协作与 Subagent 不能仅按名字被认定为 A2A 协议消费者。
- 旧配置或插件请求已退役的 A2A/LSP 能力时，必须明确诊断能力已退役/不再支持，不能
  静默忽略或假报组件加载成功。诊断不构成保留 LSP/A2A 执行器或控制器的理由。用户已有
  会话、任务、记忆、插件文件和配置文件是数据，不得随代码退役一并删除。后续验收必须
  覆盖旧配置和插件能力请求，并真实报告保留插件组件各自的加载结果。
- 撤销 API 时，旧框架 API 仍存在期间先让 EKO/SDK 解除依赖；消费者验收后才删框架。
  EKO CI 跟踪 framework main，每个中间 main 必须可构建。SDK Host 自有精确 framework
  pin（设计基线记录为 `f7bac62e`），独立于顶层 gitlink，不因退役强迫无关 pin 升级。
  新增通用 API 仍由框架先交付。
- 框架机制保持产品无关、独立可复用；EKO 策略、workspace/文件权威、IM/cron 协调和呈现
  留 app-core/adapter。本次明确的 A2A/LSP 退役不授权因 EKO 未调用就删其它框架 public API。
  独立 SDK Host stdio 保留，它不是 EKO JSONL 或 EKO 终端入口。
- 隐藏窗口时应用进程和 IM/cron owner 继续运行。真正退出停止准入、取消并等待原有生命周期
  结算，不留下 daemon。重启沿当前 identity、occurrence ledger 和 TaskRuntime 恢复，保留
  未知副作用的检查 blocker，不新增自动重放策略。已有 scheduler 组合与窗口可见性切换是
  源码事实；删终端前仍须验收 GUI 所有的 IM/重连/隐藏/退出/重启行为。
- 延续本地个人权限模型。用户主动终端/文件/MCP 操作不受 Agent 自动权限模式门控；Agent
  自动行为仍走既有权限/HumanLoop。保留防数据丢失、框架 bug 和密钥日志保护，不增加线上
  服务威胁模型闸门。
- 延续文件/内存持久化。ConversationStore 拥有会话事实，TaskRuntime `events.jsonl` 拥有
  任务事实，plan/checkpoint/UI 是投影。EKO 不启用 SQLite，不另建 scheduler、任务图或
  store；框架供独立消费者选择的存储实现继续保留。

## 参考实现与取舍

[Tauri system tray 文档](https://v2.tauri.app/learn/system-tray/)给出明确的托盘动作与进程退出；
[MCP transport 规范](https://modelcontextprotocol.io/specification/2025-06-18/basic/transports)
区分 stdio 传输和产品界面；[Temporal workflow execution](https://docs.temporal.io/workflow-execution)
与 [LangGraph persistence](https://docs.langchain.com/oss/python/langgraph/persistence)
把持久历史/checkpoint 与呈现、恢复调用分开。这些依据支持显式生命周期 owner、传输与产品
分离、保留持久权威，不意味着它们要求 GUI-only，也不声称 EKO 实现了这些系统。
GUI-only 和具名协议退役来自用户已批准的产品决定，机制复用 EKO 现有 owner。

## 影响

### 恢复与验收

本机 annotated tag `pre-retirement-2026-10-24` 与
`chore/Echoyue/full-capabilities-baseline` 分支均解析到
`c262cfed8e77abed38a27c1fff93547997ec9486`，必须保持不变。将来恢复时从该处新建任务
分支，对照届时 main 重评合同，不覆盖新工作或改指保留 tag。跨仓 refs/gitlink 和 SDK pin
由集成负责人分别核对。Exa `4343f0b7` 仍是本机独立提交，本结果不授权推送或合 main。

本 ADR 替代旧的 EKO 终端入口扩张/对等与 A2A/LSP 保留目标，包括历史 ADR 0010、0012、
0017、0042 的相关范围假设；其历史源码和实现证据保留，不把当前生产架构改写成已经退役。

最初的文档切片仅改规则、双语 ADR/架构页和 parity 登记。表达已批准方向无需改 Rust/TS、manifest/
lock、生成合同、CI 或 examples。README、功能总览和快速开始继续描述当前已实现基线，
随实际退役再修改操作承诺。官网 owner 应镜像审阅后的正式页面并区分现状与目标，不将退役
宣传为已交付。

用户新增构建资源约束与固定上游设计同时生效：各 worktree 使用独立自有 target，同机最多
两条 focused Rust 构建/测试并发，完整 all-feature/GUI/语言矩阵串行，单条 jobs 至多2，
dev/test debuginfo 关闭。重构建前后记录 `df`/`du`，低于 5 GiB 可用空间时停止新重构建。
每个编译/测试批次结束，先保存 target 外必要日志、真实退出状态和需保留的 Host/制品，
确认准确 target 没有活动 cargo/rustc/Host 或其它制品读取者，再及时 `cargo clean` 自有准确
target。原始日志放 ignored `.echo-semantic/local/<task-id>/`；不得先删后声称证据可恢复，
不得清其它聊天或 primary target。本次纯文档没有 Cargo 产物，清理不适用；源码/worktree/
ref 清理仍按独立协调与授权处理。

后续实现先证明 GUI 启动/配置、IM 收发/重连、cron 执行/occurrence 恢复、GUI/IM HITL、
Task/Subagent 恢复、终端清理、隐藏窗口运行和真正退出结算，再删终端入口。
框架删除前须验收 EKO/SDK 已不再请求或调用 A2A/LSP；随后核对活跃实现、注册、feature、
宣传确已退出，并通过剩余消费者/examples/CI 矩阵。文档 parity 或源码搜索本身不是运行时或
原生验收。


## EKO 消费者实现

EKO 移除 `lsp` feature 请求、五工具注册、manager、插件 LSP 代分支、扩展命令/类型投影、
配置热重载、GUI 设置与 invoke 注册。PluginRuntimeService 独立保留根目录权威和全部原
publication/rollback/cleanup 收据。退役组件明确诊断，用户文件保留，混合插件继续处理合法
组件。框架 A2A/LSP 实现与独立终端入口仍在；本消费者改动不代表 SDK 完成或框架已删除。
退役诊断直接消费框架桥接 `e4e50396876c5e7e85b52cdc3dbb510bab9c4f4d` 的既有
`PreparedPluginSet::diagnostics()`，保留完整冻结诊断的组件、插件、来源路径与严重程度，
仅为 `lsp` 组件映射 EKO 产品退役文案。不依赖 LSP 专属公开元数据，也不在 prepare 后重读文件。
框架加法诊断先行，EKO 再切换，后续框架物理删除继续保留通用旧输入诊断而不保留执行器。
原 applicability 与非 LSP Error 拒绝策略不变，不新增 decoder 或状态权威。
