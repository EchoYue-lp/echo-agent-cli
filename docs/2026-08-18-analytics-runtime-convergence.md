# Analytics Runtime And Tool Surface Convergence

Status: M0-M2 complete.

## Decision Basis

The implementation follows the same converging patterns already used by EKO's
tool-schema budget work:

- Claude Code and Codex favor reviewable scripts and progressive disclosure
  over eagerly exposing a large catalog of narrow data operations.
- Codex keeps a small set of strong execution and file primitives instead of
  making every library operation a model-visible tool.
- `uv` separates project locking and environment synchronization. EKO uses a
  checked-in `uv.lock`, prepares the environment outside the agent execution
  sandbox, and executes analysis with networking disabled.

Official `uv` references:

- <https://docs.astral.sh/uv/concepts/projects/sync/>
- <https://docs.astral.sh/uv/concepts/python-versions/>

This is a complexity and context-budget change, not a permission restriction.
EKO remains a local personal assistant and keeps user-directed analysis fully
available in GUI, TUI, CLI, and channel surfaces.

## Architecture Boundary

| Responsibility | Owner |
|---|---|
| Sandboxed command execution and caller-injected resolved script profile | `echo-agent` |
| Locked Python package set, environment cache, provisioning, analysis records, and product prompts | `echo-agent-cli` |
| Boundary | EKO injects one resolved profile into the existing `run_code` call; it does not create another executor or registry |

The framework profile contains only an identifier, language, executable,
argument prefix, environment values, and read-only runtime paths. It does not
know about `uv`, pandas, EKO directories, or analytics policy.

## Existing Authoritative Paths

- Framework execution: `echo-tools/src/code.rs` and
  `echo_core::sandbox::SandboxExecutor`.
- Application analysis: `echo-agent-app-core/src/analysis.rs`.
- Tool registration and deferred schemas: the framework `ToolManager` plus
  `echo-agent-app-core/src/tool_exposure.rs`.
- Analysis persistence: ordinary files under `analysis/<id>/`; no SQLite.

No second execution engine, analysis store, tool registry, task graph, or
visibility model will be introduced.

## M0 Baseline

- The deterministic production fixture currently reports Chat 15, Task 16,
  and Auto 18 first-turn schemas, all below 16 KiB and 4,000 estimated tokens.
- The all-feature framework catalog contains more than one hundred optional
  capabilities. The actual EKO Agent construction path registered 90 built-in
  tools before this change, while deferred schema search kept most of them out
  of the first turn.
- `echo-agent-app-core` explicitly enables `data` and `statistics`, which pulls
  Rust Polars into every EKO application build.
- The framework owns 15 data tools, three data-quality tools,
  `exploratory_statistics`, and the Polars-backed Excel loader behind optional
  features.
- EKO already persists analysis scripts, input hashes, output artifacts,
  environment metadata, and immutable run records, then delegates execution to
  `run_code`.
- The missing link is a deterministic application-managed Python environment;
  the host fallback is currently `python3` and does not guarantee analytics
  packages.

## Milestones

### M1: Managed analytics runtime

1. Add a generic resolved script-execution profile to framework `ToolContext`.
2. Make persisted-script `run_code` honor that profile while retaining the
   existing sandbox, timeout, cancellation, output, and network policy.
3. Add an EKO-owned `uv` project with a checked-in lockfile and a content-hash
   runtime cache under EKO user data.
4. Prepare the environment before analysis, inject its interpreter profile,
   and merge verified package versions into the immutable run record.
5. Keep ordinary non-analysis `run_code` behavior unchanged.

### M2: Remove Polars from EKO

1. Remove `data` and `statistics` from the application dependency features.
2. Migrate bundled data Skills and Subagent prompts to persisted Python scripts.
3. Preserve the framework's optional Polars feature and public tools for other
   framework consumers.
4. Prove `cargo tree -p echo-agent-app-core -i polars` has no application path.

## Measured Result

- The actual EKO built-in catalog is 70 tools after removing the 20 optional
  Polars-backed registrations. A regression budget caps it at 80.
- First-turn schemas remain Chat 15, Task 16, and Auto 18, all below 16 KiB and
  4,000 estimated tokens.
- `cargo tree -p echo-agent-app-core --locked` contains no Polars package.
- The framework still retains its optional `data` and `statistics` features for
  independent consumers; only the EKO application stopped enabling them.
- The locked runtime probe and a persisted Unicode-path Python script both pass
  through the real OS sandbox with pandas and pyarrow imported successfully.
- A missing `uv` executable fails with an actionable error; `EKO_UV_PATH` can
  select an application-bundled or user-installed executable without changing
  the lock or execution contract.

## Migration And Deletion Contract

M1 must pass before M2. M2 removes the EKO registration path by no longer
compiling the optional tools; it does not add Python wrappers with one tool per
old Polars operation. The existing analysis service and `run_code` remain the
single authority.

Later worktrees will handle canonical `apply_patch`, real multimodal image
results, MCP Resources, structured HITL, and catalog search improvements. They
are deliberately excluded from this branch so the analytics dependency change
can be reviewed and rolled back independently.
