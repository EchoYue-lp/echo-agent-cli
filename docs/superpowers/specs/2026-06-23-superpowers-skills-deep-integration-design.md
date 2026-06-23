# Superpowers + Skills 深度集成设计

> **日期**:2026-06-23
> **状态**:已批准
> **目标**:把 superpowers(14 个方法论技能)和 Anthropic skills(17 个领域技能)真正固化进 EKO 技能体系,构建"资产分类 + 三重触发 + 脚本运行时 + 装配/更新 + UI + eval"全链路能力,让 EKO 成为能在文档、设计、研究、方法论等任意场景自动用对技能的通用 agent。

---

## 背景

### 现状(代码已核实)

**技能基础设施已完备**(对齐 agentskills.io 规范):
- 框架层(`echo-agent`):`Skill` trait、`SkillRegistry`、`discover_skills`、`load_skills_from_dir`、三级渐进披露(Discovery → Catalog → Activation → Resources)、`HookRegistry`、20+ hook 事件、6 种 `HookAction`。
- 应用层(`echo-agent-cli`):`SkillsHub`(扫描 `~/.echo-agent/skills/`)、`SkillsPanel` UI、Tauri IPC(`list/get/load/enable/disable_skill`)、内置 11 个领域技能包(coding/data-visualization/evidence-medicine/paper-reader/translation/web-search 等)。

**Superpowers 方法论仅以文档形式存在**:
- 开发流程在用 superpowers 的 brainstorm/spec/plan 工作流产出文档(`docs/superpowers/{specs,plans}/`)。
- 但 brainstorming/test-driven-development/systematic-debugging/writing-plans 等**方法论技能本身从未作为可加载技能固化进产品**。
- 根目录 `lp-agent/superpowers/`(14 个 SKILL.md + hooks)和 `lp-agent/skills/`(Anthropic 官方 17 个技能)是上游参考克隆,未移植。

**三重触发机制的空白**:
- 关键词/LLM 分类:✅ 已有(`KeywordClassifier` + `LlmIntentClassifier` + `ChainedClassifier`),但 eval 的 match_fn 是假的(`contains` 而非真 classifier)。
- SessionStart hook:⚠️ 机制完整但 11 个内置技能**全部没用 hook**。
- hook 不能直接激活技能:⚠️ 只能注入提示让模型自己调 `activate_skill`;`HookAction::Agent` 是空壳;`UserPromptSubmit` 事件存在但没人触发。

**脚本运行时的限制**:
- `minimal_env` 不传 `HOME`,导致大量脚本失败。
- 无 pip/venv/requirements 管理、无外部二进制(soffice/pdftoppm)探测、无长驻进程管理。
- `SkillSandboxPolicy` 已解析但执行层未接线(形同虚设)。
- 默认 sandbox 未装配(builder 默认 None,Dangerous 脚本裸跑)。

### 产品定位约束(来自 AGENTS.md)

**EKO 是本地个人超级智能助理,运行在用户自己的机器上,不部署到线上。** 本次设计的安全边界遵循此定位:
- 不套用线上 Web 服务的威胁模型。
- 默认不加权限门控;要加必须在注释里写明"本地场景下为何仍需要"。
- CoWork 环境无 docker——脚本运行时不依赖容器化方案。

### 业界做法参考(决策依据)

调研 Claude Code / Codex / uv 等:
- **几乎所有主流 agent 都不在框架里自造依赖管理轮子,而是委托给 `uv`(Python)和 `npx`/`bun`(Node)**。
- `uv run --script` + PEP 723 内联依赖块是事实标准:脚本头部声明依赖,uv 自动建临时环境、装依赖、跑完即弃(缓存依赖、环境 ephemeral)。
- Claude Code 自己不管理 python 环境,靠 `uv run`/`python3` 让宿主兜底。
- sandbox 是进程/网络隔离手段,不解决"装不装得上包"。

---

## 决策总览

| 维度 | 决策 |
|---|---|
| 集成目标 | **全链路打通**(资产 + 触发 + 方法论内置 + 装配更新 + UI + eval) |
| 资产范围 | **全部移植 40+ 技能**(superpowers 14 + Anthropic 17 + 现有 11) + 重新分类标签 |
| 兼容深度 | **深度适配**(脚本 + hook 全打通,开箱即用) |
| 触发强度 | **三重保障**(IntentRouter 分类 + 方法论默认挂载 + SessionStart 强制检查 hook) |
| 架构方案 | **分类资产 + TriggerSupervisor**(技能按 category 组织;三源融合统一调度) |
| Python 执行器 | **uv 优先**(`uv run --script` + PEP 723),无 uv 回退裸 python3 |
| 系统二进制 | **探测 + 提示**,绝不自动安装 |
| 交付策略 | 一个 spec 全覆盖,writing-plans 阶段拆 8 个 phase |

---

## 范围边界

### In scope

1. 40+ 技能资产移植与分类(superpowers 14 + Anthropic 17 + 现有 11,去重对齐)
2. 技能分类体系(category 标签:methodology / document / design / research / development / automation)
3. TriggerSupervisor 三重触发引擎(关键词 + LLM + Hook 三源融合)
4. 方法论 baseline 默认挂载机制(SessionStart 自动注入核心方法论正文)
5. Hook 能力扩展(新增 `HookAction::ActivateSkill`,让 hook 能直接激活技能)
6. 脚本运行时加固(uv 优先解析 + SkillSandboxPolicy 接线 + 默认 sandbox 装配 + 依赖探测 + minimal_env 放宽)
7. SkillsHub 上游同步(从 git remote 拉取/更新技能包)
8. 前端技能分类展示(按 category 分组、baseline 标记、依赖/更新提示)
9. eval 触发测试集扩展(补到 50+ case + 用真 ChainedClassifier + F1 ≥ 0.85 门控)
10. 文档:技能分类说明、上游同步指南、方法论技能清单、技能编写指南增强、系统深度文档更新

### Out of scope(留作后续)

- **`HookAction::Agent` 完整实现**:当前空壳,实现 subagent 驱动的 hook 成本高。本 spec 用 `Command` + `Prompt` + 新增 `ActivateSkill` action 替代。
- **长驻服务管理**(brainstorming 的 `server.cjs` 常驻 HTTP 服务):降级为"按需冷启动",不做守护进程。若必须常驻,走 MCP server 配置承载。
- **Python venv / pip 自动化**(每技能独立虚拟环境):uv 的 ephemeral 环境已替代,无需自建 venv。
- **热卸载**(disable_skill 真正生效):需重构 SkillRegistry 加 unregister,本 spec 保持 `requires_restart` 语义,标注为已知限制。

### 仓库分工

| 仓库 | 改动 |
|---|---|
| `echo-agent/`(框架) | TriggerSupervisor、HookAction::ActivateSkill、HookResult 扩展、SkillSandboxPolicy 接线、SkillRegistry 增强(分类元数据 + baseline 注入)、默认 sandbox 装配、uv 优先解析、minimal_env 放宽、依赖探测层、hooks.json 发现 |
| `echo-agent-cli/`(应用层) | 40+ 技能资产移植、SkillsHub upstream registry + sync、Tauri IPC、前端 category UI、eval case 扩展、runtime bootstrap 接线、enabled-skills.json、文档 |

**跨仓库依赖顺序**:echo-agent 先合并(框架层改动是前置),再合并 echo-agent-cli(遵循 AGENTS.md worktree 规范第 6 条)。

---

## 1. 技能分类体系与资产移植

### 1.1 分类体系

40+ 技能归入 **6 个 category**。category 作为 SKILL.md frontmatter 的新字段(落在现有 `metadata` map 里,零 schema 改动),用于前端分组展示和触发策略分流。

| Category | 含义 | 技能(移植后) | 触发策略 |
|---|---|---|---|
| **methodology** | 工作方法论(思维框架/流程纪律) | brainstorming, systematic-debugging, test-driven-development, verification-before-completion, writing-plans, writing-skills, using-superpowers, requesting-code-review, receiving-code-review | **核心 4 个 baseline 默认挂载**(见 §3.3),其余 5 个走 catalog 按需激活 |
| **development** | 软件开发专用 | coding(已有), git-workflow(已有), using-git-worktrees, subagent-driven-development, dispatching-parallel-agents, executing-plans, finishing-a-development-branch, skill-creator | 按需激活 |
| **document** | 文档创建/编辑 | docx, pdf, pptx, xlsx, doc-writing(已有), doc-coauthoring | IntentRouter 自动分类 |
| **design** | 设计/创意 | canvas-design, brand-guidelines, theme-factory, algorithmic-art, frontend-design, web-artifacts-builder, slack-gif-creator | IntentRouter 自动分类 |
| **research** | 研究/分析 | paper-reader(已有), paper-search(已有), evidence-medicine(已有), web-search(已有), statistical-analysis(已有), data-wrangling(已有), data-visualization(已有), claude-api | IntentRouter 自动分类 |
| **automation** | 自动化/工具构建 | mcp-builder, webapp-testing, internal-comms | IntentRouter 自动分类 |

**去重对齐**:现有 11 个技能里,`coding` 保留归 development;其余 10 个(数据分析/学术/通用)归 research。与上游无命名冲突。

### 1.2 资产移植方案

**目标位置**:`echo-agent-cli/skills/<category>/<skill-name>/SKILL.md`(按 category 分子目录,便于维护和前端展示)。

**移植方式**:手工移植 + 适配,不做 git submodule(避免子仓库嵌套复杂度)。

**移植清单与适配深度**(按工作量和风险分层,供 writing-plans 拆 phase):

#### Tier A · 纯文本方法论(9 个,零脚本,最高优先级)
superpowers 的纯指令型方法论技能:brainstorming, systematic-debugging(除 find-polluter.sh), test-driven-development, verification-before-completion, writing-plans, writing-skills, using-superpowers, requesting-code-review, receiving-code-review。
- **适配**:仅 frontmatter 加 `metadata.category: methodology` + 补 `description` 触发说明。body 原样保留。
- **开箱即用**:✅

#### Tier B · 轻脚本方法论(2 个)
- subagent-driven-development(3 个 bash 脚本)、systematic-debugging 的 find-polluter.sh(1 个 bash)。
- **适配**:bash 脚本直接可用,验证路径围栏通过即可。
- **开箱即用**:✅(依赖 bash)

#### Tier C · 开发流程方法论(5 个)
using-git-worktrees, dispatching-parallel-agents, executing-plans, finishing-a-development-branch, skill-creator(9 个 python 脚本)。
- **适配**:skill-creator 的 python 脚本走 `uv run` + PEP 723 头。
- **开箱即用**:✅(装了 uv 或 python3)

#### Tier D · 文档技能(4 个,高价值,中等工作量)
docx, pdf, pptx, xlsx —— 共享 `office/` 目录 + 各自脚本。
- **适配**:给 python 脚本补 PEP 723 头(`defusedxml` 等),去重 `office/` 为共享目录,声明 `requires-binaries: [soffice]` 探测。
- **开箱即用**:✅(装了 uv + 可选 LibreOffice)

#### Tier E · 设计/创意技能(7 个,中等工作量)
canvas-design(5.5MB 字体), brand-guidelines, theme-factory, algorithmic-art, frontend-design, web-artifacts-builder, slack-gif-creator。
- **适配**:canvas-design 字体作为 resources 按需读取;web-artifacts-builder 的 bash 脚本验证;slack-gif-creator 的 Pillow 依赖走 PEP 723。
- **开箱即用**:✅

#### Tier F · 研究/自动化技能(10 个,已有 + 少量新增)
现有 10 个 research 技能保留;新增 claude-api(参考资料型)、mcp-builder、webapp-testing、internal-comms、doc-coauthoring。
- **适配**:claude-api 是纯参考资料(30 个 md),作为 resources;mcp-builder/webapp-testing 的 python 脚本走 PEP 723。
- **开箱即用**:✅

### 1.3 frontmatter 适配规范

每个移植的 SKILL.md 统一成这个格式(EKO 已支持所有字段):

```yaml
---
name: brainstorming
description: 探索用户意图、需求和设计,把想法变成完整设计稿
metadata:
  category: methodology          # 新增分类字段
  source: superpowers            # 来源标识
  upstream-version: "5.1.0"      # 上游版本(用于同步)
  author: obra
  tags: [design, planning, workflow]
triggers:                        # 触发词(methodology 类由 baseline 机制挂载,可不填)
  - 头脑风暴
  - 设计
  - brainstorm
allowed-tools: []                # 按需声明
---
```

**对上游的改动最小**:只加 `metadata.category`、`metadata.source`、`metadata.upstream-version`,其余字段上游没有就不加。

---

## 2. 脚本运行时加固(无 docker)

本节解决"深度适配后,40+ 技能的脚本怎么真正跑起来"。核心改动在 `echo-agent` 框架层。

### 2.1 改动总览

| 改动点 | 文件 | 优先级 |
|---|---|---|
| ① 解释器解析:uv run 优先(Python) | `run_script_tool.rs::resolve_interpreter` | 高 |
| ② `minimal_env` 放宽传 `HOME` | `echo-core/src/tools/skill.rs` | 高 |
| ③ SkillSandboxPolicy 接线(声明→执行) | `run_script_tool.rs` + `SandboxCommand` | 中 |
| ④ 默认 sandbox 装配 | `react/builder.rs` | 中 |
| ⑤ 外部二进制探测层 | 新增 `skill_dependency_probe` 模块 | 中 |
| ⑥ PEP 723 内联依赖(资产适配侧) | 移植 Anthropic 脚本时补头 | 高 |

### 2.2 解释器解析:uv run 优先

当前 `resolve_interpreter`(`echo-execution/src/skills/external/run_script_tool.rs:328`)的 Python 分支:`python3 → python → py -3`。

改为**三级回退**,uv 优先:

```rust
fn resolve_python() -> Invocation {
    // uv 存在 → uv run(自动处理 PEP 723 依赖 + 隔离环境)
    if which_exists("uv") {
        return Invocation::new("uv").arg("run").arg("--script");
        // --script 让 uv 读取脚本头的 # /// script 内联依赖块
    }
    // 无 uv → 裸 python3(标准库脚本照跑)
    if which_exists("python3") { return Invocation::simple("python3"); }
    if which_exists("python")  { return Invocation::simple("python"); }
    // Windows
    if which_exists("py")      { return Invocation::simple("py").arg("-3"); }
    unresolvable("python")
}
```

**关键**:`uv run --script` 配合 PEP 723 头,uv 会自动建临时环境、装声明的依赖、跑完即弃(默认缓存依赖到 `~/.cache/uv`,环境本身 ephemeral)。无 PEP 723 头的脚本 uv 也能跑(退化为裸执行)。

Node 侧不变(`node` 直接跑)。TypeScript 已有 `bun → deno → npx tsx`。

### 2.3 minimal_env 放宽传 HOME

当前致命限制:`minimal_env`(`echo-core/src/tools/skill.rs:103`)白名单只含 `PATH/LANG/LC_ALL/TMPDIR/TZ/SKILL_DIR/SESSION_ID`,**不传 `HOME`**。导致 python 脚本读 `~/.config` 失败、git 操作失败、很多工具找不到用户配置。

**改动**:白名单加 `HOME`。

```rust
pub fn minimal_env() -> HashMap<String, String> {
    let mut env = HashMap::new();
    for key in ["PATH", "LANG", "LC_ALL", "TMPDIR", "TZ", "HOME"] {  // +HOME
        if let Ok(v) = std::env::var(key) { env.insert(key.to_string(), v); }
    }
    env.insert("SKILL_DIR".into(), ...);
    env.insert("SESSION_ID".into(), ...);
    env
}
```

**理由(遵循 AGENTS.md 安全定位)**:EKO 是本地桌面应用,用户信任本机,脚本本就该能访问用户主目录。`env_clear` 的初衷是防恶意环境变量注入,`HOME` 不属于敏感泄露。

**审慎项**:`minimal_env` 有测试断言 `!env.contains_key("HOME")`(`skill.rs:217`),要同步改测试。

### 2.4 SkillSandboxPolicy 接线

当前 `SkillSandboxPolicy`(types.rs:26)声明了 `isolation/network/timeout_secs/allowed_paths/denied_paths`,但 `run_script_tool.rs` **完全不读它**,形同虚设。

**改动**:在 `RunSkillScriptsTool::execute` 的 sandbox 分支,把 `descriptor.sandbox` 翻译成 `SandboxCommand` 参数:

```rust
// run_script_tool.rs,execute() 内
if let Some(policy) = &descriptor.sandbox {
    let mut cmd = SandboxCommand::program(program, args)
        .with_working_dir(skill_dir)
        .with_timeout(policy.timeout_secs.unwrap_or(30));
    // network 策略 → sandbox 网络开关
    if policy.network.unwrap_or(false) {
        cmd = cmd.with_network_enabled(true);
    }
    // allowed_paths → LocalSandbox 的可写路径白名单(macOS Seatbelt)
    if let Some(ref mgr) = sandbox_manager {
        let cmd_with_paths = cmd.with_allowed_write_paths(&policy.allowed_paths);
        return mgr.execute(cmd_with_paths).await;
    }
}
```

**本地场景权衡**(遵循 AGENTS.md "默认不加权限门控"):本地桌面应用默认不强制沙箱,但 SkillSandboxPolicy 一旦技能声明了就生效。这是"用户/技能作者主动声明隔离需求"而非"框架强加"。

### 2.5 默认 sandbox 装配

当前 `ReactAgentBuilder` 默认 `sandbox_manager: None`(`builder.rs:129`),全仓无自动装配 → Dangerous 级脚本裸跑宿主机。

**改动**:`build()` 时默认装配 `SandboxManager::local_only()`(纯本地,不开 Docker;macOS 上自动启用 Seatbelt 文件路径限制)。

```rust
// builder.rs build()
let sandbox_manager = self.tools.sandbox_manager.clone()
    .unwrap_or_else(|| Arc::new(SandboxManager::local_only()));
```

**符合本地定位**:`local_only()` 不需要 docker(`manager.rs:118`),只做进程级 + 可选 OS 级隔离。用户若想要更强隔离可显式传 `auto_detect()`(有 docker 才用)。

> 这是"防止框架 bug / 防用户无意数据丢失"的防护,符合 AGENTS.md "何时该加防护"第 (1)(2) 条,不属于"线上服务威胁模型"。

### 2.6 外部二进制探测层

新增轻量模块 `echo-execution/src/skills/dependency_probe.rs`:

```rust
pub struct SkillDependency {
    pub kind: DepKind,           // PythonPkg / Binary / NodeModule
    pub name: String,            // "soffice" / "pypdf" / "Pillow"
    pub required: bool,          // true=缺了技能不可用;false=可选功能降级
    pub install_hint: String,    // "brew install --cask libreoffice"
}

pub fn probe_skill_dependencies(descriptor: &SkillDescriptor) -> ProbeReport {
    // 读 frontmatter 的 metadata.requires-binaries / requires-python-packages
    // 用 which_exists / importlib 探测,生成 { satisfied, missing_required, missing_optional }
}
```

**frontmatter 声明方式**(落在现有 metadata,零 schema 改动):
```yaml
metadata:
  requires-binaries: [soffice]              # 系统二进制
  requires-python-packages: [defusedxml]    # pip 包(有 PEP 723 头的话其实冗余,作为文档+降级提示)
```

**激活时行为**:激活技能前跑探测,缺失 required 依赖 → 返回明确错误给模型"技能 X 需要 soffice,未检测到,请安装:brew install --cask libreoffice";缺失 optional → 注入提示"功能 Y 需要 Z,未安装将降级"。

**绝不自动安装系统二进制**(只探测+提示)。

### 2.7 PEP 723 内联依赖(资产适配侧)

这是 Tier D(docx/pdf/pptx/xlsx)适配时的具体操作。给每个依赖第三方库的 python 脚本头部加 PEP 723 块:

```python
#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = [
#     "defusedxml",
#     "lxml",
# ]
# ///
import defusedxml.ElementTree as ET
...
```

`uv run --script` 会读这个块,自动建临时环境装 `defusedxml`+`lxml`,跑完即弃。**无需 venv、无需 pip、无需用户手动安装**。

Office 三件套共享的 `office/` 目录:把 PEP 723 头加到 `pack.py`/`unpack.py`/`validate.py` 等入口脚本,helper/validators 作为同目录 import 自动可用(uv 处理 sys.path)。

### 2.8 不做的事

- ❌ 不自动建 venv(uv 的 ephemeral 环境替代)
- ❌ 不自动 pip install(PEP 723 让 uv 按需装)
- ❌ 不自动装系统二进制(只探测提示)
- ❌ 不强制 Docker(local_only 默认)
- ❌ 不为 brainstorming 的 server.cjs 做常驻服务管理(降级为冷启动或走 MCP 配置)

### 2.9 验证标准

- `resolve_python()` 在有 uv 的机器上返回 uv 路径,无 uv 回退 python3
- 跑一个带 PEP 723 头的 `import defusedxml` 脚本能成功(无全局安装 defusedxml)
- docx 技能在装了 LibreOffice 的机器上完整工作,未装时给出明确降级提示
- `minimal_env` 含 HOME,git 操作不再因缺 HOME 失败
- SkillSandboxPolicy 声明的 allowed_paths 在 macOS Seatbelt 下生效

---

## 3. TriggerSupervisor 三重触发引擎

这是整个集成的心脏。目标:让 EKO 在任意场景(文档/设计/研究/方法论)能自动用对技能。

### 3.1 三重保障的职责划分

| 重 | 机制 | 触发时机 | 解决什么 | 现状 |
|---|---|---|---|---|
| **第一重 · 被动默认** | 方法论 baseline 挂载 | SessionStart | 方法论技能(brainstorming/debugging/verification 等)**始终可用**,无需用户/分类器主动激活 | ❌ 空白(11 个内置技能全没用此机制) |
| **第二重 · 主动分类** | IntentRouter 强化 | 每条用户消息 | 领域技能(document/design/research)按用户输入**自动识别并激活** | ⚠️ 机制有但弱(单 classifier、无三源融合) |
| **第三重 · 强制检查** | Hook 注入 + 直接激活 | SessionStart + UserPromptSubmit | 每轮**强制模型检查适用技能**,高置信度的可直接激活 | ⚠️ 机制有但空转(hook 不能直接激活技能) |

### 3.2 TriggerSupervisor:统一调度层

新增一个 `TriggerSupervisor`,作为 `IntentClassifier` trait 的实现,挂在 IntentRouter 之下,**包装并融合三源**。

```
用户消息
   │
   ▼
IntentRouter.classify()  ── 调用 ──▶  TriggerSupervisor.classify()
                                          │
                    ┌─────────────────────┼─────────────────────┐
                    ▼                     ▼                     ▼
            ① KeywordClassifier    ② LlmIntentClassifier   ③ Hook 源
              (0 token,快)           (~500 token,语义)      (UserPromptSubmit
                    │                     │                    hook 产出的
                    │                     │                    activate 建议)
                    └─────────────────────┼─────────────────────┘
                                          ▼
                                   置信度融合 + 投票
                                          │
                          ┌───────────────┴───────────────┐
                          ▼                               ▼
                   高置信度 SkillRequired          低置信度 → Fallback
                   (直接激活 + 进 ReAct)          (进 ReAct,靠第三重 hook 兜底)
```

**关键设计点**:

- **TriggerSupervisor 是 `IntentClassifier` 的实现**,通过 `set_intent_router`(`react/mod.rs:1300`)接入,无需改 IntentRouter 本身。它内部组合三个子分类器。
- **三源融合规则**(简单且确定,不引入 LLM 二次决策):
  1. ①关键词 + ②LLM 任一返回 `SkillRequired` 且 confidence ≥ 阈值 → 直接采纳(快速路径)
  2. 两源都 Fallback 但 ③hook 源有明确 `activate_skill` 建议 → 采纳 hook(兜底路径)
  3. 全部低置信度 → Fallback,进 ReAct(第三重 hook 会注入"检查清单"提示模型自查)
- **性能**:①关键词零成本、③hook 是字符串匹配几乎零成本,只有 ①失败时才跑 ②LLM。绝大多数消息走 ①快路径。

### 3.3 第一重:方法论 baseline 默认挂载

**机制**:bootstrap 时(runtime.rs 的 startup hook 之前),自动把 `category: methodology` 的技能注入 system prompt,**用更深的注入方式**(不只是 catalog 列表)。

**现有 catalog 注入 vs baseline 注入的区别**:
- catalog(Tier 1):只列出 `- name: description`,模型要自己 `activate_skill` 才能拿到正文。
- baseline:把**核心方法论技能的正文指令**直接注入 system prompt,无需激活即生效。

**为什么方法论要 baseline 而非 catalog**:方法论(brainstorming/verification/debugging)是"思维方式",应该**始终在场**影响模型每一步决策,而不是等用户说"帮我 debug"才激活。这正是 superpowers 的核心哲学("你拥有超能力,任何任务前先检查技能")。

**实现**:新增 `SkillRegistry::inject_methodology_baseline()`,在 `discover_skills` 后、catalog 注入前调用:

```rust
// registry.rs 新增
pub fn inject_methodology_baseline(&self, system_prompt: &mut String) {
    for (name, desc) in &self.descriptors {
        if desc.metadata.get("category") != Some("methodology") { continue; }
        if !desc.is_baseline_eligible() { continue; }  // 只注入核心几个(避免 prompt 膨胀)
        if let Ok(content) = self.read_skill_body(name) {  // 读 SKILL.md 正文
            system_prompt.push_str(&format!(
                "\n\n<skill name=\"{name}\">\n{}\n</skill>", content.body()
            ));
        }
    }
}
```

**baseline 资格筛选**(控制 system prompt 体积):只对**核心 4 个方法论技能**做 baseline 注入(正文直接进 system prompt),其余 5 个 methodology 技能走 catalog(模型按需 activate):

| 是否 baseline | 技能 | 理由 |
|---|---|---|
| ✅ baseline | **brainstorming** | 创造性工作前的"先理解再动手"纪律,适用面最广 |
| ✅ baseline | **systematic-debugging** | "找根因而非症状"是通用调试纪律 |
| ✅ baseline | **verification-before-completion** | "声称完成前先验证"是通用质量纪律 |
| ✅ baseline | **writing-plans** | 多步任务"先规划再执行"纪律 |
| ❌ catalog | test-driven-development | 偏 coding 专用,非通用 |
| ❌ catalog | using-superpowers | 元技能(管理如何用技能),靠 SessionStart hook 注入检查清单即可,不需正文 baseline |
| ❌ catalog | writing-skills | 偏 skill 作者专用 |
| ❌ catalog | requesting-code-review | 偏 coding 专用 |
| ❌ catalog | receiving-code-review | 偏 coding 专用 |

> 这 4 个是默认 baseline。用户可在前端 `enabled-skills.json` 调整(把某个 catalog 方法论升为 baseline,或反之)。baseline 正文总量控制在 ~2000 token 以内(粗估 4 个 SKILL.md 的精简版)。

**安全考虑**(遵循 AGENTS.md):baseline 注入会增大 system prompt,但这是用户启用方法论技能的预期行为,不属于"线上 prompt 注入攻击"。体积控制靠资格筛选。

### 3.4 第二重:IntentRouter 强化

现有 `ChainedClassifier`(关键词→LLM 级联)已经可用,主要补两处:

1. **eval 的 match_fn 修真**:当前 `echo-agent-eval/main.rs:225` 的 match_fn 是 `lower.contains(trigger)` 字符串包含,**不是真 classifier**。改为调用真 `ChainedClassifier`(或新的 `TriggerSupervisor`),让 F1 度量真实反映生产路由效果。

2. **领域技能的 `triggers` 补全**:移植 Tier D/E/F 技能时,在 frontmatter 补 `triggers` 触发词(中英双语),喂给 KeywordClassifier。这是资产适配侧的工作(见第 1 节)。

**不改 IntentRouter 核心逻辑**——它已经很完整,TriggerSupervisor 是在它之下的增强。

### 3.5 第三重:Hook 强制检查 + 直接激活

这是改动最大的一重,解决"hook 空转、不能直接激活技能"。

#### 3.5.1 新增 HookAction::ActivateSkill 变体

当前 `HookAction`(`hooks.rs:128`)的变体都不适合"直接激活技能"。新增轻量变体:

```rust
#[serde(tag = "type", rename_all = "lowercase")]
pub enum HookAction {
    Command { ... },
    Prompt { prompt },
    Permission { ... },
    Http { ... },
    McpTool { ... },
    Agent { ... },              // 仍空壳,不实现
    ActivateSkill { skill: String, reason: String },  // ★ 新增
}
```

`ActivateSkill` 的语义:hook 匹配命中时,直接激活指定技能(等价于调 `activate_skill_for_context`),不需要模型自己决策。

#### 3.5.2 HookResult 扩展

`HookResult`(`hooks.rs`)增加字段,让 `fire_lifecycle_hook` 收到激活请求后能执行:

```rust
pub struct HookResult {
    pub injected_context: Vec<String>,    // 已有
    pub messages: Vec<String>,            // 已有
    pub block: bool,                      // 已有
    pub activate_skill: Option<(String, String)>,  // ★ 新增 (skill_name, reason)
}
```

#### 3.5.3 执行接线

在 `fire_lifecycle_hook`(`context.rs:317`)收到 `HookResult.activate_skill` 后,调用 agent 的 `activate_skill_for_context`:

```rust
// context.rs fire_lifecycle_hook 内,merge_result 之后
if let Some((skill, reason)) = hook_result.activate_skill.as_ref() {
    // 直接激活,不走 IntentRouter
    self.activate_skill_for_context(skill).await;
    // reason 作为 system note 注入,告诉模型为何激活
    ctx.push(Message::system(format!("已根据上下文自动激活技能 {skill}:{reason}")));
}
```

#### 3.5.4 UserPromptSubmit 触发点补齐

当前 `UserPromptSubmit` 事件(`types.rs:75`)存在但**没人触发**。补触发点:在 `prepare_react_context`(每条用户消息进 ReAct 前)调一次:

```rust
// react/run/context.rs prepare 阶段
self.fire_lifecycle_hook(HookEvent::UserPromptSubmit, &user_msg).await;
```

这样每条用户消息都会触发 UserPromptSubmit hook,第三重保障就有了"每轮检查"的触发点。

#### 3.5.5 强制检查 hook 的形态(superpowers 式)

移植 superpowers 的 `session-start` hook 逻辑,改写成 EKO 的 frontmatter `hooks:` 格式(而非外部 hooks.json):

```yaml
# using-superpowers/SKILL.md frontmatter
hooks:
  SessionStart:
    - matcher: "startup|clear|resume"
      hooks:
        - type: prompt
          prompt: |
            收到任务后,你必须先检查是否有技能适用:
            - 创造性工作(新功能/设计)→ brainstorming
            - 修 bug/测试失败 → systematic-debugging
            - 写代码 → test-driven-development
            - (完整清单见各技能 catalog 条目,此处仅列高优先级)
            有 1% 可能适用就要检查。
  UserPromptSubmit:
    - matcher: "*"
      hooks:
        - type: prompt
          prompt: "本轮用户消息,检查上面列出的技能是否有适用的。"
```

这是 **prompt 型 hook**(已有机制),不依赖新增的 ActivateSkill——新增 ActivateSkill 是给"高确定性场景直接激活"用的(如检测到 `.docx` 文件路径 → 直接激活 docx 技能)。

#### 3.5.6 外部 hooks.json 发现(superpowers 兼容)

superpowers 用外部 `hooks.json` 而非 frontmatter。新增 `SkillLoader` 的发现路径:`loader.rs` 扫描技能目录时,除了 `SKILL.md` 也读 `hooks.json`(若存在),合并进该技能的 `HooksDefinition`。

这样新移植的 superpowers 技能可以**保留原 hooks.json**,无需手工转 frontmatter(降低适配成本)。

### 3.6 三重的协同示例

**场景:用户说"帮我把这个 markdown 转成带批注的 Word 文档"**

| 重 | 行为 |
|---|---|
| 第一重(被动) | brainstorming/verification 已在 system prompt,模型默认会"先确认需求再动手、完成后验证" |
| 第二重(主动) | TriggerSupervisor 关键词命中"Word 文档/批注" → SkillRequired(docx) → 自动激活 docx 技能 |
| 第三重(强制) | UserPromptSubmit hook 注入检查清单,即便第二重漏了,模型也被强制思考"是不是该用文档技能" |

**场景:用户说"这个测试为啥一直 flaky"**

| 重 | 行为 |
|---|---|
| 第一重 | systematic-debugging 正文已在 system prompt,模型按"找根因而非症状"的纪律推进 |
| 第二重 | 关键词"flaky/测试"可能命中也可能不命中 |
| 第三重 | 检查清单提示"调试场景考虑 systematic-debugging"(虽然已 baseline,这里起强化作用) |

### 3.7 不做的事

- ❌ 不实现 `HookAction::Agent`(空壳保留,subagent 驱动 hook 留后续)
- ❌ 不做"三源 LLM 二次裁决"(用确定性的置信度融合,避免额外 token 和延迟)
- ❌ 不让 hook 绕过 `allowed-tools` 白名单(ActivateSkill 仍受技能声明的工具权限约束)

### 3.8 验证标准

- TriggerSupervisor 在只有关键词命中时返回 SkillRequired,无需调 LLM
- methodology baseline 技能的正文出现在 system prompt 里(可用日志验证)
- UserPromptSubmit hook 在每条消息触发(日志可见)
- 高置信度 ActivateSkill hook 能直接激活技能(无需模型调用 activate_skill 工具)
- 移植的 superpowers 技能保留 hooks.json 即可生效,无需手工转 frontmatter

---

## 4. SkillsHub 上游同步 + 前端分类 UI

### 4.1 改动总览

| 改动点 | 模块 | 优先级 |
|---|---|---|
| ① SkillsHub upstream registry(记录 git remote + 版本) | `echo-agent-app-core/skills_hub` | 中 |
| ② 上游同步命令(sync/update/check-update) | `skills_hub/sync.rs` + Tauri IPC | 中 |
| ③ 前端按 category 分组展示 + baseline 标记 | `web-frontend/components/skills` | 高 |
| ④ 技能详情面板(依赖/来源/版本/触发词) | `SkillsPanel.tsx` | 中 |
| ⑤ runtime bootstrap 接线(baseline + supervisor) | `runtime.rs` | 高 |

### 4.2 SkillsHub 上游同步

#### 现状

`SkillsHub`(`echo-agent-app-core/src/skills_hub/registry.rs`)是纯数据索引,扫描 `~/.echo-agent/skills/`。`SkillHubEntry` 已有 `version/author/license/compatibility` 字段,但**没有 git remote、没有更新机制**。现有 `install.rs` 有 `install_from_git`(含 SSRF 防护),但只装一次,不支持更新检测。

#### 设计:upstream registry

新增 `~/.echo-agent/skills/.upstream-registry.json`,记录每个**非内置**(非随产品分发)技能的来源:

```json
{
  "version": 1,
  "registries": [
    {
      "name": "superpowers",
      "git_url": "https://github.com/obra/superpowers",
      "local_clone": "~/.echo-agent/skill-sources/superpowers",
      "installed_skills": ["brainstorming", "systematic-debugging"],
      "last_synced": "2026-06-23T10:00:00Z",
      "upstream_ref": "v5.1.0"
    }
  ]
}
```

**关键区分**:
- **内置技能**(`echo-agent-cli/skills/<category>/`):随产品分发、进版本控制,**不进 upstream registry**,不参与 sync。它们更新靠产品版本升级。
- **用户安装的技能**(`~/.echo-agent/skills/`):通过 SkillsHub 从上游装,进 upstream registry,可 sync。

#### 同步命令(Tauri IPC + CLI)

新增三个命令,走现有 SSRF 防护框架(`install.rs` 的 `validate_git_url`):

| 命令 | 行为 | 改动 |
|---|---|---|
| `check_skill_updates` | 对 registry 里每个源 `git fetch` + 比对 ref,返回有更新的技能列表 | 新增 `sync.rs::check_updates` |
| `sync_skill_registry` | `git pull` 指定源,把更新的 SKILL.md/scripts 复制到 `~/.echo-agent/skills/`,更新 `upstream_ref` | 新增 `sync.rs::sync_registry` |
| `install_skill_bundle` | 一键装一个 category 的所有技能(从内置 `skills/<category>/` 拷贝到用户目录,启用) | 新增 `install.rs::install_bundle` |

**安全边界**(遵循 AGENTS.md):
- git URL 走现有 `validate_git_url`(只允许 https、拒绝私网 IP)——这是对**明文 http/拼错 URL 的轻量校验**,符合"用户自扩展能力由用户负责,保留对明显错误输入的校验"。
- sync 操作是**用户主动触发**(UI 按钮或 CLI 命令),不自动后台拉取(避免无感知的远程变更)。
- sync 前提示"将更新 N 个技能,可能有行为变化",用户确认。

#### 内置技能的"启用"机制

内置技能(`echo-agent-cli/skills/`)不复制到用户目录。`enable_skill` 对内置技能的行为改为:**在独立的 `enabled-skills.json` 里登记启用状态 + baseline 标记**,bootstrap 时根据登记决定加载哪些内置技能目录、哪些走 baseline 注入。

```json
// ~/.echo-agent/enabled-skills.json
{
  "version": 1,
  "skills": {
    "brainstorming":            { "category": "methodology", "enabled": true,  "baseline": true  },
    "systematic-debugging":     { "category": "methodology", "enabled": true,  "baseline": true  },
    "verification-before-completion": { "category": "methodology", "enabled": true, "baseline": true },
    "writing-plans":            { "category": "methodology", "enabled": true,  "baseline": true  },
    "test-driven-development":  { "category": "methodology", "enabled": false, "baseline": false },
    "docx":                     { "category": "document",    "enabled": false, "baseline": false },
    "pdf":                      { "category": "document",    "enabled": false, "baseline": false }
  }
}
```

**字段语义**(消除歧义):
- `enabled`:技能是否加载进 agent(进 catalog 或 baseline)。**默认 false**(避免 40+ 技能全加载导致 prompt 膨胀),用户在前端开启。
- `baseline`:仅对 `enabled: true` 的 methodology 技能有效。`true` = 正文直接注入 system prompt;`false` = 走 catalog(模型按需 activate)。
- **首次启动的默认值**:4 个核心方法论(brainstorming/systematic-debugging/verification-before-completion/writing-plans)`enabled: true, baseline: true`;其余全部 `enabled: false`。文件不存在时按此默认值生成。
- 文件不存在时,bootstrap 用上述默认值生成它(而非依赖隐式逻辑),保证"默认全关 + 4 个 baseline 开"是**显式落盘**的状态,可被用户查看和修改。

> 非 methodology 技能的 `baseline` 字段恒为 false 且前端不可改(只有 methodology 技能能进 baseline,因为只有它们是"始终在场的思维框架")。

### 4.3 前端分类展示

#### 现状

`SkillsPanel.tsx` 是平铺列表 + 搜索。现有 `list_skills` 返回的 `SkillInfo` 有 `name/description/enabled/tool_names/source`,**没有 category 字段**。

#### 设计:SkillInfo 扩展 + 分组渲染

**后端**(`SkillInfo` 加字段,ts-rs 自动同步到前端):

```rust
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub tool_names: Vec<String>,
    pub source: SkillSource,
    pub category: SkillCategory,         // ★ 新增
    pub is_baseline: bool,               // ★ 新增(方法论 baseline 标记)
    pub is_builtin: bool,                // ★ 新增(内置 vs 用户安装)
    pub upstream_version: Option<String>,// ★ 新增
    pub has_updates: bool,               // ★ 新增(sync 后置位)
    pub missing_dependencies: Vec<String>,// ★ 新增(探测结果)
}

pub enum SkillCategory { Methodology, Development, Document, Design, Research, Automation }
```

**前端**(`SkillsPanel.tsx` 重构):

```
┌─────────────────────────────────────────────────────┐
│ 技能                          [检查更新] [从目录加载] │
├─────────────────────────────────────────────────────┤
│ 🔍 搜索技能...                                       │
├─────────────────────────────────────────────────────┤
│ ▼ 方法论 (3/9)                    [全部启用 baseline]│
│   ┌─────────────────────────────────────────────┐  │
│   │ ★ brainstorming              [baseline] [ON]│  │
│   │   探索用户意图、需求和设计                    │  │
│   │   superpowers · v5.1.0                      │  │
│   ├─────────────────────────────────────────────┤  │
│   │ ☆ systematic-debugging       [baseline] [ON]│  │
│   │   系统化调试方法论                           │  │
│   ├─────────────────────────────────────────────┤  │
│   │ ○ receiving-code-review              [OFF]  │  │
│   └─────────────────────────────────────────────┘  │
│ ▼ 文档 (2/4)                                        │
│   ┌─────────────────────────────────────────────┐  │
│   │ 📄 docx                          [⚠️缺依赖] │  │
│   │   Word 文档创建/编辑                          │  │
│   │   anthropic · v1.0 · 需: soffice           │  │
│   │                                    [启用]    │  │
│   └─────────────────────────────────────────────┘  │
│ ▶ 开发 (0/8)   ▶ 设计 (0/7)  ▶ 研究 (4/10)         │
└─────────────────────────────────────────────────────┘
```

**视觉编码**:
- ★ baseline 方法论(已默认挂载) / ☆ baseline 但未启用 / ○ 普通
- [baseline] 标签 + [ON/OFF] 开关
- ⚠️ 缺依赖标记,点击显示依赖清单和安装提示
- 折叠/展开分组,显示 `(已启用/总数)`

#### 技能详情面板

点击技能展开详情(inline 或侧抽屉):

```
┌─ brainstorming ────────────────────────────┐
│ category: 方法论  source: superpowers       │
│ version: 5.1.0    author: obra              │
│ tags: [design, planning, workflow]          │
│ triggers: [头脑风暴, 设计, brainstorm]       │
│ ─────────────────────────────────────────── │
│ 依赖: 无(纯文本方法论)                       │
│ 挂载方式: baseline(SessionStart 自动注入)    │
│ ─────────────────────────────────────────── │
│ [查看 SKILL.md 正文]  [查看触发测试]         │
└─────────────────────────────────────────────┘
```

### 4.4 runtime bootstrap 接线

`runtime.rs` 的 bootstrap 流程(`runtime.rs:60-71` 那 11 步)增补:

```
现有: create agent → load MCP → ... → load built-in skills → ...
改为: create agent
    → load built-in skills(读 enabled-skills.json,只加载启用的)
    → ★ inject methodology baseline(对启用的 methodology 技能注入正文)
    → ★ 装配 TriggerSupervisor 到 IntentRouter
    → load MCP
    → ... 其余不变
    → fire SessionStart hook(现在能触发方法论 baseline 检查清单)
```

**关键顺序**:baseline 注入必须在 catalog 注入之前(否则重复);TriggerSupervisor 装配在 IntentRouter 设置时。

### 4.5 不做的事

- ❌ 不做"技能市场在线浏览/评分"(那是商业功能,超出范围)
- ❌ 不自动后台 sync(用户主动触发)
- ❌ 不做技能间依赖图可视化(depends_on 字段保留但只做加载顺序,不画图)
- ❌ 内置技能不复制到用户目录(只在 enabled-skills.json 登记)

### 4.6 验证标准

- SkillsHub 能区分内置/用户安装技能,前端分组正确
- `check_skill_updates` 对有 upstream registry 的源返回更新列表
- 启用一个 docx 技能,前端显示 ⚠️ 缺 soffice(若未装),装了则正常
- methodology 技能启用后,系统 prompt 含 baseline 正文
- bootstrap 日志显示 baseline 注入 + TriggerSupervisor 装配

---

## 5. eval 触发测试闭环

### 5.1 现状问题

- `echo-agent-eval` 的 `trigger-test` 已实现,但 **case 数 38 ≠ 计划的 50**。
- **致命缺陷**:`main.rs:225` 的 match_fn 是 `lower.contains(trigger)` 字符串包含,**不是真 ChainedClassifier**——F1 度量失真,不能反映生产路由效果。
- 只覆盖现有 11 个技能,没有覆盖即将移植的 40+ 技能。

### 5.2 设计

#### 5.2.1 match_fn 修真

`echo-agent-eval/main.rs` 的 trigger-test 改为调用真 `ChainedClassifier`(或新的 `TriggerSupervisor`),让 F1 反映生产效果。

```rust
// main.rs trigger-test 子命令
let supervisor = TriggerSupervisor::new(keyword_clf, llm_clf, hook_source);
let match_fn = |input: &str| -> Option<String> {
    // 同步包装(避免 async 复杂度,或加 runtime block_on)
    supervisor.classify_sync(input, &context)
};
run_trigger_test(match_fn, cases_dir, threshold)?;
```

#### 5.2.2 case 扩展到 50+

按 category 补 case,覆盖移植的新技能。case 结构沿用现有 `eval/cases/skill-trigger/001_trigger_batch.yaml` 格式:

```yaml
- id: docx_001
  input: "帮我把这份 markdown 转成 Word 文档,加上修订标记"
  expected: docx            # 正向:应触发
  category: document
- id: docx_neg_001
  input: "什么是 docx 格式的技术原理"
  expected: none            # 反向:讨论格式 ≠ 创建文档,不触发
  category: document
  note: boundary            # 边界 case,不计入 F1
```

**case 分布目标**(50+):
- methodology: 8(brainstorm/debug/verify/plan 等的触发 + 反向)
- document: 10(docx/pdf/pptx/xlsx 各 2)
- design: 8(canvas/brand/theme/frontend 等)
- research: 8(现有 + claude-api)
- development: 8(git-worktree/subagent 等)
- automation: 4(mcp-builder/webapp-testing)
- boundary/反向: 4+

#### 5.2.3 CI 门控

`HARNESS_PLAN.md` 提到的 "F1 < 0.85 失败" 落地。`trigger-test --threshold 0.85` 返回非零退出码。

#### 5.2.4 per-category 报告

`trigger_test.rs` 已有 per-skill breakdown,扩展为 per-category,方便定位哪类技能触发不准。

---

## 6. 文档交付

| 文档 | 位置 | 内容 |
|---|---|---|
| 技能分类总览 | `echo-agent-cli/docs/skills-taxonomy.md` | 6 个 category 定义、40+ 技能清单、触发策略矩阵 |
| 技能编写指南(增强版) | `echo-agent/docs/{en,zh}/skill-authoring.md` | 补 category/PEP 723/requires-binaries/hooks.json 字段说明 |
| 上游同步指南 | `echo-agent-cli/docs/skill-sync.md` | 如何添加上游源、sync 命令、SSRF 约束 |
| 系统深度文档更新 | `echo-agent-cli/docs/system-deep-dive/06-skills.md` | 更新 baseline 注入、TriggerSupervisor、HookAction::ActivateSkill 章节 |
| CHANGELOG | 两仓库各自 | 记录本次集成 |

---

## 7. 风险与缓解

| 风险 | 影响 | 缓解 |
|---|---|---|
| **R1: system prompt 膨胀** | methodology baseline 注入正文,token 占用增大,影响上下文窗口和成本 | baseline 资格严格筛选(只核心 4-5 个);其余走 catalog;监控注入后的 prompt size |
| **R2: uv 未安装时体验降级** | 用户机器无 uv,Anthropic python 脚本依赖装不上 → 技能失败 | 探测层提示"建议安装 uv: `curl -LsSf https://astral.sh/uv/install.sh \| sh`";无 uv 时回退裸 python3(标准库脚本仍可用) |
| **R3: 三重触发误激活** | 低置信度场景误激活技能,打断正常对话 | 阈值门控(confidence ≥ 0.7);误激活可在 UI 关闭;第三重只注入提示不强制(除非 ActivateSkill 高确定性) |
| **R4: 移植工作量大** | 40+ 技能手工适配,Tier D/E 脚本多 | writing-plans 按 Tier A-F 拆 phase,每 phase 独立可验证;Tier A 纯收益先交付 |
| **R5: 跨仓库依赖顺序** | echo-agent 加了 HookAction 变体/SkillInfo 字段,echo-agent-cli 必须同步用 | 先合并 echo-agent,再合并 echo-agent-cli(遵循 AGENTS.md worktree 规范第 6 条) |
| **R6: hooks.json 平台变量** | superpowers 的 session-start 脚本依赖 `CLAUDE_PLUGIN_ROOT` 等宿主变量 | EKO 注入 `EKO_SKILL_ROOT` 等价变量;loader 兼容读取 hooks.json |
| **R7: 默认全关导致体验空** | 新用户启用 EKO 发现没技能可用 | methodology baseline 核心几个默认开;首次启动引导"已为你启用 X 个方法论技能,在技能面板探索更多" |

---

## 8. 整体验收标准(Definition of Done)

本次集成完成的标志,全部满足才算交付。

### 8.1 功能验收

- [ ] 40+ 技能全部移植到 `echo-agent-cli/skills/<category>/`,带正确的 frontmatter(category/source/upstream-version)
- [ ] methodology baseline 核心技能启用后,其正文出现在 system prompt
- [ ] TriggerSupervisor 装配,关键词/LLM/hook 三源融合生效
- [ ] UserPromptSubmit hook 每条消息触发
- [ ] HookAction::ActivateSkill 能直接激活技能
- [ ] docx 技能在装了 uv+LibreOffice 的机器上完整工作(创建/编辑/修订)
- [ ] pdf 技能的 python 脚本靠 PEP 723 + uv run 自动装依赖跑通
- [ ] SkillsHub 区分内置/用户安装,前端按 category 分组
- [ ] `check_skill_updates` + `sync_skill_registry` 命令工作
- [ ] `enable_skill`/`disable_skill` 对内置技能走 enabled-skills.json

### 8.2 质量验收(遵循 AGENTS.md 提交规范)

- [ ] `trigger-test --threshold 0.85` 通过(50+ case,F1 ≥ 0.85)
- [ ] 两个子仓库 `cargo check --workspace` + 全 feature 矩阵零错误
- [ ] `cargo test --workspace` 全绿
- [ ] `cargo clippy --all-targets -- -D warnings` 零警告
- [ ] `cargo fmt --all` 通过
- [ ] 前端 `npx tsc -b` + `npm run build` 零错误

### 8.3 安全验收(遵循 AGENTS.md 本地定位)

- [ ] SkillSandboxPolicy 声明在沙箱下生效;本地默认 local_only 不强制 docker
- [ ] git sync 走 SSRF 校验(只 https、拒私网)
- [ ] 二进制/系统依赖只探测提示,不自动安装
- [ ] 无对"用户主动操作"(终端/文件选择器)加 permission_mode 门控

---

## 9. Phase 切分预告(交付给 writing-plans)

spec 写完后,writing-plans 阶段会拆成大致这些 phase(每 phase 独立可验证、可合并):

| Phase | 内容 | 依赖 |
|---|---|---|
| **P1 框架底座** | HookAction::ActivateSkill、HookResult 扩展、UserPromptSubmit 触发点、hooks.json 发现 | 无 |
| **P2 脚本运行时** | uv 优先解析、minimal_env +HOME、SkillSandboxPolicy 接线、默认 sandbox 装配、二进制探测 | P1 |
| **P3 资产 Tier A** | 移植 9 个纯文本方法论 + baseline 注入机制 + enabled-skills.json | P2 |
| **P4 TriggerSupervisor** | 三源融合 + IntentRouter 接线 + 方法论 baseline 接线 | P1, P3 |
| **P5 资产 Tier D** | docx/pdf/pptx/xlsx 移植 + PEP 723 头 + office/ 去重 | P2 |
| **P6 前端 + SkillsHub** | category 分组 UI + upstream registry + sync 命令 + 详情面板 | P3, P4 |
| **P7 资产 Tier B/C/E/F** | 剩余技能移植(开发/设计/研究/自动化) | P2 |
| **P8 eval + 文档** | case 扩展到 50+、match_fn 修真、5 份文档 | P4, P7 |

---

## 附录:关键代码位置索引(供 writing-plans 和实现阶段直接引用)

### 框架层(echo-agent)
- `Skill` trait:`echo-core/src/tools/skill.rs:28-56`
- `minimal_env`:`echo-core/src/tools/skill.rs:103`(白名单,需 +HOME)
- `SkillSandboxPolicy`:`echo-execution/src/skills/external/types.rs:26`
- `SkillRegistry`:`echo-execution/src/skills/registry.rs:35-58`(需加 baseline 注入方法)
- `run_skill_script` 执行 + `resolve_interpreter`:`echo-execution/src/skills/external/run_script_tool.rs:129, 328`(需 uv 优先 + sandbox 接线)
- `HookAction` enum:`echo-execution/src/skills/hooks.rs:128-186`(需 +ActivateSkill)
- `HookResult` + `HookRegistry`:`echo-execution/src/skills/hooks.rs`(需 +activate_skill 字段)
- `execute_command_hook`(变量替换 `${CLAUDE_PLUGIN_ROOT}`):`hooks.rs:828-973`
- `SkillLoader`(只读 SKILL.md):`echo-execution/src/skills/external/loader.rs:136-193`(需 +hooks.json 发现)
- `HookEvent` 枚举:`echo-core/src/hooks/types.rs:50-136`
- `ReactAgentBuilder`(默认 sandbox None):`echo-agent/src/agent/react/builder.rs:129`(需默认 local_only)
- `SandboxManager::local_only`:`echo-execution/src/sandbox/manager.rs:118`
- IntentRouter/Classifier:`echo-agent/src/intent/{mod.rs, classifier.rs}`(`IntentClassifier` trait,TriggerSupervisor 实现它)
- `set_intent_router`:`echo-agent/src/agent/react/mod.rs:1300`
- 渐进披露 + `activate_skill_for_context`:`echo-agent/src/agent/react/capabilities.rs:504-707`
- `fire_lifecycle_hook`(SessionStart/UserPromptSubmit):`echo-agent/src/agent/react/run/context.rs:317-467`

### 应用层(echo-agent-cli)
- bootstrap 流程(11 步):`echo-agent-app-core/src/runtime.rs:60-71, 145-158`
- `SkillsHub`:`echo-agent-app-core/src/skills_hub/registry.rs`(需 +upstream registry)
- `install_from_git` + SSRF 防护:`echo-agent-app-core/src/skills_hub/install.rs`(需 +sync 命令)
- Tauri Skills 命令:`src/tauri/commands/panels.rs:448-623`(需 +sync/bundle 命令)
- `SkillsPanel.tsx`:`web-frontend/src/components/skills/SkillsPanel.tsx`(需 +category 分组)
- `SkillInfo`(ts-rs 生成):`web-frontend/src/generated/SkillInfo.ts`(需 +category 等字段)
- `skillsApi`:`web-frontend/src/api/endpoints.ts:187`(需 +sync 端点)
- eval trigger-test:`echo-agent-eval/src/{main.rs:215-237, trigger_test.rs}`(需 match_fn 修真 + case 扩展)
- eval cases:`eval/cases/skill-trigger/001_trigger_batch.yaml`(38 条,需扩到 50+)
- 内置技能目录:`echo-agent-cli/skills/`(11 个,需按 category 重组 + 新增 29 个)
