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
  - "excel_info"
  - "read_excel"
  - "excel_profile"
  - "excel_to_csv"
  - "write_excel"
  - "generate_chart"
metadata:
  author: echo-agent-cli
  version: "1.0.0"
  tags: "data, cleaning, EDA, wrangling, quality"
---

## 数据加载、清洗与探索性分析

目标是从原始输入产出可审计、可复现的数据画像或清洗结果，同时保留来源、口径和不确定性。

### 工作流程

**第一步：数据加载与概览**
- 先保存一个可审阅的 Python 脚本，再用 pandas/pyarrow/openpyxl 加载 CSV、Excel、JSON 或 Parquet；通过 `run_code` 的 `script_path` 执行，禁止把正式分析塞进内联代码
- 检查基本信息：行数、列数、数据类型、内存占用
- 抽样检查多处记录，确认编码、分隔符、sheet/table、表头和解析是否正确

**第二步：数据质量检查**
- 缺失值统计：每列的缺失数量和比例
- 重复行检查：是否有完全重复的记录
- 数据类型验证：日期列是否为日期类型，数值列是否含非数字
- 异常值检测：数值列的 min/max/mean/std，是否有极端值
- 在脚本中生成结构化数据画像，并把行列数、类型、缺失、重复、范围和解析警告写入结果文件

**第三步：数据清洗**
- 处理缺失值：先判断缺失机制与业务含义，再决定保留、标记、删除、填充或插值；记录影响行数
- 修正数据类型：日期解析、分类变量编码
- 处理异常值：标记或修正（区分真正的异常和数据错误）
- 统一格式：字符串去空格、大小写统一、编码统一

**第四步：探索性分析 (EDA)**
- 单变量分布：直方图、箱线图、频率表
- 双变量关系：散点图、相关系数
- 分组统计：按关键维度分组聚合
- 在同一持久脚本中生成描述统计和分组汇总

### 工具策略

EKO 会为持久 Python 脚本懒加载锁定的分析环境。正式数据处理必须保存脚本并使用 `run_code(script_path)`，以便执行环境、输入和产出可审计；不要退回一长串不可复现的逐操作工具调用。

#### 读取数据
- CSV / JSON：可先用 `read_file` 抽样确认编码和表头，完整加载与类型解析放在持久 Python 脚本中
- Parquet：在持久 Python 脚本中使用 pandas/pyarrow 读取并输出 schema 与抽样结果
- Excel 文件（.xlsx/.xls/.xlsb/.ods）：
  1. 先用 `excel_info` 查看 sheet 列表和行列数
  2. 用 `read_excel` 预览数据内容
  3. 用 `excel_profile` 了解列类型、缺失率和基本统计
  4. 需要完整处理时，用 `excel_to_csv` 或持久 Python 脚本读取；不要修改原文件
- 注意：`read_file` 只能读取文本文件（CSV/JSON），不能读取 Excel 二进制文件

#### 数据处理与质量
- 使用 pandas/pyarrow 完成画像、过滤、分组、Join、透视、分箱、贡献度、相关性和导出
- 每个转换步骤记录输入/输出行数、列变化、丢弃或填充值数量以及采用该规则的理由
- 缺失、异常和一致性检查要输出机器可读明细，不只打印一句总结
- 输出 CSV/JSON/Parquet 后重新读取并核对 schema、行数、关键汇总和文件哈希

#### 可视化与输出
- `generate_chart` — 生成 Vega-Lite 图表
- matplotlib/seaborn 图表也必须由同一持久脚本生成到 `outputs/`，并使用无界面后端
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
- 记录 Python 与 pandas/pyarrow/openpyxl 等实际包版本；不要声称使用了未执行的脚本

如需 EDA 检查清单，使用 `read_skill_resource("data-wrangling", "references/eda_checklist.md")`。
