# EKO Project Status

This is the EKO application status source. Framework API facts belong to
`echo-agent`; website projections are generated from reviewed revisions.

## Invariants

- EKO is a local personal assistant and the application does not enable SQLite.
- The product model is `TaskRun -> PlanTask -> SubagentRun`.
- `TaskStatus` is execution authority; Todo is a read-only projection; a plan is
  an editable, reviewable artifact.
- GUI, TUI, CLI/JSONL, channels, cron, and background work share app-core
  services and do not create surface-local runtime authorities.
- Execution roles use the single `Subagent` terminology throughout the product.

## Current Child Baselines

| Repository | Revision | Status |
| --- | --- | --- |
| framework `echo-agent` | `6499d05` | Typed delivery/config/usage/task/Subagent/LLM APIs, unified `DeliveryTransition`, `LlmTimeouts`, three-protocol SSE transport, direct ReactAgentBuilder/RunStore/EvalRunner contracts, and bilingual framework docs are committed. |
| application `echo-agent-cli` | current typed ledger baseline | AgentRouter uses the documented typed framework ledger directly; obsolete app projection/reducer and legacy wire/checkpoint codec are deleted. |
| website `echo-website` | current reviewed projection | Framework/application DeliveryLedger projection, legacy wire boundary ADR, and source-aware docs manifest follow the current reviewed CLI revision. |

## Stage Status

| Stage | Status | Application conclusion |
| --- | --- | --- |
| F0-F6 | Complete | Characterization, receipts, Task authority, Agent control, lifecycle, recovery, and five-surface parity are closed. |
| R1 framework-first migration | Complete | Generic turn, TaskRuntime, artifact, plugin, memory, tool-control, and background authorities are framework-first. |
| R2 examples convergence | Complete | Learning examples and executable contracts are unified in the framework repository. |
| R3 framework docs/website | Complete | Framework bilingual docs and website projection are synchronized to reviewed revisions. |
| R4 app-core modularization | Complete | App-core authority modules are physically split behind `echo_agent_app_core::api`; validation remains pinned to the historical R4 code baseline. |
| Framework capability placement | Complete | Product-neutral keyed admission is in the framework; EKO keeps product policy and projections. |
| AgentRouter generic delivery ledger | Typed API complete / development schema reset | Framework `DeliveryLedger<Route, Payload>` and `DeliveryTransition` are the sole lifecycle/projection/retry authority; EKO uses `AgentAddress` and `AgentMessage` directly, while file layout, supervisor, live/cold runtime, wake, retirement, and product policy remain application-owned. |
| Current product schema authority | Complete | Only `learned-rules.md`, `.eko/workspace.json`, literal cron prompts, and `TaskRuntimeStore::new()` define current product input. Retired files remain untouched but are not interpreted; journal rebuild and conservative Git cleanup remain data-protection mechanisms. ADR [0028](./adr/0028-current-product-schema-authority.md). |
| Framework-native Agent/model config | Complete | EKO stores framework `AgentSettings` directly; top-level model state contains only `default_model_id`; `ConfiguredModel` and `ModelProviderConfig` are the only model/connection authorities; one typed resolver returns explicit selection errors. ADR [0029](./adr/0029-framework-native-agent-and-model-config.md). |
| Unified Subagent prompt compilation | Complete | Built-in and plugin roles use one EKO compiler; compiled typed messages are the execution authority, effective tools/workspace are resolved per invocation, structured history is filtered once, and TaskRuntime reuses framework JSON framing. |
| Deterministic command-cell watch | Complete / static gates passed | Framework `CommandCellWatcher` is the only retained polling driver; EKO keeps durable owner/Ready/delivery/ack policy. The model-driven `awaiter` Subagent and provider summary are deleted. ADR [0031](./adr/0031-deterministic-command-cell-watch.md). |
| Enabled Skill runtime authority | Complete / simplified 2026-09 | `enabled-skills.json` remains the bundled registration authority (0032 core stance); the durable settlement machinery (generation CAS, operation-identity dedup, repair-debt replay) is removed — all five mutation paths now write-and-reconcile directly, corrupt configs fall back to the default active set (fail-open), and framework `activate_skill` surfaces an explicit error for unregistered skills. ADR [0032](./adr/0032-enabled-skill-runtime-authority.md) (partially superseded), [0036](./adr/0036-skill-policy-simplification.md). |
| Skill catalog contraction and official standardization | Complete / merge pending | Bundled `SKILL.md` files use agentskills.io official fields only (no private extension namespace), `allowed-tools` is a space-separated string, Skill files carry no private Hook sidecar, and LLM routing is description-driven; the catalog contracted again in 2026-09 from 39 → 24 (removed 4 generic-capability and 11 vendored Anthropic example skills; default-active 8 → 5, methodology baseline injection 4 → 1 keeping only verification-before-completion); the builtin root is now resolved at runtime (`$EKO_SKILLS_ROOT` → Tauri resource dir → source tree), TUI `/skill` and a GUI activate button make skills user-invocable, and install recognizes Agent Plugins 1.0 packages (skills face only). ADR [0033](./adr/0033-skill-catalog-contraction-and-official-frontmatter.md), [0036](./adr/0036-skill-policy-simplification.md). |
| Bilingual docs parity foundation | Complete / reviewed source synchronized | The zh source and reviewed en tree are published through the manifest and fail-closed checker; source-aware synchronization remains required for each reviewed revision. |
| Final integration/release | Conditional / current Phase 3 gates pending | Historical full gates remain bound to earlier revisions; current typed deferral and AgentRouter focused checks pass, but full workspace/frontend/GUI/soak, manual GUI, remote CI, push, and release remain open. |

## Authority Paths

| Meaning | Owner |
| --- | --- |
| Bootstrap, config, and pool policy | `echo-agent-app-core/src/runtime.rs`, `infra/`, `agent_pool/` |
| AgentPool execution admission | framework `KeyedExecutionAdmission`, wrapped by EKO `AgentPoolAdmission` |
| Workspace host and resources | `workspace/` and `state/` |
| Conversation routing and inbox | `agent_router/` -> `echo_agent::delivery::DeliveryLedger<..., AgentAddress, AgentMessage>` |
| Subagent outcome/evidence | framework `SubagentOutcome` is persisted and rendered directly; EKO adds task identity and review metadata around the same value, with no duplicate result DTO |
| TaskRun graph and status | framework runtime task service plus EKO file projection |
| Task/Subagent control | `tasks/task_runtime/subagent_control.rs` |

## Evidence and Residuals

Historical R4 soak and acceptance ledgers remain bound to their exact code
revisions. Documentation-only commits do not change those results. The final
release still requires manual desktop GUI acceptance, remote CI, child-first
push, and release coordination.
