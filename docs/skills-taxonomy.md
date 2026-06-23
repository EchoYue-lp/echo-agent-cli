# EKO 技能分类总览

> 本文档反映 `echo-agent-cli/skills/` 磁盘实际技能清单(共 41 个)。
> 技能布局为混合结构:部分按 `skills/<category>/<name>/` 嵌套,部分扁平放在
> `skills/<name>/`(早期内置技能,尚未迁入 category 子目录,但按 spec 归类)。

## 分类体系

EKO 将技能分为 6 个 category,用于前端分组展示和触发策略分流。

| Category | 含义 | 技能数 | 触发策略 |
|---|---|---|---|
| **methodology** | 工作方法论(思维框架/流程纪律) | 9 | 核心 4 个 baseline 默认挂载,其余按需激活 |
| **development** | 软件开发专用 | 7 | 按需激活 |
| **document** | 文档创建/编辑 | 5 | IntentRouter 自动分类 |
| **design** | 设计/创意 | 7 | IntentRouter 自动分类 |
| **research** | 研究/分析 | 10 | IntentRouter 自动分类 |
| **automation** | 自动化/工具构建 | 3 | IntentRouter 自动分类 |

## 技能清单

### methodology(9)— 嵌套 `skills/methodology/`

| 技能 | 挂载方式 | 描述 |
|---|---|---|
| brainstorming | **baseline** | 探索意图、需求和设计 |
| systematic-debugging | **baseline** | 系统化调试方法论 |
| verification-before-completion | **baseline** | 声称完成前先验证 |
| writing-plans | **baseline** | 多步任务先规划再执行 |
| test-driven-development | catalog | TDD Red/Green/Refactor |
| using-superpowers | catalog | 元技能——技能使用指南 |
| writing-skills | catalog | 技能编写指南 |
| requesting-code-review | catalog | 请求代码审查 |
| receiving-code-review | catalog | 接收代码审查反馈 |

### development(7)

| 技能 | 位置 | 描述 |
|---|---|---|
| coding | 扁平 `skills/coding/` | 编程和代码生成 |
| git-workflow | 扁平 `skills/git-workflow/` | Git 工作流管理 |
| dispatching-parallel-agents | 嵌套 `skills/development/` | 派发并行子代理 |
| executing-plans | 嵌套 `skills/development/` | 执行实施计划 |
| finishing-a-development-branch | 嵌套 `skills/development/` | 完成开发分支(合并/PR) |
| subagent-driven-development | 嵌套 `skills/development/` | 子代理驱动开发 |
| using-git-worktrees | 嵌套 `skills/development/` | Git worktree 隔离开发 |

### document(5)

| 技能 | 位置 | 描述 |
|---|---|---|
| doc-writing | 扁平 `skills/doc-writing/` | 技术文档写作 |
| docx | 嵌套 `skills/document/` | Word 文档创建/编辑/批注/修订 |
| pdf | 嵌套 `skills/document/` | PDF 处理(表单/合并/提取) |
| pptx | 嵌套 `skills/document/` | PowerPoint 演示文稿 |
| xlsx | 嵌套 `skills/document/` | Excel 表格 |

### design(7)— 嵌套 `skills/design/`

| 技能 | 描述 |
|---|---|
| algorithmic-art | 算法艺术生成(p5.js) |
| brand-guidelines | 品牌设计规范 |
| canvas-design | 画布设计(含字体资源) |
| frontend-design | 前端设计 |
| slack-gif-creator | Slack GIF 创作 |
| theme-factory | 主题工厂 |
| web-artifacts-builder | Web 构件构建 |

### research(10)

| 技能 | 位置 | 描述 |
|---|---|---|
| data-visualization | 扁平 `skills/data-visualization/` | 数据可视化 |
| data-wrangling | 扁平 `skills/data-wrangling/` | 数据整理 |
| evidence-medicine | 扁平 `skills/evidence-medicine/` | 循证医学分析 |
| paper-reader | 扁平 `skills/paper-reader/` | 论文阅读和分析 |
| paper-search | 扁平 `skills/paper-search/` | 论文学术搜索 |
| statistical-analysis | 扁平 `skills/statistical-analysis/` | 统计分析 |
| translation | 扁平 `skills/translation/` | 翻译 |
| web-search | 扁平 `skills/web-search/` | 网络搜索 |
| claude-api | 嵌套 `skills/research/` | Claude API 参考 |
| deep-research | 嵌套 `skills/research/` | 深度研究 |

### automation(3)— 嵌套 `skills/automation/`

| 技能 | 描述 |
|---|---|
| internal-comms | 内部沟通文案 |
| mcp-builder | MCP server 构建 |
| webapp-testing | Web 应用测试(Playwright) |

## 触发策略矩阵

| 用户输入特征 | 触发技能 | 优先级 | 触发源 |
|---|---|---|---|
| "设计/方案/brainstorm" | brainstorming | 1(baseline) | 第一重(默认挂载) |
| "flaky/bug/调试" | systematic-debugging | 1(baseline) | 第一重(默认挂载) |
| "Word/公文/docx/修订" | docx | 2(按需) | 第二重(IntentRouter) |
| ".docx 文件路径" | docx | 3(强制) | 第三重(ActivateSkill hook) |

## 挂载方式说明

- **baseline**:技能正文在 SessionStart 时直接注入 system prompt,始终生效,无需调用 `activate_skill`。仅 methodology 核心技能(brainstorming/systematic-debugging/verification-before-completion/writing-plans)默认 baseline,可在 enabled-skills.json 调整。
- **catalog**:技能列在目录中,模型按需调用 `activate_skill` 获取完整指令。
- **按需激活(IntentRouter)**:用户输入触发 KeywordClassifier 后自动激活。

## 布局说明

- **嵌套结构**(`skills/<category>/<name>/SKILL.md`):P5 移植的新技能,按 category 组织。
- **扁平结构**(`skills/<name>/SKILL.md`):早期内置技能,尚未迁入 category 子目录。
  `SkillLoader::scan_directory` 已支持递归(见 echo-execution loader.rs),两种布局都能被发现;
  category 字段由 SKILL.md frontmatter `metadata.category` 决定,与目录结构解耦。
