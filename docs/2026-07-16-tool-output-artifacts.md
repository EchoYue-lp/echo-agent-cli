# M6 超长工具日志 Artifact

> 2026-07-25 更新:本文的框架 artifact writer、metadata 和 retention 合同继续有效。
> GUI 会话投影、详情入口和读取策略已由
> `docs/2026-07-25-gui-tool-execution-lazy-loading.md` 取代:前端只持有不透明
> `detail_ref`,由应用层 repository 统一分页读取实时 JSONL 或框架 artifact,不再直接
> 接收物理 `artifact_path`。

## 目标

当 stdout、stderr 或通用工具结果超过 1 MiB 时，保留完整可访问日志，同时保证模型上下文、conversation 文件、trace 和三端 UI 只携带有界投影与 artifact 引用。artifact 缺失是存储状态，不改变原工具的成功/失败终态。

## 业界依据

- [Claude Code session storage](https://code.claude.com/docs/en/agent-sdk/session-storage.md)：本地磁盘是会话主存储，外部存储是可选镜像；主 session key 可拥有 sidecar/subagent 子路径，删除主 key 时级联删除子项，并由宿主明确负责 retention。
- [Claude Code sessions](https://code.claude.com/docs/en/sessions.md)：会话在执行中持续写本地 transcript，恢复读取持久化事实，而不是依赖当前 UI 内存。
- [OpenAI Codex rollout recorder](https://github.com/openai/codex/blob/main/codex-rs/core/src/rollout.rs)：完整 rollout 使用独立持久化记录器保存。
- [OpenAI Codex thread rollout truncation](https://github.com/openai/codex/blob/main/codex-rs/core/src/thread_rollout_truncation.rs)：原始持久化 rollout 与送入模型的有效有界历史分离，截断发生在派生视图而不是破坏原始事实。

跨系统共性是“完整原始记录独立持久化，有界视图用于模型/UI，生命周期由 session owner 管理”。EKO 沿用该模式，不新增数据库或平行事件源。

## 现状审计

- `AgentRunSnapshot::process_tool_output` 已能把 1 MiB 以上结果写临时文件，但目录随机、保留期仅 1 小时，缺少 hash、产品生命周期和用户入口。
- shell 流式执行只在最终 `ToolResult` 保留前 1 MiB；即使后续 spill，写出的也不是完整 10 MiB 日志。
- GUI/TUI 已保存 `ToolResult.metadata`，conversation 最终投影也已有 128 KiB/1000 行上限，因此无需新增 DTO 或 store。
- TaskRuntime 已有 `Artifact`，但 `ArtifactProduced` 事件未持久化 path/metadata，文件重建后会丢引用。

## 框架与应用边界

### `echo-agent`

- 提供通用 `ToolOutputArtifactConfig`、流式 writer、SHA-256、scope 清理原语。
- shell 在读取 stdout/stderr 时边流式发送、边写完整 artifact；最终结果继续只保留 1 MiB 内存投影。
- 非流式 Browser/MCP/file/search 等大结果在统一 truncation stage 写 artifact。
- ToolResult metadata 和 RunEvent 保存 path、bytes、SHA-256、retention，不引入 EKO 产品状态。
- 未配置应用目录时使用临时目录与 1 小时保留策略，供其它框架复用方选择。

### `echo-agent-cli`

- 统一使用 `~/.echo-agent/sessions/artifacts/tool-logs/`，避免 workspace/worktree 删除导致日志失效。
- scope 布局为 `<conversation-hash>/<run-hash>/<call-tool-uuid>.log`；可读前缀后追加原值 SHA-256 前 12 位，避免不同 ID 清洗后碰撞。
- retention 为 `conversation_or_30d`：删除 conversation 时级联删除；遗留 scope 最长 30 天，由后续写入机会清理。
- GUI 直接用系统默认应用打开；TUI 提供 `/open-artifact [call-id|path]`；CLI terminal 与 `/trace` 输出同一路径。
- artifact 不存在时显示 `artifact missing`，但不把成功工具改成失败。
- TaskRuntime `ArtifactProduced` 事件持久化 path/metadata，文件权威重建不再丢引用。

## Metadata 合同

- `output_handling=spilled`
- `artifact_kind=tool_log`
- `artifact_media_type=text/plain; charset=utf-8`
- `artifact_status=available|write_failed`
- `artifact_path`
- `artifact_bytes`
- `artifact_payload_bytes`
- `artifact_sha256`
- `artifact_retention`
- `original_bytes`、`returned_bytes`、`estimated_tokens`

## 失败语义

- artifact 写入失败：工具保持原 success/failure，metadata 标记 `artifact_status=write_failed` 和简短错误；模型仍获得有界输出。
- artifact 后续缺失：三端显示缺失，不回写 ToolResult，不触发工具重试。
- conversation 删除成功但清理失败：对话删除不回滚，记录 warning，30 天兜底清理继续生效。

## 验收

- 10.5MB shell 输出：流事件可持续消费，最终 ToolResult 仅保留 1 MiB，artifact 保存完整内容并生成 64 位 SHA-256。
- 非流式超长结果：模型只收到 500 字符预览、路径、大小和 hash；`read_file` 可恢复完整内容。
- conversation 稳定投影继续有界，并保留完整 artifact metadata。
- GUI/TUI/CLI 共享引用；缺失状态与工具终态相互独立。
- 删除 conversation 可清理整个 artifact scope；TaskRuntime artifact path/metadata 可从事件重建。
