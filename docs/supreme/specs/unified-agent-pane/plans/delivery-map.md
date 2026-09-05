---
schema_version: 1
artifact: delivery-map
design_ref: docs/supreme/specs/unified-agent-pane/design.md
outcomes:
  frontend-shell-and-context-pane:
    ships: EKO 前端以 Codex 参考的清爽三栏壳呈现主 Agent 与一层 Subagent，以单一上下文分栏保留
      TaskRun、文件和浏览器入口，并把分析、研究、工作流和结构化提取迁入独立主工作台
    depends_on: []
  framework-subagent-event-envelope:
    ships: echo-agent 提供完整身份、顺序、时间戳、可检测 gap 和关键边界不静默丢失的版本化 Subagent execution event
      envelope
    depends_on: []
  eko-event-projection-integration:
    ships: EKO 无损接入 framework Subagent event envelope，持久化产品边界并在统一 Agent 分栏中恢复实时与历史执行过程
    depends_on:
      - frontend-shell-and-context-pane
      - framework-subagent-event-envelope
design_revision: sha256:dcd276c17c2c75d4cddbe40bd8f9f1e035be2ae4e3dcde6b74ceb0e1f97b2c4b
---
前端壳与 framework event envelope 两个前置结果均已交付；当前执行 EKO app-core 事件投影、跨 surface 恢复与生成契约集成。
