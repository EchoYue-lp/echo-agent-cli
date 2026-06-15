# GUI Status

Last reviewed: 2026-06-16

This document records the real GUI/Tauri state so review work does not mistake hidden or stubbed panels for completed features.

## Development Commands

Use these commands from `echo-agent-cli/`.

```bash
# Recommended clean GUI dev entry.
cargo gui-dev

# Equivalent explicit Tauri command.
cargo tauri dev -- --no-default-features --features gui --bin echo-agent-tauri

# GUI backend compile check.
cargo gui-check

# Frontend build for Tauri mode.
cd web-frontend
npm run build:tauri
```

`cargo tauri dev` is still supported and starts the GUI because `tauri.conf.json` sets `mainBinaryName`, `runner.args = ["--bin", "echo-agent-tauri"]`, and `build.features = ["gui"]`.

Tauri CLI v2 expands Cargo default features before invoking Cargo. Because this package keeps `default = ["tui"]` to preserve the normal CLI experience, direct `cargo tauri dev` may log `--features gui,tui`. That log is expected. It does not mean the TUI binary is launched. The clean command is `cargo gui-dev`.

Do not remove the Cargo default `tui` feature just to clean this log; that changes `cargo run` / CLI default behavior.

## Connected GUI Features

These GUI surfaces are visible and backed by real Tauri IPC or runtime state:

| Area | State |
| --- | --- |
| Chat and streaming | Connected |
| Human-loop approval/input/selection | Connected |
| Conversations and sessions | Connected |
| Memory panel | Connected |
| Auto Memory GUI controls | Connected to session toggle, preview, and `.echo-agent/project.md` extraction |
| Tools panel | Connected |
| MCP panel | Connected |
| Skills panel | Connected to local directory loading |
| Providers/config | Connected |
| Scheduler | Connected |
| Permissions and audit | Connected |
| Compress/context stats | Connected |
| Evolution trajectory/curator/trace | Connected |
| Terminal | Connected |
| Workspace basics | Connected |
| Scratchpad | Connected to `.echocowork/scratchpad.md` |
| Decisions | Connected to `.echocowork/decisions.jsonl` |
| Worktree | Connected to `git worktree` |

## Hidden Or Partial GUI Features

These surfaces must not be presented as finished GUI features:

| Area | Current state | Decision |
| --- | --- | --- |
| `workflow` | List/create/delete exist; execute is wired to framework workflow graph YAML/JSON | May be shown when the UI collects framework graph definitions; simplified app-only workflow definitions are validation-only |
| `sandbox` | Config read/write and execution are wired to the framework sandbox manager | Show only behind clear permission/error UX; GUI must not reimplement sandbox execution |
| `extract` | JSON schema validation and extraction are wired to framework LLM structured extraction | Show when model configuration is available; GUI only collects schema/input and renders errors |
| `papers` | Frontend components exist; Tauri IPC returns `NotImplemented` | Not mounted; research workflow can remain CLI/chat driven |
| Legacy history export | IPC still returns `NotImplemented` | Use conversations export instead |

## Review Notes

- GUI feature completeness should be reviewed by visible surface plus backend behavior, not by the existence of frontend components.
- Hidden frontend components are allowed while backend work is pending, but README/review docs must list them as hidden or incomplete.
- `cargo gui-dev` is the preferred daily GUI command. `cargo tauri dev` is compatibility convenience.
