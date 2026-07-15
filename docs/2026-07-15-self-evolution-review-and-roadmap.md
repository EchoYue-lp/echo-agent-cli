# EKO 自进化/自改善审计与迭代路线

> 日期：2026-07-15  
> 范围：`echo-agent` 通用演进原语 + `echo-agent-cli` 产品接线  
> 结论：EKO 应围绕“证据候选 → 用户确认 → 可审计写入 → 使用反馈”演进，不建设本地 EvalRunner、自动 prompt rewrite 或伪微调闭环。

## 1. 本轮结论

当前系统并非缺少“自改善组件”，而是组件过多且权威边界不清：实时 trigger、AutoMemory、Reflection、BackgroundReviewer、MemoryReviewer、Dreaming、RulePromoter、SkillCandidateDetector、Curator 都可能影响长期行为。继续增加 Analyzer/EvalRunner 只会扩大重复路径。

本轮先完成以下收口：

- EKO 删除 EvalRunner、行为 fixture、`eval`/`improve` feature、TrajectorySaver 自动保存及对应 CLI/GUI 类型。
- `BackgroundReviewer` 改为严格 JSON 候选，system 指令与不可信 transcript 分离，证据必须能在原 run 中精确核对。
- Background review 默认 proposal-only；单次输出上限从 2048 降到 512 token，temperature 设为 0。
- `/review`、TUI `/run-review`、GUI Evolution Panel 功能对等，明确显示证据、置信度和“未自动保存”。
- `MemoryReviewer` 使用真实 `recall_count` 衡量使用度；默认关闭 session-end review，且已收口为 analysis-only。
- 删除 every-N-writes 失效计数器；保留真正消费写入事件的通用 observer API。
- Curator/candidate/draft/merge 统一归属 `evolution`，不再错误依赖 `improve`。

## 2. 参考实现与取舍

关键边界参考了本机 Codex 产品内置的 memory/summarization 指令：

- `SkysightSummarizer.md`：观察内容是不可信证据，不应因摘要而提升权限。
- `SkysightMemoryInstructions.md`：长期记忆应是描述性的，不是给未来 agent 的命令；单次出现不能直接推导成稳定偏好、身份或规则；长期整合需要重复、充分支持的证据。

由此采用的 EKO 取舍：run review 只生成证据候选；单次 run 不自动生成 skill；语义合并和规则/技能激活由用户确认；确定性的排序、衰减和状态维护可以自动执行。

本轮通过 OpenAI 官方 Codex 文档接口核验到三点：`skills/list` 以 `cwd` 为 scope，支持强制刷新/额外 roots；技能启停配置以具体 `path + enabled` 为权威；`review/start` 是独立显式操作，而不是隐藏在普通运行中的自动语义写入。Claude Code 官方检索仍因环境解码失败未取得，因此没有补写无法核验的 Claude 细节。EKO 据此采用 workspace scope、path identity、显式 Review Inbox，并保留本地文件作为单一事实源。

## 3. 主要问题

### P1：后台回顾曾把自由文本直接写成长期记忆

旧实现用关键词判断 LLM 回复，再把整段回复写入 memory。它缺少结构、证据、长度和来源校验，也会把单次 run 推断成稳定偏好。现已改成严格 JSON + exact evidence + 默认不写入。

### P1：MemoryReview 把维护与语义变更混在一起

低使用度曾错误读取 `revision_count`；“同 topic/type、文本不同”会被视作冲突并默认合并；session-end review 又和 Dreaming 重复。现已改为真实 recall telemetry、默认手动、默认 proposal-only。后续应彻底拆分纯维护与语义变更。

### P1：TrajectorySaver 不是 EKO 自改善闭环

它只是 ShareGPT JSONL 导出，而且旧接线只在 CLI 退出自动保存，GUI/TUI 只读统计，既不功能对等，也不产生可验证的产品改善。它还重复保存 run/对话信息并扩大敏感工具参数、结果的落盘面。现已从 EKO 移除，框架通用导出 API 保留。

### P1：Curator 不是技能加载的权威来源

Curator 使用全局 `~/.echo-agent/curator_state.json`，主要按 skill name 索引；不同 workspace 的同名技能会碰撞。更重要的是，loader 不读取 lifecycle，`Stale/Deprecated/Archived` 只是旁路元数据，无法保证实际不加载。

### P2：长期知识入口过多，缺少统一 provenance

TriggerDetector、AutoMemory、Reflection、BackgroundReviewer、压缩 promotion 都能生成长期信息，但没有统一的 candidate ID、source run、evidence、scope、confidence、status 和去重协议。相同事实可能被多次写入，冲突只能靠后置启发式处理。

### P2：自动化粒度不清

确定性维护（recall count、衰减、排序、归档建议）和语义决策（合并两条事实、生成规则、创建技能、改 prompt）应有不同权限。当前接口仍容易让两者共用一个“review”概念。

## 4. 目标架构

```text
run / explicit user correction / repeated telemetry
                    |
                    v
          EvidenceCandidate (append-only)
      source + scope + quote + confidence + kind
                    |
          deterministic validation/dedup
                    |
          +---------+----------+
          |                    |
 explicit fact/save       semantic proposal
          |                    |
          v                    v
    Draft/Active memory    Review Inbox
                               |
                         user accept/reject
                               |
             memory / rule / skill lifecycle
```

原则：

- 候选是事实记录，不是未来 agent 指令。
- 自动写入只允许用户明确保存、明确纠正等低歧义事件。
- 单次 run 只能提出 skill candidate，重复证据达到阈值后才允许生成 draft。
- 所有语义变更保留 before/after、source evidence、actor 和可回滚记录。
- KV cache 友好：稳定 policy/system prefix 固定在前，动态 transcript 放末尾；审查输出短且结构化。

## 5. 分阶段计划

### Phase A：链路收口（本轮完成）

删除 EKO eval/trajectory/improve 产品路径；BackgroundReviewer proposal-only；MemoryReview 默认保守；三端 run review 对等；修正 feature ownership 与文档。

### Phase B：统一 EvidenceCandidate（已完成）

在应用层定义统一候选文件协议（JSONL，不用 SQLite）：`candidate_id`、`kind`、`scope`、`source_run_id`、`source_role`、`evidence_quote`、`content`、`confidence`、`status`、`created_at`。把 BackgroundReviewer、TriggerDetector、AutoMemory 的语义输出汇入候选层，先去重再决定写 memory。

验收：同一事实不会因多入口重复写入；任何记忆都能追到证据；无证据候选不能进入长期记忆。

实现：`echo-agent-app-core/src/evolution/evidence.rs` 使用 append-only JSONL snapshot，候选 ID 使用独立 UUID，按 `scope + kind + normalized content` 生成 SHA-256 fingerprint；重复来源合并 evidence，Rejected 不会被后续重复检测复活，候选编辑也不会造成 fingerprint-derived ID 冲突。读写使用共享/独占文件锁，accept/undo 在状态日志失败时执行 memory 补偿回滚。BackgroundReviewer、TriggerDetector、AutoMemory 已统一进入此协议；Trigger inbox 失败时 fail-closed，不会绕过 review gate 直接写长期记忆；显式 `/remember` 仍直接保存。

### Phase C：Review Inbox 与三端确认（已完成）

GUI/TUI/CLI 共用候选列表、接受、编辑、拒绝、撤销操作。用户接受后才生成 ProjectFact/RuleProposal/SkillDraft；显式 `/remember` 继续允许直接保存。

验收：三端能力对等；接受/拒绝可审计；没有隐藏的 LLM 后台写入。

实现：CLI/TUI 均提供 `/evidence-inbox`，GUI EvolutionPanel 提供 pending/applied/rejected/all 视图与 edit/accept/reject/undo。三端优先复用 runtime 已绑定、可随 workspace 切换重绑的 `ReviewIntegration`，不再各自按进程 cwd 推导 inbox。accept 才通过共享 `MemoryLayerManager` 写 typed memory，undo 删除对应 memory 并恢复 pending；规则晋升和 skill draft/activation 继续走各自已有的显式 review gate。

### Phase D：Curator workspace scope + loader authority（已完成）

把 Curator state 放入 workspace 的 `.eko/`，identity 使用稳定 skill descriptor/path，而不是全局 name。SkillLoader 在 catalog/discovery 阶段读取 lifecycle：Draft 不进 catalog，Deprecated/Archived 不加载，Pinned/用户技能不自动降级。

验收：两个 workspace 同名 skill 不冲突；状态变化能真实改变加载行为；GUI/TUI/CLI 显示同一权威状态。

实现：EKO Curator state 迁至 `{workspace}/.eko/evolution/curator-state.json`，`SkillMeta` 记录具体 `SKILL.md` path；框架新增通用 `SkillLoadPolicy` 与 reconcile API，EKO policy 阻止 Draft/Deprecated/Archived 及其它 workspace 的 `.eko/skills` 进入 catalog。workspace/curator 状态切换会立即 reconcile，skill 激活会绑定正式路径并即时加载。

### Phase E：拆分 maintenance 与 semantic mutation（已完成）

Dreaming 是唯一的确定性记忆维护执行器：只根据 recall/inactivity 做可解释的 promote/revive/archive，不改写语义内容，并返回逐项 decision report。MemoryReviewer 只做 staleness/conflict 分析；冲突转成带完整 proposal 的 `EvidenceCandidate(action=merge_memories)`，用户在三端 Review Inbox 采纳后才执行合并。合并执行前重新核对 topic/type/member/content/status/confidence/推荐 primary，过期建议 fail-before-mutation；accept 保存 before snapshot，undo 恢复内容和 typed metadata。单次最多 10 个冲突建议、每组最多 16 条，避免 JSONL 与上下文无界增长。every-N-writes cadence/counter 已删除，真实 recall telemetry 增加 `last_recalled_at`，hot/warm 往返不再丢 recall/revision 元数据。

验收：无人确认时不会自动改写已有事实或技能；自动任务幂等、低 token、可关闭。

### Phase F：基于真实使用反馈改善能力

不做本地 benchmark loop。使用真实运行信号：用户纠正、撤销、候选接受率、skill 成功/失败、重复工具失败、人工重试。先改善 skill/rule/memory，再考虑是否存在足够证据修改基础 prompt；基础 prompt 修改必须是版本化 artifact，不能在线自改。

验收：每项改善都有真实来源和前后指标；不引入 EvalRunner、SQLite 或自动 prompt rewrite。

## 6. 下一阶段优先级

下一阶段进入 Phase F，但仍不建设本地 benchmark/EvalRunner。优先把真实使用反馈做成低成本产品指标：候选接受/拒绝/撤销率、过期 proposal 数、skill 成功/失败和重复工具失败。指标先服务于 memory/rule/skill 改善；基础 prompt 只有在真实证据充分时才以版本化 artifact 修改，不允许在线自改。
