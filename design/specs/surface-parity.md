# EKO Surface Parity Cleanup

> Status: Pending
> Priority: after P0 runtime reliability closure
> Scope: production reachability and GUI/TUI/CLI/channel parity only

## Why This Remains

The old GUI status document mixed completed backend work, hidden React components, and product
acceptance. A fresh code reachability check leaves two real gaps:

| Capability            | Implemented authority                                                                    | Missing production path                                                                                       |
| --------------------- | ---------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------- |
| Workflow              | `WorkflowService`, registered Tauri CRUD/execute commands, CLI/TUI `/workflow`           | `WorkflowPanel` has no production import or mount, so GUI users cannot reach it                               |
| Structured extraction | framework `ReactAgent::extract_json`, registered Tauri IPC, `extractApi`, `ExtractPanel` | `ExtractPanel` has no production import or mount; TUI/CLI/channel have no equivalent app-core surface service |

Sandbox execution and the paper/systematic-review workbench are not gaps: both are mounted in the
production GUI and call real backend paths.

## Constraints

- Reuse the existing framework workflow executor and `extract_json`; do not create a second
  executor, schema engine, store, or lifecycle owner.
- Put EKO orchestration and typed outcomes in `echo-agent-app-core`; Tauri and other surfaces stay
  thin adapters.
- GUI, TUI, CLI, and channel must expose equivalent capability. A hidden component or registered
  IPC command alone is not implementation evidence.
- Do not add SQLite, online permission gates, or a surface-local queue/state machine.
- Before choosing the final structured-extraction command/API shape, perform the repository-wide
  duplicate search and industry implementation check required by `AGENTS.md`.

## Work Items

1. Mount workflow management/execution in the production GUI using the existing service and typed
   execution result. Replace `console.error`-only failure handling with visible product errors.
2. Define one app-core structured-extraction service over framework `extract_json`, then connect
   GUI/TUI/CLI/channel adapters without duplicating schema validation.
3. Add reachability tests proving the production navigation/command registries expose both
   capabilities and their adapters return the same typed outcomes.
4. Delete orphan components, endpoints, DTOs, and tests superseded during the cutover.

## Acceptance

- No capability is claimed from a definition-only component or an IPC command with no UI/command
  route.
- Workflow uses one persisted definition and one executor across all surfaces.
- Structured extraction uses one schema validation/execution path across all surfaces.
- GUI errors are rendered; TUI/CLI/channel errors preserve the same typed cause.
- Applicable Rust, GUI, and frontend gates pass before this specification is marked Complete.

After acceptance, merge the stable facts into `docs/features.md` and delete this file.
