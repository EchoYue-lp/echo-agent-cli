# ADR-0022: Framework-prepared plugin generations with captured EKO targets

## Status

Accepted and implemented, 2026-08-29.

## Context

ADR 0012 made `ExtensionControlService` the EKO mutation coordinator, but the
plugin path still had two authorities below admission. Each target rescanned and
reparsed framework components, rollback reread the previous MCP files, and the
coordinator captured `ExtensionRuntimeTargets` only to discard it before
`AppState` enumerated followers again. A workspace created after a global commit
also started a fresh plugin runtime at local generation zero even though its
pool already inherited the committed global Agent generation.

This produced four concrete risks:

1. files could change between validation, primary publication, follower
   publication and rollback;
2. workspace deletion/recreation or focus changes could replace the target set
   after admission;
3. a constructor failure could warn and expose an empty runtime instead of
   rejecting workspace/application bootstrap;
4. global and workspace monitors used the same scheduler key, so one target
   could remove another target's task.

The portable problem is preparing and applying plugin components. EKO-specific
workspace generations, Subagent construction, LSP process binding, monitors,
themes, output styles and surface receipts remain application policy.

## Evidence and industry research

This decision also checked OpenAI Codex at fixed commit
`cdde711fac008cd4e1115603ead713cf23b1a580`: its
[PluginsManager](https://github.com/openai/codex/blob/cdde711fac008cd4e1115603ead713cf23b1a580/codex-rs/core-plugins/src/manager.rs#L398-L506)
shares cached load outcomes, its
[SkillsManager](https://github.com/openai/codex/blob/cdde711fac008cd4e1115603ead713cf23b1a580/codex-rs/core-skills/src/manager.rs#L51-L121)
caches by config/workspace inputs, and its
[reload tests](https://github.com/openai/codex/blob/cdde711fac008cd4e1115603ead713cf23b1a580/codex-rs/core-skills/src/manager_tests.rs#L559-L616)
keep disk mutation invisible until explicit force reload. Claude Code's official
[plugin documentation](https://code.claude.com/docs/en/plugins) likewise treats a
plugin as a scoped bundle with explicit lifecycle operations. These independent
implementations support the same narrow conclusion used here: resolve an
immutable input once, bind mutation work to a captured target, and return
explicit target results instead of rediscovering mutable state during fanout.

## Options considered

### Keep target-local scan, wire and rollback

Rejected. It preserves time-of-check/time-of-use gaps and makes rollback depend
on files that may no longer describe the committed generation.

### Put EKO workspace and UI policy in the framework

Rejected. Workspace ABA identity, scheduler monitor keys, theme/style choices
and surface receipts are EKO product decisions and would pollute the reusable
framework.

### Framework prepared set plus thin EKO target adapter

Accepted. The framework freezes portable components in a
`PreparedPluginSet`, applies that set with a typed ownership receipt, and rolls
back from the receipt. EKO captures exact target generations once and adds only
product-specific components and receipt fields.

## Decision

1. `PluginIntegrator::prepare` is the only portable plugin parse and dependency
   resolution path. A prepared set freezes Skill, Hook, MCP, Subagent document
   and LSP document inputs and carries an opaque generation identity.
2. `PluginRuntimeService` stores the prepared set and framework apply receipt.
   Publication uses `wire_prepared`; retirement and rollback use the exact
   receipt. No rollback path rescans a registry or rereads MCP files.
3. EKO parses only EKO-owned components from the prepared plugin roots:
   executable Subagents, LSP manager bindings, monitors, themes and output
   styles. Frozen Subagent and LSP documents are never reread from disk.
4. `ExtensionRuntimeTargets` captures scope, workspace generation, primary,
   pool, plugin runtime and a lifetime lease under the workspace transition
   guard. Plugin and MCP settlement use this exact cut for their entire
   accepted operation. `AppState` does not enumerate followers again.
5. Every plugin target returns a typed generation receipt containing target
   scope, workspace identity, previous and candidate prepared generation,
   settlement status and diagnostics. The authority also publishes one overall
   settled/degraded status; Tauri and frontend adapters copy these fields without
   inferring settlement from error text.
6. A cold workspace pool inherits the authority's committed framework
   generation and revision before target overlay preparation. Target preparation
   includes that workspace's Project/Local scopes; bootstrap failure rejects the
   workspace instead of exposing generation zero.
7. Monitor scheduler IDs are qualified by target scope. LSP config changes
   replace only the affected target's LSP manager and do not trigger a broad
   plugin, Skill, MCP, Subagent or monitor reload.
8. Plugin runtime construction returns `Result`. There is no warn-and-continue
   empty generation. Enabled application Skills are loaded once through durable
   on-load reconciliation, not once in bootstrap and again in reconciliation.
9. Tauri plugin commands only adapt IPC values and dispatch the shared
   extension command. Local source validation and runtime reads stay in the
   app-core authority.

## Consequences

Plugin validation, apply and rollback now refer to one immutable portable
generation. Workspace focus changes cannot redirect accepted fanout, and
delete/recreate ABA is visible in receipts. New workspace Agents start from the
committed authority generation, while workspace-only overlays remain isolated.

The application retains explicit adapter code for Subagent construction, LSP,
monitors and UI preferences. This is intentional product policy, not a second
portable plugin parser. The additional prepared generation and per-target
receipt plumbing is the cost of deterministic recovery and diagnostics.
