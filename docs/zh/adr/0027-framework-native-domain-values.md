# ADR 0027：Framework 原生领域值

- 状态：已采纳
- 范围：`echo-agent-cli/echo-agent-app-core` 和 Tauri permission commands

## 决策

EKO 直接保存并传递 framework 所有的领域值。权限状态使用
`echo_agent::tools::permission::PermissionRule`，不再定义平行的
`PermissionRuleConfig` 或 `PermissionBehavior`。Tauri 只使用 framework 的
`FromStr` 实现解析包括 `PermissionMode` 在内的 transport 字符串，再注入 EKO description。

这样产品 policy 和 surface 解析仍留在 EKO，而通用权限语义只有 SDK 类型这一份权威。
delivery 和 Subagent outcome 也遵循同一标准：开发期不保留来源命名的 framework 转换
helper 或镜像通用 DTO。
`ConversationInputOutcome` 同样只是 EKO wire 名称，Rust 值直接使用
`echo_agent::agent::AgentSteerTurnOutcome`，不再逐枚举转换。
`ChatSteerOutcome` 也只保留为 GUI wire 名称，Rust 直接复用同一个 framework outcome。
`PluginInstallScope` 只作为 EKO command wire 名称，Rust 值直接使用 framework
`PluginScope`，CLI 简写由 framework 的标准 `FromStr` 实现（`scope_value.parse()`）解析。
Model provider view 同样直接使用 framework `LlmApiProtocol` 和
`ModelInputModality`；旧的 `*Wire` 枚举只是逐枚举转换，现已删除。
MCP server entry 也直接使用 framework `McpServerEntry`，仅在 EKO command 边界保留
稳定的 `McpServerConfig` wire 名称；顶层 command document 仍由 EKO 拥有并负责 request schema 校验。
Agent delivery receipt 直接使用 framework `JournalDurabilityStatus`；其带 serde tag 的形状
现在就是 canonical durability wire 值，不再定义 EKO enum 副本。
带 revision 的 disabled-tool 策略直接使用 framework `ToolControlService` 和
`ToolControlSnapshot`；EKO 只增加 registered-tool 校验、pool fan-out 和
`effective_enabled` UI receipt 字段。
EKO 同样直接保存 framework `ExecutionUsage`；`SubagentRunUsage` 只作为 generated
TypeScript wire 名称保留。Task executor 不再定义 `TaskExecutionUsage` 临时 DTO；delegated
Subagent 与 primary-Agent turn 都通过各自结果的 `usage()` API 直接提供同一个 usage 值。
chat、continuation 与 TaskRuntime lifecycle 也直接传递 framework `TurnReceipt`；EKO 不再定义
`ChatTurnOutcome`，秒数取整和展示截断只在最终 surface 边界处理。
持久化 Subagent command identity 直接使用 framework
`SubagentCommandIdentity`；EKO 名称只是同一值的应用层公开别名。
durable command phase 同样直接别名 framework `SubagentCommandPhase`；EKO 只保留
自己的 UI/status projection。

## 影响

- 权限列表响应使用 framework 的 serde 形状。
- matcher、behavior 或 source 无效时，在请求边界直接失败。
- framework 缺少通用操作时，优先扩展 framework，不在应用层增加转换 helper。
