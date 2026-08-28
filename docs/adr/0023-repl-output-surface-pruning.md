# ADR 0023: REPL Output Surface Pruning

## Context

`echo-agent-app-core::output` described `OutputRenderer` as a shared REPL/TUI
facade and exposed format, Markdown, syntax, table, and six-theme APIs. Whole
repository reachability inspection showed that this description was false:

- the REPL used only its banner, session summary, status messages, and two
  event-summary flags;
- the TUI used its own ratatui Markdown renderer and palette;
- `OutputFormat`, `FormatContext`, Markdown-to-terminal, syntax highlighting,
  table rendering, theme switching, and their renderer methods had no
  production caller;
- `/theme` and `/output` only printed the requested value and changed no state.

This is a reachability-driven cleanup rather than a new architectural choice,
so no new framework mechanism or external implementation comparison is needed.

## Options

1. Keep the APIs behind `#![allow(dead_code)]`. Rejected because it hides
   regressions and advertises capabilities that do not exist.
2. Wire every historical renderer and command into production. Rejected because
   the TUI already has a feature-complete renderer and no product requirement
   selects the dormant REPL formats.
3. Preserve the live REPL surface and delete the unreachable APIs. Selected.

## Decision

- `output` is explicitly a REPL ANSI presentation helper, not a cross-surface
  facade.
- Keep only the `OutputRenderer` and `OutputConfig` fields and methods consumed
  by the production REPL.
- The TUI owns its ratatui renderer and palette directly.
- Delete the format, Markdown, syntax, table, and theme modules and their
  app-core-only dependencies.
- Delete the no-op `/theme` and `/output` commands and completions.
- Do not change GUI rendering or plugin-owned theme/output-style behavior.

## Consequences

- Removing `#![allow(dead_code)]` leaves the output module covered by normal
  warning gates.
- App-core no longer compiles syntect, pulldown-cmark, or terminal-size solely
  for unreachable REPL code. The TUI retains its own optional Markdown and
  syntax dependencies.
- Existing REPL banner, session information, status messages, tool/usage event
  summaries, and TUI rendering behavior remain unchanged.

## Verification

- Repository-wide negative scans find no removed output types, methods,
  `ColorTheme`, or no-op commands.
- Focused output/REPL/TUI tests compile and pass.
- The standard CLI all-feature, no-default, GUI, and formatting gates remain
  green.
