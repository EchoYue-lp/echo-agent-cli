# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **统一 turn-run 绑定与长程恢复**:每个 store-backed turn 急切绑定内部 TaskRun；
  typed provenance 区分 conversation 与 orchestrated direct run，boot recovery 和
  quiet-wake 使用原子 journal 决策，GUI/TUI/CLI/channel user steer 记录到 exact run，
  planless conversation run 不进入任务 UI，长 Goal artifact 按 revision/SHA-256 原子发布。
  ADR [0037](docs/zh/adr/0037-unified-turn-run-binding.md)。

- **Skill 系统重构（2026-09）**:
  - 内置 creator 能力：`writing-skills` 收敛为面向用户的 `skill-creator`，新增 EKO
    原生 `plugin-creator`；两者默认启用、按需加载，新增三个模型可调用薄工具并复用
    framework Skill validator 与应用 PluginRuntime 的 scaffold / validate 权威。ADR
    [0038](docs/zh/adr/0038-builtin-creator-skills.md)。
  - app-core 数据根与 binary 入口统一尊重绝对 `EKO_DATA_DIR`，使隔离测试和本地多实例
    不再误占用真实 `~/.eko` authority。
  - 用户可调用 Skill：TUI 新增 `/skill <name> [instructions]`（激活 + 可选引导输入），GUI Settings 的 Skills 面板新增"在当前会话激活"；framework `activate_skill` 对未安装/被禁用 skill 从静默返回改为显式报错。
  - 内置目录先收敛 39 → 24，随后因 creator 能力调整为 25：删除通用能力型（coding/translation/doc-writing/web-search，行为准则由基础 prompt 承担）与 vendored Anthropic 示例 11 个（design/automation/research，可经 SkillsHub 安装）；默认启用先从 8 → 5，随后加入两个 creator 变为 7；methodology baseline 常驻注入 4 → 1（仅 verification-before-completion）。
  - 打包修复：builtin skills 根从编译期 `CARGO_MANIFEST_DIR` 改为运行时解析（`$EKO_SKILLS_ROOT` → Tauri bundle resources → 源码树），`tauri.conf.json` 增加 `bundle.resources`，修复安装态应用丢失全部内置 skill 的问题。
  - durable 管制机器移除（约 3000 行）：`enabled-skills.json` 保留平铺 `{category, enabled, baseline}` + 原子写，删除 generation CAS / operation identity / repair debt；坏配置回退默认启用集（fail-closed → fail-open）。ADR [0036](docs/zh/adr/0036-skill-policy-simplification.md)（取代 [0032](docs/zh/adr/0032-enabled-skill-runtime-authority.md) 的结算部分）。
  - Agent Plugins 1.0 留口：`install` 复用 framework manifest validator 识别
    `plugin.json` 标准包，原子安装并启用其全部 `skills/` 子目录，为每项保留精确 Git
    subdir；含 `mcp.json` 的包明确提示暂不支持。

- Made framework `TaskStatus` the sole PlanTask execution authority, exposed
  immutable `PlanRevision` artifacts to surfaces, and reduced Todo state to a
  read-only projection with no reverse mutation path.
- **Skill catalog 收缩与官方标准化**: 全部捆绑 `SKILL.md` 迁移到 agentskills.io
  官方标准字段（`allowed-tools` 为空格分隔字符串、`metadata` 仅字符串，Skill 文件不携带
  Hook 扩展，不引入任何私有扩展命名空间）；路由改为
  description-driven。catalog 先从 41 → 39 删除 `using-superpowers` 与
  `deep-research`，随后按本节顶部重构收敛到 24。新增 `skills_hub::catalog_gate` 门禁测试（零违规 +
  `BUILTIN_SKILL_NAMES` 一致）与 `skills_hub::policy_contract` 行为级契约测试
  （disabled 全投影缺席、reload 生效、fail-open、用户 Skill 不受误伤、同名优先级、
  路径 canonicalize 边界）。ADR
  [0033](docs/zh/adr/0033-skill-catalog-contraction-and-official-frontmatter.md)。
- **Enabled Skill 运行时权威**: `enabled-skills.json` 成为 bundled Skill 的注册
  权威（ADR
  [0032](docs/zh/adr/0032-enabled-skill-runtime-authority.md)）；损坏配置
  fail-open，builtin/user Skill 来源标记区分，pooled Agent 刷新路径补齐。

### Added

- **技能分类体系**: 6 个 category（methodology / development / document /
  design / research / automation），按 `skills/<category>/<name>/SKILL.md` 组织。
- **技能资产移植与收缩**: superpowers 方法论/工作流 13 个 + Anthropic 领域技能
  15 个 + 现有技能，全部移植到 `skills/<category>/` 目录；经本轮质量收缩后
  最终随附 25 个，见上方 Changed 条目。
- **方法论 baseline 默认挂载**: `verification-before-completion` 的正文在
  primary 与 pooled conversation Agent 创建时注入 system prompt。
- **enabled-skills.json 配置管理**: `EnabledSkillsConfig` 模块，管理技能
  启用状态和 baseline 标记，默认配置自动生成并落盘。
- **TriggerSupervisor bootstrap 装配**: 用 TriggerSupervisor（Keyword +
  LlmIntent（可选）+ Hook slot）替代 ChainedClassifier。
- **SkillsHub category 扫描**: `scan()` 支持 `skills/<category>/<name>/`
  嵌套目录结构，`SkillHubEntry` 新增 category / is_baseline / is_builtin /
  source / upstream_version 字段。
- **前端技能分组展示**: SkillsPanel 按 category 折叠分组，显示 baseline ★
  标记、缺依赖 ⚠️ 提示、来源/版本信息。
- **eval match_fn 修真**: 用 `KeywordClassifier` 替代 `String::contains`
  字符串匹配，F1 度量反映生产路由效果。`load_skill_triggers` 支持
  category 子目录扫描。
- **文档**: Skill 分类与上游同步说明现统一维护在
  `docs/{zh,en}/operations/skill-sync.md`。

### Changed

- Conversation follow-ups now have a single application-owned durable ingress
  contract in the existing ChatEventLog reducer. Revisioned attempts project
  persisted, mailbox-accepted, drained, settled, deferred, and recovery-required
  receipts without introducing another mailbox or driver; surface migration is
  staged behind this core authority.
- GUI, TUI, CLI, and channel active steering now use the framework tracked
  receipt (`MailboxAccepted -> Drained -> TurnSettled`) through one
  SubagentControl adapter. Cold Conversation Agent delivery carries the
  framework initial-input receipt through the shared chat driver; router
  delivery records expose the same typed phase, outcome, and drained facts.
  Terminal-before-drain and restart-after-drain remain non-replayable.
- Channel now carries framework sender-scoped sessions through EKO AgentPool,
  TaskRun, cache, foreground control, exact resume, bounded outbound rendering,
  and bidirectional canonical tool identity quarantine. Framework session
  timeout/reset now close old key admission, await exact foreground/lease
  settlement, retire the old cached Agent, reclaim its exact persisted runtime,
  and rotate model/checkpoint/cache identity while preserving stable product
  history and TaskRun state. Channel TaskRuntime and attachment/compression file
  work now use the bounded store/product-data owners; aggregate product deletion
  clears every runtime incarnation before retiring the stable transcript.
  Product-data blocking work is now owned by one per-application service that
  survives caller drop and is sealed/joined by application shutdown; the
  process-global primitive only limits concurrency.
- `SkillInfo` TypeScript 类型新增 category / is_baseline / is_builtin /
  upstream_version / has_updates / missing_dependencies 字段。
- SkillsPanel 重构为分组折叠 UI。
- bootstrap 流程新增 Step 5b（baseline 注入）和 Step 12（TriggerSupervisor）。
