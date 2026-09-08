# ADR 0041：EKO 只消费外部 Skill

## 状态

已采纳

## 背景

EKO 曾在应用仓库中维护一套内置 Skill catalog，并同时拥有默认启用策略、方法论 baseline、
运行时内置目录解析、Tauri resource 打包、内置来源字段和 creator 模型工具。这让一个本地
个人助理承担了 Skill 内容仓库、发布包和运行时策略三重职责；每次增删内容都要同步 Rust、
前端契约、打包配置、测试与文档。

framework 已经提供标准 `SKILL.md` 解析、验证、渐进激活和 Plugin generation。EKO 也已经
具备用户目录 `~/.eko/skills/`、Git 安装/同步、启停、卸载和 PluginRuntime。因此应用内置
内容不是能力成立的前提；Skill 或 creator 工作流更适合作为独立 Skill 或独立插件按需安装。

## 候选方案

1. 保留并继续收缩内置 catalog：迁移最小，但 EKO 仍承担内容选择、默认策略与发布成本。
2. 删除内容但保留空的 builtin root、baseline 和来源字段：短期改动较少，却留下没有所有者
   的双轨语义，未来容易再次把独立内容塞回应用。
3. 删除 EKO 的内置 Skill authority，只保留外部 Skill 与 Plugin 能力（采用）。

## 决策

1. 删除仓库根 `skills/` 及其 Tauri resource；EKO 不再从源码树、环境变量或应用 bundle
   解析内置 Skill 根目录。
2. 删除默认启用集、方法论 baseline 注入、内置 registration policy、catalog gate，以及
   `is_builtin` / `is_baseline` wire 和 UI 语义。
3. `~/.eko/enabled-skills.json` 升级为 version 3，每个条目只保存 `{enabled}`。旧文件中的
   `category`、`baseline` 和结算字段被忽略，但已有外部 Skill 的启用选择保留；损坏配置
   回退空集合，不生成默认条目。
4. SkillsHub 只列出和管理 `~/.eko/skills/` 下的独立 Skill；Plugin Skill 继续由 framework
   prepared generation 提供。所有 surface 仍通过同一个 Extension authority 变更并立即
   reconcile 当前运行时目标。
5. 删除只为内置 `skill-creator` / `plugin-creator` 服务的三个模型工具。现有 PluginRuntime
   scaffold、validate、install、reload 以及 `/plugins` 命令继续保留，独立 Skill 或插件可以
   讲解和调用这些既有能力。
6. framework 的 `SkillDocument`、loader、validator、progressive activation 与 Plugin
   协议保持不变；本决策只收缩 EKO 应用层的内容和产品策略。
7. 仓库 `.agents/skills/` 是开发 EKO 的协作指令，不是产品运行时资源，也不随应用打包。

## 分层与失败处理

- 通用机制：Skill/Plugin 协议、解析、验证与 runtime registration 属于 `echo-agent`。
- EKO 产品策略：用户安装目录、启停文件、上游同步、surface receipt 与 plugin overlay
  属于 `echo-agent-cli`。
- 外部内容：具体 Skill 指令、creator 教程和配套资源由独立 Skill/插件仓库拥有。

启用不存在的 Skill 会显式失败；禁用已有配置条目可以幂等完成。文件提交后若部分 runtime
target reconcile 失败，receipt 返回 `Degraded`，下一次操作、启动或 workspace load 再次
收敛。删除内置内容不会删除用户已安装的 `~/.eko/skills/`。

## 影响

- 取代 ADR 0032 的 bundled activation authority、ADR 0033 的 bundled catalog 部分、
  ADR 0036 的 `{category, enabled, baseline}` schema，以及 ADR 0038 的内置 creator 决策。
- 保留外部 Skill 的安装、启停、卸载、同步与会话激活，也保留完整 PluginRuntime。
- 文档与前端不再展示 baseline 或 builtin 标识；应用包不再包含 `skills/` resource。
- framework SDK/API 与示例无需修改；本次变化是 EKO 产品内容边界收缩。
