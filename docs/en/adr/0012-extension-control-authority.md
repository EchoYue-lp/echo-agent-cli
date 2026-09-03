# ADR-0012: EKO Extension Control Authority

## Status

Accepted, 2026-08-25. Implemented, 2026-08-26. The Skill durable-settlement
state machine is superseded by [ADR 0036](./0036-skill-policy-simplification.md);
the single Extension authority, mutation mutex, and shared-surface contract remain.

## Context

EKO already has specialist extension owners. The framework owns `SkillRegistry`,
`HookRegistry`, MCP/LSP protocols and their reusable managers. The application
owns `McpConfigRuntime`, `PluginRuntimeService`, `BrowserRuntime`, workspace
focus, local configuration and AgentPool publication. Product surfaces had also
grown independent mutation paths: CLI MCP commands could print without a real
reconcile, several surfaces treated `SkillsHub.loaded` as live truth, Hook/LSP
reload could derive project identity from process cwd, MCP health could be tied
to a bootstrap Agent, and output-style changes did not cover every existing and
future pooled Agent.

The first coordinator implementation removed several of those direct paths, but
Skill enablement still applied runtime changes before writing
`enabled-skills.json`, attempted in-memory rollback on failure, and returned an
untyped list/error result. A cancelled caller could also cancel work that had
already been accepted. That behavior cannot represent a durable commit which
later fails to reach one runtime target.

This is an EKO product lifecycle problem. Workspace focus, local extension
configuration, plugin preferences, settlement receipts and surface rendering do
not belong in the reusable `echo-agent` framework.

## Evidence and industry research

The design reviewed the checked-in [Codex capability catalog](./0002-codex-tool-capability-catalog.md)
and [Claude Code capability catalog](./0003-claude-code-capability-catalog.md).
They distinguish discoverable extension packages, durable enablement and live
tool availability, while sharing extension behavior across product surfaces.

The fixed-commit
[Codex app-server protocol](https://github.com/openai/codex/blob/fde2156057c38c0227ce94c8514d04c7498df60d/codex-rs/app-server/README.md)
also separates Thread/Turn/Item lifecycle from Skill configuration, Hook listing
and Plugin/MCP runtime state. Cursor's
[Skills](https://cursor.com/docs/skills) and [MCP](https://cursor.com/docs/mcp)
documentation separates discovery/scope/configuration from live server/tool
state. EKO adopts that ownership separation rather than copying product-specific
command names. The fixed Codex commit and checked-in catalogs are the review
snapshots; this ADR does not claim that unpinned online behavior is immutable.

The cross-system pattern used here is deliberately narrow:

1. discovery, desired configuration and live execution state are distinct;
2. an accepted mutation has one lifecycle owner and a terminal receipt;
3. project/workspace scope is captured once at admission;
4. surfaces adapt requests and render receipts, but do not own registries;
5. new Agent instances start from the latest retained committed generation.

## Layering decision

| Layer                     | Authority                                                                                                                          |
| ------------------------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| framework                 | Reusable Skill/Hook registries, MCP/LSP protocols and generic managers                                                             |
| specialist runtime        | Plugin scan/wiring, MCP config/reconcile, LSP process control, Hook execution and Browser session execution                        |
| EKO app-core              | Workspace capture, extension mutation admission, `enabled-skills.json`, lifecycle settlement, generation fanout and typed receipts |
| GUI/TUI/CLI/JSONL/channel | Request conversion and receipt rendering only                                                                                      |

`ExtensionControlService` coordinates existing specialist owners. It must not
become another Plugin registry, MCP/LSP manager, Hook executor, Browser driver,
Skill parser or file store.

## Options considered

### Keep independent surface implementations

Rejected. It preserves fake commands and lets surfaces mutate different
workspaces or report different outcomes.

### Move EKO extension lifecycle into `echo-agent`

Rejected. EKO's local files, focus policy, plugin preferences, workspace
generation and UI receipts are product decisions.

### Add a thin application coordinator over existing specialist owners

Accepted. The coordinator captures an exact authority scope, admits one owned
operation and delegates execution to the existing specialist runtime.

## Decision

### One accepted-operation owner

- `ExtensionControlService` is the only EKO mutation admission for Skills,
  Hooks, MCP, Plugin, LSP and Browser controls.
- Admission captures the exact workspace host generation and specialist
  runtime. No surface may resolve GUI focus again after acceptance.
- Accepted work runs under the existing application lifecycle/ProductData
  ownership pattern. Dropping the caller drops only its waiter; settlement
  continues under the service owner.
- Shutdown first closes admission and then joins every accepted settlement.
  Panic/join failure becomes a typed terminal outcome.
- Bounded recent operation identities in the desired-state file let an
  identical retry reconstruct its receipt without adding another authority.

### Durable-first Skill settlement

`~/.eko/enabled-skills.json` is the only durable desired-state authority. Its
schema carries a monotonic desired generation, canonical content identity,
enabled Skill map and bounded recent operation identities. It does not store a
second runtime state machine.

The commit order is fixed:

```text
validate + capture catalog/scope
  -> canonicalize desired content
  -> deduplicate operation/content identity
  -> stage in the destination directory
  -> sync staged file
  -> atomic replace
  -> sync parent directory
  -> publish committed desired generation
  -> fan out through specialist owners
  -> return Settled or Degraded
```

A validation or durable write failure is a pre-commit error. Once the file is
committed, fanout failure is reported as committed-but-degraded. Runtime rollback
must not pretend the durable change did not occur.

The typed Skill receipt carries:

- operation identity, content identity/hash and desired generation;
- durable commit marker, settled generation and settlement state
  (`Committed`, `Settled` or `Degraded`);
- committed `enabled-skills.json` path;
- per-target scope, workspace generation, specialist generation,
  settled/degraded status, changed entries and error;
- repair debt generation/content identity, attempt count and artifact removals;
- each `SkillRepairTargetDebt` records target, component,
  expected/observed generation, reason and retryability.

The outer `ExtensionCommandReceipt` adds request/operation identity and the
captured authority scope for structured surfaces.

Install, uninstall and upstream sync wrap this same Skill settlement rather than
discarding it. Artifact removal failure is recorded in the same bounded debt
snapshot and replayed with runtime target debt.

### Idempotency, replay and ABA

- same operation identity plus the same command-parameter identity returns the
  cached or reconstructed receipt, even after unrelated global content changes;
- same operation identity plus a different command-parameter identity is a
  typed conflict;
- same content does not advance desired generation, but may reconcile an
  unsettled target;
- an older completion cannot overwrite a newer desired/specialist generation;
- workspace identity includes its host generation, so workspace A -> B -> A
  cannot publish an old A result into the new A host;
- global seed, every loaded workspace, existing pooled Agents and future pooled
  Agents converge on the same desired generation;
- restart, workspace load and the next mutation reconcile desired state before
  accepting newer publication.

Repair debt is derived from durable desired generation versus observed live
generation. A bounded debt snapshot may live beside desired state in the same
`enabled-skills.json` file, but it is not a second authority or a second store.
Restart revalidates/reconstructs it from that desired generation and live
targets.

### Specialist ownership

- framework `SkillRegistry` remains live Skill authority; `SkillsHub` discovers
  and manages Skill artifacts but does not own a loaded-state registry;
- `PluginRuntimeService` owns Plugin scan, dependency resolution, component
  wiring and AgentPluginGeneration publication;
- `McpConfigRuntime` owns canonical `mcp.json` commit and real reconciliation;
- framework `HookRegistry` and `LspManager` execute Hook and LSP behavior using
  the captured project root; process cwd is not workspace identity;
- `BrowserRuntime` owns Browser sessions; Extension control only scopes and
  sequences explicit user commands;
- output-style instructions are part of the retained AgentPluginGeneration so
  primary, existing pooled and future pooled Agents receive one generation.

No permission-mode gate is added. These controls are explicit user actions in a
local personal assistant.

## Implemented authority

The durable Skill path now writes the v2 desired/settled generation with
`atomic_write`, hashes policy plus enabled `SKILL.md` content, runs accepted
settlement in a ProductData-owned task, and returns generated
`SkillSyncReceipt`/`SkillTargetSettlementReceipt` DTOs. The target receipt
carries workspace and specialist generations, and `SkillHubEntry` no longer
owns loaded state.

GUI and headless startup call the shared on-load reconciliation before durable
Agent delivery recovery. Workspace create/switch settlement calls the same
reconciliation and reports remaining debt as a degraded workspace subsystem,
rather than hiding it.

`ExtensionCommandDispatcher` owns the surface-neutral request/receipt contract
for Skills, Plugins, MCP, Hooks, LSP and Browser. GUI uses typed Tauri IPC; JSONL
emits the generated receipt through the canonical journal without invoking the
model. CLI, TUI and channel adapters use the same app-core authority and preserve
settled/degraded/failed terminal meaning in their renderers. Browser actions and
LSP controls are available on every product surface.

MCP health is keyed by captured authority scope and maintained by Extension
control rather than a bootstrap Agent. Hook and LSP reload receive the captured
workspace project root; process cwd is not their workspace identity. Generated
DTO tests and lifecycle tests cover caller drop, atomic commit boundaries,
mid-fanout degradation, restart/workspace-load/next-mutation repair,
operation/content idempotency, workspace ABA, stale completion rejection and
existing/future AgentPool convergence.

## Consequences

Durable desired state remains truthful when a runtime target fails. Operators
and surfaces can distinguish pre-commit failure from committed degradation and
can inspect repair obligations. Focus changes cannot redirect accepted work.
The framework remains reusable and specialist runtimes remain independently
testable, at the cost of explicit generation and settlement plumbing in
app-core.
