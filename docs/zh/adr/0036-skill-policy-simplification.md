# ADR 0036:Skill 启停简化为直写直同步,移除 durable 结算状态机

## 状态

已采纳(2026-09-03)

取代 [ADR 0032](./0032-enabled-skill-runtime-authority.md) 的 durable
结算部分;0032 中"enabled-skills.json 是运行时激活权威"的核心立场保留。

## 背景

ADR 0032 为 `~/.eko/enabled-skills.json` 引入了一套 durable 期望状态机:
`desired_generation` / `settled_generation` 的 generation CAS、
`operation_identities` 操作幂等去重、`content_identity` 内容指纹,以及
`repair_debt`(target_failures / artifact_removals / artifact_syncs /
artifact_enablements)崩溃恢复重放。配套约 3000 行实现与 16 个 settlement
测试。

EKO 是本地单用户桌面助理(见 AGENTS.md 产品定位):没有并发租户、没有
跨进程写竞争、没有分布式对账需求。用户改坏一个 JSON 字段时,fail-closed
会让全部内置 skill 静默消失——这正是"把线上服务的威胁模型硬套本地应用"
的同款错误。这套机器的收益(崩溃时精确恢复到"文件已提交、运行时未同步"
的中间态)远低于其维护成本。

## 候选方案

1. **保留机器,仅改 fail-closed 为 fail-open**:改动最小,但 ~3000 行
   结算路径与测试继续存在,每次 skill 相关改动都要穿过 CAS/debt 语义。
2. **全量移除,直写直同步(本决策)**:enable/disable/install/uninstall/
   sync 全部简化为"锁内 修改 JSON → 原子写 → reconcile 到所有运行时
   目标";receipt 只报告即时结果。
3. 折中保留 operation identity 去重:UI 层已有按钮 busy 态,重复提交
   本身罕见;保留去重等于保留一半机器。

## 决策

采用方案 2:

- `EnabledSkillsConfig` 只保留 `{version, skills: {name → {category,
  enabled, baseline}}}`;旧文件中的 generation/repair_debt 字段被 serde
  直接忽略(无迁移成本,开发期产品无兼容负担)。
- 配置解析失败/读取失败**回退默认启用集**(fail-open),记 warn 日志。
- 五个变更路径统一走 `reconcile_skill_runtimes`:解析期望集合 →
  逐 target reconcile 内置目录 + 用户/插件 skill → 汇总即时 receipt。
- `SkillSyncReceipt` 收敛为 `{operation_id, idempotent, status,
  target_receipts}`;`idempotent` 语义变为"本次操作未改变状态"(重复
  开关同一 skill),供 UI 选择文案。
- 删除 `SkillOperationIdentity` / `SkillRepairDebt` /
  `SkillRepairTargetDebt` / `SkillArtifactSyncDebt` 四个类型及其 TS 绑定。
- 保留:extension mutation 互斥锁(防并发写)、product_data_io flow
  (settlement 与 caller 生命周期解耦)、subagent_control 的泛化
  operation_identity(非 skill 专属)、curator 生命周期与 skill 自动
  创建闭环。

## 取舍理由

崩溃窗口内最坏情形:JSON 已写、部分 agent 未 reconcile——下次任何
skill 操作或应用重启时的 `reconcile_enabled_skills_on_load` 会补齐,
状态收敛只需一次重启,不需要精确的 debt 重放。用"可容忍的一次重启
收敛"换掉 3000 行机器,符合本地个人助理的定位与 YAGNI。

## 影响

- `echo-agent-app-core/src/skills_hub/enabled_skills.rs`、
  `extension_control/{skills,service,types,tests}.rs`、
  `extension_commands.rs`、`state/app_state.rs`、`runtime.rs`。
- 前端 `SkillSyncReceipt` 等 TS 绑定与 SkillsPanel 文案。
- 同批相关改动见同日提交:builtin 打包路径运行时修复、TUI `/skill`
  与 GUI 激活按钮(用户可调用 skill)、内置目录 39→24 收敛。
