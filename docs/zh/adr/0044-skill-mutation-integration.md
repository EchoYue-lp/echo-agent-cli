# ADR 0044：EKO Skill 操作使用框架 Mutation Authority

## 状态

已采纳

## 背景

最新框架 [ADR 0069](https://github.com/EchoYue-lp/echo-agent/blob/main/docs/adr/0069-skill-lifecycle-mutation-authority.md) 规定 `SkillMutationAuthority` 拥有 Skill 文件与 Curator 变更。旧 Curator 直写方法，以及没有批准的 Draft/Merge/Patch 调用，不再是应用可用 API。EKO 仍提供 GUI 和 CLI 显式 Skill 操作，ProductData I/O flow 还必须在调用方断开后持有任务直到结算。

## 候选方案

1. 在 EKO 重做 Curator 写入。拒绝：形成第二套生命周期权威，缺少 journal 恢复与精确批准。
2. 删除 Skill 操作。拒绝：已有 surface 会失去普通 Agent 能力。
3. EKO 保留用户与配置策略，精确变更交给框架 authority。采用。

## 决策

- 框架拥有 Skill mutation CAS、文件与 Curator 投影、审计绑定、恢复及回滚；EKO 拥有 workspace 选择、用户显式操作或已配置的自动草稿策略、生命周期阈值和运行时发布。
- `.eko/evolution/skill-change-log.jsonl` 是绑定 Skill authority 的独立持久业务 ChangeLog。既有 `change-log.jsonl` 仍是更广的 evolution/candidate 日志；不能在已有历史记录后把它默默改绑到新 Skill journal。
- GUI 和 CLI 通过共享的 generation-bound 候选读取、preview、摘要绑定批准和 apply 生成 Draft；显式 merge/patch 命令走相同框架合同。EKO Curator 元数据操作构造完整 before/after 状态并提交给同一 authority。
- Curated Skill 发布仍归 ProductData I/O owner；新的 async 操作由 `ProductDataIoFlow::run_async` 监督，调用方退出不能取消已提交变更，也不能绕过 shutdown 结算。
- 最新框架中的普通 `write_memory` 只创建 Draft。EKO 不把它当成已批准的 hot-memory 发布；现有 hot projection 只在框架批准激活后变化。

## 影响

- 框架 Skill 操作不再修改旧的 `CuratorStatus.last_run_at` 文件字段。EKO 维护命令报告确切已提交迁移，不能把旧时间戳展示成新的结算收据。
- 框架 examples 不变；`echo-website` 不说明 EKO 本地 Skill 命令合同，无需更新。
