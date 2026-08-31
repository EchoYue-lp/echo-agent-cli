---
name: evidence-medicine
description: 医学文献检索与循证分析。当用户需要搜索医学文献、进行系统综述、评估临床证据、 撰写医学论文或查阅临床试验时激活。使用 PubMed、ClinicalTrials.gov 等数据源， 遵循 PICO 框架和 GRADE 证据分级体系。
allowed-tools: read_file read_artifact apply_patch web_search web_fetch pubmed_search clinical_trials_search pdf_fetch bibtex_generate
metadata:
  author: echo-agent-cli
  version: 1.0.0
  tags: medical, evidence, pubmed, clinical, PICO, GRADE
---

## 医学文献检索与循证分析

目标是形成可追溯、适用边界清楚的医学证据综合。它不替代个体诊断、处方或紧急医疗处理。

### 核心原则
- 遵循循证医学原则，所有医学声明必须有文献支持
- 使用可核验引用（PMID、DOI、指南/注册号或稳定 URL），严禁编造文献
- 区分已证实的临床证据和实验性/推测性结论
- 承认不确定性，标注证据强度和样本量

### 检索流程（PICO 框架）
1. **明确问题** → PICO 框架：
   - **P**opulation（人群）— 患者特征、疾病类型
   - **I**ntervention（干预）— 治疗方案、诊断方法
   - **C**omparison（对照）— 对照组、替代方案
   - **O**utcome（结局）— 主要/次要结局指标
2. **构建检索式** → 使用 MeSH 标准术语 + 自由词，布尔运算组合
3. **选择来源** → 按问题选择指南机构、PubMed、Cochrane、试验注册或监管来源；不要为了“多库”重复同一证据
4. **筛选文献** → 按纳入/排除标准筛选，记录筛选过程（PRISMA 流程图）
5. **全文阅读** → 仅在工具真实可用且合法可访问时获取全文；摘要证据必须明确标注
6. **证据分级** → 使用 GRADE 系统评估证据质量
7. **综述撰写** → 结构化系统综述，附 PRISMA 流程图

### 医学检索规范
- 使用 MeSH（Medical Subject Headings）标准术语
- 构建检索式时包含同义词和相关术语
- 使用布尔运算符（AND、OR、NOT）组合检索词
- 限定发表日期范围以获取最新证据
- 记录检索策略以便复现

### 工具策略
- `pubmed_search` — 首选，PubMed 医学文献（PMID、摘要、MeSH）
- `clinical_trials_search` — 临床试验注册信息（ClinicalTrials.gov）
- `pdf_fetch` — 下载论文全文进行深度阅读
- `bibtex_generate` — 管理引用
- `web_search` + `web_fetch` — 补充搜索（临床指南、专家共识）

### 证据评价
- 证据层级取决于问题类型；诊断、预后、患病率、危害和治疗问题不能机械使用同一排序
- 系统综述质量取决于纳入研究、异质性、偏倚和方法，不因“Meta”标签自动可信
- 评价研究设计、偏倚风险、不一致性、间接性、不精确性、发表偏倚和适用性
- 区分证据确定性、效应大小、推荐强度、患者价值偏好和资源/可行性

### 质量检查
- 所有医学声明都标注了 PMID 或 DOI
- 引用格式统一（Vancouver 格式：作者. 标题. 期刊. 年份;卷(期):页码. PMID）
- 区分了相关性和因果性
- 标注了证据级别和推荐强度
- 评估偏倚风险（使用 Cochrane RoB 2 或 ROBINS-I 工具）
- 检查利益冲突声明和资金来源
- 标注了研究局限性

### 输出规范
- 结构清晰，使用标题和子标题
- 先给结论，再给证据
- 使用表格对比不同研究的结果
- 引用时使用 PMID 或 DOI
- 不使用 emoji，保持医学专业风格

### 安全边界
对个体症状或用药问题，明确内容是一般证据信息，并提示由合格医疗专业人员结合完整病史判断。出现可能紧急的危险信号时，优先建议及时寻求当地急救或专业医疗帮助。

如需证据分级详细标准，使用 `read_skill_resource("evidence-medicine", "references/evidence_grading.md")`。
如需 PRISMA 报告清单，使用 `read_skill_resource("evidence-medicine", "references/prisma_checklist.md")`。
