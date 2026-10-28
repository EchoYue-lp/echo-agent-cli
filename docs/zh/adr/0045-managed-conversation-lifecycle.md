# ADR 0045：EKO 绑定 Managed Conversation 创建与聚合删除

## 状态

已采纳

## 背景

最新框架在 `ConversationStore` 与 `RuntimeStateStore` 宣告 managed persistence 时拒绝旧 transcript 写入和聚合删除。EKO 已拥有 conversation identity guard、workspace generation、聚合删除 tombstone 和独立 runtime conversation ID。原始 `ensure_conversation` 或 `delete_persisted_conversation` 不再建立所需 epoch 与稳定重试身份。

## 候选方案

1. 保留旧调用并忽略 Managed 失败。拒绝：conversation 可见却无有效 projection epoch，删除也会失去 ABA fence。
2. 新增 EKO transcript/delete store。拒绝：重复框架与现有聚合权威。
3. 用现有 EKO identity guard 和 tombstone 适配框架 managed API。采用。

## 决策

- GUI 显式 Fork 按钮为既有 `branch_conversation` 增加 `include_turn`：false 保留编辑/重新生成的目标用户回合前边界；true 复制到该回合已提交的最终回复。读取快照期间持有源会话 identity 与空闲回合准入。GUI 用户回合序号通过既有可见性投影排除内部 Agent 投递，canonical 工具历史仍完整保留。既有 managed import 协调器恢复新的普通会话后，GUI 刷新工作区列表并加载该会话，不启动另一轮模型调用；迟到响应不能覆盖更新的工作区/会话选择。这借鉴用户提供的 Codex 回复操作区，不新增 Side 关系或 Store。
- EKO 在现有 conversation identity lock 内以 `ensure_projection_epoch` 作为唯一的 Managed 创建/确保操作，并在向 turn 发布前取得框架 projection epoch。tombstone 或 epoch 冲突是准入失败，不能暗中重建。
- GUI 分支、Side 首轮快照、TUI 原有 fork/rewind/clear/resume，以及 REPL resume/reset/new/undo 共用应用协调器：导入 canonical 消息、CAS 对应 runtime 游标、再恢复 Agent。直接替换命令全程持有会话 identity lock 并暂停回合准入。若导入已提交而检查点尚未写入，只能用 Store 持久 locator 和精确消息重建并重放原请求；直接重试先结算这笔债，不再推进第二个 epoch。新导入在提交前验证检查点历史。
- Rename 先修复导入债务，再使用框架 revision-fenced 元数据更新。跨工作区移动按固定顺序取得源/目标准入并严格证明目标 ID 不存在，导入并恢复目标 Agent 后，用持久、绑定目标 epoch 的可见性转移意图把源隐藏行映射到目标新 ID，再通过 EKO 聚合删除退休源会话。目标若已有退休的 runtime generation，则拒绝同名回迁；回迁需使用新的 conversation 身份。持久的 Prepared/TargetImported/SourceRetired handoff 意图在启动时重放，不导入第二个目标 epoch；follow-up 准入后保留绑定源 epoch 的完成收据，让成功响应丢失后的相同重试得到原结果。若源身份随后重现，旧收据显式报冲突而不返回假成功。未完成意图在调用方返回错误后仍阻止两端普通 turn、替换、创建和删除准入。目标准入保持关闭直到源退休或受控回滚完成；源退休结果不确定时保留目标并报告债务。
- Side 内部投递由 AgentRouter 持有投递事实，既有 EKO conversation visibility 文件在 effect 前持久化 intent，并在 Router terminal 前结算精确消息 ID。主 GUI 隐藏这些 ID，Side GUI 标注；用户随后发送相同文本仍是用户消息。visibility 写失败时非终态前沿保留供恢复。owner-loss 恢复先结算框架 transcript projection；若首行尚未提交，则先记录空 visibility 再写未知结果终态。Side 快照读取前后必须有相同 transcript revision，且 visibility 都已结算。不为 UI 元数据重写 Managed transcript。
- 聚合删除跨越 `ConversationCommitStarted` 前，EKO 从精确 live epoch 准备 `ManagedConversationDelete`，并写入既有删除 tombstone。每次重试都提交同一请求；没有该身份的旧 tombstone 不能在 commit 边界后猜测 epoch。
- 根删除必须获得已完成的框架 scope-retirement 收据。随后 EKO 删除收据返回的每个独立 runtime conversation transcript；若它是 managed transcript，则用 epoch-fenced delete。所有支线结算后才能推进既有 RuntimeState 删除步骤。
- EKO 的 `task_execute` 工具只弱引用所属 Agent，避免 Agent→工具→Agent 环。已准入工作结算后，workspace shutdown 卸载仍可能持有旧 manager 的 layered-memory 工具与 context-promoter 投影；Memory journal 仍是权威。Host 的 shared shutdown future 完成后也只保存结果，不再强引用自身。

## 影响

- Managed 历史导入只序列化调用方的 canonical rows 与 transcript cursor，不合成基础 System 消息。当前 canonical 根 prompt 的渲染和恢复由框架 `ReactAgent` resume 唯一负责，避免应用基础 prompt 与框架完整根 prompt 并存。原有可见 transcript 输入校验及框架对独立 System 上下文的保留规则均不变。
- 聚合删除仍是唯一 EKO 协调权威；框架 Managed transcript 与 runtime-state journal 保留自己的 CAS 语义。目标回滚已提交而 handoff 意图清理失败时，启动恢复识别目标 tombstone 并结算废弃意图，不再导入新 epoch。
- 缺少 managed 请求的旧未完成 tombstone 明确保留恢复债务，不绑定到新建的同名 conversation。
- 框架 examples 与 `echo-website` 不消费 EKO 删除 tombstone，无需同步修改。

## 完整回复的两种工作空间分叉（2026-10-27）

本节落实已批准合同。正式集成、产品构建、原生窗口及真实 SSH 的资格分别记录，ADR 的采纳不表示这些验证已完成。

- 每轮只由运行时确认的最终完整回复提供一个入口；正文非空、工具调用结束或旧 ThinkEnd 不独立拥有最终资格。原生消息阶段和执行结算沿框架/EKO 唯一权威。未知旧历史不按文字措辞补造成功。编辑/重新生成保持原回合前边界；既有 Side Conversation 历史和树形导航保留。
- 当前工作空间与新 Worktree 共用 canonical 前缀和原 managed import。新 Worktree 固定准备时的源 commit OID，源 HEAD 后来前进不改变本请求基点；不复制 dirty、untracked 或 ignored 代码。detached HEAD 有有效提交时可用，非 Git/unborn 只禁新 Worktree并说明原因。
- 两种形式都创建空闲普通会话，沿目标工作空间原模型/权限装配。不克隆 Goal、TaskRun、Subagent、控制 owner 或源会话模型覆盖，不自动发出用户输入。复制历史中的目标文字不等于复制仍可执行的 Goal。
- 稳定 request_id 绑定来源 workspace/generation、conversation、用户回合与 canonical 前缀 SHA。目标登记后仍按返回 generation 获取真实 scoped runtime 并持有原 lease，不能仅凭同名 ID 取得后继代际。迟到回执不能抢新导航或覆盖已重开目标的列表信息。
- 原 create_worktree 与回复分叉使用相同 Git/FileSystem/锁。原生 `git worktree list --porcelain -z` 保留准确路径、完整 OID 和请求锁原因。add 成功而 list 失败保留已创建事实；重试先重读，不重复 add、不 force 删除。普通删除遇到迟到用户写入而失败时，重读原身份后恢复本请求锁；确认失败继续报告原错误与资源位置。
- SSH 来源仅供原 Git 操作；目标由原 GUI 建立独立认证 draft。禁止迁移来源 workspace 的连接键或同 Arc 双登记。原创建/重连在实际转移点核验刚才验证的物理连接，被替换即拒绝转移；双方各自关闭只结算自己的 owner。
- 只搬运已知产品原文/输出描述符与持久内联附件，不字符串改写用户任意路径，不复制整个 `.eko`、journal、凭据或运行控制。目标副本位于原 reader 根，校验实际 bytes、大小与 SHA，并由原 bounded reader 完整回读。没有资源引用的历史无需 artifact reader 配置。
- 资源失败不发布半份历史，已写文件仍随回执保留，并明确 reader_verified 是否成立。Git目录、Workspace、conversation 与 import 分别报告真实事实。既有目标没有 managed import 回执时拒绝覆盖，不能以空会话或名称推断所有权；已提交导入继续走原 locator/CAS，不重复推进 epoch。

依据沿 [Git Worktree](https://git-scm.com/docs/git-worktree)、[Claude Code Worktrees](https://code.claude.com/docs/en/worktrees.md) 与既有 managed conversation/Workspace 设计。Eclip 采用用户明确的本地 HEAD-only；对话前缀、提交代码和活动执行资源各由原 owner 负责。该能力属于 EKO 产品策略，不新增框架 Workspace API、任务 store、Git 算法或全局补偿状态机。


## 2026-10-28 补充：资源关联随原生 Message 持久化

背景：正文描述符、path 命名空间、正文或 Parts 的哈希、相近时间都不能证明资源来源。终态后补记 journal 会漏掉 live Steer，以及非终态 observation 后仍运行的物理 owner。

决策：复用 Framework 已有 ContentPart::ResourceLink / LinkedResource.meta。PreparedUserTurn::to_message 从真实 InputResourceRef 在同一条 Message 生成 eko_input_resource v1，用户正文与内联图片仍在原 Parts。初始输入与 GUI/IM live Steer 共用此 producer；TrackedSteer、Context、canonical projection 和 checkpoint 直接携带关联，不增加 Message 字段、store、后台 capture 或终态 join。

关联包含真实 path、bytes、SHA、source，以及原 producer 生成文字的字符范围和哈希。范围只定位真实引用，不能授予来源。Reply Fork 只迁移 typed link；原 reader 校验 bytes/SHA，复制并实际回读后更新 link 和该范围，保留未知 metadata、消息身份、时间及原调用/结果 JSON。中途失败不发布半段历史，仍返回已落地副本的回执。旧纯文字、用户 JSON/Markdown、未知外部链接保留为历史，不扫描文件、不升级来源。

分层：Framework 拥有 Parts 的序列化、Steer/Context/checkpoint 和通用工具结果资源载体；EKO 拥有输入落盘、input meta、目标 workspace 复制及呈现。工具载体须由真实 ToolResult.artifact producer 生成，不能用唯一 call_id/tool_name 或应用私有 ToolExecutionRepository 猜测补回。

GUI 与可编辑目标正文通过 resource_history_display_text 隐藏本产品 link 的模型 fallback，原 display_content 继续优先。旧引用提示经原 Fork receipt 的 warnings 呈现，不改变 native final 准入。

弃用正文扫描、按内容命中 sidecar、终态补记 journal。复用已有资源能力，无新增 F Message 字段、存储或 canonical digest 算法；新 Parts 自然进入既有 digest。参考本轮按发布索引核对的 types、conversation projection、steer、stream_channel、snapshot 源码，精确 SHA 见 framework-read-evidence.json。现有 provider adapter 已将 ResourceLink 转成模型文字，无新增供应商 wire variant。

实际 reader、Managed 文件存储、源删除后再 Fork 由候选 Rust 回归覆盖。本轮未执行 Cargo/native/SSH/PTY，结构闭合不等于运行验收完成。工具 producer 的最小 F 增量尚未正式采用。


## 2026-10-28：当前消息元数据合同与分叉步骤

用户明确开发阶段不保留旧格式兼容。AttachmentsPayload 只接受当前 object，旧 thinking 数组解析与 thinking_segments/execution_rounds 双轨 DTO、round types 和旧接纳测试退役。Reasoning/正文继续来自原生 assistant blocks；工具步骤通过 canonical tool_calls 和 execution_steps 的原 call_id 承载；实际 execution_status/final_answer_source、native_history_import、附件与原始资源 bytes/SHA/reader 不受这次表面退役影响。

Fork 和 Side 复制前缀只移除来源会话本地的 UI message_id，不再清除 canonical 工具步骤。测试沿实际当前 Message producer 验证 clone 后的原消息内容与 call_id 不变，不依赖手写旧 wrapper。目标 runtime/scope 的普通 idle、同版 cold/history/reload/refork、CAS/幂等及不重放真实副作用仍属于必验合同；没有原 publication 的文本不能因此被补写为最终成功，两个旧准入旁路由 EKO 主 owner 与原 typed producer 统一闭合。
