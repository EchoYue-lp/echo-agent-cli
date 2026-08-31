# ADR 0032：Enabled Skill 是运行时激活权威

## 状态

已采纳

## 背景

EKO 携带了较大的内置 Skill catalog，并在启动时把所有内置 `SKILL.md` 加载进每个 Agent。
`enabled-skills.json` 过去只控制 methodology baseline 注入，因此标记为 disabled 的 Skill 仍会
注册 descriptor、progressive activation entry 和 IntentRouter 候选，产品状态与 Agent 实际能力
不一致。官方 Skill 文件格式不包含 per-skill Hook。

framework 已经通过 `SkillLoadPolicy` 分离 discovery/catalog 与 runtime registration；产品生命周期
文件及其 policy 应由应用拥有。

## 决策

1. `SkillsHub` 与 install/update command 继续作为 catalog 和 artifact 权威；Skill 可以被列出或
   安装，但不代表已经激活。
2. `ActiveSkillLoadPolicy` 是 EKO 的 registration policy。对于应用内置 `skills/` 根目录，它读取
   `enabled-skills.json`；user/plugin Skill 还要通过既有 curator/draft/workspace policy。
3. 缺失的 builtin entry 使用一组精简的 shipped core bundle 默认值：`coding`、`brainstorming`、
   `systematic-debugging`、`verification-before-completion`、`writing-plans`、`git-workflow`、
   `web-search`、`translation`。其它内置 Skill 全部 opt-in。
4. registration 在 descriptor insertion 之前过滤。因此 disabled Skill 不会注册 progressive
   activation/resource entry，也不会进入 IntentRouter keyword/LLM 候选。Hook 配置继续由宿主
   application 或 Plugin component 负责。
5. enable/disable/refresh reconciliation 在 primary Agent 上重新加载 builtin root，移除不再允许
   的 entry，并刷新 primary 与所有 live pooled Agent 的 IntentRouter。future Agent 在构造时使用
   同一 policy。
6. dependency 声明必须显式处理。未来带 dependency 的 builtin Skill 必须在同一个 durable policy
   中启用 dependency，或在 registration 前拒绝；activation 不能静默绕过 enabled set。

## 影响

- prompt/catalog surface 与 runtime capability surface 现在和用户启用 policy 一致。
- 仓库可以保留丰富的可选 Skill，而不会让每次会话都承担它们的 Hook 与 routing 成本。
- 用户仍可发现和安装 disabled Skill；enable 才是显式 runtime transition。
- Skill policy 是产品语义，继续留在 EKO；framework 只保留通用 loader 和 `SkillLoadPolicy` contract。
