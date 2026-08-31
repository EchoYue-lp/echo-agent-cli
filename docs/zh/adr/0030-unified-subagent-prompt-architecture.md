# ADR 0030：Subagent 提示词统一架构

## 状态

已采纳

## 背景

EKO 此前只有部分路径完成收敛：builtin 角色使用 `EkoSubagentPromptCompiler`，plugin
角色仍可能直接使用 Markdown 和 framework 默认 compiler；TaskRuntime 把通用任务事实塞进
opaque JSON payload；team 历史单独转发；应用还自行解析可选 fenced JSON。仅在 system
prompt 中增加工具名称，也无法证明 description、disabled 状态和具体工具面真实一致。

语言、TaskRuntime、文件边界和 follow-up policy 属于 EKO 产品决策；compiler 与结构化消息
执行机制属于 framework 通用能力。本决策遵循 framework ADR 0024。

## 决策

1. builtin、plugin、direct、planned、fork、teammate、team 与 primary TaskRuntime 路径统一使用
   一个 `EkoSubagentPromptCompiler`。
2. 稳定 system prompt 只包含角色/方法知识、具体工具注册完成后生成的
   `ToolCapabilitySnapshot`、typed read/write/isolation/delegation boundary、语言规则和
   framework Result Contract。
3. capability snapshot 保存 tool name、限长 description、visible 与 disabled 集合。builtin
   role 在 disabled policy 发布时重新生成稳定 prompt；plugin factory 共享父 Agent 的 framework
   `ToolVisibilityPolicy`，后续实例观察同一个权威状态。
4. `SubagentTaskContext` 拥有动态 user goal、task title、workspace、files、execution checks、
   semantic acceptance criteria、artifacts 与 constraints。这些通用事实不再进入 EKO opaque
   payload；payload 只保留 DomainProfile、依赖摘要和产品 task-boundary policy。
5. `CompiledSubagentInvocation.messages` 是精确执行输入。compiler 拥有当前 typed message 并保留
   附件，同时删除紧邻且重复的当前 user turn。历史保持真实 user/assistant message；parent
   system prompt、tool traffic、reasoning 和 runtime projection 不再渲染成文本。
6. invocation tool allowlist 会在有效 workspace 已确定后，与 concrete Agent 的注册定义和共享
   disabled policy 合并；`SubagentInvocation.capability_override` 与 task context 分离，并且只在
   allowlist 缩窄稳定能力面时输出，普通 invocation 不重复注册期 catalog。
7. primary Agent 在工具注册完成后编译稳定 TaskRuntime system profile；
   `compile_primary_invocation` 只生成动态 messages。begin/end marker 保证 runtime prompt 或
   methodology baseline 变化时不会丢失其它 prompt section。
8. 应用 Subagent catalog 只从 EKO definitions 生成，删除 lossy 的 `from_registered` tag decoder。
9. 可选 suggested-task 字段复用 framework JSON framing；EKO 只校验和归一化产品字段。
10. plugin 转换直接保留 framework `SubagentDefinition.access_mode`；tool-control publication
    永不从 tags 重建 access。

## 影响

- prompt 声明可以直接与 concrete registered tool surface 对账；
- 新增 dispatch mode 或角色不能再建立第二个 prompt builder；
- system prompt 稳定可缓存，workspace 与 task facts 保持 invocation scoped；
- plugin/team 与 builtin Subagent 使用同一语言、边界和 outcome policy；
- 角色 Markdown 只保留 identity、method 和 domain knowledge。
