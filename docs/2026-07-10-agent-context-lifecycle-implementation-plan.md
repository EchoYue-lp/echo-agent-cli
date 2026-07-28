# Agent Context Lifecycle Repair Implementation Plan

> **For agentic subagents:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make main-agent recovery state replaceable and current at every model call, preserve clean subagent task isolation, and make GUI context/usage diagnostics accurate and inexpensive.

**Architecture:** `echo-agent` provides generic tagged context projections, idempotent canonical re-injection, and protected-message diagnostics. `echo-agent-cli` supplies the EKO-specific TaskRuntime projection and keeps TaskRuntime file storage authoritative. The frontend consumes provider baselines and canonical usage events rather than reconstructing backend state from rendered history.

**Tech Stack:** Rust, Tokio, echo-core/echo-state/echo-agent, React 19, TypeScript 5.8, Zustand, Vitest.

## Global Constraints

- EKO-specific run/plan/todo concepts remain in `echo-agent-cli`; framework APIs contain no EKO types.
- No SQLite dependency or schema.
- All Rust string truncation remains UTF-8 safe and no new panic-prone APIs are introduced.
- Fork subagents already enter through `StreamMode::Execute`, which clears prior messages before each invocation. Add regression coverage; do not add a post-run reset that can race the next dispatch.
- Do not create a git commit unless the user explicitly requests one.

---

### Task 1: Generic pre-model projections and compression invariants

**Files:**
- Modify: `../echo-agent/echo-core/src/compression.rs`
- Modify: `../echo-agent/echo-state/src/compression/mod.rs`
- Modify: `../echo-agent/src/agent/react/run/phases/compact.rs`
- Modify: `../echo-agent/src/agent/react/run/snapshot.rs`
- Test: `../echo-agent/echo-state/src/compression/mod.rs`
- Test: `../echo-agent/src/agent/react/run/phases/compact.rs`

**Interfaces:**
- Produce `ContextProjection { marker: String, message: Option<Message> }`.
- Produce async trait `PreModelContextProjector::project(&ProjectionContext) -> Result<Vec<ContextProjection>>`.
- Add `ContextManager::apply_projections(&[ContextProjection])`.
- Add `ContextManager::protected_message_count() -> usize`.

- [x] Write failing echo-state tests proving repeated projection application leaves one message and `None` removes stale projection.
- [x] Run `cargo test -p echo_state context_projection -- --nocapture`; verify failures are caused by missing projection APIs.
- [x] Implement marker-based remove-then-insert projection semantics. Insert projection messages at the system/history boundary without interpreting marker contents.
- [x] Write failing tests proving canonical system context is not duplicated and force-compression checkpoints match final `messages.len()` and `token_estimate()`.
- [x] Run the canonical tests and verify the duplicate/count assertions fail on current code.
- [x] Make canonical re-injection idempotent and finalize `retained_count`/`token_after` after re-injection in `prepare` and all three force-compression methods.
- [x] Wire the optional projector through `AgentRunSnapshot` immediately before the single production `ContextManager::prepare` call.
- [x] Add a phase test whose projector changes between two iterations and verify the second prepared message list contains only the new projection.
- [x] Run `cargo test -p echo_state` and the focused `echo_agent` compact-phase tests.

### Task 2: EKO TaskRuntime projection and Rust runtime metadata

**Files:**
- Modify: `echo-agent-app-core/src/tasks/task_runtime/compact_context.rs`
- Modify: `echo-agent-app-core/src/chat_driver.rs`
- Modify: `echo-agent-app-core/src/infra.rs`
- Modify: `echo-agent-app-core/src/tasks/task_runtime/task_tools.rs`
- Modify: `echo-agent-app-core/src/tasks/task_runtime/task_execute_tool.rs`
- Modify: `echo-agent-app-core/src/tasks/task_runtime/executor.rs`
- Modify: `src/tauri/commands/panels.rs`

**Interfaces:**
- Implement `TaskRuntimeContextProjector` in the application, capturing `Arc<TaskRuntimeStore>` and the active `run_id`.
- Tool results contain ordinary status text only; no recovery marker.
- Runtime contract uses `isolation_requested`; observed fallback emits `isolation_observed = "primary-fallback"`.

- [x] Add failing capsule tests: replacing an existing run with a no-plan run removes the old capsule; repeated refresh leaves exactly one current capsule.
- [x] Add failing tool tests asserting `task_create` and every `task_execute` outcome omit `RUNTIME_RECOVERY_MARKER`.
- [x] Register/replace the per-turn projector in `drive_chat_inner`; remove one-shot upsert and remove `append_runtime_recovery_capsule`.
- [x] Keep `[task_context]` protection for the current Fork invocation. Add a framework regression test confirming two sequential `execute_stream` calls reset prior task messages; make no production reset change if the test passes.
- [x] Add failing evidence tests for Markdown links, `path:start-end`, URL rejection, and slash prose such as `and/or`.
- [x] Tighten extraction to explicit Markdown destinations and credible file references with a filename extension or recognized relative/absolute path prefix.
- [x] Add requested/observed isolation fields to execution events and emit the observed primary fallback when writer dispatch falls back.
- [x] Replace hardcoded protected marker checks in `get_compression_stats` with `ContextManager::protected_message_count()`.
- [x] Run focused app-core tests for `compact_context`, `task_tools`, `task_execute`, and `evidence_path`.

### Task 3: Pure frontend context/usage helpers with red-green tests

**Files:**
- Create: `web-frontend/src/components/chat/contextUsage.ts`
- Create: `web-frontend/src/components/chat/contextUsage.test.ts`
- Create: `web-frontend/src/components/compress/subagentUsage.ts`
- Create: `web-frontend/src/components/compress/subagentUsage.test.ts`
- Modify: `web-frontend/package.json`
- Modify: `web-frontend/package-lock.json`

**Interfaces:**
- `estimateDraftTokens(draft, pendingFiles) -> number`.
- `computeContextUsage(reportedTokens, draftTokens, windowSize) -> { used, pct, source, tier }`.
- `isCanonicalUsageEvent(event) -> boolean`.
- `summarizeSubagentUsage(runs, activeConversationId) -> totals`.

- [x] Install the latest Vitest with `npm install --save-dev vitest` and add `"test": "vitest run"`.
- [x] Write failing tests proving reported tokens plus draft are used, null reported state never scans/reconstructs history, and compression reset returns draft-only state.
- [x] Implement the minimal pure context helper and run its tests green.
- [x] Write failing tests proving thinking-ended usage is excluded, duplicate canonical events are not counted, and runs from other conversations are excluded.
- [x] Implement canonical-event and conversation filtering helpers and run their tests green.

### Task 4: Frontend integration and truthful diagnostics

**Files:**
- Modify: `web-frontend/src/components/chat/ChatInput.tsx`
- Modify: `web-frontend/src/components/compress/CompressPanel.tsx`
- Modify: `web-frontend/src/components/task/SubagentDetailView.tsx`
- Modify: `web-frontend/src/components/task/TaskRuntimePanel.tsx`
- Modify: `web-frontend/src/stores/subagentRunStore.ts`
- Modify: `web-frontend/src/types/api.ts`
- Modify: `src/tauri/mod.rs`

**Interfaces:**
- `DispatchThinkingEnded` maps to `thinking_ended`, not `usage`.
- Store `usageEvents` contains only canonical LLM usage events.
- Runtime state carries `isolationRequested` and `isolationObserved`.

- [x] Change the Tauri bridge mapping and add/extend a Rust bridge test that distinguishes `thinking_ended` from canonical `usage`.
- [x] Remove the `messages` subscription and transcript traversal from `ChatInput`; call the pure helper with provider snapshot plus draft/pending files.
- [x] Filter CompressPanel runs by the active conversation and use canonical usage summaries.
- [x] Make TaskRuntimePanel consume canonical `usageEvents` instead of all event records.
- [x] Render absent prompt/context/return/isolation contract fields as `unknown`; display requested and observed isolation separately.
- [x] Run `npm test`, `npx tsc -b`, and `npm run build`.

### Task 5: Full verification and cleanup

**Files:**
- Verify all modified files in both repositories.

- [x] Run `cargo fmt --all` and `cargo fmt --all -- --check` in `echo-agent`.
- [x] Run `./scripts/verify-all-crates.sh` in `echo-agent`, including its feature matrix.
- [x] Run `cargo fmt --all` and `cargo fmt --all -- --check` in `echo-agent-cli`.
- [x] Run `cargo check --workspace`, `cargo test --workspace`, GUI check/test commands, and clippy with warnings denied in `echo-agent-cli`.
- [x] Run frontend `npm test`, `npx tsc -b`, and `npm run build`.
- [x] Inspect `git diff` in both repositories for accidental changes, absolute worktree paths, panic-prone APIs, and stale comments.
- [x] Run `cargo clean` in both Rust repositories after every verification command passes.
- [x] Report results without committing.
