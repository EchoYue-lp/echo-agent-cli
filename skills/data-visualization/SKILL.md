---
name: data-visualization
description: >-
  数据可视化与图表制作。当用户需要制作图表、可视化数据趋势、
  创建仪表盘或选择合适的图表类型时激活。
triggers:
  - 图表
  - 可视化
  - 柱状图
  - 折线图
  - 饼图
  - 散点图
  - chart
  - plot
  - visualization
  - 画图
  - 画个图
allowed-tools:
  - "shell"
  - "run_code"
  - "read_file"
  - "read_artifact"
  - "apply_patch"
  - "generate_chart"
metadata:
  author: echo-agent-cli
  version: "1.0.0"
  tags: "visualization, chart, plot, dashboard, graphics"
---

## 数据可视化与图表制作

目标是让读者准确看见数据中的比较、趋势、分布或关系。先定义要传达的判断，再选择编码方式；不要为了“有图”而画图。

### 图表类型选择指南

**展示趋势/变化？**
- 时间序列 → **折线图** (line chart)
- 多类别对比随时间变化 → **堆叠面积图** (stacked area)
- 变化幅度 → **瀑布图** (waterfall)

**比较大小/排名？**
- 分类比较 → **柱状图** (bar chart)，≤7 类
- 多组分类比较 → **分组柱状图** (grouped bar)
- 排名 → **水平柱状图**（标签更易读）

**展示分布？**
- 单变量分布 → **直方图** (histogram) 或 **箱线图** (box plot)
- 分布形状细节 → **小提琴图** (violin plot)
- 密度估计 → **核密度图** (KDE)

**展示关系？**
- 两变量关系 → **散点图** (scatter plot)
- 多变量关系 → **气泡图** / **热力图** (heatmap)
- 网络关系 → **力导向图** (force-directed graph)

**展示占比？**
- 部分与整体 → 优先排序条形图；仅在类别很少且精确比较不重要时考虑饼图/环形图
- 层级占比 → **矩形树图** (treemap)

**展示地理分布？**
- 地图热力图 / 分级统计图 (choropleth)

### 设计原则
1. **数据墨水比最大化** — 去掉不必要的装饰元素
2. **色彩一致性** — 同一类别始终使用相同颜色
3. **可访问性** — 不要仅依赖颜色区分（加标签/图案）
4. **适当标注** — 标题、轴标签、单位、数据来源
5. **选择正确的起点** — 柱状图的 y 轴通常从 0 开始

### 工具策略
- 对自定义图表，先保存 Python 脚本，再使用 `run_code(script_path)` 在 EKO 锁定的 matplotlib/seaborn 环境执行；不要把正式制图塞进内联代码
- 简单 Vega-Lite 图表可使用 `generate_chart`，但生成图表所需的数据变换、参数和代码仍须保存并可复现
- 图表文件写入分析目录的 `outputs/`，记录输入哈希、实际包版本和生成参数

### 输出规范
- 选择合适的图表类型（参考上方决策树）
- 添加标题、轴标签和图例
- 使用一致的颜色方案
- 标注数据单位和时间范围
- 图表和文字说明相结合
- 不使用 emoji，保持专业风格
- 生成后检查实际图像中的裁切、重叠、颜色对比、标签密度、轴范围和数据映射

如需图表选择详细决策树，使用 `read_skill_resource("data-visualization", "references/chart_guide.md")`。
