# M13 文件化数据分析工作台

## 目标

把 EKO 的 coding 主流程延伸到数据分析：用户在对话中与 AI 共创可审阅的 Python/R 脚本，保存后执行同一文件，查看结果、lineage 和告警，修改后可重跑。文件是事实源，界面只是投影。

最小闭环：

```text
对话提出问题
→ 数据 Subagent 创建 analysis/<id>/manifest.json 与 analysis.py/R
→ 用户审阅或修改脚本
→ run_code(script_path=...) 执行已保存文件
→ latest-run.json 记录输入/脚本哈希、环境、输出和终态
→ GUI/TUI/CLI/channel 查看并重跑同一分析
```

## 业界依据

- [OpenAI Data analysis with ChatGPT](https://help.openai.com/en/articles/8437071-data-analysis-with-chatgpt)：数据分析可运行 Python 计算、转换与统计；用户应审阅生成代码、输出和假设，并可要求调整具体方法。
- [Jupyter Server Contents API](https://jupyter-server.readthedocs.io/en/latest/developers/contents.html)：Notebook、普通文件和目录都由统一内容模型表示；默认实现落在本地文件系统，并暴露路径、修改时间与内容哈希。
- [Quarto Execution Options](https://quarto.org/docs/computations/execution-options.html)：代码执行合同显式区分代码、输出、warning、error、参数和渲染结果，支持从脚本生成可复现产物。

共同模式是“代码和文件可检查，执行结果可追溯”。Jupyter 的有状态 kernel 是一种执行环境，不应被误当成持久化和正确性的唯一来源。

## 现状审计

- `NotebookPanel` 只写浏览器 `localStorage`，未接入主界面，运行按钮只提示用户回到聊天。
- `run_code` 原先只接受内联 `code`，无法证明执行内容与磁盘上的审阅脚本相同。
- EKO 已有沙箱化 `run_code`、文件 revision、TaskRuntime artifact、取消令牌和工作区文件浏览器，不需要第二套执行器。
- 数据工作区模板创建 `notebooks/analysis.md`，但没有真实执行和 lineage 合同。

## 架构取舍

### `echo-agent`

- `run_code` 增加 `script_path` 模式，直接通过对应解释器执行 working directory 内的相对脚本。
- `code` 与 `script_path` 严格二选一；拒绝绝对路径、父目录穿越、缺失文件和越过 working directory 的符号链接。
- 继续复用同一 SandboxExecutor、timeout、cancel、资源限制和输出上限。

这是任何 coding agent 都可复用的通用执行原语，属于框架。

### `echo-agent-cli`

- 分析定义、输入 lineage、stale 判定、run record、GUI 工作台和多入口命令属于 EKO 产品层。
- 使用普通文件持久化，不新增 SQLite、TaskRun 状态或 Notebook kernel。
- 目录合同：

```text
analysis/<analysis-id>/
├── manifest.json
├── analysis.py | analysis.R
├── latest-run.json
├── environment.json
├── result.json
├── outputs/
└── runs/<run-id>.json
```

## 数据合同

`manifest.json` 记录标题、语言、脚本路径、workspace-relative 输入、参数、随机种子与更新时间。

每次运行记录：

- 脚本 SHA-256 和输入 SHA-256；
- success/failed/cancelled/timed_out；
- exit code、sandbox、耗时、有界 stdout/stderr 投影；
- `environment.json` 中的运行时与包版本；
- 输出文件路径、类型、大小和 SHA-256。

每次运行会重建契约内的 `environment.json`、`result.json` 和 `outputs/`，避免失败重跑继承上一次的旧产物；脚本与 manifest 不受影响。

脚本、输入列表、输入哈希、参数或随机种子变化后，旧结果标记为 stale。保存脚本使用 revision 比对，外部编辑发生后拒绝覆盖。

## UI 与入口

- GUI 右侧工作区增加“分析”标签：分析列表、Python/R 新建、代码编辑、输入/参数、保存、运行、取消、结果、artifact 和 lineage。
- TUI/CLI/channel 提供 `/analysis list|create|show|run`，共用应用核心服务和文本 formatter。
- 对话仍是主要创作入口；工作台负责审阅和重跑，不做营销式说明页。

## 非目标

- 不实现 cell 状态、隐藏执行顺序或自研 Jupyter kernel。
- 不增加统计 DSL、自研统计推断或第二个 Python 进程管理器。
- 不把分析状态放在前端 localStorage。
- 不引入数据库、线上权限模型或新运行状态机。

## 验收

- `run_code(script_path)` 确实构造直接解释器程序调用，并拒绝路径逃逸。
- 创建、保存、运行、输入变化 stale、revision 冲突均有测试。
- GUI 能创建、编辑、执行、取消并查看 lineage/artifact。
- TUI、CLI、channel 能列出、查看、创建和重跑同一文件化分析。
- Python/R 脚本、中文标题/路径、缺失输入、sandbox unavailable、timeout/cancel 均有明确结果。
- 两个仓库完整 workspace/feature/前端验证通过。
