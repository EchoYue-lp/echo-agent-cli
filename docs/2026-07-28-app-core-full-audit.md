# echo-agent-app-core Full Migration Audit

> Status: **Audit complete; verdicts revised 2026-07-28 after user review**
> Date: 2026-07-28
> Scope: every module in `echo-agent-cli/echo-agent-app-core/src/` (~50 modules)
> Method: 4 parallel deep-verification passes reading actual code (not docstrings),
>   each challenging the default "it's product-specific" verdict and comparing
>   against the real framework counterpart.
> Companion to: `2026-07-28-task-tools-framework-migration-design.md`

---

## Revised Verdicts (supersedes the original findings below)

The user reviewed this audit and pushed back on the original 8 findings. Re-verification
confirmed **3 of the original 8 were wrong** (misread or over-eager). The corrected
conclusion: **only 3 storage migrations are real** (S1/S2/S3). Everything else stays in
the app — some with bug fixes, some deleted as dead code.

| Item | Original verdict | **Corrected verdict** | Action |
|---|---|---|---|
| D1 `instruction_provider` | DUPLICATE (collapses onto framework `InstructionResolver`) | **FALSE ALARM — not a duplicate** | Stays in app. Framework's `project-rules` feature is NOT enabled in EKO (`echo-agent-app-core/Cargo.toml` confirmed), so there is no double-scan. App's tier set (`user.md`/`project.md`/`AGENTS.md`/`local.md`/`MEMORY.md`) is EKO's instruction+memory protocol, not a strict subset of the framework's project-rules resolver. |
| D2 `sensitive.rs` | DUPLICATE (framework `ProtectedPathChecker` wins) | **APP DEAD CODE** (not a duplicate of anything live) | **Delete entirely** — zero callers (`rg` confirmed). Done in Iteration 0. |
| D3 `utils.rs::strip_yaml_frontmatter` | Near-duplicate of framework `parse_frontmatter` | **Different semantics, not a duplicate** | Stays in app. Skill frontmatter parser vs MEMORY.md body-stripper serve different purposes; tighten the app parser's robustness locally instead of migrating. |
| **S1** `runtime_state_file.rs` | SPLIT (file impl of framework trait) | **CONFIRMED — real framework gap** | Migrate `FileRuntimeStateStore` down to `echo-agent/src/state/file.rs`. Fix bugs first (corrupt-JSON errors, path-safe IDs, unique temp names, parent-dir sync). |
| **S2** `conversation_restore.rs` | SPLIT (inverse of `project_message`) | **CONFIRMED — real framework gap** | Migrate `restore_messages` down to `echo-state`. Return `Result`, don't silently demote unknown roles. |
| **S3** `conversation_file.rs::FileConversationStore` | SPLIT (file impl of framework trait) | **CONFIRMED — real framework gap** | Migrate store down to `echo-state`. Fix corrupt-JSON errors + path-safe IDs. `SessionSearchEngine` stays in app. |
| A1 `webhook/emitter.rs` | SPLIT (framework owns config, no impl) | **CONFIRMED STAYS — but app has real bugs** | Stays in app (product delivery policy). Fix: delete global singleton, unify `AppState.webhook.emitter` with real emit calls for ChatCompleted/ToolFailed/AgentError/CronTaskCompleted. |
| A2 `hitl/dispatcher.rs` | SPLIT (generic fan-out composite missing) | **CONFIRMED STAYS — EKO multi-surface arbitration policy** | Stays in app. Fix: shared deadline (not per-provider timeout), cancel remaining futures after first response, clone provider snapshot before await. |
| A3 `config_watcher.rs` | SPLIT (framework has no hot-reload) | **CONFIRMED STAYS — EKO lifecycle capability** | Stays in app. Fix: resettable debounce, parent-dir watch + file filter, explicit reload scope (hooks/webhook live; model/MCP/runtime = "restart required"). Possibly rename to `hooks_config_watcher` if only hooks reload. |

**Final migration scope: only 3 storage migrations (S1/S2/S3).** Plus Iteration 0
dead-code cleanup (`sensitive.rs`, `embedded_server.rs`, `server_pid.rs`, `config.rs`
shim — all deleted) and the bug-fix iterations on app-side instruction/webhook/HITL/
watcher code (no framework changes).

The original "8 findings" section below is retained as history; the table above is
the operative conclusion.

---

## Original Findings (retained for traceability; see revised table above for corrections)

This audit was triggered by the user correctly pointing out that my first pass
gave one-line verdicts for the non-`tasks/` modules. The deep re-verification
**found 8 real issues the first pass missed** — your skepticism was warranted.

The findings split into three buckets:

1. **GENUINE DUPLICATES** (3 modules) — app reimplements something the framework
   already has, often worse. **Fix: collapse onto framework, delete app code.**
2. **CLEAN SPLITS** (3 modules) — generic file-trait impls the framework is
   missing because it only ships SQLite. **Fix: migrate the impl down as a
   framework feature option.** (Same pattern as `FileConversationStore`.)
3. **ASPIRATIONAL SPLITS** (2 modules) — framework owns the config/trait but
   ships no implementation; app fills the gap. **Fix: migrate generic impl down,
   keep product event types.**

Plus the `task_*` migration already designed separately.

Everything else (~40 modules) is **correctly placed** — verified by reading the
code, not trusting docstrings.

---

## 🔴 Bucket 1: Genuine Duplicates (collapse onto framework)

### D1. `instruction_provider.rs` — duplicates framework `InstructionResolver`

**The framework already has a strictly-better version.** `echo-core/src/project_rules.rs`
defines `InstructionResolver` that:

- Scans `.echo-agent/AGENT.md`, `AGENTS.md`, `AGENTS.override.md`, `CLAUDE.md`
  (`project_rules.rs:5-14`)
- Walks root→leaf directories with proper precedence, one file per directory
  (`:64-99`)
- Has symlink-escape protection (`:136`) and project-root boundary enforcement
  (`:70`) — **the app version has NEITHER**
- Integrates into `CanonicalContext` so instructions **survive compression**
  (`react/mod.rs:338-360`) — **the app version is a separate suffix that is LOST
  on compression** (latent bug)
- Already wired into agent build behind the `project-rules` feature
  (`react/mod.rs:676-690`)

The app's `InstructionProvider` (`instruction_provider.rs:18-26`) reimplements
project+local tier loading from `<root>/.eko/project.md` and `<cwd>/.eko/local.md`
— a strict subset of what the framework already does, minus the safety.

**Action:**
- **Delete from app**: `load_project_instructions` (`:89-94`),
  `load_local_instructions` (`:104-108`), and the root/precedence logic. Replace
  with a call to `echo_core::project_rules::rules_injection_with_root`.
- **Keep in app** (product adapter): `load_user_instructions` (`~/.eko/user.md`),
  `load_agents_instructions` (EKO `RulePromoter` output),
  `load_hot_memory` (`.eko/MEMORY.md`), all `save_*` writers.
- **Verify**: `echo-agent-app-core/Cargo.toml` enables the `project-rules`
  feature on `echo-agent`.

**Bonus**: fixes the compression-loss bug for free.

### D2. `sensitive.rs` — duplicates framework `ProtectedPathChecker`

`echo-orchestration/src/human_loop/protected.rs:77` defines
`ProtectedPathChecker` with `DEFAULT_PROTECTED_PATTERNS` (`:24-59`) covering
**the same entries** the app reimplements: `.ssh/id_rsa`, `.aws/credentials`,
`.env`, `*.pem`, `*.key`, `.docker/config.json`, `.pgpass`, etc.

The framework version is the canonical wired-in gate (used by permission/HITL
system). The app's `sensitive.rs:10-50` is a parallel pattern list with a
hand-rolled glob matcher.

**Action:**
- **Delete from app**: `SENSITIVE_PATTERNS` + `is_sensitive_path` +
  `glob_match`/`segment_glob_match` (`:10-159`). Use framework
  `ProtectedPathChecker` everywhere.
- **Migrate down**: `is_outside_project` (`:163`, workdir isolation) — generic
  sandboxing helper, belongs in the framework's protected module.

### D3. `utils.rs::strip_yaml_frontmatter` — near-duplicate of framework parser

Framework `echo-execution/src/skills/external/loader.rs:377` defines
`parse_frontmatter` — a **stricter** parser (validates closing `---` at line
start, rejects mid-document horizontal rules, returns typed errors).

The app's `utils.rs:26-45` is a looser 20-line version that silently returns
the raw string on parse failure.

**Action:**
- Expose a `strip_frontmatter_body` helper on the framework loader module.
- **Delete** the app copy.

---

## 🟡 Bucket 2: Clean Splits — file impls the framework is missing

All three follow the **same pattern**: framework defines the trait, framework
ships only a SQLite impl (gated behind `sqlite` feature), app writes the file
impl because EKO doesn't use SQLite. The file impls are **100% generic** (zero
`.eko` references, zero product concepts) — they are missing framework feature
options, not product code.

### S1. `runtime_state_file.rs` — `FileRuntimeStateStore`

- Implements framework trait `RuntimeStateStore` (`echo-agent/src/state/mod.rs:244`)
  over `<base>/runtime_state/<conv>/nodes.json` + `checkpoint.json`.
- All 6 trait methods implemented; `Mutex` serializes RMW; `atomic_write` does
  tmp+sync+rename; `update_status` explicitly mirrors SQL UPDATE-0-rows semantics.
- **Zero EKO references** — takes `base: impl AsRef<Path>`.
- Framework has `SqliteRuntimeStateStore` under `sqlite` feature but **no
  default/no-deps impl**.

**Action:** Move `FileRuntimeStateStore` + `atomic_write` to
`echo-agent/src/state/file.rs` (sibling of `state/sqlite.rs`), re-export under
a `file` feature (or no feature — only deps are `serde_json`/`futures`). Tests
move with it. App keeps a one-line constructor.

### S2. `conversation_restore.rs` — `restore_messages`

- Single function `restore_messages(&[StoredMessage]) -> Vec<Message>`
  (`:17-56`) — the exact inverse of framework's `project_message`
  (`echo-state/src/memory/conversation.rs:19-48`).
- The `ToolResultMeta` JSON shape it parses is **defined by the framework's own
  projection code** — the app is parsing framework-produced JSON.
- Framework has the forward direction but **lacks the inverse**.

**Action:** Move `restore_messages` + `ToolResultMeta` (renamed, `pub(crate)`)
into `echo-state/src/memory/conversation.rs` next to `project_message`. App
becomes a re-export. Tests move with it.

### S3. `conversation_file.rs::FileConversationStore`

(Prior-round verdict, reconfirmed.) Implements framework `ConversationStore`
trait over `<base>/conversations/<id>.json` + `_meta.json` id counter. Atomic
writes, Mutex-serialized, faithfully reproduces SQL `LIKE`/`LIMIT`/`OFFSET`
semantics. Framework only ships `SqliteConversationStore`.

**Action:** Move to `echo-state/src/memory/file_conversation.rs` next to
`FileStore`. App keeps a one-line constructor.

> Note: `persistence.rs` (the `SavedSession`/`SavedMessage` format with
> `thinking_segments`/`execution_rounds`/attachment data URLs) is a **separate,
> older, frontend-shaped projection** — it stays in the app.

---

## 🟠 Bucket 3: Aspirational Splits — framework owns config/trait, no impl

### A1. `webhook/emitter.rs` — framework owns config, ships no emitter

- Framework `echo-agent/src/config.rs:643-661` defines `WebhooksConfig` /
  `WebhookEntryConfig` (`url`, `events`, `secret`) — but provides **no emitter**.
- App's `WebhookEmitter` (`emitter.rs:51`) does outgoing fire-and-forget POST
  with HMAC-SHA256 signing (`X-Webhook-Signature: sha256=...`), 1 retry,
  event-type filtering.
- **Distinct from** framework's `WebhookHumanLoopProvider` (which is *incoming*
  approval requests, not outgoing notifications).
- The HMAC-signing outgoing-POST-with-retry is generic plumbing parameterized
  over an `Event` type.

**Action:**
- **Migrate down**: a generic `WebhookEmitter<E>` (HMAC-SHA256 + retry + filter +
  endpoint registry) into `echo-orchestration` or a new module.
- **Keep in app**: the concrete `WebhookEvent` enum (`ChatCompleted`, `ToolCalled`,
  `AgentError`, `CronTaskCompleted`) and the global singleton wiring.

### A2. `hitl/dispatcher.rs` — generic fan-out composite missing from framework

- `HitlDispatcher` (`dispatcher.rs:22`) holds `Vec<Arc<dyn HumanLoopProvider>>`
  and implements `HumanLoopProvider` by **broadcasting to all providers in
  parallel** and taking the **first substantive response**, with fail-closed
  default-deny on all-fail/timeout.
- **Zero approval semantics** — pure fan-out over the framework trait.
- Framework has `HumanLoopManager` (single-handler routing) and
  `BatchApprovalProvider` (batching items, not providers). **No multi-provider
  first-resolver composite exists.**
- This is genuinely generic — any multi-surface app (CLI+Web+Tauri) wants it.

**Action:**
- **Migrate down**: `HitlDispatcher` → framework
  `FanOutHumanLoopProvider` / `CompositeHumanLoopProvider` in
  `echo-orchestration/src/human_loop/`.
- **Keep in app**: leaf providers (`repl_provider`, `tui_provider`,
  `channel_provider`) — they bind to EKO's IO surfaces.

### A3. (Bonus) `config_watcher.rs` — generic file watcher, framework has none

- The `notify::RecommendedWatcher` + 500ms debounce + `tokio::select!` cancel
  loop (`config_watcher.rs:74-114`) is **100% generic file-watch machinery**.
- Framework has **zero `notify` dependency** and no hot-reload mechanism.
- Only `handle_config_change` (`:118-139`) is EKO-specific (fires `ConfigChange`
  hook, reloads user hooks).

**Action (lower priority):**
- **Migrate down**: generic `FileWatcher`/`ConfigReloader` (notify + debounce +
  cancel + pluggable reload callback).
- **Keep in app**: `handle_config_change` reload action.

---

## Borderline-reusable machinery (extract ONLY on second consumer)

These are *not currently duplicated* by the framework, but contain generic
patterns worth noting. Per AGENTS.md "拿不准时先留应用层" — **do not migrate
now**; revisit only if a second consumer appears.

| Module | Generic pattern | Why it stays for now |
|---|---|---|
| `browser/mod.rs:1031-1086` | MCP-retry-safety classifier (`PartialSideEffect` distinction) | Browser-specific surface; no other MCP-heavy consumer yet |
| `observability/diagnostics.rs:347-383` | Cache-fingerprint-instability detector | Tied to EKO's diagnostic SLO thresholds |
| `project/prompt.rs` | `PromptAssembler` (budget-aware module composition) | Bound to EKO's `CORE_ASSISTANT_PROMPT` module set |
| `prompt_contract.rs::audit_prompt` | Prompt-contract auditing | Only caller is EKO |
| `skills_hub/install.rs:404-549` | Atomic skill-dir install + content hashing | Distribution is app-tier concern |

---

## ✅ Confirmed correctly placed (~40 modules)

Reading the actual code confirmed these are genuinely EKO-product or correct
adapters. Highlights:

**Agent/runtime glue:**
- `runtime.rs` — EKO bootstrap composing product collaborators; **not** a
  duplicate of `ReactAgentBuilder` (~40 framework builder calls confirm the
  builder exposes the right extension points). Minor opportunity: LSP
  auto-discovery (`register_lsp_tools:617-714`) could move down since
  `LspManager` is already framework-side.
- `chat_resources.rs` — **broken duplicate** of framework `ToolContext`/
  `ExternalRunContext` (uses cross-spawn-unsafe `task_local!` for fields
  including `cancel`). Stays in app (fields are EKO types) but should read
  `conv_id`/`message_id`/`cancel` from `ToolContext` where possible.
- `agent_handle.rs` — 1-line re-export; fine.
- `agent_pool.rs`, `tool_execution.rs`, `subagent_loader.rs`, `subagent_prompt.rs`
  — prior-round deep-dives confirmed PRODUCT/ADAPTER.

**Memory/persistence:**
- `auto_memory/mod.rs` — **ADAPTER already split**; kernel
  (`extract_observations`) already in framework, app routes to evidence inbox.
- `workspace/` folder — challenged hard; holds. Generic skeleton
  (`WorkspaceId`/registry pattern) is small and welded to EKO's concrete
  `WorkspaceKind` enum + layout catalogue + templates + migration. Extraction
  would be a new framework *design*, not a migration.
- `workspace_routing.rs` — trivial `match` glue on framework APIs; substance is
  EKO skill lists + prompts.
- `unified_memory.rs` — EKO memory tiering (`AGENTS.md`/`MEMORY.md` semantics).
- `server_pid.rs` — app-tier daemon infrastructure, not framework kernel.

**Config:**
- `config.rs` — already a 5-line re-export shim; could delete.
- `hooks_config.rs` — **clean ADAPTER**: loads framework `HooksDefinition`,
  calls framework `merge()`; zero reimplementation.
- `config_discovery.rs`, `model_config.rs`, `profiles/` — PRODUCT (EKO paths,
  UI views, config presets; `profiles/` is **unrelated** to framework
  `echo-state/profiles.rs` telemetry profiles — do not conflate).

**Channels/IO/UI:**
- `output/`, `diff.rs`, `context_window.rs` — **zero framework dependency**;
  self-contained UI code. Framework correctly stays UI-neutral.
- `embedded_server.rs` — **currently dead code** (no caller, no routes
  anywhere in CLI). Flag to team before investing.
- `hitl/` leaf providers (`repl_provider`, `tui_provider`, `channel_provider`)
  — PRODUCT (bind to EKO IO surfaces).

**Product features:** `research.rs`, `research_connectors.rs`, `research_tool.rs`,
`analysis.rs`, `export/`, `browser/`, `project/`, `skills_hub/`, `evolution/`,
`observability/`, `sessions/`, `scheduler/`, `attachments.rs`, `error.rs`,
`infra.rs`, `types/`, `prompt_contract.rs` — all verified PRODUCT or ADAPTER.
Notable clean delegations:
- `analysis.rs:421` calls framework `run_code` tool (no sandbox reimpl).
- `evolution/` composes framework `MemoryLayerManager`/`Dreaming` (no reimpl).
- `scheduler/` re-exports framework `CronTask`/`SchedulerRunner`; only
  `build_fire_fn` is EKO-routing.
- `research_connectors.rs` adapts framework `tools::research` clients.

---

## Prioritization

If you want to act on this audit, here's the ranked order (independent of the
`task_*` migration, which proceeds on its own track):

| Priority | Item | Bucket | Risk | Value |
|---|---|---|---|---|
| **P1** | `instruction_provider.rs` project+local tiers → framework `InstructionResolver` | D1 | Low | High (also fixes compression-loss bug) |
| **P1** | `runtime_state_file.rs` → framework `state/file.rs` | S1 | Low | High (fills missing no-SQLite feature) |
| **P1** | `conversation_file.rs::FileConversationStore` → framework `echo-state` | S3 | Low | High (same pattern) |
| **P2** | `conversation_restore.rs::restore_messages` → framework `echo-state` | S2 | Low | Medium (symmetry with `project_message`) |
| **P2** | `sensitive.rs` → retire in favor of framework `ProtectedPathChecker` | D2 | Low | Medium (dedup) |
| **P2** | `utils.rs::strip_yaml_frontmatter` → delete, use framework parser | D3 | Trivial | Low |
| **P3** | `webhook/emitter.rs` generic → framework | A1 | Medium | Medium (asymmetric gap) |
| **P3** | `hitl/dispatcher.rs` → framework `FanOutHumanLoopProvider` | A2 | Medium | Medium (generic composite) |
| **P4** | `config_watcher.rs` generic watcher → framework | A3 | Medium | Low (no second consumer yet) |

P1 items are the cleanest, lowest-risk wins — three small migrations that fill
real framework gaps. P2 is type/file hygiene. P3/P4 are design judgment calls
(generic composites with no current second consumer — defensible either way).

---

## Methodology note

This audit used 4 parallel `Explore` subagents, each instructed to **challenge
the default "PRODUCT" verdict** and read actual code with file:line evidence.
The first round's one-line verdicts were replaced where the deep read changed
the conclusion. The 8 findings above are the delta — modules where the shallow
read said "PRODUCT/ADAPTER, stays" but the deep read found a real duplicate,
missing framework feature, or asymmetric gap.

The remaining ~40 modules were reconfirmed as correctly placed by the deep read.
No silent "leave it" verdicts remain — every module has been read, not just
docstring-scanned.
