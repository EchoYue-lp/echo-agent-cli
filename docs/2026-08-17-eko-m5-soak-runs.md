# EKO M5 Real Soak Runs

> Launch mode: concurrent launchd processes, explicitly approved on 2026-08-17
> Runtime authority: each run's `ledger.json` and TaskRuntime `events.jsonl`
> Harness: `echo-agent-app-core/examples/task_runtime_soak.rs`
> Launch commit: `61a3e389dde16b8cdf80b5e71489a388303d8748`

The 12, 24, and 48 hour runs use one release binary and one clean commit, but
write to isolated directories. Running them concurrently shortens wall-clock
evaluation time without sharing TaskRun identity, event logs, projections,
ledgers, PIDs, or process output.

| Duration | Run record | Runtime directory | Initial status |
|---:|---|---|---|
| 12 hours | `docs/2026-08-17-eko-m5-soak-12h.md` | `.eko/soak/m5-12h` | Running; first heartbeat verified |
| 24 hours | `docs/2026-08-17-eko-m5-soak-24h.md` | `.eko/soak/m5-24h` | Running; first heartbeat verified |
| 48 hours | `docs/2026-08-17-eko-m5-soak-48h.md` | `.eko/soak/m5-48h` | Running; first heartbeat verified |

All three ledgers started at `2026-08-17T08:50:35Z`. The first common
observation showed about 30.6 seconds active time, 8 contiguous events, one
ended RunTurn, 3 tokens, and no failure fingerprint per run. The next durable
heartbeat reached about 60.6 seconds, 12 events and two ended RunTurns for each.

## Launch

From the `echo-agent-cli` repository on a clean worktree:

```bash
cargo build -p echo-agent-app-core --release \
  --example task_runtime_soak --locked --offline
./scripts/start-m5-soaks.sh
```

The launcher refuses a dirty worktree, a missing release binary, an existing
launchd service, or an already live PID for any target duration. Each process
is submitted to the macOS user launchd domain so it survives the Codex terminal.
It never removes or overwrites an existing TaskRuntime ledger. The harness itself
validates the current commit and rejects an incompatible resume.

## Inspect

Each duration directory contains:

| File | Meaning |
|---|---|
| `ledger.json` | Atomically replaced progress, configuration, commit, metrics, failures, and final evidence |
| `process.log` | Process stdout, including the final pretty-printed ledger |
| `process.err.log` | Fatal error and diagnostic stderr |
| `process.pid` | Detached process identity captured by the launcher |
| `process.label` | User launchd service label captured by the launcher |
| `tasks/<run-id>/events.jsonl` | Sole TaskRuntime event authority |
| `tasks/<run-id>/checkpoint.json` | Discardable schema/hash-validated fold cache |
| `tasks/<run-id>/run-state.json` | Validated run-state projection |

Compact status view:

```bash
for hours in 12 24 48; do
  jq '{duration_hours,status,commit,active_elapsed_millis,process_starts,runtime_reopens,recoveries,metrics,failure_fingerprints,final_evidence}' \
    ".eko/soak/m5-${hours}h/ledger.json"
done
```

Process and log view:

```bash
for hours in 12 24 48; do
  pid=$(tr -d '[:space:]' < ".eko/soak/m5-${hours}h/process.pid")
  ps -p "$pid" -o pid=,etime=,state=,command=
  tail -n 20 ".eko/soak/m5-${hours}h/process.log"
  tail -n 20 ".eko/soak/m5-${hours}h/process.err.log"
done
```

`status: passed` plus non-null `final_evidence` is the only success condition.
A stopped process with `running` or `interrupted` can resume using the same
binary, duration, output directory, and commit. `failed` requires a fix and a
new output directory for that duration; a longer run never hides the failure.

The Markdown status above is only the verified launch snapshot; consult each
ledger for live state. After all processes stop, copy the three final summaries into
`docs/2026-08-17-eko-long-horizon-runtime-m5-evaluation.md`, run the final
repository gates, and only then mark M5 and the Runtime Goal complete.
