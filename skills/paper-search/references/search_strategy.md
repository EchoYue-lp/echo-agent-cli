## 学术检索策略构建指南

### 检索式构建原则

**1. 关键词提取**
- 从用户问题中提取核心概念（通常 2-4 个）
- 为每个概念列出同义词和相关术语
- 英文学术数据库优先使用英文关键词

**2. 布尔运算**
```
(同义词1 OR 同义词2) AND (概念A) AND (概念B)
```
- `AND` — 缩小范围，提高精确度
- `OR` — 扩大范围，提高召回率
- `NOT` — 排除不相关的概念

**3. ArXiv 分类代码**
| 领域 | 代码 |
|------|------|
| 人工智能 | cs.AI |
| 机器学习 | cs.LG, stat.ML |
| 计算机视觉 | cs.CV |
| 自然语言处理 | cs.CL |
| 机器人学 | cs.RO |
| 物理学 | hep-th, quant-ph, cond-mat |
| 数学 | math.*, q-bio.* |

**4. Semantic Scholar 高级特性**
- `fieldsOfStudy` — 按学科过滤
- `year` — 按年份范围过滤
- `sort` — 按引用数 (citationCount:desc) 或年份 (publicationDate:desc) 排序
- 利用引用网络发现相关论文（citation/reference graphs）

### 常见检索模式

| 用户需求 | 检索策略 |
|---------|---------|
| "最新的 XX 论文" | Semantic Scholar, sort by date, 近 2 年 |
| "XX 领域经典论文" | Semantic Scholar, sort by citations |
| "XX 方法的改进" | ArXiv (cs.LG/cs.AI) + 关键词 |
| "XX 的综述文章" | 关键词 + "survey" OR "review" |
| "XX 和 YY 的关系" | 两个概念 AND, 按引用排序 |
