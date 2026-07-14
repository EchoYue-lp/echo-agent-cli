---
name: statistical-analysis
description: >-
  统计分析与假设检验。当用户需要进行统计检验（t检验、卡方、ANOVA 等）、
  回归分析、统计建模、或选择合适的统计方法时激活。
triggers:
  - 假设检验
  - t检验
  - 卡方检验
  - 回归分析
  - ANOVA
  - p值
  - 显著性检验
  - statistics
  - hypothesis test
  - regression
  - 统计建模
  - 置信区间
  - 效应量
allowed-tools:
  - "Bash(*)"
  - "Read"
  - "Write"
  - "Edit"
metadata:
  author: echo-agent-cli
  version: "1.0.0"
  tags: "statistics, analysis, hypothesis, regression, testing"
---

## 统计分析与假设检验

目标是回答明确的统计问题并量化不确定性。不要先从“想跑哪个检验”出发；先定义 estimand、总体、样本设计、变量、比较和决策用途。

### 方法选择提示

下列映射只是候选方法，不是自动决策树。必须同时检查独立性、配对/重复测量、分布、样本量、缺失机制、协变量、抽样设计和多重比较。

**比较两组差异？**
- 连续变量 + 正态分布 → 独立样本 t 检验
- 连续变量 + 非正态 → Mann-Whitney U 检验
- 分类变量 → 卡方检验 / Fisher 精确检验

**比较三组及以上？**
- 正态分布 → ANOVA（事后 Bonferroni / Tukey HSD）
- 非正态 → Kruskal-Wallis 检验

**探索变量关系？**
- 两个连续变量 → Pearson/Spearman 相关
- 预测连续因变量 → 线性回归
- 预测分类因变量 → Logistic 回归
- 时间到事件 → Cox 比例风险回归

**评估诊断方法？**
- 灵敏度、特异度、ROC-AUC

### 分析规范
1. **问题定义** — 明确 estimand、主要/次要结局、分析总体和预先指定的比较
2. **设计识别** — 区分观察性/实验性、独立/配对/聚类/纵向、探索性/确认性分析
3. **数据质量** — 检查缺失、异常、测量误差、选择偏倚、数据泄漏和有效样本量
4. **方法与假设** — 解释方法为何适合，并检查关键假设；必要时使用稳健、非参数或重抽样方法
5. **不确定性** — 报告效应量、置信/可信区间和样本量；p 值只是证据的一部分
6. **稳健性** — 对关键结论做合理的敏感性分析、模型诊断或替代规格检查

### 工具策略
- `data_stats` — 描述统计、分组聚合
- 可执行代码工具 — 执行可保存、可复现的 Python/R 分析；记录包版本、随机种子和参数
- `generate_chart` — 结果可视化

### 统计报告规范
- 报告样本量和置信区间
- 标注统计显著性（p-value）和效应量
- 区分相关性和因果性
- 使用适当的效应量指标（Cohen's d, OR, RR 等）
- 处理多重比较问题（Bonferroni / FDR 校正）
- 验证统计假设后再报告结果

### 输出规范
- 统计检验结果表格（检验统计量、p 值、效应量、95% CI）
- 用问题本身的语言解释结果，不把“不显著”写成“没有差异”，也不把显著性写成因果性或实际重要性
- 实际意义解读（不只是统计显著性）
- 先给关键发现，再给详细分析
- 不使用 emoji，保持专业风格

如需方法选择详细指南，使用 `read_skill_resource("statistical-analysis", "references/method_selection.md")`。
