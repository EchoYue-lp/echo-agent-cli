# M12 统计推断正确性与可复现 Artifact

## 目标

EKO 不再用自研近似算法对外宣称统计显著性或多元回归结果。探索性摘要与正式统计推断拆成两条明确路径：

1. `exploratory_statistics` 只计算描述性分布摘要，不返回 p 值、显著性结论或置信区间。
2. 正式推断由 `analyst` 生成可审阅的 Python/R 脚本，使用 SciPy/statsmodels 或成熟 R 包，并通过现有 `run_code` 沙箱执行；不新增统计 DSL 或第二个 Python 执行器。
3. 正式推断必须产出实际执行的脚本、结果和 manifest；记录输入 SHA-256、脚本 SHA-256、随机种子、包版本和分析参数。
4. 删除旧的 `hypothesis_test`、`regression`、`descriptive_advanced` 路径，不保留两套推断实现。

## 业界依据

- [statsmodels OLS](https://www.statsmodels.org/stable/generated/statsmodels.regression.linear_model.OLS.html)：OLS 接受 `nobs × k` 设计矩阵；截距需显式加入，拟合结果提供系数、标准误、t/F 检验与区间。
- [SciPy `ttest_ind`](https://docs.scipy.org/doc/scipy/reference/generated/scipy.stats.ttest_ind.html)：`equal_var=False` 执行 Welch t 检验，p 值基于理论 t 分布，并返回自由度和均值差置信区间。
- [SciPy `pearsonr`](https://docs.scipy.org/doc/scipy/reference/generated/scipy.stats.pearsonr.html)：默认 p 值使用零相关假设下相关系数的精确 beta 分布，并提供置信区间与常量输入告警。
- [SciPy `chi2_contingency`](https://docs.scipy.org/doc/scipy/reference/generated/scipy.stats.contingency.chi2_contingency.html)：返回统计量、p 值、自由度和期望频数；小样本或低期望频数时应考虑精确检验或重采样。

这些实现已经处理自由度、分布尾概率、缺失值、常量输入和数值边界。EKO 的职责是参数化、隔离执行、记录 lineage 和呈现限制，不重新实现统计分布或线性代数。

## 现状审计

- `HypothesisTestTool` 对 Welch t 和 Pearson 相关使用正态近似 p 值，卡方使用 Wilson-Hilferty 近似。
- `RegressionTool` 对每个特征分别拟合一元斜率，再把斜率相加形成预测；这不是多元 OLS，但工具描述声称支持多个特征。
- `DescriptiveAdvancedTool` 使用正态近似均值区间，却没有暴露小样本假设。
- 统计工具被注册进 read-only 工具集，但旧 regression 可写 output file，读写边界不一致。
- `run_code` 已有 OS 级沙箱、取消、timeout、working directory 和错误分类；正式统计推断应直接复用它，不另起未受控 Python 进程或封闭参数 DSL。

## 框架与应用边界

### `echo-agent`

- `echo-tools` 只提供通用的 Polars 探索性摘要。
- `statistics` feature 不再实现分布尾概率、显著性检验或回归推断。
- 正式推断继续使用框架已有 `run_code`；框架不增加新的统计执行工具。

### `echo-agent-cli`

- 数据分析 Subagent 决定何时使用探索性摘要、何时编写正式推断脚本，并负责解释假设与限制。
- 正式分析先把 `.py` / `.R` 文件写入任务 working directory，再用 `run_code` 执行同一脚本；脚本、输入哈希、环境版本、随机种子、结果和诊断都是必需 artifact。
- Notebook、数据 lineage UI 和报告展示属于 EKO 产品层，后续阶段继续建设。
- EKO 不引入 SQLite，不新增 Run 状态，也不创建第二套统计执行器。

## 工具合同

### `exploratory_statistics`

输出每列的有效样本数、缺失数、均值、样本标准差、最小值、四分位数、最大值、偏度和超额峰度。结果明确标记 `inference=false`，禁止出现 `p_value/significant/confidence_interval`。

### 正式推断脚本合同

`analyst` 根据问题生成代码，不把分析能力限制成固定枚举。常见实现包括：

- `welch_t_test`：两个数值列，listwise omission，双侧 Welch t 检验。
- `chi_square`：两个分类列，输出 observed/expected、低期望频数告警。
- `pearson_correlation`：两个数值列，输出 r、p 值和置信区间。
- `ols`：一个数值 target + 多个数值 feature，显式 constant，输出 params、standard errors、t/p、置信区间、R²/adjusted R²、F 检验、condition number 和基础残差诊断。

脚本与结果 artifact 至少包含：

- `contract_version`
- `engine` 和实际包版本
- `input_path/input_sha256`
- `analysis_type/parameters`
- `random_seed`
- `script_sha256`
- `artifacts`（有 working directory 时写入 `analysis.py/result.json/manifest.json`）
- `assumptions/limitations/warnings`

依赖缺失、沙箱不可用、数据类型不符或引擎失败必须返回结构化失败，不得回退到自研近似。用户可在对话中审阅、修改脚本后重新执行，EKO 复用 coding 能力完成数据分析协作。

## 验收

- 多特征 OLS 的脚本使用一个完整设计矩阵，而不是逐特征一元回归。
- registry 只暴露 `exploratory_statistics`，旧 `hypothesis_test/regression/descriptive_advanced` 均不可达。
- 探索性结果不含任何推断字段。
- 数据 Subagent 指令明确要求正式推断使用成熟库、保存实际脚本并记录可复现信息。
- 本机 SciPy/statsmodels fixture 验证推荐脚本模式可执行且与已知结果对齐。
- framework 全 workspace/default/all-features/独立 feature 矩阵、EKO workspace/GUI/前端门禁全部通过。
