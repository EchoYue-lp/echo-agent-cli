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

- EKO 在现有 conversation identity lock 内以 `ensure_projection_epoch` 作为唯一的 Managed 创建/确保操作，并在向 turn 发布前取得框架 projection epoch。tombstone 或 epoch 冲突是准入失败，不能暗中重建。
- GUI 分支、Side 首轮快照、TUI 原有 fork/rewind/clear/resume，以及 REPL resume/reset/new/undo 共用应用协调器：导入 canonical 消息、CAS 对应 runtime 游标、再恢复 Agent。直接替换命令全程持有会话 identity lock 并暂停回合准入。若导入已提交而检查点尚未写入，只能用 Store 持久 locator 和精确消息重建并重放原请求；直接重试先结算这笔债，不再推进第二个 epoch。新导入在提交前验证检查点历史。
- Rename 先修复导入债务，再使用框架 revision-fenced 元数据更新。跨工作区移动按固定顺序取得源/目标准入并严格证明目标 ID 不存在，导入并恢复目标 Agent 后，用持久、绑定目标 epoch 的可见性转移意图把源隐藏行映射到目标新 ID，再通过 EKO 聚合删除退休源会话。目标若已有退休的 runtime generation，则拒绝同名回迁；回迁需使用新的 conversation 身份。持久的 Prepared/TargetImported/SourceRetired handoff 意图在启动时重放，不导入第二个目标 epoch；follow-up 准入后保留绑定源 epoch 的完成收据，让成功响应丢失后的相同重试得到原结果。若源身份随后重现，旧收据显式报冲突而不返回假成功。未完成意图在调用方返回错误后仍阻止两端普通 turn、替换、创建和删除准入。目标准入保持关闭直到源退休或受控回滚完成；源退休结果不确定时保留目标并报告债务。
- Side 内部投递由 AgentRouter 持有投递事实，既有 EKO conversation visibility 文件在 effect 前持久化 intent，并在 Router terminal 前结算精确消息 ID。主 GUI 隐藏这些 ID，Side GUI 标注；用户随后发送相同文本仍是用户消息。visibility 写失败时非终态前沿保留供恢复。owner-loss 恢复先结算框架 transcript projection；若首行尚未提交，则先记录空 visibility 再写未知结果终态。Side 快照读取前后必须有相同 transcript revision，且 visibility 都已结算。不为 UI 元数据重写 Managed transcript。
- 聚合删除跨越 `ConversationCommitStarted` 前，EKO 从精确 live epoch 准备 `ManagedConversationDelete`，并写入既有删除 tombstone。每次重试都提交同一请求；没有该身份的旧 tombstone 不能在 commit 边界后猜测 epoch。
- 根删除必须获得已完成的框架 scope-retirement 收据。随后 EKO 删除收据返回的每个独立 runtime conversation transcript；若它是 managed transcript，则用 epoch-fenced delete。所有支线结算后才能推进既有 RuntimeState 删除步骤。
- EKO 的 `task_execute` 工具只弱引用所属 Agent，避免 Agent→工具→Agent 环。已准入工作结算后，workspace shutdown 卸载仍可能持有旧 manager 的 layered-memory 工具与 context-promoter 投影；Memory journal 仍是权威。Host 的 shared shutdown future 完成后也只保存结果，不再强引用自身。

## 影响

- 聚合删除仍是唯一 EKO 协调权威；框架 Managed transcript 与 runtime-state journal 保留自己的 CAS 语义。目标回滚已提交而 handoff 意图清理失败时，启动恢复识别目标 tombstone 并结算废弃意图，不再导入新 epoch。
- 缺少 managed 请求的旧未完成 tombstone 明确保留恢复债务，不绑定到新建的同名 conversation。
- 框架 examples 与 `echo-website` 不消费 EKO 删除 tombstone，无需同步修改。
