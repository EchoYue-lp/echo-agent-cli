# GUI 主窗口一条流重构 + agent_tool 框架解耦 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 主窗口回归「一条流」范式(对齐 Cursor/Codex/Claude Code 桌面版),删除 6 张割裂卡片;右侧栏精简为三块;修复 markdown 不渲染;框架解耦让 EKO 的 LLM 不再调 `agent_tool`。

**Architecture:** 分两条独立合并线——① `echo-agent` 框架层解耦(拆 `enable_subagent` 的两个职责,新增 `register_agent_dispatch_tool` flag);② `echo-agent-cli` 产品层(EKO 不注册 LLM 工具 + 前端 GUI 重构)。先合并①再合并②(AGENTS.md 跨仓库依赖顺序)。前端无测试框架,前端 task 用 `tsc -b` + `npm run build` + 手动验证;Rust task 用 `cargo test`。

**Tech Stack:** Rust(echo-agent 框架)/ React 19 + TypeScript + Zustand + Tailwind v4(echo-agent-cli 前端)/ Tauri(桌面壳)

**Spec:** `echo-agent-cli/docs/superpowers/specs/2026-06-22-gui-main-window-redesign.md`

---

## 执行顺序总览

```
Phase 1: echo-agent 框架解耦(Task 1-4)→ 合并到 echo-agent main
    ↓
Phase 2: echo-agent-cli 框架适配(Task 5)→ 依赖 Phase 1
    ↓
Phase 3: 前端 Markdown 修复(Task 6)→ 独立,可并行
Phase 4: 前端主窗口一条流重构(Task 7-11)→ 依赖 Task 6
Phase 5: 前端右侧栏重构(Task 12)→ 独立
Phase 6: 前端删除清单 + 清理(Task 13)→ 依赖 Task 7-12
    ↓
Phase 7: 全量验证 + 提交(Task 14)
```

---

## Phase 1: echo-agent 框架解耦

> 在 `echo-agent` 子仓库内完成。所有命令在 `/Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent` 下执行。

### Task 1: 在 AgentConfig 新增 `register_agent_dispatch_tool` 字段

**Files:**
- Modify: `echo-agent/src/agent/config.rs:60`(字段定义)
- Modify: `echo-agent/src/agent/config.rs:170`(默认值)
- Modify: `echo-agent/src/agent/config.rs:312-315`(setter 方法)

**目标:** 新增独立 flag 控制 `AgentDispatchTool` 注册,与 `enable_subagent`(worker 注册基础设施)解耦。

- [ ] **Step 1: 在 `AgentConfig` 结构体新增字段**

`echo-agent/src/agent/config.rs:60` 附近,在 `enable_subagent` 字段下方新增:

```rust
    pub(crate) enable_subagent: bool,
    /// Whether to register the `agent_tool` LLM-callable dispatch tool.
    ///
    /// Decoupled from `enable_subagent` (which controls the subagent registry
    /// infrastructure that `delegate_to_agent_with_parent_and_cancel` depends on).
    /// When `false`, the LLM cannot call `agent_tool`; subagent dispatch still
    /// works via the framework's `delegate_to_agent*` methods (TaskRuntime path A).
    /// Default: `false` (product layers should opt in explicitly only if they
    /// want the LLM-driven dispatch escape hatch).
    pub(crate) register_agent_dispatch_tool: bool,
```

- [ ] **Step 2: 在 `Default` 实现里设默认值 `false`**

`echo-agent/src/agent/config.rs:170` 附近,在 `enable_subagent: false,` 下方新增:

```rust
            enable_subagent: false,
            register_agent_dispatch_tool: false,
```

- [ ] **Step 3: 新增 setter 方法**

`echo-agent/src/agent/config.rs:312-315` 的 `enable_subagent` setter 后方新增:

```rust
    /// Set whether to register the `agent_tool` LLM-callable dispatch tool.
    ///
    /// Independent of `enable_subagent`. When `true`, the `AgentDispatchTool`
    /// is registered into the tool manager so the LLM can call `agent_tool`.
    /// When `false` (default), the LLM cannot call `agent_tool`, but the
    /// framework's `delegate_to_agent*` methods still work (used by TaskRuntime).
    pub fn register_agent_dispatch_tool(mut self, enabled: bool) -> Self {
        self.register_agent_dispatch_tool = enabled;
        self
    }
```

- [ ] **Step 4: 检查 `should_enable_subagent` 之类的派生访问点**

搜索是否有其他地方读 `config.enable_subagent` 做派生判断:

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent
grep -rn "\.enable_subagent" src/
```

预期:只有 `capabilities.rs:291`、`mod.rs:423`、`config.rs:367`(`should_*` 辅助,若有)几处。若 `config.rs:367` 是 `self.enable_subagent` 的 getter,不动它(它仍表示"subagent 基础设施启用")。

- [ ] **Step 5: 编译验证**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent
cargo check -p echo_agent --features subagent
```

预期:PASS(新字段有默认值,不破坏现有构造)。

- [ ] **Step 6: Commit**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent
git add src/agent/config.rs
git -c commit.gpgsign=false commit -m "feat(config): add register_agent_dispatch_tool flag decoupled from enable_subagent"
```

---

### Task 2: 在 ReactAgentBuilder 新增 builder 方法并传递到 config

**Files:**
- Modify: `echo-agent/src/agent/react/builder.rs:38`(字段定义)
- Modify: `echo-agent/src/agent/react/builder.rs:103`(默认值)
- Modify: `echo-agent/src/agent/react/builder.rs:282-285`(新增 builder 方法)
- Modify: `echo-agent/src/agent/react/builder.rs:715`(传递给 config)

- [ ] **Step 1: 在 builder 结构体新增字段**

`builder.rs:38` 附近,在 `enable_subagent: bool,` 下方新增:

```rust
    enable_subagent: bool,
    register_agent_dispatch_tool: bool,
```

- [ ] **Step 2: 在 `Default` 实现里设默认值 `false`**

`builder.rs:103` 附近,在 `enable_subagent: false,` 下方新增:

```rust
            enable_subagent: false,
            register_agent_dispatch_tool: false,
```

- [ ] **Step 3: 新增 builder 方法**

`builder.rs:282-285` 的 `enable_subagent` 方法后方新增:

```rust
    /// Enable sub-Agent dispatch
    pub fn enable_subagent(mut self) -> Self {
        self.enable_subagent = true;
        self
    }

    /// Register the `agent_tool` LLM-callable dispatch tool.
    ///
    /// Independent of `enable_subagent`. When called, the `AgentDispatchTool`
    /// is registered so the LLM can invoke `agent_tool`. When not called
    /// (default), the LLM cannot call `agent_tool`, but framework-level
    /// dispatch via `delegate_to_agent*` still works.
    pub fn register_agent_dispatch_tool(mut self) -> Self {
        self.register_agent_dispatch_tool = true;
        self
    }
```

- [ ] **Step 4: 在 `build()` 里传递给 config**

`builder.rs:715` 附近,在 `.enable_subagent(self.enable_subagent)` 下方新增一行:

```rust
            .enable_subagent(self.enable_subagent)
            .register_agent_dispatch_tool(self.register_agent_dispatch_tool)
```

- [ ] **Step 5: 编译验证**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent
cargo check -p echo_agent --features subagent
```

预期:PASS。

- [ ] **Step 6: Commit**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent
git add src/agent/react/builder.rs
git -c commit.gpgsign=false commit -m "feat(builder): add register_agent_dispatch_tool builder method"
```

---

### Task 3: 解耦 capabilities.rs 的 worker 注册守卫

**Files:**
- Modify: `echo-agent/src/agent/react/capabilities.rs:285-308`(`register_subagent_with_definition`)

**目标:** 让 worker 注册不再受 `enable_subagent` early return 守卫——路径 A(TaskRuntime `delegate_to_agent_with_parent_and_cancel`)依赖 worker 进 registry,守卫去掉后即便 `enable_subagent=false` 也能注册 worker。

- [ ] **Step 1: 改 `register_subagent_with_definition`**

`capabilities.rs:285-308` 当前:

```rust
    #[cfg(feature = "subagent")]
    pub fn register_subagent_with_definition(
        &mut self,
        def: SubagentDefinition,
        agent: Box<dyn Agent>,
    ) {
        if !self.config.enable_subagent {
            warn!(
                agent = %self.config.agent_name,
                subagent = %def.name,
                "subagent capability disabled, ignoring registration"
            );
            return;
        }
        let name = def.name.clone();
        if self
            .tools
            .subagent_registry
            .register_sync(def.clone(), agent)
        {
            self.update_dispatch_catalog(&def);
            info!(agent = %self.config.agent_name, subagent = %name, "Subagent registered");
        }
    }
```

改为(去掉 `enable_subagent` early return,改为只受 `#[cfg(feature = "subagent")]` 控制):

```rust
    #[cfg(feature = "subagent")]
    pub fn register_subagent_with_definition(
        &mut self,
        def: SubagentDefinition,
        agent: Box<dyn Agent>,
    ) {
        // Note: worker registration is decoupled from `enable_subagent` flag.
        // `enable_subagent` historically controlled two things: (1) worker
        // registration here, and (2) `AgentDispatchTool` LLM tool registration
        // in `ReactAgent::new`. They are now split: worker registration is
        // unconditional (framework dispatch via `delegate_to_agent*` depends on
        // it), and LLM tool registration is gated by `register_agent_dispatch_tool`.
        let name = def.name.clone();
        if self
            .tools
            .subagent_registry
            .register_sync(def.clone(), agent)
        {
            self.update_dispatch_catalog(&def);
            info!(agent = %self.config.agent_name, subagent = %name, "Subagent registered");
        }
    }
```

- [ ] **Step 2: 检查 `update_dispatch_catalog` 是否还依赖 `enable_subagent`**

`capabilities.rs:311-325` 的 `update_dispatch_catalog` 读的是 `self.dispatch_catalog_handle`(在 `mod.rs:441` 只有 `register_agent_dispatch_tool=true` 时才 `Some`)。当 `register_agent_dispatch_tool=false` 时 `dispatch_catalog_handle` 为 `None`,`update_dispatch_catalog` 的 `if let Some(handle)` 分支不会执行——这是安全的(no-op),不影响 worker 注册本身。

无需改动 `update_dispatch_catalog`,确认即可。

- [ ] **Step 3: 编译验证**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent
cargo check -p echo_agent --features subagent
```

预期:PASS。若有 `warn!(...)` 的 `warn` import 未使用警告,保留 import(其他地方可能用),或按 clippy 提示清理。

- [ ] **Step 4: Commit**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent
git add src/agent/react/capabilities.rs
git -c commit.gpgsign=false commit -m "refactor(capabilities): decouple worker registration from enable_subagent flag"
```

---

### Task 4: 解耦 mod.rs 的 AgentDispatchTool 注册守卫 + 全 feature 验证

**Files:**
- Modify: `echo-agent/src/agent/react/mod.rs:423`(`if config.enable_subagent` → `if config.register_agent_dispatch_tool`)

- [ ] **Step 1: 改工具注册守卫**

`mod.rs:423` 当前:

```rust
        #[cfg(feature = "subagent")]
        if config.enable_subagent {
```

改为:

```rust
        #[cfg(feature = "subagent")]
        if config.register_agent_dispatch_tool {
```

> 注:`config.enable_subagent` 仍保留(控制 worker 注册基础设施语义),但不再控制 LLM 工具注册。`subagent_registry` / `subagent_executor` 的构造(`mod.rs:348-372`)只受 `#[cfg(feature = "subagent")]` 控制,与运行时 flag 无关,不受影响。

- [ ] **Step 2: 写测试验证解耦生效**

在 `echo-agent/src/agent/react/tests.rs` 新增测试(若已有类似测试结构,参考 `tests.rs:880-886` 的 `agent_tool_registration_isolation`):

```rust
#[cfg(feature = "subagent")]
#[test]
fn worker_registers_without_agent_dispatch_tool() {
    // When enable_subagent=true but register_agent_dispatch_tool=false,
    // worker should still register, but agent_tool should NOT be in tools.
    use crate::agent::react::ReactAgentBuilder;
    use crate::agent::subagent::SubAgent;

    let agent = ReactAgentBuilder::new()
        .model("test-model")
        .name("test-parent")
        .system_prompt("test")
        .enable_tools()
        .enable_subagent()           // worker registry infrastructure on
        // .register_agent_dispatch_tool() // NOT called → LLM tool off
        .build()
        .expect("agent build");

    // Worker can register (decoupled from agent_tool registration)
    let worker = SubAgent::new("worker-1", "test-model", "worker prompt")
        .build()
        .expect("worker build");
    agent.register_agent(worker);

    // agent_tool NOT in tool definitions
    let tool_names: Vec<String> = agent
        .tool_definitions()
        .iter()
        .map(|d| d.function.name.clone())
        .collect();
    assert!(
        !tool_names.contains(&"agent_tool".to_string()),
        "agent_tool must NOT be registered when register_agent_dispatch_tool is false"
    );

    // But worker IS in registry (verify via delegate path or registry check)
    // If there's a public accessor for subagent_registry, use it; otherwise
    // verify via delegate_to_agent returning Ok for a registered worker.
    let worker_def = agent.list_subagents();
    assert!(
        worker_def.iter().any(|d| d.name == "worker-1"),
        "worker must be registered even when agent_tool LLM tool is off"
    );
}
```

> 注意:测试里用的 `SubAgent::new` / `register_agent` / `tool_definitions` / `list_subagents` 等 API 名称需与现有 `tests.rs` 里其他 subagent 测试保持一致。实现时先读 `tests.rs:697-886` 看现有 subagent 测试怎么构造 agent 和 worker,照抄 API 调用方式。若 `list_subagents` 不存在,改用 `delegate_to_agent` 尝试派发一个 no-op worker 验证不报"not found"。

- [ ] **Step 3: 运行测试验证失败(新测试应先失败,因为解耦还没完全生效)**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent
cargo test -p echo_agent --features subagent worker_registers_without_agent_dispatch_tool -- --nocapture
```

预期:若 Task 3 已生效,worker 注册部分应通过;若 `register_agent_dispatch_tool` 默认 false 已让 agent_tool 不注册,整个测试应 PASS。若失败,根据失败信息调整测试 API 调用。

- [ ] **Step 4: 更新现有 subagent 注册测试**

`tests.rs:817-823` / `828-875` 现有测试断言"enable_subagent 时 agent_tool 在 tool_names 里"——这些断言现在会失败(因为 agent_tool 注册改由 `register_agent_dispatch_tool` 控制)。修改这些测试:

- `tests.rs:817-823`(测 `enable_subagent` 时 agent_tool 注册):改为同时调 `.enable_subagent().register_agent_dispatch_tool()` 才断言 agent_tool 存在。
- `tests.rs:880-886`(测不 enable_subagent 时 agent_tool 不注册):保持不变(本来就不注册)。

具体:读 `tests.rs:810-890` 全文,凡是用 `.enable_subagent()` 构造 agent 并断言 `agent_tool` 在 tool_names 的,都补加 `.register_agent_dispatch_tool()`。

- [ ] **Step 5: 全 feature 矩阵验证**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent
cargo check --workspace
cargo test  --workspace
cargo check -p echo_agent --no-default-features --features sqlite
cargo check -p echo_agent --no-default-features --features subagent
cargo check -p echo_agent --no-default-features --features human-loop
cargo fmt --all
cargo clippy --all-targets -- -D warnings
```

预期:全部 PASS,零警告。若 clippy 报 `warn` import 未使用(因 Task 3 去掉了 early return),按提示清理。

- [ ] **Step 6: cargo clean + Commit + 合并到 main**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent
cargo clean
git add -A
git -c commit.gpgsign=false commit -m "refactor(react): gate AgentDispatchTool registration behind register_agent_dispatch_tool flag

Decouples LLM-callable agent_tool registration from worker registry
infrastructure. enable_subagent now only controls the registry/executor
construction; agent_tool LLM tool registration requires explicit
register_agent_dispatch_tool. TaskRuntime path A (delegate_to_agent*)
works without agent_tool."
```

若在 worktree 开发,合并前改回 `Cargo.toml` 相对路径,merge main,再 squash merge 到 main(AGENTS.md worktree 规范)。

---

## Phase 2: echo-agent-cli 框架适配

> 在 `echo-agent-cli` 子仓库内完成。依赖 Phase 1 已合并到 echo-agent main。所有命令在 `/Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli` 下执行。

### Task 5: EKO 不再注册 agent_tool(保留 worker 注册)

**Files:**
- Modify: `echo-agent-cli/echo-agent-app-core/src/infra.rs:172`

**目标:** EKO 保留 `.enable_subagent()`(worker 注册照常,路径 A 正常),但不调 `.register_agent_dispatch_tool()`(LLM 不再能调 agent_tool)。由于新 flag 默认 false,只需确认 `infra.rs:172` 不新增调用即可——实际改动是"什么都不加",但要验证。

- [ ] **Step 1: 确认 infra.rs 当前状态**

读 `echo-agent-cli/echo-agent-app-core/src/infra.rs:165-185`,确认当前是:

```rust
        .enable_tools()
        .enable_memory()
        .enable_planning()
        .enable_subagent()
        .enable_human_in_loop()
```

**不新增** `.register_agent_dispatch_tool()` 调用。新 flag 默认 false,所以 LLM 不会看到 agent_tool。

- [ ] **Step 2: 验证 Cargo.toml 指向已合并的 echo-agent**

若 Phase 1 在 worktree 开发,`echo-agent-cli/Cargo.toml` 的 `path` 可能指向 worktree 绝对路径。确认:

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli
grep -n "echo-agent\|worktrees" Cargo.toml echo-agent-app-core/Cargo.toml
```

预期:`path = "../echo-agent"`(相对路径)。若为 worktree 绝对路径,改回相对路径。

- [ ] **Step 3: 编译验证(GUI target)**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli
cargo check --no-default-features --features gui --bin echo-agent-tauri
```

预期:PASS。若失败说 `register_agent_dispatch_tool` 方法不存在,说明 Phase 1 未合并到 echo-agent main,需先完成 Phase 1 合并。

- [ ] **Step 4: 验证 agent_tool 不在工具列表 + 路径 A 正常**

这一步靠手动运行 EKO 验证(无单测覆盖 LLM 工具列表)。编译通过后,启动 EKO,触发一个 `parallel_readonly_delegation` 路由的任务(如"审查这个项目的代码结构"),观察:
- worker 正常派发并完成(路径 A 正常)。
- 主 agent 回答里没有 `agent_tool` 工具调用(LLM 看不到它)。

若路径 A 失败(worker not found),回查 Phase 1 Task 3 是否真的去掉了 early return。

- [ ] **Step 5: Commit**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli
git add -A
git -c commit.gpgsign=false commit -m "chore(eko): do not register agent_tool LLM tool (use TaskRuntime path A only)"
```

> 注:此 commit 暂不合并,等 Phase 3-6 前端改完一起合并。

---

## Phase 3: 前端 Markdown 修复(独立,可并行)

> 在 `echo-agent-cli/web-frontend` 内完成。所有命令在 `/Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli/web-frontend` 下执行。前端无测试框架,用 `tsc -b` + `npm run build` + 手动验证。

### Task 6: MarkdownContent 自带 md-content 类

**Files:**
- Modify: `echo-agent-cli/web-frontend/src/components/common/MarkdownContent.tsx:68-75`
- Modify: `echo-agent-cli/web-frontend/src/components/chat/MessageBubble.tsx:244`(去掉外层 md-content)

**目标:** 修复 spec §5。`MarkdownContent` 内部 div 默认带 `md-content` 类,所有调用点生效。

- [ ] **Step 1: 改 MarkdownContent.tsx**

`MarkdownContent.tsx:68-75` 当前:

```tsx
  return (
    <div
      ref={ref}
      className={className}
      style={containerStyle}
      dangerouslySetInnerHTML={{ __html: renderMarkdown(content) }}
    />
  );
```

改为:

```tsx
  return (
    <div
      ref={ref}
      className={`md-content ${className ?? ''}`.trim()}
      style={containerStyle}
      dangerouslySetInnerHTML={{ __html: renderMarkdown(content) }}
    />
  );
```

- [ ] **Step 2: 去掉 MessageBubble 外层重复的 md-content**

`MessageBubble.tsx:244` 附近,当前 assistant 消息正文外层 div:

```tsx
                  className={`text-sm leading-relaxed
                    ${
                      isUser
                        ? 'rounded-2xl bg-[var(--bg-user-msg)] px-4 py-2.5 text-[var(--text-user-msg)]'
                        : 'border-l-2 border-[var(--border-primary)] px-4 py-1 text-[var(--text-assistant-msg)] md-content'
                    }`}
```

去掉末尾的 `md-content`(现在由 MarkdownContent 自带):

```tsx
                  className={`text-sm leading-relaxed
                    ${
                      isUser
                        ? 'rounded-2xl bg-[var(--bg-user-msg)] px-4 py-2.5 text-[var(--text-user-msg)]'
                        : 'border-l-2 border-[var(--border-primary)] px-4 py-1 text-[var(--text-assistant-msg)]'
                    }`}
```

- [ ] **Step 3: 类型检查 + 构建**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli/web-frontend
npx tsc -b
npm run build
```

预期:零错误。

- [ ] **Step 4: 手动验证**

启动前端 `npm run dev`,发起一个会话,让 agent 回答含 markdown(标题/列表/代码块/表格)的问题。确认:
- 主回答 markdown 正确渲染(标题加粗、列表缩进、代码块带语法背景、表格有边框)。
- 之前不渲染的卡片内 markdown(TaskRuntimePanel 等,删除前仍在)也正确渲染。

- [ ] **Step 5: Commit**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli
git add web-frontend/src/components/common/MarkdownContent.tsx web-frontend/src/components/chat/MessageBubble.tsx
git -c commit.gpgsign=false commit -m "fix(markdown): MarkdownContent carries md-content class so CSS selectors apply everywhere"
```

---

## Phase 4: 前端主窗口一条流重构

> 依赖 Task 6(已修 markdown)。这是核心改动。

### Task 7: 新建 ThinkingSegment 组件(思考段折叠块)

**Files:**
- Create: `echo-agent-cli/web-frontend/src/components/chat/ThinkingSegment.tsx`

**目标:** spec §3.2「思考段」。可折叠,默认 streaming 时展开/结束后折叠。一段 markdown,左边紫色细竖线 + "思考 N" 小标签 + Brain 图标。

- [ ] **Step 1: 创建 ThinkingSegment.tsx**

```tsx
import { useState, memo } from 'react';
import { Brain, ChevronDown, ChevronRight } from 'lucide-react';
import MarkdownContent from '../common/MarkdownContent';

interface ThinkingSegmentProps {
  /** 1-based index among thinking segments in this message */
  index: number;
  /** Total thinking segments in this message (for "思考 1/3" labeling) */
  total: number;
  content: string;
  /** True while the parent message is still streaming */
  isStreaming?: boolean;
}

/**
 * One "thinking" segment in the inline one-stream layout.
 * Collapsible; expanded by default while streaming, collapsed after streaming ends.
 */
export const ThinkingSegment = memo(function ThinkingSegment({
  index,
  total,
  content,
  isStreaming,
}: ThinkingSegmentProps) {
  const [expanded, setExpanded] = useState(Boolean(isStreaming));

  // Re-expand when streaming resumes, collapse when it ends.
  // (useState initial only fires once; use effect to track streaming transitions.)
  // Simpler: leave it user-controlled after mount. Initial state follows isStreaming.

  const label = total > 1 ? `思考 ${index}/${total}` : '思考';

  return (
    <div
      className="my-1 rounded-md border-l-2 border-[var(--color-purple)] bg-[var(--bg-primary)] px-3 py-1.5"
    >
      <button
        onClick={() => setExpanded((e) => !e)}
        className="flex w-full items-center gap-1.5 text-left"
      >
        {expanded ? (
          <ChevronDown size={11} className="text-[var(--text-tertiary)]" />
        ) : (
          <ChevronRight size={11} className="text-[var(--text-tertiary)]" />
        )}
        <Brain
          size={11}
          className={isStreaming ? 'text-[var(--color-purple)] animate-pulse' : 'text-[var(--color-purple)]'}
        />
        <span className="text-[10px] font-medium text-[var(--color-purple)]">{label}</span>
      </button>
      {expanded && (
        <div className="mt-1.5 text-xs leading-relaxed text-[var(--text-secondary)]">
          <MarkdownContent content={content} className="text-xs" />
        </div>
      )}
    </div>
  );
});
```

- [ ] **Step 2: 类型检查**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli/web-frontend
npx tsc -b
```

预期:PASS。

- [ ] **Step 3: Commit**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli
git add web-frontend/src/components/chat/ThinkingSegment.tsx
git -c commit.gpgsign=false commit -m "feat(chat): add ThinkingSegment collapsible component for one-stream layout"
```

---

### Task 8: 新建 InlineToolCall 组件(工具调用折叠行)

**Files:**
- Create: `echo-agent-cli/web-frontend/src/components/chat/InlineToolCall.tsx`

**目标:** spec §3.2「工具调用行」。轻量一行摘要(图标+工具名+参数预览+状态),点击展开参数/结果。复用 `ToolCallCard` 的展开逻辑但改成 inline 折叠行样式(左侧细竖线,非独立 bordered card)。

- [ ] **Step 1: 创建 InlineToolCall.tsx**

```tsx
import { useState, memo } from 'react';
import { ChevronDown, ChevronRight, Wrench, Copy, Check } from 'lucide-react';
import type { ToolCallInfo } from '../../generated';

interface InlineToolCallProps {
  toolCall: ToolCallInfo;
  /** 1-based index among tool calls in this round */
  index: number;
}

/**
 * One tool call rendered as a lightweight inline collapsible row in the
 * one-stream layout. Not a standalone bordered card — just a left-bordered
 * row with a one-line summary, expandable to show args/result.
 */
export const InlineToolCall = memo(function InlineToolCall({ toolCall, index }: InlineToolCallProps) {
  const [expanded, setExpanded] = useState(false);
  const [copied, setCopied] = useState<string | null>(null);

  const statusColor = toolCall.success ? 'var(--color-success)' : 'var(--color-error)';
  const statusLabel = toolCall.success ? '✓' : '✗';

  // One-line arg preview: first string-ish value, truncated.
  const argPreview = (() => {
    const args = toolCall.args;
    if (args == null) return '';
    if (typeof args === 'string') return args;
    if (typeof args === 'object') {
      const entries = Object.entries(args as Record<string, unknown>);
      if (entries.length === 0) return '';
      const [k, v] = entries[0];
      const vStr = typeof v === 'string' ? v : JSON.stringify(v);
      return `${k}: ${vStr}`;
    }
    return String(args);
  })();
  const argPreviewTruncated =
    argPreview.length > 60 ? argPreview.slice(0, 60) + '…' : argPreview;

  const copyText = (text: string, label: string) => {
    navigator.clipboard.writeText(text);
    setCopied(label);
    setTimeout(() => setCopied(null), 2000);
  };

  return (
    <div
      className="my-0.5 border-l-2 pl-2"
      style={{ borderColor: 'var(--border-primary)' }}
    >
      <button
        onClick={() => setExpanded((e) => !e)}
        className="flex w-full items-center gap-1.5 py-0.5 text-left text-[11px]"
      >
        {expanded ? (
          <ChevronDown size={10} className="shrink-0 text-[var(--text-tertiary)]" />
        ) : (
          <ChevronRight size={10} className="shrink-0 text-[var(--text-tertiary)]" />
        )}
        <Wrench size={10} className="shrink-0" style={{ color: statusColor }} />
        <span className="font-mono font-medium text-[var(--text-primary)]">{toolCall.name}</span>
        {argPreviewTruncated && (
          <span className="truncate text-[var(--text-tertiary)]">{argPreviewTruncated}</span>
        )}
        <span className="ml-auto shrink-0 font-mono" style={{ color: statusColor }}>
          {statusLabel}
        </span>
      </button>
      {expanded && (
        <div className="mt-1 space-y-2 pb-1">
          <div>
            <div className="mb-0.5 flex items-center justify-between">
              <span className="text-[9px] font-medium uppercase tracking-wider text-[var(--text-tertiary)]">
                参数
              </span>
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  copyText(JSON.stringify(toolCall.args, null, 2), 'args');
                }}
                className="flex items-center gap-0.5 text-[10px] text-[var(--text-tertiary)]"
              >
                {copied === 'args' ? <Check size={9} /> : <Copy size={9} />}
                {copied === 'args' ? '已复制' : '复制'}
              </button>
            </div>
            <pre className="max-h-32 overflow-auto rounded bg-[var(--bg-code)] p-2 text-[10px] leading-relaxed text-[var(--color-code-text)]">
              {JSON.stringify(toolCall.args, null, 2)}
            </pre>
          </div>
          <div>
            <div className="mb-0.5 flex items-center justify-between">
              <span className="text-[9px] font-medium uppercase tracking-wider text-[var(--text-tertiary)]">
                结果
              </span>
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  copyText(toolCall.result, 'result');
                }}
                className="flex items-center gap-0.5 text-[10px] text-[var(--text-tertiary)]"
              >
                {copied === 'result' ? <Check size={9} /> : <Copy size={9} />}
                {copied === 'result' ? '已复制' : '复制'}
              </button>
            </div>
            <pre className="max-h-40 overflow-auto rounded bg-[var(--bg-code)] p-2 text-[10px] leading-relaxed text-[var(--color-code-text)]">
              {toolCall.result.length > 2000 ? toolCall.result.slice(0, 2000) + '\n...' : toolCall.result}
            </pre>
          </div>
        </div>
      )}
    </div>
  );
});
```

- [ ] **Step 2: 类型检查**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli/web-frontend
npx tsc -b
```

预期:PASS。

- [ ] **Step 3: Commit**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli
git add web-frontend/src/components/chat/InlineToolCall.tsx
git -c commit.gpgsign=false commit -m "feat(chat): add InlineToolCall lightweight collapsible row for one-stream layout"
```

---

### Task 9: 新建 WorkerStreamBlock 组件(sub-agent 嵌套块 + 进度摘要)

**Files:**
- Create: `echo-agent-cli/web-frontend/src/components/chat/WorkerStreamBlock.tsx`
- Create: `echo-agent-cli/web-frontend/src/utils/workerProgress.ts`

**目标:** spec §3.3.3 / §3.3.4。每个 worker 一个嵌套折叠块,折叠态 header 显示进度摘要 `[状态] N 工具 · 已读 M · 思考 K 轮`,展开态三段(提示词/执行过程/结果)。执行过程内部是思考+工具循环(递归嵌套)。

- [ ] **Step 1: 创建 workerProgress.ts(进度摘要计算)**

```ts
// echo-agent-cli/web-frontend/src/utils/workerProgress.ts
import type { WorkerTraceState, WorkerTraceEvent } from '../stores/workerTraceStore';

/** Tool names considered "read" operations (exploration). Frontend heuristic set. */
const READ_TOOL_NAMES = new Set([
  'read_file', 'read', 'read_files',
  'glob', 'grep', 'rg', 'search',
  'list', 'list_files', 'ls', 'list_dir',
  'view', 'cat', 'head', 'tail',
]);

export interface WorkerProgress {
  status: 'running' | 'completed' | 'failed' | 'cancelled' | 'planned';
  toolCount: number;
  readCount: number;
  thinkingRounds: number;
}

/** Count `worker_tool_start` events with a read-like tool name. */
function countReadTools(events: WorkerTraceEvent[]): number {
  return events.filter(
    (e) =>
      e.event_type === 'worker_tool_start' &&
      READ_TOOL_NAMES.has(String((e.payload as Record<string, unknown> | null)?.name ?? '').toLowerCase())
  ).length;
}

export function computeWorkerProgress(worker: WorkerTraceState): WorkerProgress {
  const events = worker.events;
  const toolCount = events.filter((e) => e.event_type === 'worker_tool_start').length;
  const readCount = countReadTools(events);
  const thinkingRounds = events.filter((e) => e.event_type === 'worker_thinking_end').length;
  return {
    status: worker.status,
    toolCount,
    readCount,
    thinkingRounds,
  };
}

export function progressSummary(p: WorkerProgress): string {
  if (p.status === 'failed' || p.status === 'cancelled') {
    return p.status === 'failed' ? '失败' : '已取消';
  }
  const parts: string[] = [];
  if (p.toolCount > 0) parts.push(`${p.toolCount} 工具`);
  if (p.readCount > 0) parts.push(`已读 ${p.readCount}`);
  if (p.thinkingRounds > 0) parts.push(`思考 ${p.thinkingRounds} 轮`);
  return parts.join(' · ');
}

export function statusLabel(status: WorkerProgress['status']): string {
  switch (status) {
    case 'running': return '运行中';
    case 'completed': return '已完成';
    case 'failed': return '失败';
    case 'cancelled': return '已取消';
    case 'planned': return '已规划';
  }
}
```

- [ ] **Step 2: 创建 WorkerStreamBlock.tsx(嵌套块 + 递归)**

```tsx
// echo-agent-cli/web-frontend/src/components/chat/WorkerStreamBlock.tsx
import { useState, memo, useMemo } from 'react';
import { Loader2, CheckCircle2, AlertCircle, Circle, ChevronDown, ChevronRight, Brain } from 'lucide-react';
import type { WorkerTraceState, WorkerTraceEvent } from '../../stores/workerTraceStore';
import MarkdownContent from '../common/MarkdownContent';
import { InlineToolCall } from './InlineToolCall';
import { computeWorkerProgress, progressSummary, statusLabel } from '../../utils/workerProgress';

interface WorkerStreamBlockProps {
  worker: WorkerTraceState;
  /** All workers in this run (for recursive child lookup via parentWorkerId) */
  allWorkers: WorkerTraceState[];
}

/** Reconstruct worker's thinking+tool loop from raw events. */
interface WorkerStep {
  type: 'thinking' | 'tool';
  // for thinking: concatenated content
  content?: string;
  // for tool: start event + matched result event
  toolStart?: WorkerTraceEvent;
  toolResult?: WorkerTraceEvent;
}

function reconstructSteps(events: WorkerTraceEvent[]): { steps: WorkerStep[]; thinkingTotal: number } {
  const steps: WorkerStep[] = [];
  let thinkingTotal = 0;
  let currentThinking: string[] = [];
  const pendingTools: WorkerTraceEvent[] = [];

  const flushThinking = () => {
    if (currentThinking.length > 0) {
      const content = currentThinking.join('').trim();
      if (content) {
        steps.push({ type: 'thinking', content });
        thinkingTotal++;
      }
      currentThinking = [];
    }
  };

  for (const e of events) {
    if (e.event_type === 'worker_thinking_delta') {
      const c = String((e.payload as Record<string, unknown> | null)?.content ?? '');
      if (c) currentThinking.push(c);
    } else if (e.event_type === 'worker_thinking_end') {
      flushThinking();
    } else if (e.event_type === 'worker_tool_start') {
      flushThinking();
      pendingTools.push(e);
    } else if (e.event_type === 'worker_tool_result') {
      const name = String((e.payload as Record<string, unknown> | null)?.name ?? '');
      // Match the earliest pending tool_start with same name (FIFO).
      const idx = pendingTools.findIndex(
        (p) => String((p.payload as Record<string, unknown> | null)?.name ?? '') === name
      );
      const start = idx >= 0 ? pendingTools.splice(idx, 1)[0] : undefined;
      steps.push({ type: 'tool', toolStart: start, toolResult: e });
    }
  }
  // Flush trailing thinking (no thinking_end yet, streaming).
  flushThinking();
  // Unmatched pending tool_starts (still running).
  for (const start of pendingTools) {
    steps.push({ type: 'tool', toolStart: start });
  }
  return { steps, thinkingTotal };
}

function workerResult(worker: WorkerTraceState): string {
  const completed = [...worker.events].reverse().find((e) => e.event_type === 'worker_completed');
  const summary = completed ? String((completed.payload as Record<string, unknown> | null)?.summary ?? '') : '';
  if (summary) return summary;
  return worker.events
    .filter((e) => e.event_type === 'worker_token_delta')
    .map((e) => String((e.payload as Record<string, unknown> | null)?.content ?? ''))
    .join('')
    .trim();
}

export const WorkerStreamBlock = memo(function WorkerStreamBlock({ worker, allWorkers }: WorkerStreamBlockProps) {
  const [expanded, setExpanded] = useState(worker.status === 'running');
  const [sectionExpanded, setSectionExpanded] = useState({
    prompt: false,
    process: true,
    result: true,
  });

  const progress = useMemo(() => computeWorkerProgress(worker), [worker.events, worker.status]);
  const summary = progressSummary(progress);
  const { steps } = useMemo(() => reconstructSteps(worker.events), [worker.events]);
  const result = useMemo(() => workerResult(worker), [worker.events]);

  // Recursive children: workers whose parentWorkerId === this worker.workerId
  const children = useMemo(
    () => allWorkers.filter((w) => w.parentWorkerId === worker.workerId),
    [allWorkers, worker.workerId]
  );

  const statusIcon =
    worker.status === 'running' ? (
      <Loader2 size={11} className="animate-spin" style={{ color: 'var(--color-info)' }} />
    ) : worker.status === 'completed' ? (
      <CheckCircle2 size={11} style={{ color: 'var(--color-success)' }} />
    ) : worker.status === 'failed' ? (
      <AlertCircle size={11} style={{ color: 'var(--color-error)' }} />
    ) : (
      <Circle size={11} style={{ color: 'var(--text-tertiary)' }} />
    );

  return (
    <div className="my-1 rounded-md border border-[var(--border-primary)] bg-[var(--bg-secondary)]">
      {/* Header (always visible): title + status + progress summary */}
      <button
        onClick={() => setExpanded((e) => !e)}
        className="flex w-full items-center gap-1.5 px-2 py-1.5 text-left text-[11px]"
      >
        {expanded ? <ChevronDown size={10} className="text-[var(--text-tertiary)]" /> : <ChevronRight size={10} className="text-[var(--text-tertiary)]" />}
        {statusIcon}
        <span className="truncate font-medium text-[var(--text-primary)]">
          sub-agent: {worker.title || worker.agentName || worker.workerId}
        </span>
        <span className="ml-auto shrink-0 text-[10px] text-[var(--text-tertiary)]">
          {statusLabel(progress.status)} · {summary}
        </span>
      </button>

      {expanded && (
        <div className="space-y-2 border-t border-[var(--border-primary)] px-2 pb-2 pt-2">
          {/* Prompt */}
          {worker.task && (
            <div>
              <button
                onClick={() => setSectionExpanded((s) => ({ ...s, prompt: !s.prompt }))}
                className="flex items-center gap-1 text-[10px] font-medium text-[var(--text-tertiary)]"
              >
                {sectionExpanded.prompt ? <ChevronDown size={9} /> : <ChevronRight size={9} />}
                提示词
              </button>
              {sectionExpanded.prompt && (
                <div className="mt-1">
                  <MarkdownContent content={worker.task} className="text-[11px]" maxHeight={288} />
                </div>
              )}
            </div>
          )}

          {/* Execution process: thinking + tool loop */}
          <div>
            <button
              onClick={() => setSectionExpanded((s) => ({ ...s, process: !s.process }))}
              className="flex items-center gap-1 text-[10px] font-medium text-[var(--text-tertiary)]"
            >
              {sectionExpanded.process ? <ChevronDown size={9} /> : <ChevronRight size={9} />}
              执行过程
            </button>
            {sectionExpanded.process && (
              <div className="mt-1 space-y-1">
                {steps.length === 0 && (
                  <div className="text-[10px] text-[var(--text-tertiary)]">暂无事件</div>
                )}
                {steps.map((step, i) => {
                  if (step.type === 'thinking') {
                    return (
                      <div key={i} className="rounded border-l-2 border-[var(--color-purple)] bg-[var(--bg-primary)] px-2 py-1">
                        <div className="mb-0.5 flex items-center gap-1">
                          <Brain size={9} className="text-[var(--color-purple)]" />
                          <span className="text-[9px] font-medium text-[var(--color-purple)]">思考</span>
                        </div>
                        <MarkdownContent content={step.content || ''} className="text-[10px]" maxHeight={200} />
                      </div>
                    );
                  }
                  // tool step — reconstruct a ToolCallInfo-like object
                  const name = String((step.toolStart?.payload as Record<string, unknown> | null)?.name ?? 'tool');
                  const args = (step.toolStart?.payload as Record<string, unknown> | null)?.args ?? {};
                  const resultStr = String((step.toolResult?.payload as Record<string, unknown> | null)?.result ?? '');
                  const success = (step.toolResult?.payload as Record<string, unknown> | null)?.success !== 'false';
                  return (
                    <InlineToolCall
                      key={i}
                      toolCall={{ name, args, result: resultStr, success }}
                      index={i}
                    />
                  );
                })}
                {/* Recursive children (nested sub-agents) */}
                {children.length > 0 && (
                  <div className="ml-2 border-l border-[var(--border-primary)] pl-2">
                    {children.map((child) => (
                      <WorkerStreamBlock key={child.workerId} worker={child} allWorkers={allWorkers} />
                    ))}
                  </div>
                )}
              </div>
            )}
          </div>

          {/* Result */}
          {result && (
            <div>
              <button
                onClick={() => setSectionExpanded((s) => ({ ...s, result: !s.result }))}
                className="flex items-center gap-1 text-[10px] font-medium text-[var(--text-tertiary)]"
              >
                {sectionExpanded.result ? <ChevronDown size={9} /> : <ChevronRight size={9} />}
                结果
              </button>
              {sectionExpanded.result && (
                <div className="mt-1">
                  <MarkdownContent content={result} className="text-[11px]" maxHeight={400} />
                </div>
              )}
            </div>
          )}
        </div>
      )}
    </div>
  );
});
```

- [ ] **Step 3: 类型检查**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli/web-frontend
npx tsc -b
```

预期:PASS。若 `WorkerTraceEvent` 的 `payload` 类型不是 `unknown`(可能是 `serde_json::Value` 对应的 TS 类型),按实际类型调整 `(e.payload as Record<string, unknown> | null)` 的断言。

- [ ] **Step 4: Commit**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli
git add web-frontend/src/components/chat/WorkerStreamBlock.tsx web-frontend/src/utils/workerProgress.ts
git -c commit.gpgsign=false commit -m "feat(chat): add WorkerStreamBlock nested sub-agent block with progress summary"
```

---

### Task 10: 新建 ParallelExecutionBlock 组件(并行执行段)

**Files:**
- Create: `echo-agent-cli/web-frontend/src/components/chat/ParallelExecutionBlock.tsx`

**目标:** spec §3.3.2。从 `useTaskRuntimeStore().activeRun.run_id` + `useWorkerTraceStore().workers` 筛出本次 run 的顶层 worker(`parentWorkerId` 为空),渲染「并行执行」段,内含 N 个 `WorkerStreamBlock`。

- [ ] **Step 1: 创建 ParallelExecutionBlock.tsx**

```tsx
// echo-agent-cli/web-frontend/src/components/chat/ParallelExecutionBlock.tsx
import { useMemo, memo } from 'react';
import { ChevronDown, ChevronRight } from 'lucide-react';
import { useState } from 'react';
import { useTaskRuntimeStore } from '../../stores/taskRuntimeStore';
import { useWorkerTraceStore } from '../../stores/workerTraceStore';
import { WorkerStreamBlock } from './WorkerStreamBlock';

/**
 * The "并行执行" segment in the one-stream layout.
 * Shows top-level workers (parentWorkerId empty) for the active run.
 * Renders nothing if no workers match (per spec §3.3.2 rule 4).
 */
export const ParallelExecutionBlock = memo(function ParallelExecutionBlock() {
  const [expanded, setExpanded] = useState(true);
  const activeRun = useTaskRuntimeStore((s) => s.activeRun);
  const workers = useWorkerTraceStore((s) => s.workers);

  const visibleWorkers = useMemo(() => {
    if (!activeRun) return [];
    return Object.values(workers)
      .filter((w) => w.runId === activeRun.run_id && !w.parentWorkerId)
      .sort((a, b) => (a.startedAt ?? '').localeCompare(b.startedAt ?? ''));
  }, [activeRun, workers]);

  if (visibleWorkers.length === 0) return null;

  const runningCount = visibleWorkers.filter((w) => w.status === 'running').length;

  return (
    <div className="my-2 rounded-lg border border-[var(--border-primary)] bg-[var(--bg-primary)]">
      <button
        onClick={() => setExpanded((e) => !e)}
        className="flex w-full items-center gap-1.5 px-3 py-1.5 text-left text-xs"
      >
        {expanded ? <ChevronDown size={12} className="text-[var(--text-tertiary)]" /> : <ChevronRight size={12} className="text-[var(--text-tertiary)]" />}
        <span className="font-medium text-[var(--text-secondary)]">并行执行</span>
        <span className="text-[10px] text-[var(--text-tertiary)]">
          {visibleWorkers.length} 个 worker · {runningCount} 运行中
        </span>
      </button>
      {expanded && (
        <div className="space-y-1 border-t border-[var(--border-primary)] px-2 py-2">
          {visibleWorkers.map((w) => (
            <WorkerStreamBlock key={w.workerId} worker={w} allWorkers={Object.values(workers).filter((x) => x.runId === activeRun!.run_id)} />
          ))}
        </div>
      )}
    </div>
  );
});
```

- [ ] **Step 2: 类型检查**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli/web-frontend
npx tsc -b
```

预期:PASS。

- [ ] **Step 3: Commit**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli
git add web-frontend/src/components/chat/ParallelExecutionBlock.tsx
git -c commit.gpgsign=false commit -m "feat(chat): add ParallelExecutionBlock segment for run-level worker rendering"
```

---

### Task 11: 重写 MessageBubble 为一条流 + 删除旧 ExecutionProcessBlock

**Files:**
- Modify: `echo-agent-cli/web-frontend/src/components/chat/MessageBubble.tsx`(重写,删除 `ExecutionProcessBlock` 函数)

**目标:** spec §3.2。一条流:思考段(`ThinkingSegment`)+ 工具调用行(`InlineToolCall`)+ 并行执行段(`ParallelExecutionBlock`)+ 最终正文(markdown)。删除旧的 `ExecutionProcessBlock`。

- [ ] **Step 1: 重写 MessageBubble.tsx**

完整替换 `MessageBubble.tsx` 内容(保留用户消息渲染、附件、hover 按钮、编辑逻辑不变;只重写 assistant 消息内容区):

```tsx
import { useState, memo } from 'react';
import type { ChatMessage, ExecutionRound } from '../../types/api';
import type { ToolCallInfo } from '../../generated';
import {
  User,
  Bot,
  Copy,
  Check,
  RefreshCw,
  Pencil,
  X,
  ArrowUp,
  File,
  Download,
} from 'lucide-react';
import MarkdownContent from '../common/MarkdownContent';
import { ThinkingSegment } from './ThinkingSegment';
import { InlineToolCall } from './InlineToolCall';
import { ParallelExecutionBlock } from './ParallelExecutionBlock';

interface MessageBubbleProps {
  message: ChatMessage;
  onRegenerate?: () => void;
  onEditAndResend?: (messageId: string, newContent: string) => void;
}

function formatFileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

async function copyToClipboard(text: string): Promise<boolean> {
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {}
  const textarea = document.createElement('textarea');
  textarea.value = text;
  textarea.style.position = 'fixed';
  textarea.style.left = '-9999px';
  document.body.appendChild(textarea);
  textarea.select();
  try {
    document.execCommand('copy');
    return true;
  } catch {
    return false;
  } finally {
    document.body.removeChild(textarea);
  }
}

function isImageFile(mime: string): boolean {
  return mime.startsWith('image/');
}

/** Flatten executionRounds (or legacy fields) into an ordered step list. */
interface FlatStep {
  type: 'thinking' | 'tool';
  thinkingContent?: string;
  toolCall?: ToolCallInfo;
  toolIndex: number;
}

function flattenSteps(message: ChatMessage): { steps: FlatStep[]; thinkingTotal: number } {
  const steps: FlatStep[] = [];
  let thinkingTotal = 0;

  if (message.executionRounds && message.executionRounds.length > 0) {
    message.executionRounds.forEach((round: ExecutionRound) => {
      if (round.thinking && round.thinking.content.trim()) {
        steps.push({ type: 'thinking', thinkingContent: round.thinking.content, toolIndex: 0 });
        thinkingTotal++;
      }
      round.tools.forEach((tc) => {
        steps.push({ type: 'tool', toolCall: tc, toolIndex: steps.length });
      });
    });
  } else if (message.executionSteps && message.executionSteps.length > 0) {
    message.executionSteps.forEach((step) => {
      if (step.type === 'thinking') {
        const seg = message.thinkingSegments?.[step.index];
        if (seg && seg.content.trim()) {
          steps.push({ type: 'thinking', thinkingContent: seg.content, toolIndex: 0 });
          thinkingTotal++;
        }
      } else if (step.type === 'tool') {
        const tc = message.toolCalls?.[step.index];
        if (tc) steps.push({ type: 'tool', toolCall: tc, toolIndex: step.index });
      }
    });
  } else {
    // Fallback: thinkingSegments + toolCalls flat
    const segs = (message.thinkingSegments || []).filter((s) => s.content.trim());
    segs.forEach((s) => {
      steps.push({ type: 'thinking', thinkingContent: s.content, toolIndex: 0 });
      thinkingTotal++;
    });
    if (thinkingTotal === 0 && message.thinkingContent) {
      steps.push({ type: 'thinking', thinkingContent: message.thinkingContent, toolIndex: 0 });
      thinkingTotal++;
    }
    (message.toolCalls || []).forEach((tc, i) => {
      steps.push({ type: 'tool', toolCall: tc, toolIndex: i });
    });
  }
  return { steps, thinkingTotal };
}

export const MessageBubble = memo(function MessageBubble({
  message,
  onRegenerate,
  onEditAndResend,
}: MessageBubbleProps) {
  const isUser = message.role === 'user';
  const [editing, setEditing] = useState(false);
  const [editText, setEditText] = useState(message.content);

  const startEdit = () => {
    setEditText(message.content);
    setEditing(true);
  };
  const cancelEdit = () => {
    setEditing(false);
    setEditText(message.content);
  };
  const submitEdit = () => {
    const trimmed = editText.trim();
    if (!trimmed || trimmed === message.content) {
      cancelEdit();
      return;
    }
    onEditAndResend?.(message.id, trimmed);
    setEditing(false);
  };

  const images = message.attachments?.filter((a) => isImageFile(a.mime_type)) ?? [];
  const files = message.attachments?.filter((a) => !isImageFile(a.mime_type)) ?? [];

  const { steps, thinkingTotal } = flattenSteps(message);
  let thinkingIndex = 0;

  return (
    <div className={`flex gap-3 py-3.5 ${isUser ? 'flex-row-reverse' : ''}`}>
      {/* Avatar */}
      <div
        className={`flex h-7 w-7 shrink-0 items-center justify-center rounded-lg text-xs font-semibold
          ${
            isUser
              ? 'bg-[var(--bg-user-msg)] text-[var(--text-user-msg)]'
              : 'border border-[var(--border-primary)] bg-[var(--bg-secondary)] text-[var(--text-secondary)]'
          }`}
      >
        {isUser ? <User size={14} /> : <Bot size={14} />}
      </div>

      {/* Content — one stream */}
      <div className={`min-w-0 space-y-2 ${isUser ? 'max-w-[72%] items-end' : 'w-full max-w-[92%]'}`}>
        {/* Images */}
        {images.length > 0 && (
          <div className={`grid gap-2 ${images.length === 1 ? 'grid-cols-1' : 'grid-cols-2'}`}>
            {images.map((img, i) => (
              <div key={i} className="overflow-hidden rounded-xl border border-[var(--border-primary)] bg-[var(--bg-secondary)]">
                <img src={img.url} alt={img.name} className="w-full object-cover" style={{ maxHeight: '300px' }} onClick={() => window.open(img.url, '_blank')} />
              </div>
            ))}
          </div>
        )}

        {/* Files */}
        {files.length > 0 && (
          <div className="space-y-1.5">
            {files.map((file, i) => (
              <a key={i} href={file.url} download={file.name} className="flex items-center gap-3 rounded-lg border border-[var(--border-primary)] bg-[var(--bg-secondary)] p-3 transition-colors hover:bg-[var(--bg-hover)]">
                <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-[var(--bg-primary)]">
                  <File size={16} className="text-[var(--text-tertiary)]" />
                </div>
                <div className="min-w-0 flex-1">
                  <div className="truncate text-xs font-medium text-[var(--text-primary)]">{file.name}</div>
                  <div className="text-[10px] text-[var(--text-tertiary)]">{formatFileSize(file.size)}</div>
                </div>
                <Download size={14} className="shrink-0 text-[var(--text-tertiary)]" />
              </a>
            ))}
          </div>
        )}

        {/* One-stream content: thinking + tools + parallel execution, then final text */}
        {!isUser && steps.length > 0 && (
          <div className="space-y-1">
            {steps.map((step, i) => {
              if (step.type === 'thinking') {
                thinkingIndex++;
                return (
                  <ThinkingSegment
                    key={`think-${i}`}
                    index={thinkingIndex}
                    total={thinkingTotal}
                    content={step.thinkingContent || ''}
                    isStreaming={message.isStreaming}
                  />
                );
              }
              return (
                <InlineToolCall
                  key={`tool-${i}`}
                  toolCall={step.toolCall!}
                  index={step.toolIndex}
                />
              );
            })}
          </div>
        )}

        {/* Parallel execution segment (run-level workers) */}
        {!isUser && <ParallelExecutionBlock />}

        {/* Final text content */}
        {message.content && (
          <div className="group/msg relative">
            {!message.isStreaming && !editing && (
              <div className={`absolute -top-3 z-10 flex gap-0.5 rounded-lg border border-[var(--border-primary)] bg-[var(--bg-primary)] px-1 py-0.5 opacity-0 shadow-[var(--shadow-md)] transition-all duration-200 group-hover/msg:opacity-100 group-hover/msg:-translate-y-0.5 ${isUser ? 'left-0' : 'right-0'}`}>
                <ActionButton icon={<Copy size={13} />} label="复制" onClick={() => copyToClipboard(message.content)} copyMode />
                {isUser && <ActionButton icon={<Pencil size={13} />} label="编辑" onClick={startEdit} />}
                {!isUser && onRegenerate && <ActionButton icon={<RefreshCw size={13} />} label="重新生成" onClick={onRegenerate} />}
              </div>
            )}
            {editing ? (
              <div className="rounded-2xl border-2 border-[var(--accent)] bg-[var(--bg-primary)] px-4 py-3 shadow-[var(--shadow-md)]">
                <textarea
                  value={editText}
                  onChange={(e) => setEditText(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); submitEdit(); }
                    if (e.key === 'Escape') cancelEdit();
                  }}
                  rows={3}
                  className="w-full resize-none bg-transparent text-sm leading-relaxed text-[var(--text-primary)] outline-none"
                  autoFocus
                />
                <div className="mt-3 flex items-center justify-end gap-1.5">
                  <button onClick={cancelEdit} className="flex items-center gap-1 rounded-md px-2.5 py-1 text-xs text-[var(--text-secondary)]">
                    <X size={12} /> 取消
                  </button>
                  <button onClick={submitEdit} disabled={!editText.trim()} className="flex items-center gap-1 rounded-md px-2.5 py-1 text-xs text-[var(--text-on-accent)]" style={{ background: 'var(--accent)' }}>
                    <ArrowUp size={12} /> 发送
                  </button>
                </div>
              </div>
            ) : (
              <div className={`text-sm leading-relaxed ${isUser ? 'rounded-2xl bg-[var(--bg-user-msg)] px-4 py-2.5 text-[var(--text-user-msg)]' : 'border-l-2 border-[var(--border-primary)] px-4 py-1 text-[var(--text-assistant-msg)]'}`}>
                {isUser ? (
                  <div className="whitespace-pre-wrap break-words">{message.content}</div>
                ) : (
                  <MarkdownContent className="break-words" content={message.content} />
                )}
                {message.isStreaming && (
                  <span className="ml-0.5 inline-block h-[14px] w-[3px] animate-pulse rounded-full bg-[var(--accent)] align-text-bottom" />
                )}
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
});

function ActionButton({ icon, label, onClick, copyMode }: { icon: React.ReactNode; label: string; onClick: () => void; copyMode?: boolean; }) {
  const [done, setDone] = useState(false);
  const handleClick = (e: React.MouseEvent) => {
    e.stopPropagation();
    onClick();
    if (copyMode) { setDone(true); setTimeout(() => setDone(false), 2000); }
  };
  return (
    <button onClick={handleClick} className={`flex items-center gap-1 rounded-md px-1.5 py-1 text-[11px] transition-colors ${done ? 'text-[var(--color-success)]' : 'text-[var(--text-tertiary)] hover:text-[var(--text-primary)]'}`} title={label}>
      {done ? <Check size={13} /> : icon}
    </button>
  );
}
```

> 注:删除了旧的 `ExecutionProcessBlock` 函数和它的 `ChartCard` 用法(spec 没要求保留 chart,若需要可后续加回)。也删除了 `ChevronDown/ChevronRight/Brain` 等不再直接用的 import。

- [ ] **Step 2: 类型检查**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli/web-frontend
npx tsc -b
```

预期:PASS。若有未使用 import 警告,清理。

- [ ] **Step 3: 删除 ChatPanel 里的 TaskRuntimeMainPanel 和 ConversationTimeline 挂载**

`echo-agent-cli/web-frontend/src/components/chat/ChatPanel.tsx:150-151` 当前:

```tsx
              <ConversationTimeline />
              <TaskRuntimeMainPanel />
```

删除这两行。同时删除文件顶部对应的 import(`ChatPanel.tsx:11` `TaskRuntimeMainPanel`、`ChatPanel.tsx:12` `ConversationTimeline`)。

- [ ] **Step 4: 类型检查 + 构建**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli/web-frontend
npx tsc -b
npm run build
```

预期:PASS。

- [ ] **Step 5: 手动验证**

启动前端,发起一个会话:
- 普通对话:用户提问 + agent 回答(markdown 渲染),无卡片。
- 触发 `parallel_readonly_delegation`:主 agent 回答流里有「并行执行」段,每个 worker 折叠态显示进度摘要,展开看 提示词/执行过程/结果。

- [ ] **Step 6: Commit**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli
git add web-frontend/src/components/chat/MessageBubble.tsx web-frontend/src/components/chat/ChatPanel.tsx
git -c commit.gpgsign=false commit -m "feat(chat): rewrite MessageBubble as one-stream layout, remove ExecutionProcessBlock and global panel mounts"
```

---

## Phase 5: 前端右侧栏重构

### Task 12: 重写 RightRail 为三块

**Files:**
- Modify: `echo-agent-cli/web-frontend/src/components/layout/RightRail.tsx`(重写)

**目标:** spec §4。只留三块:任务执行进度 / 输出产物 / Token & Cache。删除 BackgroundTask 进度区、底部三行全局状态。

- [ ] **Step 1: 重写 RightRail.tsx**

完整替换(保留文件改动区逻辑、ChangesDrawer;重写主体):

```tsx
// echo-agent-cli/web-frontend/src/components/layout/RightRail.tsx
import { useEffect, useMemo, useState } from 'react';
import { RefreshCw, ListTodo, FileText, Gauge } from 'lucide-react';
import { useConversationStore } from '../../stores/conversationStore';
import { useChangesStore } from '../../stores/changesStore';
import { useTaskRuntimeStore } from '../../stores/taskRuntimeStore';
import { useWorkerTraceStore } from '../../stores/workerTraceStore';
import { deriveChangedFiles } from '../../utils/deriveChangedFiles';
import { useChatStore } from '../../stores/chatStore';
import { ChangesDrawer } from '../changes/ChangesDrawer';
import { CacheUsageCard, cacheUsageForWorkers } from '../task/TaskRuntimePanel';

export function RightRail() {
  const activeId = useConversationStore((s) => s.activeId);
  const messages = useChatStore((s) => s.messages);
  const { activeRun, plan, todos, artifacts, awaitingApproval, approve, reject, refresh } =
    useTaskRuntimeStore();
  const traceWorkers = useWorkerTraceStore((s) => s.workers);

  const changesFiles = useChangesStore((s) => s.files);
  const setSelected = useChangesStore((s) => s.setSelected);

  // Session change detection
  useEffect(() => {
    useChangesStore.getState().checkSessionChange(activeId);
  }, [activeId]);

  // Derive changed files from messages on tool-call fingerprint
  const toolCallCount = useMemo(() => {
    let n = 0;
    for (const m of messages) n += (m.toolCalls ?? []).length;
    return n;
  }, [messages]);
  useEffect(() => {
    useChangesStore.getState().setFiles(deriveChangedFiles(messages));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [toolCallCount]);

  const visibleWorkers = useMemo(() => {
    if (!activeRun) return [];
    return Object.values(traceWorkers)
      .filter((w) => w.runId === activeRun.run_id)
      .sort((a, b) => (a.startedAt ?? '').localeCompare(b.startedAt ?? ''));
  }, [activeRun, traceWorkers]);

  const displayedChanges = changesFiles.slice(0, 12);
  const usageSummary = cacheUsageForWorkers(visibleWorkers);

  return (
    <aside className="hidden h-full w-[300px] shrink-0 border-l border-[var(--border-primary)] bg-[var(--bg-rail)] px-4 py-5 xl:block">
      <div className="flex h-full flex-col gap-5 overflow-y-auto">
        {/* ── 任务执行进度 ── */}
        <section>
          <div className="mb-3 flex items-center justify-between">
            <div className="flex items-center gap-1.5">
              <ListTodo size={13} style={{ color: 'var(--accent)' }} />
              <h2 className="text-sm font-semibold text-[var(--text-primary)]">任务执行进度</h2>
            </div>
            {activeRun && (
              <button onClick={() => refresh(activeRun.run_id)} className="text-[var(--text-tertiary)]">
                <RefreshCw size={11} />
              </button>
            )}
          </div>
          {activeRun ? (
            <>
              <div className="mb-2 truncate rounded-md px-2 py-1.5 text-[11px]" style={{ background: 'var(--bg-secondary)', color: 'var(--text-secondary)' }} title={activeRun.goal}>
                {activeRun.goal}
              </div>
              <div className="mb-2">
                <span className="rounded px-1.5 py-0.5 text-[10px] font-medium" style={{ color: statusColor(activeRun.status), background: 'var(--bg-hover)' }}>
                  {STATUS_LABEL[activeRun.status] ?? activeRun.status}
                </span>
              </div>
              {/* Worker list (compact) */}
              {visibleWorkers.length > 0 && (
                <div className="space-y-1">
                  {visibleWorkers.map((w) => (
                    <div key={w.workerId} className="rounded px-2 py-1 text-[10px]" style={{ background: 'var(--bg-secondary)', color: 'var(--text-secondary)' }}>
                      <div className="truncate">{w.title || w.agentName || w.workerId}</div>
                      <div className="text-[9px]" style={{ color: statusColor(w.status) }}>{w.status}</div>
                    </div>
                  ))}
                </div>
              )}
              {/* Plan approval (when awaiting) */}
              {awaitingApproval && plan && (
                <div className="mt-2 flex gap-1.5">
                  <button onClick={() => approve(activeRun.run_id)} className="flex-1 rounded px-2 py-1 text-[11px] font-medium" style={{ background: 'var(--accent)', color: 'var(--text-on-accent)' }}>执行全部</button>
                  <button onClick={() => reject(activeRun.run_id)} className="rounded px-2 py-1 text-[11px]" style={{ background: 'var(--bg-hover)', color: 'var(--text-secondary)' }}>取消</button>
                </div>
              )}
            </>
          ) : (
            <div className="rounded-lg border border-dashed border-[var(--border-primary)] px-3 py-3 text-xs text-[var(--text-tertiary)]">
              暂无运行中的任务
            </div>
          )}
        </section>

        {/* ── 输出产物 ── */}
        <section>
          <div className="mb-3 flex items-center gap-1.5">
            <FileText size={13} style={{ color: 'var(--accent)' }} />
            <h2 className="text-sm font-semibold text-[var(--text-primary)]">输出产物</h2>
            <span className="ml-auto text-xs text-[var(--text-tertiary)]">
              {changesFiles.length ? `${changesFiles.length} 改动` : ''}
            </span>
          </div>
          <div className="space-y-1">
            {displayedChanges.length === 0 ? (
              <div className="rounded-lg border border-dashed border-[var(--border-primary)] px-3 py-3 text-xs text-[var(--text-tertiary)]">
                本会话暂无文件改动
              </div>
            ) : (
              displayedChanges.map((file) => {
                const meta = file.status === 'added' ? { label: 'A', color: 'var(--color-success, #22c55e)' } : file.status === 'deleted' ? { label: 'D', color: 'var(--color-error, #ef4444)' } : { label: 'M', color: 'var(--color-warning, #f59e0b)' };
                return (
                  <button key={file.path} onClick={() => setSelected(file.path)} className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left transition-colors hover:bg-[var(--bg-hover)]" title={file.path}>
                    <span className="inline-flex h-4 w-4 shrink-0 items-center justify-center rounded text-[9px] font-bold" style={{ background: `color-mix(in srgb, ${meta.color} 18%, transparent)`, color: meta.color }}>{meta.label}</span>
                    <span className="min-w-0 flex-1 truncate text-xs text-[var(--text-secondary)]">
                      <span className="text-[var(--text-primary)]">{file.basename}</span>
                      {file.dir && <span className="text-[var(--text-tertiary)]"> · {file.dir}</span>}
                    </span>
                  </button>
                );
              })
            )}
            {/* Artifacts */}
            {artifacts.length > 0 && (
              <div className="mt-2 space-y-0.5">
                {artifacts.map((a) => (
                  <div key={a.id} className="flex items-center gap-1 truncate px-1 py-0.5 text-[10px] text-[var(--text-secondary)]" title={a.path ?? a.title}>
                    <FileText size={10} className="text-[var(--text-tertiary)]" />
                    <span className="truncate">{a.title}</span>
                  </div>
                ))}
              </div>
            )}
          </div>
        </section>

        {/* ── Token / Cache ── */}
        <section>
          <div className="mb-3 flex items-center gap-1.5">
            <Gauge size={13} style={{ color: 'var(--accent)' }} />
            <h2 className="text-sm font-semibold text-[var(--text-primary)]">Token / Cache</h2>
          </div>
          {usageSummary.calls > 0 ? (
            <CacheUsageCard summary={usageSummary} compact />
          ) : (
            <div className="rounded-lg border border-dashed border-[var(--border-primary)] px-3 py-3 text-xs text-[var(--text-tertiary)]">
              暂无 LLM 调用数据
            </div>
          )}
        </section>
      </div>
      <ChangesDrawer />
    </aside>
  );
}

const STATUS_LABEL: Record<string, string> = {
  pending: '待处理', planning: '规划中', awaiting_plan_approval: '待确认计划', ready: '就绪',
  running: '执行中', waiting_approval: '等待审批', waiting_input: '等待输入', suspended: '已挂起',
  cancelling: '取消中', cancelled: '已取消', failed: '失败', completed: '已完成',
};

function statusColor(status: string): string {
  if (['completed'].includes(status)) return 'var(--color-success)';
  if (['running', 'planning', 'ready'].includes(status)) return 'var(--color-info)';
  if (['failed', 'cancelled'].includes(status)) return 'var(--color-error)';
  if (['suspended', 'blocked', 'waiting_approval', 'awaiting_plan_approval', 'waiting_input', 'cancelling'].includes(status)) return 'var(--color-warning)';
  return 'var(--text-tertiary)';
}
```

> 注:`CacheUsageCard` 和 `cacheUsageForWorkers` 从 `TaskRuntimePanel` 导出复用(它们是模块内函数,需在 TaskRuntimePanel.tsx 里加 `export`)。若未导出,Task 13 会处理;此处先假设已导出,若 tsc 报错,在 Task 13 Step 1 补导出。

- [ ] **Step 2: 类型检查**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli/web-frontend
npx tsc -b
```

预期:若 `CacheUsageCard`/`cacheUsageForWorkers` 未导出,会报错。先记下,在 Task 13 Step 1 修复。

- [ ] **Step 3: Commit(可能先不 commit,等 Task 13 修完导出再一起)**

暂不 commit,标记为 WIP。

---

## Phase 6: 删除清单 + 清理

### Task 13: 删除废弃组件 + 导出复用函数

**Files:**
- Delete: `echo-agent-cli/web-frontend/src/components/chat/ConversationTimeline.tsx`
- Delete: `echo-agent-cli/web-frontend/src/stores/conversationRuntimeStore.ts`(若无其他引用)
- Delete: `echo-agent-cli/web-frontend/src/components/task/RuntimeStoryCard.tsx`(若无其他引用)
- Modify: `echo-agent-cli/web-frontend/src/components/task/TaskRuntimePanel.tsx`(导出 `CacheUsageCard`/`cacheUsageForWorkers`,删除 `TaskRuntimeMainPanel`)

- [ ] **Step 1: 在 TaskRuntimePanel.tsx 导出复用函数**

`TaskRuntimePanel.tsx` 里 `function CacheUsageCard(...)` 和 `function cacheUsageForWorkers(...)` 当前是模块私有。加 `export`:

```tsx
export function CacheUsageCard({ summary, compact = false }: { summary: CacheUsageSummary; compact?: boolean }) {
  // ... 原内容不变
}

export function cacheUsageForWorkers(workers: WorkerTraceState[]): CacheUsageSummary {
  // ... 原内容不变
}
```

同时删除 `TaskRuntimePanel.tsx` 里的 `export function TaskRuntimeMainPanel()` 整个函数(约 1079-1616 行)及其专属 import(`RuntimeStoryCard`、`PlanEditor`、`ResultFullView`、`MarkdownContent` 若只被它用)。

- [ ] **Step 2: 删除 ConversationTimeline.tsx**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli
rm web-frontend/src/components/chat/ConversationTimeline.tsx
```

- [ ] **Step 3: 检查 conversationRuntimeStore 是否还有引用**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli/web-frontend
grep -rn "conversationRuntimeStore" src/
```

若只剩自身定义(无其他 import),删除:

```bash
rm src/stores/conversationRuntimeStore.ts
```

若有其他引用(如 WebSocket 事件分发),保留——只删 ConversationTimeline 组件,store 留着。

- [ ] **Step 4: 检查 RuntimeStoryCard 是否还有引用**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli/web-frontend
grep -rn "RuntimeStoryCard" src/
```

若 Task 13 Step 1 删除 `TaskRuntimeMainPanel` 后无其他引用,删除:

```bash
rm src/components/task/RuntimeStoryCard.tsx
```

- [ ] **Step 5: 类型检查 + 构建**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli/web-frontend
npx tsc -b
npm run build
```

预期:PASS。修复所有 "cannot find module" 或未使用 import 错误。

- [ ] **Step 6: Commit(含 Task 12 的 RightRail)**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli
git add -A
git -c commit.gpgsign=false commit -m "refactor(frontend): rewrite RightRail to 3 sections, delete ConversationTimeline/RuntimeStoryCard/TaskRuntimeMainPanel"
```

---

## Phase 7: 全量验证 + 提交

### Task 14: 全量验证 + cargo clean + 最终提交

- [ ] **Step 1: echo-agent 全 feature 矩阵(若 Phase 1 未在 Task 4 完成全部)**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent
cargo check --workspace
cargo test  --workspace
cargo check -p echo_agent --no-default-features --features sqlite
cargo check -p echo_agent --no-default-features --features subagent
cargo check -p echo_agent --no-default-features --features human-loop
cargo fmt --all
cargo clippy --all-targets -- -D warnings
```

- [ ] **Step 2: echo-agent-cli 全量验证**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli
cargo check --workspace
cargo test  --workspace
cargo check --no-default-features --features gui --bin echo-agent-tauri
cargo test  --no-default-features --features gui
cargo fmt --all
cargo clippy --all-targets -- -D warnings
```

- [ ] **Step 3: 前端验证**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli/web-frontend
npx tsc -b
npm run build
```

- [ ] **Step 4: cargo clean 两个子仓库**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent && cargo clean
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli && cargo clean
```

- [ ] **Step 5: 检查 Cargo.toml 相对路径(worktree 规范)**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent
grep -rn "worktrees\|/Users/" echo-agent-cli/Cargo.toml echo-agent-cli/echo-agent-app-core/Cargo.toml echo-agent-cli/echo-agent-eval/Cargo.toml 2>/dev/null
```

预期:零命中。若有 worktree 绝对路径,改回相对路径(`../echo-agent` / `../../echo-agent`)。

- [ ] **Step 6: 合并顺序**

1. echo-agent:Phase 1(Task 1-4)合并到 echo-agent main(若 worktree,先 merge main 再 squash merge)。
2. echo-agent-cli:Phase 2-6(Task 5-13)合并到 echo-agent-cli main(必须在 echo-agent 合并之后,否则编译失败)。

- [ ] **Step 7: 最终验收对照 spec §9**

逐条核对 spec §9 的 10 条验收标准,确认全部满足。

---

## Self-Review 备注

**Spec 覆盖检查:**
- §3.2 一条流 → Task 7-11 ✅
- §3.3 并行 sub-agent 渲染 → Task 9-10 ✅
- §3.3.4 进度摘要 → Task 9 Step 1(workerProgress.ts)✅
- §3.4 按需卡片 → Task 11 保留(计划确认/审批/输入/选择已在 ChatPanel,失败 toast 见下方缺口)
- §4 右侧栏三块 → Task 12 ✅
- §5 markdown 修复 → Task 6 ✅
- §6 删除清单 → Task 13 ✅
- §8 框架解耦 → Task 1-5 ✅

**已知缺口(实现时补):**
- spec §3.4「执行失败 toast」(todos 有 failed 项时主窗口 toast)在 plan 里没有独立 Task。实现 Task 11 后,作为 Task 11 的补充步骤加一个 `<FailureToast />` 组件挂到 ChatPanel。若时间紧可标记为后续 task。
- spec §3.4「计划确认卡」主窗口弹出:当前 plan 把计划确认放在右侧栏(Task 12),主窗口的弹出卡未单独实现。实现时在 ChatPanel 末尾加一个 `{awaitingApproval && <PlanApprovalCard />}` 即可,复用 Task 12 的按钮逻辑。
