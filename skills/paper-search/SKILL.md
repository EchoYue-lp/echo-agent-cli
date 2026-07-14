---
name: paper-search
description: >-
  学术论文检索。当用户需要搜索学术论文、查找文献综述、了解某领域研究进展、
  或寻找特定主题的学术文章时激活。支持 ArXiv 和 Semantic Scholar 交叉检索。
  注意：医学文献请使用 evidence-medicine skill。
triggers:
  - 论文检索
  - 学术论文
  - 文献检索
  - arxiv
  - semantic scholar
  - 引用数
  - paper search
  - academic paper
  - research paper
  - 找论文
  - 文献综述
allowed-tools:
  - "Read"
  - "Write"
  - "WebSearch"
  - "WebFetch"
  - "ArxivSearch"
  - "SemanticScholarSearch"
metadata:
  author: echo-agent-cli
  version: "1.0.0"
  tags: "academic, paper, arxiv, search, literature"
---

## 学术论文检索

目标是建立可复现的检索结果集，并说明覆盖范围、来源质量和仍可能遗漏的证据。

### 核心原则
- **按学科选择来源** — ArXiv、Semantic Scholar 只是可选来源；优先使用与问题匹配的数据库和出版方
- **检索式构建** — 使用精确的学术关键词，包含同义词和布尔运算符
- **相关性和方法优先** — 引用数、期刊/会议声望和新旧程度只是信号，不能替代方法质量与问题匹配度
- **严禁编造** — 每篇论文的标题、作者、DOI 必须来自工具返回的真实结果

### 检索流程
1. **明确检索主题** → 提取核心概念，确定学科领域
2. **构建检索式** → 使用英文关键词（学术数据库以英文为主），包含同义词
3. **检索与迭代** → 使用当前可用数据库，查看关键词命中后调整同义词、作者、引用链和时间范围
4. **筛选记录** → 按预先定义的相关性、研究设计和质量标准筛选，并记录排除理由
5. **深度获取** → 对重要论文使用 `pdf_fetch` 下载全文
6. **引用管理** → 使用 `bibtex_generate` 生成标准引用格式

### 工具策略
- `arxiv_search` — CS/AI/ML/物理/数学预印本，免费全文
- `semantic_scholar_search` — 全学科，引用数、h-index、相关论文网络
- `pdf_fetch` — 下载 PDF 全文，提取文本进行深度分析
- `bibtex_generate` — 生成标准 BibTeX 引用条目
- `web_search` — 补充搜索（期刊官网、作者主页等）

### 质量检查
- 所有事实性声明必须有来源支持
- 引用格式一致且完整
- 区分相关性和因果性
- 标注证据强度（证据等级见下方）
- 承认不确定性，区分已证实和推测性结论

### 质量判断
- 区分同行评审论文、预印本、技术报告、数据集论文和二手综述
- 检查研究问题、方法、数据、基线、统计支持、可复现性、利益冲突和外部效度
- 引用数受领域规模和发表年份影响，不作为单一排序依据

### 输出规范
- 每篇论文包含：标题、作者、年份、来源（期刊/会议）、摘要、DOI/URL
- 标注引用数（来自 Semantic Scholar）
- 按主题分组呈现，标注论文之间的关联
- 引用时使用作者-年份格式
- 不使用 emoji，保持学术风格
- 标注检索日期、查询范围、数据库和可能遗漏项

如需检索策略构建指南，使用 `read_skill_resource("paper-search", "references/search_strategy.md")`。
