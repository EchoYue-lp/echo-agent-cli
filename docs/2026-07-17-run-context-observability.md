# M9 单 Run Usage/Cache/Protected Context 可观测性

## 目标

M9 将 provider usage、prompt cache、上下文预算、protected context 和 compression 串成一个可恢复、可按 run 查询的诊断事实。用户必须能从同一个 run 回答：实际 token 花在哪里、usage 是否由 provider 报告、cache 前缀为何变化、哪些内容被保护、何时发生压缩、是否存在 protected context 过量。

本阶段只提供可观测性和人工优化依据，不建设后台评分、自动建议、EvalRunner 或自动改 prompt；`echo-agent-cli` 不引入 SQLite。

## 业界依据

- [OpenAI Codex developer commands](https://learn.chatgpt.com/docs/developer-commands)：`/status` 展示当前会话配置、token usage 和剩余 context capacity，`/usage` 展示账户累计活动。当前上下文与累计用量是不同语义，不能共用一个累加器冒充。
- [OpenAI prompt caching](https://developers.openai.com/api/docs/guides/prompt-caching#requirements)：每次响应的 provider `usage` 是 cache read/write 的权威；Responses 与 Chat Completions 分别在 input/prompt token details 中返回 cached tokens，新模型还返回 cache write tokens。估算不能覆盖 provider 事实。
- [Claude Code status line](https://code.claude.com/docs/en/statusline)：`context_window.total_input_tokens/total_output_tokens` 来自最近一次 API 响应，不是会话累计值；status line 在 assistant response 和 `/compact` 后刷新。客户端估算成本明确标注可能与账单不同。
- [OpenTelemetry traces](https://opentelemetry.io/docs/concepts/signals/traces/)：一次端到端操作用 trace correlation 关联多个独立 span；每个执行单元有自己的 identity，通过 parent/link 表达层级，不能把业务 run id 与并发执行 span id 混成同一个可写记录。

跨系统共性：provider response 是准确 usage 的来源；当前上下文快照、会话累计成本和本地估算必须分开展示；压缩是有时间意义的事件；并行 subagent 使用独立 trace identity，再按业务 run 关联聚合。

## 现状审计

- `echo-core::llm::Usage` 已归一 OpenAI-compatible、Anthropic 和 DeepSeek 的 prompt/cache 语义，并有 provider fixture；缺口是 OpenAI token details 的 `cache_write_tokens` 尚未读取。
- framework `RunEvent::LlmCall` 已持久化 provider token、estimated context、protected token/message 数，`JsonlRunStore` 可跨重启；但没有 model/provider、cache fingerprint、角色 token breakdown、context limit 和 compression event。
- framework 已有 `PromptCacheLayout`、SHA-256 `stable_prefix_hash` 和 `PromptCacheShape` 日志。后者又使用一套 FNV hash，形成重复指纹实现；实际指纹没有进入 durable trace。
- external TaskRuntime run id 当前被复用为 trace run id。并行 subagent 因而可能写同一个 read-modify-write `Run`，而且 external run 已存在时 `start_trace_run` 不创建 framework trace，正式 TaskRuntime run 的 durable trace 可能为空。
- EKO GUI 另有内存 `TraceCollector`，只记录部分 main-agent 事件；重启即丢失，tool/agent/compression 统计大多不可达。CLI `/trace` 读取 framework run store，GUI 与 CLI 不是同一事实。
- TaskRuntime 又维护内存 `UsageRecord`、`query_usage_records`、`RunUsageSummary` 和 `SubagentLlmUsage` 事件。注释一处称“持久化”，另一处明确“重启丢失”；main usage 不写 runtime events，subagent usage重复写两套。
- GUI `UsageTrendsPanel` 是跨时间窗口指标面板，与总纲“只做单 run 诊断、不建设指标平台”冲突。
- `PromptAssembly` 已报告模块估算与截断，ContextManager 已有 latest-wins projection、role breakdown、protected count/tokens 和 compression metrics；这些能力尚未与 run trace 串联。

## 框架与应用边界

### `echo-agent`

负责任何 Agent 都需要的通用事实：

- provider usage/cache write 归一；
- 唯一 trace identity 与 parent business-run correlation；
- durable LLM call、cache fingerprint、context breakdown、protected usage 和 compression event；
- `JsonlRunStore` 的跨重启查询。

不加入 EKO 面板、TaskRuntime DTO、中文诊断建议或产品路由字段。

### `echo-agent-cli`

负责 EKO 产品视图：

- 将一个 framework run 或同一 parent run 下的 child traces投影为单 run 诊断；
- 合并 primary-agent `PromptAssembly` 模块报告；
- 生成 cache/protected/context 的人工诊断说明；
- GUI/TUI/CLI 使用同一个 DTO/formatter。

不再维护第二套 trace collector、usage ledger 或趋势数据库。

## Identity 与持久化合同

1. 每次具体 Agent invocation 创建唯一 `trace_run_id`，无论是否带 external runtime context。
2. TaskRuntime `run_id`、turn id、execution id 作为 correlation metadata 写入 trace run；工具仍使用 external runtime run id，不受 trace identity 影响。
3. 一个 TaskRuntime run 可关联多个 main/subagent trace；每个 trace 文件只有一个执行者，避免并行 read-modify-write 丢事件。
4. GUI 按 parent run 聚合；普通 chat 没有 parent run 时按 trace run 展示。
5. provider usage、LLM call context 和 compression 全部写入 framework JSONL，重启后可重建；实时 UI 事件只负责增量渲染，不成为权威。

## LLM Call 诊断合同

每次 `RunEvent::LlmCall` 记录：

- provider-reported prompt/output/cache-read/cache-write 与 `usage_reported`；
- 本地 `estimated_context_tokens`，明确仅为 estimate；
- context limit、system/user/assistant/tool/summary/memory 估算分布；
- protected message count/tokens；
- SHA-256 stable prefix、system/canonical、tools schema hash；
- message/tool 数和调用耗时。

`usage_reported=false` 时准确 usage 为 unknown，UI 只展示 estimated context，不把估算写进 provider totals。

## Compression 与 Protected 合同

- auto/manual compression 均记录 before/after messages/tokens、protected count/tokens 和来源。
- projection 继续使用 framework envelope + marker latest-wins；M9 不新增重复注入机制。
- protected context 超过配置 context limit 的 25% 或 32k tokens 时产生 warning 诊断；这只是本地数据保真/容量预警，不是权限门控。
- tool result 原文继续由 M6 artifact 合同保存，protected context 只保留有界摘要、任务事实和引用。

## 删除与归一

- 删除 app `TraceCollector` 及其 in-memory session API。
- 删除 TaskRuntime `UsageRecord`、aggregation/query/summary、`SubagentLlmUsage` 和对应 Tauri/TS 绑定。
- 删除 GUI `UsageTrendsPanel`，Observability 改为 durable single-run inspector。
- 删除 Tauri main-agent 自算 system/tools/cwd hash 和 usage 双写；fingerprint 在真实 LLM request 边界统一计算。
- 删除 framework `PromptCacheShape` 的 FNV hash，复用 `echo-core::llm::cache` 的 canonical SHA-256 实现。
- 删除未初始化、无读取点的 `TraceState.analyzer` 字段。

## 三端合同

- GUI：run 列表 + 选中 run 的 provider totals、每 call source、cache fingerprint变化、context breakdown、protected warning、compression timeline 和 prompt modules。
- CLI：`/trace [run-id]` 输出同一 DTO；无 id 时选择最近 run。
- TUI：新增同语义 `/trace [run-id]`，复用同一 formatter；`/status` 继续只显示当前上下文快照，不冒充累计成本。

## 验收

- OpenAI cached/cache-write、Anthropic cache read/write、DeepSeek hit/miss fixture 全绿；provider totals 与 estimate 分离。
- 带 external TaskRuntime run id 的 main/subagent 各自生成唯一 trace，parent correlation 一致，并行事件不丢失。
- 相同 system/tools 的连续 call 保持 component hash；改变 tool schema 或 system/canonical prefix 能定位到具体维度。
- auto/manual compression 都进入 durable timeline；重启后仍可查询。
- protected context 超阈值给出明确 warning，未超阈值不误报。
- GUI/TUI/CLI 对同一 fixture 的 totals、source、cache hashes、protected 与 compression 结果一致。
- app 内不再存在 `TraceCollector`、`UsageRecord`、`UsageTrendsPanel` 或 SQLite usage 路径。
