# Canonical Edit And Image Tools

Date: 2026-08-19

## Outcome

EKO now has one default file-mutation interface, `apply_patch`, instead of
seven overlapping edit/write/create/delete/update/append/move entries. The old
`analyze_image` placeholder has also been replaced by `view_image`, which sends
the actual local image bytes to an image-capable model rather than returning
only metadata.

Measured on this independent iteration-2 branch:

| Budget | Before | After |
|---|---:|---:|
| Default registered catalog | 90 | 84 |
| Chat first-turn schemas | 15 | 14 |
| Task first-turn schemas | 16 | 15 |
| Auto first-turn schemas | 18 | 17 |
| Chat estimated schema tokens | 3,647 | 3,289 |
| Task estimated schema tokens | 3,906 | 3,547 |
| Auto estimated schema tokens | 3,929 | 3,570 |

Iteration 1 independently removes twenty EKO-only Rust Polars tools and moves
analysis to the locked Python runtime. After both branches are integrated, the
combined default-catalog target is therefore `90 -> 64`.

## Industry Reference

This design was checked against the official OpenAI Codex implementation
before coding:

- Codex's
  [`apply_patch` parser](https://github.com/openai/codex/blob/main/codex-rs/apply-patch/src/parser.rs)
  uses one `*** Begin Patch` document with add, delete, update, move, context
  hunk, and end-of-file directives. EKO adopts the same model-facing grammar so
  one tool call can express one coherent multi-file change.
- Codex's
  [`view_image` handler](https://github.com/openai/codex/blob/main/codex-rs/core/src/tools/handlers/view_image.rs)
  accepts a local path, validates that it is an image, and returns pixels as
  model input. EKO follows the same local-path contract and does not duplicate
  URL download or arbitrary base64-input behavior in this tool.
- Codex's
  [tool-output representation](https://github.com/openai/codex/blob/main/codex-rs/tools/src/tool_output.rs)
  separates model-facing rich output from bounded log output. EKO similarly
  keeps image data in an in-memory `ToolResultContent`, excludes it from
  serialization and debug output, and emits only the small textual result to
  UI events and traces.

The cross-system pattern is more important than matching names: keep a small
set of high-leverage primitives, allow one atomic edit intent, and distinguish
the model payload from operational telemetry.

## Architecture Boundary

| Responsibility | Owner | Reason |
|---|---|---|
| Patch grammar, validation, preflight, commit, rollback, unified diff | `echo-agent` | Generic file-edit mechanism usable by any framework consumer |
| Rich tool-result content and model modality requirements | `echo-agent` | Generic tool/model contract, independent of EKO UI |
| Local image validation and pixel loading | `echo-agent` | Generic multimodal read tool |
| Default tool exposure by Chat/Task/Auto mode | `echo-agent-cli` | EKO product policy |
| Skill allowlists and coding Subagent instructions | `echo-agent-cli` | EKO-owned capability catalog and prompts |
| TUI/CLI rendering and unattended-write policy | `echo-agent-cli` | Product interaction and worktree policy |

No Task graph, DomainProfile, worktree lifecycle, UI projection, or EKO
approval state was moved into the framework.

## Framework Implementation

### Transactional `apply_patch`

The canonical parser supports:

- `*** Add File: path`
- `*** Update File: path`
- optional `*** Move to: path`
- `*** Delete File: path`
- `@@` context hunks and `*** End of File`
- `dry_run=true`

All actions are parsed and preflighted before the first write. Paths must be
relative, cannot contain `..`, and are checked through the nearest existing
ancestor so symlink escapes are rejected. Existing inputs are re-read before
commit. Writes use the framework's durable atomic-write primitive; a later
failure rolls earlier mutations back. A partial-side-effect result is returned
only when rollback itself fails. UTF-8 text and CRLF line endings are handled
without byte slicing.

The default registry and built-in filesystem Skill expose only `apply_patch`
for mutation. More granular public file tools remain explicitly constructible
for framework consumers that need them, but they are no longer injected into
every Agent. The superseded `EditFileTool` implementation was removed because
`apply_patch` covers its edit, preview, diff, and multi-replacement use cases.

### Real `view_image`

`view_image` accepts a local `path` and optional `detail=auto|low|high`. It:

1. resolves the path against the tool base directory or run working directory;
2. enforces the framework file-size limit;
3. validates PNG, JPEG, GIF, or WebP magic bytes;
4. returns a small `ToolResultKind::Image` event plus an in-memory data URL for
   the live model conversation.

Image bytes are skipped by Serde and redacted by `Debug`, preventing large or
sensitive payloads from entering JSON events, audit logs, TUI/GUI state, or
Subagent event persistence.

### Model Capability Filtering

Tools can declare required model input modalities. `view_image` requires
`Image`; its schema and name are omitted for a configured text-only model.
Runtime snapshots also retain the active model's modalities. Rich image output
from mixed-purpose tools such as browser screenshot operations is projected
only when the model accepts images; their normal text output remains available
to text-only models.

The rich user message is appended only after the matching text tool-result
message. This preserves the strict tool-call/result ordering required by model
providers.

## EKO Migration

- Chat, Task, and Auto policies now expose `apply_patch` instead of
  `write_file` plus `edit_file`.
- Every bundled Skill allowlist and coding Subagent prompt uses the canonical
  name.
- Unattended-task preflight treats `apply_patch` as the one direct mutation
  capability and preserves EKO's existing worktree rules.
- TUI and CLI count and render patch results through one path; TUI displays the
  returned unified diff.
- Browser screenshots attach their frame bytes to the model-only rich result
  while retaining the existing GUI frame event.
- A regression gate rejects all eight superseded default names and caps this
  branch's registered catalog at 84.

## Verification

Focused verification completed during implementation:

- patch add/update/delete/move with Unicode paths and content;
- stale multi-file patch leaves every file unchanged;
- path traversal and overwrite-by-add rejection;
- CRLF preservation and dry-run behavior;
- real image-byte projection and non-image rejection;
- rich payload omitted from serialization;
- image schema omitted for text-only models;
- image result omitted from text-only conversations;
- canonical default registry and EKO mode exposure snapshots;
- browser screenshot pixel preservation;
- unattended write-policy preflight;
- TUI tool rendering.

Framework commit `5ba485f` passed:

- `cargo fmt --all` and `cargo fmt --all -- --check`;
- both required all-feature Clippy matrices;
- `cargo test --workspace --all-targets --all-features --locked`;
- workspace no-default-feature check;
- all required isolated feature checks plus an additional `files` check.

The EKO application passed:

- `cargo fmt --all` and `cargo fmt --all -- --check`;
- both required all-feature Clippy matrices;
- `cargo test --workspace --all-features --locked`: 931 app-core tests,
  5 runtime-state E2E tests, 143 CLI/TUI library tests, and 10 CLI binary
  tests passed; only explicitly opt-in smoke/performance/doc tests were ignored;
- `cargo check -p echo-agent-app-core --no-default-features --locked`.

## Follow-Up

The next catalog work should not add another broad set of always-visible tools.
The highest-value Codex patterns still worth evaluating are:

1. first-class MCP Resources (`list/read`) so reference data does not have to
   masquerade as executable tools;
2. a compact long-running operation handle shared by background tasks rather
   than one tool per lifecycle transition;
3. generated catalog reports that separate registered, model-compatible,
   first-turn-visible, deferred, and Skill-activated capabilities.

These belong in later bounded iterations. They must reuse the existing MCP,
CommandCell, TaskRun, and Tool Search authorities rather than create parallel
runtimes.
