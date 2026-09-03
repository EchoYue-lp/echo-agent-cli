# ADR 0033：Skill catalog 收缩与 SKILL.md 官方标准化

## 状态

已采纳；2026-09-03 修订 catalog 与 baseline 清单

## 背景

EKO 捆绑的 Skill catalog 使用私有 frontmatter 扩展字段（顶层 `triggers:`、`hooks:`、`shell:`、`paths:`、`sandbox:`、`depends_on:`）承载路由与运行时语义。这与 [agentskills.io 官方规范](https://agentskills.io/specification)不兼容：

- 官方顶层字段只有 `name` / `description` / `license` / `compatibility` / `metadata` / `allowed-tools`，且 `allowed-tools` 必须是空格分隔字符串；
- 官方 `skills-ref validate` 会把非官方顶层字段判为违规，EKO 的 Skill 无法在标准工具链下流通；
- `metadata` 按规范是 string → string 映射，不应承载结构化扩展。

同时，catalog 里存在重复能力（全局选技逻辑、与 `web-search` 重叠的 `deep-research`）与空壳指导（`writing-skills`）。

曾评估过把扩展字段迁移到 `metadata.echo-agent.*` 命名空间，最终被否决：**只允许标准格式**，不引入任何私有的 frontmatter 扩展概念。

## 决策

1. **SKILL.md 只使用官方标准字段**。顶层允许：`name`、`description`、`license`、`compatibility`、`metadata`（string → string）、`allowed-tools`（空格分隔字符串，空则省略）。framework parser 对非官方顶层字段 fail-closed（`deny_unknown_fields` 解析失败，loader 跳过并告警）。
2. **不引入 vendor 命名空间**。`metadata` 只放字符串值；LLM intent routing 改为 **description-driven**——把"何时使用"的场景与关键词写进 `description`（规范自身的建议）。`SkillDescriptor` 的 `triggers` / `paths` / `depends_on` / `sandbox` / `shell` 字段保留为程序化 API，不再有文件来源；关键词 fast path 只服务于这些程序化 descriptor。
3. **Skill 文件不包含 Hook 扩展**。官方格式没有 per-skill Hook 字段或 sidecar。Hook action 继续通过宿主应用的 HookRegistry 和 Plugin Hook component 提供。文档约定的 `HookAction::ActivateSkill` wire name 为 `activate_skill`。
4. **validator 门禁**。framework 提供 `validate_skill_markdown` / `validate_skill_dir`（`skills-ref validate` 的进程内等价物）；`echo-agent-app-core` 的 `skills_hub::catalog_gate` 测试遍历 `skills/`，断言全部捆绑 Skill 零违规，且 `BUILTIN_SKILL_NAMES` 与磁盘目录一一对应。
5. **catalog 收缩（41 → 39）**：
   - 删除 `using-superpowers`（重复的全局选技逻辑，与本仓库技能体系冲突）；
   - 删除 `deep-research`（与 `web-search` 重复；其独有贡献——按结论分解、按结论综合、引用前读原文——并入 `web-search` 的"深度调研模式"）；
   - 补强 `writing-skills`（改为教授官方标准布局与本仓库工作流）与 `mcp-builder`（补步骤化工作流与失败处理）。
6. **evolution 子系统跟随标准**：`SkillDraftGenerator` 生成的草稿只写标准字段（trigger patterns 留在 curator state）；`SkillMerger` 落盘时只合并 `allowed-tools`，trigger/path 联合保留在内存 descriptor。
7. **路径边界规范化**：`builtin_skills_root()`、`ActiveSkillLoadPolicy` 与 `reload_skills_from_dir` 全部与 loader 一样 canonicalize 路径，消除 symlink 前缀失配导致的 policy 绕过与 reload 静默 no-op。
8. **ADR 0003 保留为历史快照**：其记录的 25 条目/14 个 superpowers 的清单是当时的 catalog 状态，不回填修改；当前清单以 `/skills list` 与 catalog gate 为准。
9. **2026-09 第二次收缩（39 → 24）**：删除已由基础 prompt 覆盖的
   `coding` / `translation` / `doc-writing` / `web-search`，以及 11 个可从
   `anthropics/skills` 安装的 vendored design/automation/research 示例；默认启用集
   8 → 5，methodology baseline 4 → 1，仅保留
   `verification-before-completion`。工作区专属行为由有界 profile prompt 与仍存在的领域
   Skill 承担。

## 影响

- 捆绑 Skill 与官方生态兼容，可用 `skills-ref validate` 或进程内 gate 校验。
- 关键词路由不再来自文件；`KeywordClassifier` 的词表为空，路由依赖 LLM 意图分类器读取 description（无 LLM 时走 DirectAnswer/Fallback）。
- 捆绑 catalog 先从 41 收缩到 39，再于 2026-09 收缩到 24；
  `BUILTIN_SKILL_NAMES`、`DEFAULT_ACTIVE_BUILTIN_SKILLS`、baseline、CHANGELOG 与本 ADR 同步。
- 用户安装的第三方 Skill 若使用旧私有字段，将解析失败并在 discovery 诊断中显式报错——按"无需向后兼容"的项目原则，不提供迁移垫片。
