# Browser Runtime Design

## Goal

Give EKO a first-class browser workspace while keeping browser process state,
task/session ownership, UI projection, and consequential-action confirmation in
the `echo-agent-cli` application layer.

The implementation proceeds in independently committable stages:

1. remove obsolete browser entry points and normalize Playwright MCP config;
2. manage a Playwright MCP sidecar;
3. add task-scoped browser sessions and a unified event stream;
4. add the GUI browser workspace panel;
5. add DOM/visual fallback and optional CDP diagnostics;
6. add consequential-action confirmation and an authorized Chrome extension.

## Industry references

- [Codex browser](https://learn.chatgpt.com/docs/browser) uses an isolated
  browser profile and combines DOM-based interaction, screenshots, and CDP
  developer capabilities.
- [Codex Chrome](https://learn.chatgpt.com/docs/chrome-extension) connects an
  extension to the user's existing Chrome profile for authorized tabs and
  authenticated browsing.
- [Playwright MCP](https://github.com/microsoft/playwright-mcp) provides the
  initial browser execution engine so EKO does not duplicate semantic locators,
  navigation waits, tab handling, or page lifecycle behavior.
- [Claude Code hooks](https://docs.anthropic.com/en/docs/claude-code/hooks)
  distinguish a stable `session_id`, per-prompt `prompt_id`, subagent
  `agent_id`, and tool `tool_use_id`. Codex's JSON event/runtime model likewise
  separates a persistent thread, each turn, and items inside the turn.

The shared pattern is to keep browser execution outside the agent loop, expose
structured browser actions and observations as tools/events, and treat an
existing signed-in Chrome profile as a separate, explicitly authorized mode.

## Ownership boundary

### `echo-agent-cli` application responsibilities

- Start, health-check, restart, and stop the Playwright MCP sidecar.
- Own browser profiles, processes, sessions, tabs, leases, and screenshots.
- Bind a browser session to an EKO conversation and temporary tab leases to a
  run or subagent execution.
- Project browser events into TUI, GUI, CLI, and channel renderers.
- Classify browser actions and request confirmation through EKO's existing HITL
  path when an action has a real external side effect.
- Store only lightweight file-backed metadata; live browser processes are never
  reconstructed from conversation history.

### `echo-agent` framework responsibilities

- Reuse the existing generic Tool, MCP, event, cancellation, and subagent
  primitives.
- Remain unaware of EKO browser sessions, tab ownership, UI state, domain
  policy, Chrome authorization, and browser-specific approval rules.

No SQLite dependency or schema is introduced. Browser controls are available
equally to TUI, GUI, CLI/channel, the main agent, and eligible subagents; only
their rendering differs.

## Phase 0: obsolete entry-point cleanup

The previous `BrowserTool` was an unexported, unregistered stub that always
returned an error. It is removed instead of being advertised as a capability.
Manual MCP configuration remains supported through the existing MCP loader,
using the official package:

```json
{
  "mcpServers": {
    "playwright": {
      "command": "npx",
      "args": ["-y", "@playwright/mcp@latest"]
    }
  }
}
```

## Managed runtime

`BrowserRuntime` is an application-owned service shared by all interaction
modes. Its first implementation wraps a managed Playwright MCP sidecar:

```text
BrowserRuntime
  BrowserSidecar
  BrowserSessionManager
    BrowserSession (conversation_id)
      BrowserTab (leased by run_id/execution_id)
  BrowserEventSink
```

Sidecar startup detects Node/npm and the Playwright MCP package, uses an
independent browser data directory, waits for readiness, and restarts after an
unexpected exit. A sidecar failure produces browser failure events but does not
terminate the chat run or application.

The first managed tool set is navigation, snapshot, click, fill, screenshot,
back, reload, and tab management. Tool injection must use the same construction
path for main and subagents so interaction modes remain functionally equal.

### Phase 1 implementation

Phase 1 uses one application-owned `BrowserRuntime` and one Playwright MCP stdio
client. The framework's public `McpClient` remains the protocol/process primitive;
EKO owns prerequisite checks, managed paths, stable tool names, connection
serialization, restart policy, and shutdown.

- Startup checks Node.js 18+, npm, and npx, then creates
  `~/.echo-agent/browser/profiles/managed` and
  `~/.echo-agent/browser/output`.
- Sidecar startup is prewarmed in the background so an unavailable npm registry
  does not block chat startup. The first browser call waits for or creates the
  same connection.
- One client is shared by the primary agent, built-in subagents, and pooled
  conversation/task agents. This avoids multiple Playwright processes trying to
  lock the same profile.
- A transport/tool failure invalidates the client, closes the failed sidecar,
  serializes one restart, and retries the action once. Browser failure returns a
  tool error and does not terminate the chat run.
- TUI/CLI/channel and GUI shutdown paths explicitly close the managed sidecar.

The stable Phase 1 EKO tool contract is:

```text
browser_navigate
browser_snapshot
browser_click
browser_fill
browser_screenshot
browser_back
browser_reload
browser_tabs
```

Where Playwright MCP uses different names, the application adapter translates
them (`browser_fill` to `browser_type`, `browser_screenshot` to
`browser_take_screenshot`, and `browser_back` to `browser_navigate_back`).

Environment overrides:

| Variable | Purpose |
| --- | --- |
| `EKO_BROWSER_ENABLED` | Enable/disable the managed runtime; defaults to enabled. |
| `EKO_BROWSER_HEADLESS` | Run Playwright headless; defaults to headed. |
| `EKO_BROWSER_NODE` / `EKO_BROWSER_NPM` / `EKO_BROWSER_NPX` | Override prerequisite executable names. |
| `EKO_BROWSER_MCP_PACKAGE` | Override `@playwright/mcp@latest`. |
| `EKO_BROWSER_PROFILE_DIR` | Override the managed browser profile path. |
| `EKO_BROWSER_OUTPUT_DIR` | Override browser output path. |
| `EKO_BROWSER_SESSION_DIR` | Override lightweight browser session metadata path. |
| `EKO_BROWSER_STARTUP_TIMEOUT_SECS` | MCP handshake timeout; defaults to 60 seconds. |
| `EKO_BROWSER_EXTENSION_ENABLED` | Enable the official Playwright Extension backend; defaults to enabled. |
| `EKO_BROWSER_EXTENSION_TOKEN` | Optional token configured in the Playwright Extension to bypass repeated connection approval. |

## Session model

- Identity is layered rather than collapsed: `conversation_id` is stable across
  turns, `turn_id` identifies one user prompt, `run_id` exists only for an
  actual formal/background/inline task run, and `execution_id` identifies a
  concrete subagent execution. An ordinary chat turn does not create a
  `TaskRuntimeStore` run record.
- `conversation_id` owns one logical `BrowserSession`.
- The main agent always leases the conversation's main tab across turns. A
  subagent leases a separate tab keyed by `execution_id`, falling back to the
  formal run or turn id only for legacy callers.
- Playwright MCP's stdio connection exposes one selected tab for the whole
  browser context. EKO therefore holds a context-level atomic operation lock
  around select-tab plus action. This safely prevents a subagent from redirecting
  the main agent's action, but does not claim true same-context tab concurrency.
  Per-tab concurrent writes require a future driver with independent page
  handles/contexts (the DOM/CDP phase), not additional mutex bookkeeping.
- Run cancellation cancels in-flight browser waits and actions.
- Observations are bounded structured fragments. Complete DOM or accessibility
  trees are not appended to model context.
- Persisted metadata may describe prior URLs and tabs, but a restored
  conversation never claims that an old browser process is still alive.

### Phase 2 implementation

`BrowserSessionManager` owns in-memory sessions, tab leases, a bounded broadcast
event stream, and JSON metadata under `~/.echo-agent/browser/sessions`. Metadata
contains only session/tab identity, URL/title, status, and timestamps. It does
not contain DOM snapshots, screenshots, cookies, authorization headers, or form
values. Restored records are always marked `closed`; the next browser use starts
a fresh live session id.

Managed browser tools consume framework `ToolContext` through
`execute_with_context`. The generic context now carries optional `run_id` plus
`conversation_id`, `turn_id`, and `execution_id` across spawned subagents. This
is a generic invocation identity correction in `echo-agent`; browser ownership,
leases, persistence, and events remain entirely in `echo-agent-cli`.

The runtime emits session, tab, navigation, snapshot, screenshot, action, and
close events. Backend events are authoritative for URL, tab, status, and frame
identity; the GUI does not independently infer browser state.

## GUI workspace

The GUI adds a collapsible right-side browser panel coordinated with the
existing diff/right rail. The initial viewport is screenshot-driven rather
than video-driven: capture after navigation and actions, refresh at a bounded
idle rate where needed, and allow manual refresh.

The panel contains browser-native controls, tabs, URL/status state, the latest
frame, active-target highlighting, and loading/error/disconnected/confirmation
states. It is an unframed workspace region, not a card nested in another card.

### Phase 3 implementation

`BrowserRuntime` remains the single authority for browser actions. Tauri holds
the application-owned runtime, exposes thin GUI commands for navigation, back,
reload, stop, screenshot refresh, and tab operations, and forwards the existing
`BrowserEvent` stream over `browser://event`. GUI commands call the same runtime
path as agent tools and therefore reuse conversation-scoped sessions and the
main-tab lease instead of creating a second browser controller.

Screenshot events include an optional bounded `data:` frame. Successful
navigation, click, fill, back, and reload actions capture a best-effort frame
before emitting completion; an explicit screenshot also emits its image. Raw
frames larger than 8 MiB are omitted from the event rather than growing IPC and
frontend memory without limit. Screenshots remain ephemeral and are not written
to conversation/session metadata.

The frontend `browserStore` keys views by stable `conversation_id` and merges
session/tab/navigation/action/screenshot events by `session_id`. Per-turn
`run_id` changes therefore do not reset the browser workspace. The original
Phase 3 desktop panel used a constrained 360-680 px split and kept the task rail
as a sibling surface. The later unified workspace section replaces that layout
while retaining the screenshot-driven runtime and deliberately disabled forward
navigation until the managed runtime exposes that action.

## DOM and visual control

Action resolution follows this order:

1. accessibility/DOM role, label, and test id;
2. stable CSS or DOM attributes;
3. screenshot understanding and coordinate actions;
4. optional session-level CDP developer mode.

After an action, the runtime verifies at least one observable result: URL,
target DOM state, or screenshot. Repeated failure with the same locator is
bounded and then falls back or returns a structured error. Console, network,
DOM inspection, and performance traces are diagnostic observations, not
unbounded context dumps.

### Phase 4 implementation

The implementation follows the actual `@playwright/mcp` capability contract
(reviewed against the official package README, locally resolved as 0.0.78):
core automation supplies accessibility snapshots, targeted `browser_find`,
console messages, and network request lists; the opt-in `vision` capability
supplies coordinate mouse input; the opt-in `devtools` capability supplies
element highlights and Playwright traces. EKO enables `vision,devtools` on the
managed sidecar but continues to expose its own stable application-level tool
names.

The Phase 4 tools are:

```text
browser_click_at
browser_type_at
browser_scroll
browser_console
browser_network
browser_dom_inspect
browser_performance_trace
browser_developer_mode
```

Semantic `browser_click`/`browser_fill` remain the default. Their target is the
snapshot ref or unique stable selector accepted by Playwright MCP. A target is
keyed by session, tab, action, and locator; after two failures the same locator
is rejected with guidance to inspect a fresh bounded DOM fragment or use the
coordinate fallback. Application-level MCP errors (`is_error`) now enter the
failure path instead of being emitted as successful actions.

Coordinate typing focuses the requested point and emits UTF-8-safe key presses,
bounded to 500 characters. Coordinate click, type, and scroll all trigger the
same post-action screenshot verification used by semantic actions. Semantic
click/fill briefly use the DevTools highlight overlay so the GUI screenshot
shows the active target, then remove it.

`browser_dom_inspect` uses `browser_find` for text/regex snippets or a bounded
target/depth/box snapshot. Console and network results use the existing 12K
observation limit. Network diagnostics intentionally expose request lists only,
not full request headers or bodies. Console/network text is filtered for common
authorization, cookie, token, and password markers, and structured payloads are
dropped before the result reaches the model or GUI.

Developer Mode is a lightweight per-conversation flag stored with browser
session metadata. It gates `browser_performance_trace`; it is not folded into
ordinary browser actions or global agent permission mode. Trace start/stop uses
Playwright MCP's supported DevTools tools rather than introducing an EKO CDP
protocol or framework-layer browser state.

## Confirmation and trust model

EKO is a local personal assistant. Ordinary browsing is not gated by agent
`permission_mode`: navigation, reading, scrolling, normal links, and filling an
unsubmitted form work in the default mode.

`BrowserActionRisk` requests confirmation only for consequential actions such
as purchase/payment, publishing or sending messages, account/permission
changes, cloud deletion, sensitive form submission, or automatically executing
a downloaded file. Confirmation uses the existing HITL event path.

Domain allow/block configuration belongs to the browser runtime and is not a
replacement for agent permission mode. Page content is untrusted input. Logs
must redact cookies, authorization headers, form secrets, and equivalent
credentials.

### Phase 5 implementation

The implementation follows Codex Browser's action-time confirmation pattern:
reading and ordinary interaction stay available, while a concrete operation
that sends data or changes external state presents the destination and data
categories immediately before execution. EKO uses the same separation because
static tool-level permissions cannot distinguish a harmless link click from a
purchase button. Browser risk therefore remains application-owned and does not
change `echo-agent` permission semantics.

`BrowserActionRisk` classifies effect-capable click/fill/coordinate actions as
`none`, sensitive submission, purchase/payment, publish, send message, account
change, permission change, or cloud deletion. The effect is an explicit tool
argument rather than an inference from untrusted page text. A consequential
effect is valid only on a committing click, submitted fill, or coordinate type
that presses Enter. Ordinary actions use `effect: none` and never enter HITL,
including under the default permission mode.

Consequential actions use the existing `HumanLoopProvider`. The shared runtime
dispatcher covers TUI/CLI/channel operation, while GUI turns install their
conversation-scoped Tauri provider so concurrent conversations do not race.
Confirmation events project `waiting_confirmation` into the Browser Panel.
Rejection restores the session to `Ready`; it rejects only the proposed action
and does not falsely mark the browser process as failed.

The confirmation payload contains only risk, action name, a bounded summary,
destination, and names of data categories. Form text and locator arguments are
never forwarded to HITL or Playwright as confirmation metadata. Summaries are
UTF-8 safely limited to 300 characters and common authorization/cookie/token
markers are redacted. Schema descriptions explicitly prohibit secret values.

`EKO_BROWSER_ALLOWED_DOMAINS` and `EKO_BROWSER_BLOCKED_DOMAINS` provide optional
comma-separated browser navigation policy. Block rules win over allow rules,
exact hosts and subdomains match, and an empty allow list preserves the local
assistant default of unrestricted browsing. This policy is independent of
agent `permission_mode`.

## Chrome extension mode

The Chrome backend is additive and does not replace managed Chromium. Public
pages and localhost use the managed browser by default; existing Chrome is
selected only when a task needs the user's current authenticated state. EKO
does not read cookie databases, password stores, or Chrome profile files.

### Phase 6 implementation

The implementation follows the official
[Playwright MCP extension mode](https://github.com/microsoft/playwright-mcp#extension-mode).
`@playwright/mcp@latest --extension` connects to the user's existing Chrome or
Edge session through the official
[Playwright Extension](https://github.com/microsoft/playwright/tree/main/packages/extension#readme).
This preserves logged-in state while retaining the same MCP accessibility
snapshot and action protocol used by managed Chromium. Codex's browser model
provides the product-level precedent: use an existing signed-in browser only
when authenticated state is needed, while keeping an isolated browser as the
default for public pages and localhost.

`browser_backend` explicitly selects `managed` or `chrome` for one
conversation. The selection is stored in lightweight `BrowserSession` metadata
but restored sessions always return to managed mode because a historical file
cannot prove that the extension connection or selected tab remains live.
Switching backends resets EKO's synthetic tab-index mapping so stale indices
cannot target a different browser. Navigation, snapshots, semantic and
coordinate input, screenshots, diagnostics, tracing, and tab operations all go
through the same adapter and MCP tool names on both backends.

The application owns two independent sidecars: managed mode supplies a private
profile and output directory; Chrome mode supplies `--extension` and no profile
path. The optional `EKO_BROWSER_EXTENSION_TOKEN` is forwarded only as
`PLAYWRIGHT_MCP_EXTENSION_TOKEN`. Without it, Playwright presents its own tab
selection and approval flow. Startup and connection failures remain visible in
`chrome_setup_status` instead of silently appearing connected.

EKO intentionally does not maintain a parallel Chrome control protocol. The
former custom Manifest V3 extension, Native Messaging host, endpoint file, DOM
injection selectors, and CDP allowlist were removed. Delegating those details
to the official implementation reduces duplicate browser semantics and keeps
managed and signed-in browsing behavior aligned as Playwright evolves.

The initially selected Chrome tab is treated as user-owned by EKO's session
projection and cannot be closed through `browser_tabs`. Tabs created after the
connection may be closed normally. Closing or changing the originally selected
tab remains an explicit user action in Chrome.

## Unified preview workspace and controlled editing

The GUI now presents tasks, browser, and files in one right-side workspace
instead of competing drawers and nested preview tabs. Chat-header buttons open
the intended destination directly, and the collapse button is labelled
independently from browser actions. On narrow screens the workspace becomes a
full-width overlay above the left navigation, so browser and file controls
remain operable.

The web preview works without an active conversation by using a workspace-level
UI scope. URL submission, reload, screenshot refresh, backend changes, and tab
actions surface command errors instead of failing silently. Managed sessions
refresh their screenshot at a bounded 1.5-second interval only while the panel
is mounted and ready. Chrome remains an explicit user or agent choice; EKO never
switches from managed browsing based on the URL or login-page detection.

Selecting `Connect Chrome...` opens a setup flow that reports the extension MCP
state, links to the official Chrome Web Store listing, and starts Playwright's
tab selection/approval flow. Switching back to managed mode drops the extension
sidecar's active EKO session mapping but does not close a user-owned tab.

The file preview uses workspace-relative Tauri commands for the tree, Git
changes, file content, and diffs. Text, images, PDFs, unsupported binaries, and
deleted/untracked Git states have explicit render paths. Text files can enter an
optional CodeMirror edit mode; preview remains the default. Saving sends the
SHA-256 revision that was read, writes through a unique temporary file and
rename, and rejects stale saves if an agent or external editor changed the file.
Dirty drafts are preserved and shown as conflicts rather than overwritten.

This is intentionally a controlled cowork editor, not a second full IDE:
project inspection and review remain lightweight, while small corrections can
be made without leaving EKO. Larger refactors still belong to agent tools or an
external editor. TUI exposes the same product capabilities through
`/preview`, `/edit`, and `/browser [status|managed|chrome]`; rendering differs,
but browser/backend selection and file access are not GUI-only product logic.

## Verification gates

Each phase must pass Rust formatting, workspace checks/tests, GUI feature
checks/tests, clippy/feature matrix validation, frontend tests/typecheck/build,
and browser-panel Playwright screenshots at desktop and narrow widths when UI
work begins. Each completed phase updates `docs/MASTER-PLAN.md`, runs
`cargo clean`, and is committed independently.
