---
name: semantic-verify
description: >-
  验证 EKO 语义材料的结构、引用、源码快照、新鲜度、跨仓边界和问题记录解决条件，并按实际影响运行最小必要的
  Rust、前端、契约或集成检查；仅在已有 `semantic/` 材料或问题记录解决候选时使用，不替代普通完成验证。
metadata:
  category: 语义质量
  scope: EKO
---

# EKO 语义验证

语义验证补充现有仓库门禁，不替代 Cargo、前端、契约、GUI、集成或发布验证，也不能把 Skill 提示词当成通过证据。
先调用 `semantic-contract` 执行确定性结构检查并取得合同版本；结构未通过时不得继续语义结论。

## 三层验证

1. **结构**：检查 Markdown/YAML、唯一标识、对象 `kind`、目录、引用、能力图归属、场景处置、区域覆盖和唯一
   `primary_focus`。
2. **语义**：对照当前实现、人的期望、规则、证据、问题记录、审查版本和框架/应用/适配
   边界；确认未映射变化和 stale 单元已处置。
3. **执行**：按 `semantic-diff` 生成的矩阵运行最小充分命令。触及 Rust 公共接口/feature、CLI GUI 或
   `web-frontend` 时遵循对应 `AGENTS.md` 条件门禁，并检查文档/示例/官网影响。

`risk_accepted` 必须有人类裁决，`resolved` 必须有修复、验证和复审证据。验证失败、快照漂移、环境阻塞或
证据缺口都要明确保留为未完成；禁止使用 `clean`、`safe` 或“没有缺陷”。
