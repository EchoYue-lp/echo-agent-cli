# 06 · Skill 系统

> **归属**：横跨框架（`Skill` trait + `SkillRegistry` + 三个内置 skill 工具）与产品（`SkillsHub` marketplace + 内置 11 个 file-based skill）。
> **接口**：`ToolExecutionSubsystem.skill_registry` 是运行时入口；产品 bootstrap（`AgentRuntime::new`）调用 `discover_skills` 把内置 skill 装入；`SkillsHub` 是与 `SkillRegistry` **正交**的另一个组件。

本文剖析 Skill 系统的现状：trait + registry 的边界、三级渐进披露、SKILL.md frontmatter 的全集字段（含 legacy 与当前）、**两条激活路径产物的不对称**（重点）、`SkillsHub` 与 `SkillRegistry` 的关键区别、内置 11 个 skill 一览、工具集成与 `allowed-tools` 白名单语义、`skill_telemetry` 的当前状态。

---

## §1 `Skill` trait + `SkillRegistry`

### §1.1 Trait 定义

```rust,ignore
// echo-agent/echo-core/src/tools/skill.rs:28
pub trait Skill: Send + Sync {
    fn name(&self)        -> &str;
    fn description(&self) -> &str;
    fn tools(&self)       -> Vec<Box<dyn Tool>>;
    fn tools_with_sandbox(&self, sandbox: Option<Arc<SandboxManager>>)
                          -> Vec<Box<dyn Tool>>;
    fn system_prompt_injection(&self) -> Option<String>;
    fn shutdown(&self) -> BoxFuture<'_, ()> { /* default no-op */ }
}
```

`SkillInfo` (`skill.rs:60`)：`{name, description, tool_names, has_prompt_injection}`。

### §1.2 `SkillRegistry`（不再叫 SkillManager）

```rust,ignore
// echo-agent/echo-execution/src/skills/registry.rs:35
pub struct SkillRegistry {
    descriptors:              HashMap<String, SkillDescriptor>,    // file-based Tier-1
    legacy_instructions:      HashMap<String, String>,             // 老式纯文本 fallback
    activated:                Mutex<HashSet<String>>,              // 已激活 dedup
    code_skills:              HashMap<String, SkillInfo>,          // code-based eager 注册
    session_id:               Option<String>,
    sandbox:                  Option<Arc<SandboxManager>>,
    active_sandbox_policies:  HashMap<String, SandboxPolicy>,
}
pub type SharedRegistry = Arc<RwLock<SkillRegistry>>;       // L469-474
```

> **没有 `SkillManager` 类型** —— 历史命名已经统一为 `SkillRegistry`。

---

## §2 三级渐进披露

核心设计原则：不一次性加载所有内容，按需逐层展开。

| 层级 | 内容 | 触发 | Token 成本 |
|------|------|------|-----------|
| **Tier 1: Catalog** | name + description + 标注（path/triggers/depends） | 启动时 `discover_skills` 自动 | ~50–100 / skill |
| **Tier 2: Activation** | 完整 SKILL.md 指令 + 资源清单 | LLM 调 `activate_skill` 或 IntentRouter 命中 | <5000 / skill |
| **Tier 3a: Resources** | 引用文件内容 | LLM 调 `read_skill_resource` | 按需 |
| **Tier 3b: Scripts** | 脚本执行（py/sh/ts/ps1/rb 等） | LLM 调 `run_skill_script` | 按需 |

### §2.1 Catalog 文本

```rust,ignore
// echo-agent/echo-execution/src/skills/registry.rs:126
pub fn catalog_prompt(&self) -> Option<String> {
    if self.descriptors.is_empty() { return None; }
    let mut lines = vec![
        "## Available skills".to_string(),
        "When a task matches a skill's description, call the `activate_skill` tool with the skill's name to load its full instructions.".to_string(),
    ];
    /* 排序的 descriptor.catalog_line() */
}
```

每条 catalog line 由 `SkillDescriptor::catalog_line()`（`echo-execution/src/skills/external/types.rs:146-165`）生成，格式：

```
- {name}: {description} [activates for: ...; triggers: ...; depends: ...]
```

注入点：[`discover_skills`](#§7-加载链路) 把这段 prompt **追加**到 `config.system_prompt`，并通过 `ContextManager::update_system` 立即生效。

### §2.2 Activation 工具

```rust,ignore
// echo-agent/echo-execution/src/skills/external/activate_tool.rs:25
pub struct ActivateSkillTool {
    registry: SharedRegistry,
}
```

工具名 `"activate_skill"`，参数 `{name, arguments?, context_path?}`。

激活内部步骤（`registry.rs:236-325`）：
1. 递归激活 `depends_on`
2. 读 `descriptor.location` 的 SKILL.md
3. `extract_body` 剥离 frontmatter（`registry.rs:479-500`）
4. 若正文为空，回退 `legacy_instructions`
5. `process_skill_content` 做 `${SKILL_DIR}` / `${SESSION_ID}` / `${ARGUMENTS}` / `${1..}` 变量替换 + 内联命令执行（`` `!`cmd` `` 与 ```` ```! cmd ``` ````）
6. `enumerate_resources(skill_dir)` 枚举 `scripts/`、`references/`、`assets/` + 顶层非 SKILL 的 md/txt/yaml/json
7. 标记 activated，记录 sandbox policy

### §2.3 Resources 工具

```rust,ignore
// echo-execution/src/skills/external/resource_tool.rs:26
pub struct ReadSkillResourceTool { /* L26-206 */ }
```

工具名 `"read_skill_resource"`。校验：
- skill 必须已激活
- 路径不能含 `..`
- canonicalize 后必须仍在 skill 目录下
- 大小 ≤ 1 MB（默认）
- 通过 `descriptor.permits_tool(self.name())` 检查 allowed-tools 白名单

### §2.4 Scripts 工具

```rust,ignore
// echo-execution/src/skills/external/run_script_tool.rs:61
pub struct RunSkillScriptTool { /* L62-315 */ }
```

工具名 `"run_skill_script"`，risk 标记 `Dangerous`。Cross-platform 解释器表（`L328-347`）：

| 扩展 | Unix | Windows |
|------|------|---------|
| `.py` | `python3` | `python` / `py -3` |
| `.js` | `node` | `node` |
| `.ts` | `bun` → `deno` → `npx tsx` | 同左 |
| `.sh` | `bash` | Git Bash → PowerShell fallback |
| `.ps1` | `pwsh` | `powershell` |
| `.rb` | `ruby` | `ruby` |

挂了 `SandboxManager` 走沙箱；否则直接 `tokio::process::Command::env_clear() + minimal_env`（仅 `PATH`、`SKILL_DIR`、`SESSION_ID`）。

---

## §3 SKILL.md frontmatter 全集

定义在 `echo-agent/echo-execution/src/skills/external/types.rs`（`SkillDescriptor` L75，`RawFrontmatter` L415）。

### §3.1 当前字段

| 字段 | 必需 | 说明 |
|------|------|------|
| `name` | 是 | kebab-case，1–64 字符，唯一 |
| `description` | 是 | ≤1024 字符，告诉 LLM 何时启用 |
| `license` | | SPDX license id |
| `compatibility` | | 自由格式（agent 版本 / OS） |
| `metadata` | | 任意键值 |
| `shell` | | 内联命令默认 shell：`bash`（默认）或 `powershell` |
| `paths` | | 条件激活的 glob（`["*.py"]`），同时进 catalog 标注 |
| `triggers` | | 用户语句关键词，由 `KeywordClassifier` 消费 |
| `allowed-tools`（别名 `allowed_tools`） | | 已注册工具的白名单（详见 §8） |
| `depends_on` | | 自动先激活的其他 skill；DFS 检测循环 + warn (`loader.rs:387-446`) |
| `hooks` | | 31 个主 Hook 事件中的任意事件规则；Action 共 `command` / `prompt` / `permission` / `http` / `mcp_tool` / `agent` / `activate_skill` 7 类 |
| `sandbox` | | 单 skill 沙箱策略：`isolation`/`network`/`allowed_paths`/`denied_paths`/`timeout` |

Hook stdout 的规范控制字段是 `updatedInput`、`injected_context` 和
`permission_mode_override`；`modified_input`、`message`、`permission_mode` 不是别名。
插件拥有的 Skill 在解析 frontmatter 前先对完整 `SKILL.md` 应用
`PluginVariables`，所以 `${user_config.KEY}` 等变量在正文和 frontmatter Hook Action
中都生效。事件、matcher 和返回契约的权威说明见
`echo-agent/docs/{en,zh}/23-hooks.md`。

### §3.2 Legacy 字段（仍解析但 deprecation warn）

`loader.rs:279-285` 检测到这些字段时发 warning：

| Legacy 字段 | 替代方案 |
|------------|---------|
| `version` / `author` / `tags` | 移到 `metadata:` |
| `instructions` | 直接放在 `---` 之后的 Markdown 正文 |
| `resources` | 移除：自动枚举 `references/`/`scripts/`/`assets/` |

历史细节：早期 echo-agent 自己定义过这些字段，后来对齐 [agentskills.io](https://agentskills.io/specification) 标准。文档：`echo-agent/docs/{en,zh}/07-skills.md`、`skill-authoring.md`。

---

## §4 ⚠️ 两条 Skill 激活路径

> **重点**：两条路径产生**不同类型**的消息，且**只有一条**受压缩保护。

### §4.1 路径 1：LLM 通过 `activate_skill` 工具激活

LLM 在 ReAct 循环中决定调用 `activate_skill` 工具。工具成功结果由 `ActivateContent::to_prompt_block` (`echo-execution/src/skills/external/types.rs:334-372`) 包装为 XML 信封：

```xml
<skill_content name="paper-search">
{instructions}

Skill directory: ...
<allowed_tools>...</allowed_tools>
<skill_resources>
  <file kind="reference">references/...</file>
</skill_resources>
</skill_content>
```

该内容作为 `Role::Tool` 消息进入消息流。它含子串 `"<skill_content"`，已注册为 [`protected_marker`](./05-compression.md#§4-protected_markers-机制)（`agent/react/capabilities.rs:589-597`）。

→ **会被压缩绕过**（`ContextManager::is_protected` 命中），并在压缩后通过 `merge_protected` 重新插入回原位。

### §4.2 路径 2：IntentRouter 预分类激活

非流式入口的 `IntentRouter` 命中 `Intent::SkillRequired` 时：

```rust,ignore
// echo-agent/src/agent/react/run/react_loop.rs:738-764
if registry.is_installed(&skill_name) && !registry.is_activated(&skill_name) {
    match registry.activate(&skill_name).await {
        Ok(content) => {
            self.memory.context.lock().await
                .push(Message::system(content.instructions));    // ← 关键
        }
        // ...
    }
}
```

它把 `content.instructions`（**裸文本，无 XML 包装**）作为 `Role::System` 消息推入 context。

→ ⚠️ **不含 `<skill_content` 子串，因此不受 protected_marker 保护，可被压缩淘汰**。

### §4.3 ⚠️ 影响

| 路径 | Role | 包装 | 受保护？ |
|------|------|------|---------|
| LLM 工具激活 | `Tool` | `<skill_content>` 包装 | **是** |
| IntentRouter 激活 | `System` | 裸 `content.instructions` | **否** |

**用例对比**：

- GUI/TUI 流式对话只走路径 1（流式入口跳过 IntentRouter，详见 [01-runtime.md §7](./01-runtime.md#§7-intentrouter仅在非流式入口生效)），所以 skill 内容受保护。
- CLI 一次性 `chat()`/`execute()` 走路径 2，skill 内容**不**受保护，长对话+压缩有可能丢失。

记录在 [07-cross-cutting.md §3](./07-cross-cutting.md#3-已知陷阱清单) 第 2 项。可能的后续：让路径 2 也走 `to_prompt_block()`，或注册新的 marker 兼容 raw injection。

### §4.4 `triggers` 来自哪里

`KeywordClassifier` 在 runtime 由产品层用每个 `SkillDescriptor.triggers` 填充：

```rust,ignore
// echo-agent-cli/echo-agent-app-core/src/runtime.rs:196-255 (大致区间)
let mut classifier = KeywordClassifier::new();
for desc in agent.skill_descriptors() {
    classifier.add_skill_keywords(&desc.name, &desc.triggers);
}
```

如果某 skill 的 `triggers` 字段为空（很多内置 skill 都没填），IntentRouter **永远不会**对它产出 `Intent::SkillRequired` —— 这意味着这些 skill 仅靠路径 1 触发。

---

## §5 `SkillsHub` —— 与 `SkillRegistry` 完全不同的概念

⚠️ **这是两个不同的事物。共存于代码库，但不要混淆。**

| 关注点 | `SkillRegistry`（框架） | `SkillsHub`（产品） |
|--------|-----------------------|--------------------|
| 文件 | `echo-agent/echo-execution/src/skills/registry.rs` | `echo-agent-cli/echo-agent-app-core/src/skills_hub/` |
| 用途 | Agent 实例运行时：discover → catalog → activate → resources | 本地 skill marketplace UI：浏览 / 安装 / 卸载 / 搜索 |
| 默认扫描路径 | `<project>/skills/`、`<project>/.agents/skills/`、`~/.agents/skills/` | **仅** `~/.echo-agent/skills/` |
| Frontmatter 解析 | 完整 `serde_yaml_ng`（`loader.rs`） | 手写 mini key-value 解析器 |
| 状态 | 已激活集合 / sandbox policies / code skills / session_id | 仅 `loaded_skills: Vec<String>` |
| 谁在用 | 每一轮 ReactAgent | CLI `/skills` 命令 |
| 安装/卸载 | 不负责安装，只负责运行时发现 | 提供：`install`（git clone https only）/ `uninstall` |

```rust,ignore
// echo-agent-cli/echo-agent-app-core/src/skills_hub/registry.rs:42
pub struct SkillsHub {
    root:           PathBuf,                                  // 默认 ~/.echo-agent/skills/
    entries:        HashMap<String, SkillHubEntry>,
    loaded_skills:  Vec<String>,
}
```

CLI 子命令在 `echo-agent-cli/src/cli/cmd_impls/skills.rs`：`list/ls`、`search/find`、`install`、`uninstall/remove/rm`、`info`、`refresh`。

⚠️ **每个 CLI 子命令各自 `SkillsHub::new()`**（`skills.rs:11, 28, ...`）—— 不共享 AppState 中的 hub 实例。如果某个命令更新了 hub 状态而其他命令仍持着旧实例，可能漂移。记录在 [07-cross-cutting.md §3](./07-cross-cutting.md#3-已知陷阱清单) 第 9 项。

`SkillsHub` **不**会把 skill 装载到 agent。内置 skill 由框架的 `discover_skills` 路径在启动时载入（详见 §7），与 hub **完全不交互**。

---

## §6 内置 11 个 file-based skills

发布在 `echo-agent-cli/skills/` 下：

| Skill | 一句话角色 |
|-------|----------|
| `coding` | 编程与软件工程：编写/调试/重构/审查代码 |
| `data-visualization` | 图表与可视化（柱状图/折线图/仪表盘） |
| `data-wrangling` | CSV/Excel/JSON 加载、清洗、EDA |
| `doc-writing` | 报告、技术文档、文章、邮件、提案 |
| `evidence-medicine` | 医学文献检索 + 循证分析（PubMed、ClinicalTrials、PICO、GRADE） |
| `git-workflow` | Git 操作（分支、提交、PR/MR、冲突） |
| `paper-reader` | 单篇论文的深度阅读与批判性评估 |
| `paper-search` | 跨 ArXiv + Semantic Scholar 的学术论文搜索（非医学） |
| `statistical-analysis` | 统计检验（t/χ²/ANOVA）、回归、建模 |
| `translation` | 翻译、校对、本地化 |
| `web-search` | 网络信息检索 + 多源交叉验证 |

每个 skill 目录至少有 `SKILL.md`，多数还带 `references/` 子目录。它们**不**经过 `SkillsHub` 安装流程，由 §7 的 bootstrap 路径直接加载。

---

## §7 加载链路（bootstrap → 用户对话）

| 步骤 | 文件:行 | 做什么 |
|------|---------|--------|
| 0. bootstrap 发现 | `echo-agent-cli/echo-agent-app-core/src/runtime.rs:133-153` | 编译期解析 `CARGO_MANIFEST_DIR` 拼接 `skills/` 路径，调 `agent.load_skills_from_dir(...)` |
| 0a. 装载 | `src/agent/react/capabilities.rs:611-617` | `load_skills_from_dir` 等价于 `discover_skills(&[DiscoveryScope::Custom(path)])` |
| 0b. 解析 SKILL.md | `echo-execution/src/skills/external/loader.rs:64-228` | 递归扫描，解析 frontmatter，构造 `SkillDescriptor` |
| 0c. 注册 catalog | `src/agent/react/capabilities.rs:519-534` | 把 `catalog_prompt()` 拼到 `config.system_prompt`，调 `ctx.update_system(...)` |
| 0d. 注册三个 skill 工具 | `capabilities.rs:577-587` | `replace_tool` 注册 `activate_skill`/`read_skill_resource`/`run_skill_script`，每次重新 discovery 都刷新内部 registry |
| 0e. 注册 protected marker | `capabilities.rs:589-597` | `try_lock` 后 `add_protected_marker("<skill_content")`（⚠️ 静默失败陷阱） |
| 0f. 构建 IntentRouter | `runtime.rs:196-255` | `KeywordClassifier::new()` + 每个 descriptor 的 `triggers` 注入 → `ChainedClassifier` → `IntentRouter`（threshold 0.7）→ `agent.set_intent_router(...)` |
| 1. 用户输入 | `react_loop.rs:708` 或 `stream_channel.rs:29` | 走非流式或流式入口 |
| 2. （非流式）IntentRouter | `react_loop.rs:725-728` | 路径 2 激活 |
| 3. ReAct 循环 | `run_core_loop` | 路径 1 激活由 LLM 触发 |
| 4. 工具白名单过滤 | `registry.rs:178-199` | 已激活 skills 的 `allowed-tools` 取并集，下游工具调用按此过滤 |

---

## §8 工具集成 + `allowed-tools` 白名单

### §8.1 `allowed-tools` 是过滤白名单

```yaml
allowed-tools:
  - read_skill_resource
  - run_skill_script
  - Bash
  - "Bash(git:*)"
  - "*"           # 通配
```

> **不**是工具注册列表 —— 工具早已通过 `add_tool` / `register_all_tools` 等独立路径注册。`allowed-tools` 只在某 skill 激活后**过滤**已存在的工具调用。

匹配语义（`types.rs:277-307`）：

- 精确名（`"read_skill_resource"`）
- 通配符 `"*"`（允许所有）
- 前缀-括号（`"Bash"` 匹配 `"Bash(git:status)"`）
- glob via `glob::Pattern`（`"Bash(git:*)"`）

`SkillRegistry::active_skill_allowed_tools`（`registry.rs:178-199`）取所有已激活 skills 白名单的**并集** —— 命中任一 skill 的允许列表即放行。

### §8.2 ToolManager 动态 API

```rust,ignore
// echo-agent/echo-execution/src/tools.rs:120-138
impl ToolManager {
    pub fn register(&self, tool: Box<dyn Tool>) { ... }
    pub fn register_tools(&self, tools: Vec<Box<dyn Tool>>) { ... }
    pub fn unregister(&self, name: &str) -> Option<Box<dyn Tool>> { ... }
    // L71-75
    definitions_version: AtomicU64,    // 用于 prefix-cache 稳定性
}
```

ReactAgent 包装层（`echo-agent/src/agent/react/capabilities.rs:31-71`）：

```rust,ignore
pub fn add_tool(&mut self, tool: Box<dyn Tool>);            // L31，自动 enable_tool=true
pub fn add_tools(&mut self, tools: ...);                    // L37
pub fn remove_tool(&mut self, name: &str) -> Option<Box<dyn Tool>>;  // L58
pub fn replace_tool(&mut self, tool: Box<dyn Tool>) -> Option<Box<dyn Tool>>;  // L66
```

`replace_tool` 是 skill discovery 用来刷新 `activate_skill`/`read_skill_resource`/`run_skill_script` 三个工具内部 registry 视图的关键 —— 第二次 `discover_skills` 时不需要 unregister + register，直接 replace。

`definitions_version: AtomicU64` 用于支撑 LLM provider 的 prefix-cache：tool definitions 版本不变就可以复用 prompt cache，注册新工具会 bump 版本号。

### §8.3 Code-based skill 注册工具

```rust,ignore
// echo-agent/src/agent/react/capabilities.rs:391-396
let tools = skill.tools_with_sandbox(sandbox);
for tool in tools {
    tool_manager.register(tool);
}
```

只有 **code-based** skill（实现 `Skill` trait）会通过 `add_skill` 注册工具。当前框架内仅有：

| Code-based skill | 文件 | feature |
|-----------------|------|---------|
| `FileSystemSkill` | `echo-execution/src/skills/builtin/filesystem.rs` | `files` |
| `ShellSkill` | `echo-execution/src/skills/builtin/shell.rs` | `shell` |

File-based skill **不**通过 `Skill` trait 注册工具 —— 它通过 `allowed-tools` 过滤已注册工具。

---

## §9 ⚠️ `skill_telemetry` —— 模块在但无写入点

```rust,ignore
// echo-agent/echo-state/src/skill_telemetry.rs (大致 200 行)
pub struct SkillExecutionRecord { /* :16-33 */
    pub skill_name, pub session_id, pub activated_at,
    pub duration_ms, pub tools_used, pub tool_calls_count,
    pub success, pub error_message,
}
pub struct SkillTelemetry { /* :37-56 */
    pub activation_count, pub success_count, pub failure_count,
    pub total_duration_ms,
    pub common_tools: HashMap<String, u64>,
    pub common_failures: Vec<FailurePattern>,
    pub first_used, pub last_used,
}
```

存储位置：`Store` trait，namespace `["agent", "skill_telemetry"]`，key = skill name（`L170, L186, L208`）。

**消费方**：CLI `evolution` 子命令（`echo-agent-cli/src/cli/cmd_impls/evolution.rs:7, 344`）—— `SkillTelemetryStore` 类型 + `print_skill_review` helper。

⚠️ **无 runtime 写入点**：grep `record_execution` 在整个 `echo-agent/` runtime 路径中**零命中**。schema + 类型 + 读取端都齐了，但生产侧（每次 skill 激活/工具调用记录一行）尚未接入激活路径。`/evolution` 命令读到的实际上是空的或来自外部种子数据。

记录在 [07-cross-cutting.md §3](./07-cross-cutting.md#3-已知陷阱清单) 第 5 项，待跟进。可能的后续：在 `SkillRegistry::activate` 末尾加一次 `record_execution`，或在 `phases/tools.rs` 工具批结束时按已激活 skill 维度归并。

---

## §10 已删除组件：`SkillGateway`

`SkillGateway` 是早期产品层的 skill 路由器，已**完全删除**（类型本身不存在；曾引用它的 echo-agent-eval crate 也已移除）。

它的职责拆给：

- `IntentRouter` + `KeywordClassifier`（关键词/语义路由，框架）
- `SkillRegistry`（激活和生命周期，框架）
- `SkillsHub`（用户可见的 marketplace UI，产品）

如果某些 audit/legacy 文档仍引用 `SkillGateway`，按历史遗留处理 —— 当前代码中无此 trait。

---

## §11 与其他文档的接口

- **激活路径产物为何只有一条受压缩保护** → [05-compression.md §4](./05-compression.md#§4-protected_markers-机制)
- **IntentRouter 仅在非流式入口生效** → [01-runtime.md §7](./01-runtime.md#§7-intentrouter仅在非流式入口生效)
- **`SubagentRegistry` 与 `SkillRegistry` 是两个不同的注册表** → [03-subagent.md §3](./03-subagent.md#§3-subagentregistry--lazy-factory--竞态保护)
- **既有 API 参考**（`Skill` trait 实现、SKILL.md 标准、`agentskills.io`） → `echo-agent/docs/{en,zh}/07-skills.md`、`skill-authoring.md`
