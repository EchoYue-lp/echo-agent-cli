# EKO 项目文档

`docs/` 只保存与当前代码同步、需要长期维护的项目文档。实施设计、阶段规格、
审计报告和一次性验证记录不放在这里。

## 文档导航

| 文档                                                                 | 内容                                                 |
| -------------------------------------------------------------------- | ---------------------------------------------------- |
| [快速入门](./getting-started.md)                                     | 环境准备、TUI/GUI/JSONL 启动与首次配置               |
| [功能总览](./features.md)                                            | 已接入真实主路径的产品能力与代码依据                 |
| [架构说明](./architecture.md)                                        | 框架/应用边界、运行时所有权、数据流和文件布局        |
| [持久化概念](./persistence.md)                                       | Store、Journal、Checkpoint、Trace 与权威关系         |
| [配置指南](./configuration.md)                                       | 模型、MCP、Hooks、Browser、Channel 与环境变量        |
| [Provider 架构](./architecture/providers.md)                         | 动态 Provider、模型协议和思考能力解析                |
| [RuntimeTaskService 决策](./architecture/runtime-task-service.md)    | Task DAG 权威、EKO adapter、journal 与 blocking 边界 |
| [Agent 协同 ADR](./adr/0001-agent-collaboration.md)                  | Codex 协同机制与 EKO 功能设计                        |
| [Codex 工具目录 ADR](./adr/0002-codex-tool-capability-catalog.md)    | Codex 工具能力与 EKO 参考设计                        |
| [Claude Code 能力目录](./adr/0003-claude-code-capability-catalog.md) | Claude Code 工具、子智能体和 Skills 快照             |
| [应用生命周期 ADR](./adr/0004-application-lifecycle-supervisor.md)   | GUI/headless admission、取消、join 与 rollback       |
| [Skill 同步](./skill-sync.md)                                        | 内置/用户 Skill、启用状态与上游同步                  |
| [项目状态](./MASTER-PLAN.md)                                         | 当前权威路径、活跃工作与下一步                       |

仓库根 [README](../README.md) 负责产品介绍、构建命令和常用交互；本文档集不再
复制完整的 slash command 或历史里程碑清单。

## 设计文档边界

尚未完成、仍驱动代码变更的设计与规格放在 [`design/specs/`](../design/specs/)。
架构决策记录放在 [`docs/adr/`](./adr/)。
完成验收后删除对应规格，把仍有长期价值的事实合并回 `docs/` 或代码注释。

当前活跃规格：

- [workspace/conversation runtime reliability](../design/specs/runtime-reliability.md)
- [long-horizon runtime closure](../design/specs/long-horizon-runtime-closure.md)

## 维护规则

1. 功能只有在生产入口可达，并且至少有对应测试或稳定调用点时，才写入“已实现”。
2. `docs/` 描述当前事实，不保留按日期命名的实施日志、review diff 或 soak 账本。
3. `echo-agent` 的通用能力在框架仓库文档中维护；这里仅说明 EKO 如何使用它。
4. EKO 使用文件持久化，不启用 SQLite；所有 surface 共享同一套核心能力。
5. 产品术语统一为 `TaskRun -> PlanTask -> SubagentRun`。
