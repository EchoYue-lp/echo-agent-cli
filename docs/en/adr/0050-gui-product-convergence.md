# ADR 0050: Converge EKO on a GUI product entry

## Status

Accepted product direction on 2026-10-24; runtime retirement is pending.

## Context

EKO is a local personal assistant, not a hosted multi-user service. The approved
product direction replaces the old long-term TUI/GUI parity requirement with one
GUI entry while retaining IM and cron background triggers. The full-capability
baseline still implements independent TUI, REPL, CLI/JSONL and LSP. This ADR
records the contract for later implementation; it does not remove those paths.

Console directory names do not describe ownership. The desktop parses CLI Args,
GUI panels read a CLI memory switch, and IM handlers and connection lifecycle
currently live under `src/cli`. LSP is registered for bootstrap and pooled Agents
and participates in plugin generations, configuration and extension control.
Removing a menu, feature default or whole directory would either leave reachable
implementations or lose retained capabilities. See the paired
[runtime disposition inventory](../architecture/runtime.md#product-convergence-disposition-inventory).

## Options

1. Keep multiple complete product entries. This preserves existing console
   workflows but maintains parallel interaction, configuration and acceptance
   obligations contrary to the approved GUI product scope.
2. Hide or disable console/A2A/LSP entry points. This reduces discovery but keeps
   code, dependency, registration and lifecycle obligations; it does not deliver
   actual retirement and can leave alternate dispatch paths reachable.
3. Move retained shared owners, decouple protocol consumers and physically retire
   the named entries and capabilities (chosen). This requires ordered acceptance
   and a recovery anchor, but results in a clear supported product boundary.

## Decision

- EKO's only retained primary product entry is the desktop GUI. IM receive/send
  and cron remain background triggers over the same app-core runtime. GUI PTY
  terminals, Agent shell, desktop configuration arguments and build commands
  remain; none is an independent conversational CLI product entry.
- Retire EKO's independent TUI, REPL, CLI and JSONL adapters and their exclusive
  rendering, completion, HITL provider, help, configuration, dependencies and
  tests/examples only after auditing consumers. Move desktop Args, shared memory
  state and IM connection/session/input-pump owners first. Preserve ordinary
  conversation, Task/Subagent, MCP, HITL, attachments and recovery semantics
  through retained owners. Do not add a replacement console controller.
- GUI Side Conversation navigation and parallel panels remain GUI/Tauri-specific.
  Retained IM keeps ordinary conversation, Subagent and existing fork behavior,
  with no Side Conversation-specific wire extension. Historical console `/fork`
  behavior is inventoried during adapter retirement, not a reason to preserve
  console entry points forever.
- Retire A2A and LSP across framework, SDK and EKO, including production feature
  requests, implementations, registration, config, plugin LSP components,
  generated consumers and advertisements. Keep other legal plugin capabilities
  and their existing preparation/publication/rollback ownership. Ordinary
  AgentRouter collaboration and Subagent are not A2A protocol consumers by name.
- Old configuration or plugin requests for a retired A2A/LSP capability must
  receive an explicit unsupported/retired-capability diagnostic. Do not silently
  ignore the request or report that the component loaded successfully. This
  diagnostic does not preserve an LSP/A2A executor or controller. Existing user
  conversations, tasks, memory, plugin files and configuration files are data,
  and must not be deleted as part of retiring code. Later acceptance must cover
  both old configuration and plugin capability requests, including truthful
  results for retained plugin components.
- For withdrawal, EKO and SDK consumers decouple while the framework still has
  the API; framework deletion follows only after consumer acceptance. EKO CI
  follows framework main, so every intermediate main must remain buildable.
  SDK Host uses its own exact framework pin (the design baseline records
  `f7bac62e`), independent of the superproject gitlink; retirement need not force
  an unrelated pin upgrade. New generic APIs still ship framework-first.
- Framework mechanisms remain product-neutral and independently reusable.
  EKO policy, workspace/file authority, IM/cron coordination and presentation stay
  in app-core/adapters. This explicit A2A/LSP retirement does not justify deleting
  other framework public APIs because EKO does not call them. Independent SDK
  Host stdio remains supported; it is not EKO JSONL or an EKO console entry.
- Hiding a window leaves the application process and its IM/cron owners running.
  Real exit seals admission, cancels and awaits existing lifecycle settlement;
  it does not leave a daemon. Restart uses current identity, occurrence ledger
  and TaskRuntime recovery, preserves unsafe-side-effect blockers and adds no
  new automatic replay policy. Existing scheduler composition and visibility
  toggling are source facts; GUI-owned IM/reconnect/hide/quit/restart acceptance
  is still required before console removal.
- Keep the local personal permission model. Direct terminal/file/MCP interaction
  is not gated by Agent automation permission modes. Agent-decided actions use
  existing permission/HumanLoop paths. Retain protections against accidental
  data loss, framework bugs and secret logging; introduce no hosted-service gate.
- Keep file/in-memory persistence. ConversationStore owns conversation facts;
  TaskRuntime `events.jsonl` owns task facts; plans/checkpoints/UI state remain
  projections. EKO does not enable SQLite or create another scheduler, task graph
  or store. The framework's independent storage options remain available.

## References and rationale

[Tauri's system tray documentation](https://v2.tauri.app/learn/system-tray/)
shows explicit tray actions, including process exit. The
[MCP transport specification](https://modelcontextprotocol.io/specification/2025-06-18/basic/transports)
separates stdio transport from product UI, while
[Temporal's workflow execution model](https://docs.temporal.io/workflow-execution)
and [LangGraph persistence](https://docs.langchain.com/oss/python/langgraph/persistence)
separate durable history/checkpoints from presentation and recovery invocation.
These references support explicit lifecycle ownership, transport/product separation
and retained durable authority. They do not prescribe GUI-only products or imply
that EKO implements those systems. GUI-only scope and named protocol retirement
are the user's approved product choices; existing EKO owners supply the mechanisms.

## Impact, recovery and acceptance

The local annotated `pre-retirement-2026-10-24` tag and
`chore/Echoyue/full-capabilities-baseline` branch both resolve to
`c262cfed8e77abed38a27c1fff93547997ec9486`. Keep these anchors unchanged. A future
restoration starts a new task branch there, compares future main and reassesses
contracts; it does not overwrite new work or retarget the preserved tag.
Cross-repository refs/gitlinks and the SDK pin are independently checked by the
integrator. Exa `4343f0b7` remains a local independent commit, without push or
main adoption through this outcome.

This ADR supersedes prior forward requirements for EKO console expansion/parity
and A2A/LSP retention, including scope assumptions in historical ADR 0010, 0012,
0017 and 0042; their source and historical implementation evidence remain intact.
The current production architecture is not rewritten as if retirement occurred.

This documentation slice changes rules, bilingual ADR/architecture pages and
parity registration only. No Rust/TS, manifest/lock, generated contract, CI or
example changes are needed to express the approved direction. README, feature
and quickstart pages continue to describe the current implemented baseline;
change their operational claims alongside actual retirement. The website owner
must mirror the reviewed formal pages and distinguish current behavior from
future scope, without advertising retirement as delivered.

The user's incremental build-resource constraint applies alongside the fixed
upstream design: use an independent owned target per worktree, at most two
focused Rust builds/tests concurrently, only one full all-feature/GUI/language
matrix at a time, at most two build jobs and no dev/test debuginfo. Record disk
`df`/`du` before and after heavy builds; below 5 GiB free, start no new heavy build.
After each completed compile/test batch, preserve required logs, real exit status
and retained Host/artifacts outside target, confirm no active cargo/rustc/Host or
other artifact reader uses that exact target, then promptly run `cargo clean`
only for the owned exact target. Raw logs stay in ignored
`.echo-semantic/local/<task-id>/`. Never remove artifacts before preserving their
evidence, or clean another chat's/primary target. This documentation-only slice
creates no Cargo artifacts, so cleanup is inapplicable; source/worktree/ref
cleanup remains separately coordinated and authorized.

Later implementation must prove GUI launch/configuration, IM send/receive and
reconnect, cron operation/occurrence recovery, GUI/IM HITL, Task/Subagent recovery,
terminal cleanup, hidden-window operation and actual-exit settlement before
console removal. EKO/SDK no longer requesting or invoking A2A/LSP must be accepted
before framework removal. Then verify absent active implementations, registrations,
features and advertisements with remaining consumer/example/CI matrices. Documentation
parity or a source search alone is not runtime or native acceptance.
