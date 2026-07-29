---
name: data-wrangling
description: >-
  数据加载、清洗与探索性分析。当用户需要加载 CSV/Excel/JSON 等数据文件、
  检查数据质量、处理缺失值、进行探索性数据分析 (EDA) 时激活。
triggers:
  - 加载数据
  - 读取CSV
  - 读取Excel
  - 数据清洗
  - 缺失值处理
  - EDA
  - 探索性分析
  - 数据预处理
  - data wrangling
  - data cleaning
  - dataframe
  - 数据质量
allowed-tools:
  - "shell"
  - "run_code"
  - "read_file"
  - "read_artifact"
  - "write_file"
  - "edit_file"
  - "read_data"
  - "filter_data"
  - "aggregate_data"
  - "data_stats"
  - "transform_data"
  - "profile_data"
  - "topn_data"
  - "contribution_data"
  - "bin_data"
  - "ratio_data"
  - "join_data"
  - "correlate_data"
  - "pivot_data"
  - "missing_value_analysis"
  - "outlier_detection"
  - "consistency_check"
metadata:
  author: echo-agent-cli
  version: "1.0.0"
  tags: "data, cleaning, EDA, wrangling, quality"
---

## 数据加载、清洗与探索性分析

目标是从原始输入产出可审计、可复现的数据画像或清洗结果，同时保留来源、口径和不确定性。

### 工作流程

**第一步：数据加载与概览**
- 使用 `read_data` 加载数据文件（CSV、Excel、JSON、Parquet 等）
- 检查基本信息：行数、列数、数据类型、内存占用
- 抽样检查多处记录，确认编码、分隔符、sheet/table、表头和解析是否正确

**第二步：数据质量检查**
- 缺失值统计：每列的缺失数量和比例
- 重复行检查：是否有完全重复的记录
- 数据类型验证：日期列是否为日期类型，数值列是否含非数字
- 异常值检测：数值列的 min/max/mean/std，是否有极端值
- 使用 `profile_data` 生成完整的数据画像

**第三步：数据清洗**
- 处理缺失值：先判断缺失机制与业务含义，再决定保留、标记、删除、填充或插值；记录影响行数
- 修正数据类型：日期解析、分类变量编码
- 处理异常值：标记或修正（区分真正的异常和数据错误）
- 统一格式：字符串去空格、大小写统一、编码统一

**第四步：探索性分析 (EDA)**
- 单变量分布：直方图、箱线图、频率表
- 双变量关系：散点图、相关系数
- 分组统计：按关键维度分组聚合
- 使用 `data_stats` 进行统计汇总

### 工具策略

以下工具仅在当前上下文真实可用时使用；若名称或能力不同，选择等价的结构化数据工具，不要假装调用成功。

#### 读取数据
- CSV / JSON / Parquet 文件：使用 `read_data` 读取
- Excel 文件（.xlsx/.xls/.xlsb/.ods）：
  1. 先用 `excel_info` 查看 sheet 列表和行列数
  2. 用 `read_excel` 预览数据内容
  3. 用 `excel_profile` 了解列类型、缺失率和基本统计
  4. 用 `excel_load` 将 Excel 转为 Parquet/CSV，解锁全部数据工具
- 注意：`read_file` 只能读取文本文件（CSV/JSON），不能读取 Excel 二进制文件

#### 数据分析（需先通过 read_data 或 excel_load 加载）
- `profile_data` — 自动识别维度/指标列，分析缺失率和数据质量
- `data_stats` — 每列描述统计（均值、标准差、分位数等）
- `filter_data` — 条件过滤（支持 "列名" > 100, col == "val", A > 0 AND B < 5）
- `aggregate_data` — 分组聚合（sum, mean, count, median, p25/p75/p95 等）
- `topn_data` — Top-N 排名分析
- `contribution_data` — 帕累托分析（贡献度、累计占比）
- `bin_data` — 等宽/等频分箱（直方图）
- `ratio_data` — 列间算术表达式（利润率 = (revenue-cost)/revenue*100）
- `correlate_data` — 相关系数矩阵（Pearson 或 Spearman）
- `pivot_data` — 透视表（按一列展开为多列）
- `join_data` — 双表 Join（inner/left/outer/cross）
- `transform_data` — 排序、选择列、重命名列
- `export_data` — 导出为 CSV/JSON/Parquet

#### 数据质量
- `missing_value_analysis` — 缺失值分析和模式识别
- `outlier_detection` — 异常值检测（IQR / Z-score）
- `consistency_check` — 类型一致性和范围检查

#### 可视化与输出
- `generate_chart` — 生成 Vega-Lite 图表
- `write_excel` — 创建 .xlsx 文件
- `excel_to_csv` — 将 Excel sheet 导出为 CSV

### 输出规范
- 报告数据概况（维度、类型、质量评分）
- 清洗操作全部记录（做了什么、为什么、影响多少行）
- EDA 发现用图表和文字结合呈现
- 先给关键发现，再给详细分析
- 使用表格呈现统计数据
- 标注数据中可能需要注意的问题
- 不使用 emoji，保持专业风格
- 明确原始数据是否保持未修改、产出文件路径、转换脚本/参数和关键校验结果

如需 EDA 检查清单，使用 `read_skill_resource("data-wrangling", "references/eda_checklist.md")`。
