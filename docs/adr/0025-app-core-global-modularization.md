# ADR 0025: App-Core Global Modularization

- Status: accepted for R4 implementation
- Date: 2026-08-29
- Scope: `echo-agent-cli/echo-agent-app-core`

## Context

`echo-agent-app-core` had a public aggregate surface and several large physical
modules. The aggregate made ownership difficult to audit even though the
runtime contracts were already split conceptually. This ADR records the
physical migration boundary; it does not move EKO policy into the framework.

## Decision

`echo-agent` remains the only owner of product-neutral runtime mechanisms:

| Framework authority | App-core boundary |
| --- | --- |
| `AgentTurnDriver` and typed turn outcomes | surface admission and EKO `drive_chat` policy |
| `RuntimeTaskService` and `RuntimeDagController` | file-backed `TaskRuntimeStore`, review/worktree and resource policy |
| `ToolManager`, tool artifacts and permission primitives | direct-user visibility and EKO tool projections |
| `PreparedPluginSet` and framework plugin preparation | target publication, generation receipts and EKO preferences |
| framework `Journal`/checkpoint primitives | EKO event payloads, file layout and product projections |

The app-core package remains the EKO application kernel. Its public entry is
`echo_agent_app_core::api`; implementation modules are crate-private and old
direct paths are removed from the external API. The `api` facade is an adapter surface only:
it owns no store, reducer, DAG traversal, retry loop, or second status source.

The following app-owned authorities are physically split into directory
facades and authority files without changing their Rust namespace or wire
shape:

- `state`: configuration, connection/storage DTOs, workspace/delivery scope,
  and the `AppState` aggregate;
- `tasks::task_runtime::store`: journal/plan/run state, projection, recovery,
  workspace supervisor and bounded query;
- `tasks::task_runtime::executor`: resource limits, dispatch/run orchestration,
  review, unattended policy and event mapping;
- `agent_router`: address/group, inbox/delivery, recovery and projection;
- `chat_event_log`: event/journal, retention, projection and recovery;
- `agent_pool`: admission, generation and lease lifecycle;
- `extension_control`, `plugin_runtime`, and `infra`: policy, preparation,
  publication, component wiring, stores, diagnostics and background owners.

The physical split uses complete Rust item boundaries in one parent namespace.
This preserves private-field access and exact serialization while allowing each
authority to be reviewed and tested independently. A later change may turn an
authority file into a private Rust child module only when its visibility seam
and compile-time contract are explicit.

## Compatibility contract

- JSON/JSONL event tags, serde field names, TypeScript binding names, error
  codes, file names, workspace layout and five-surface behavior remain stable.
- `TaskRun -> PlanTask -> SubagentRun` remains the sole task graph. Framework
  `TaskStatus` is the execution authority; Todo is a read-only projection.
- GUI, TUI, CLI/JSONL, channel, cron and background use the same app-core
  authority and are functionally equivalent.
- EKO does not enable SQLite. Framework SQLite APIs remain valid public
  options for other consumers.
- No new `worker` terminology or framework-to-app dependency is introduced.

## Deletion and extraction conditions

The old aggregate files are deleted only after their replacement facade
compiles and the focused behavior contracts pass. No new EKO contracts/domain
crate is created in R4 unless dependency and compile measurements demonstrate
at least two real consumers and a cycle-free boundary. Without that evidence,
the single app-core package is the authoritative deployment unit.

## Measured extraction decision

The 2026-08-29 pre-decision R4 baseline was measured before this decision was recorded:

| Measurement | Result |
| --- | --- |
| app-core no-default `cargo check` | exit 0, 72.0 s wall time |
| app-core no-default tests | exit 0, 1,515 unit tests passed, 9 ignored; integration targets 1+6+2+5+0+2 passed |
| app-core public item inventory | 1,189 public item declarations across 150 Rust files; 69 explicit facade modules |
| surface boundary inventory | 1,480 external `echo_agent_app_core::api::` call sites in the pre-decision baseline (current scan: 1,473) across CLI/TUI/Tauri/channel/examples/tests; zero supported surface imports bypass the facade |
| Cargo dependency graph | one production consumer package (`echo-agent-cli`) depends on `echo-agent-app-core`; the app-core package depends on framework `echo-agent` and has no reverse edge |
| cycle check | `cargo metadata --no-deps --format-version 1` reports no app-core/framework cycle |

There is no second independent product consuming an EKO contracts/domain/runtime
crate. The measured graph therefore does not satisfy the multi-consumer and
dependency-isolation conditions for extraction. R4 keeps one app-core package;
the physical authority split and `api` facade provide the needed review and
compile boundaries without introducing a crate solely to reduce file size.

## Consequences

The migration changes physical paths and import discipline, not runtime
semantics. The final R4 scan records the crate-private implementation paths and
the single supported facade; no external compatibility path is retained.
