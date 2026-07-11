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
| `EKO_BROWSER_STARTUP_TIMEOUT_SECS` | MCP handshake timeout; defaults to 60 seconds. |

## Session model

- `conversation_id` owns one logical `BrowserSession`.
- A main run or subagent execution may lease a tab without changing another
  execution's current tab.
- Writes to one tab are serialized; independent tabs may operate concurrently.
- Run cancellation cancels in-flight browser waits and actions.
- Observations are bounded structured fragments. Complete DOM or accessibility
  trees are not appended to model context.
- Persisted metadata may describe prior URLs and tabs, but a restored
  conversation never claims that an old browser process is still alive.

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

## Chrome extension mode

The Chrome extension is additive and does not replace managed Chromium. Public
pages and localhost use the managed browser by default; existing Chrome is
selected only when a task needs the user's current authenticated state.

The extension uses Manifest V3, a native messaging host, and an EKO connection
manager. It operates only on explicitly authorized tabs/tab groups through
Chrome APIs and a bounded message protocol. EKO does not read cookie databases,
password stores, or Chrome profile files. Releasing a task stops EKO control
without closing pages that the user already owned.

## Verification gates

Each phase must pass Rust formatting, workspace checks/tests, GUI feature
checks/tests, clippy/feature matrix validation, frontend tests/typecheck/build,
and browser-panel Playwright screenshots at desktop and narrow widths when UI
work begins. Each completed phase updates `docs/MASTER-PLAN.md`, runs
`cargo clean`, and is committed independently.
