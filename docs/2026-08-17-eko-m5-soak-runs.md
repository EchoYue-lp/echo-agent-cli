# EKO M5 Real Soak Runs

> Launch mode: concurrent detached processes, explicitly approved on 2026-08-17
> Runtime authority: each run's `ledger.json` and TaskRuntime `events.jsonl`
> Harness: `echo-agent-app-core/examples/task_runtime_soak.rs`

The 12, 24, and 48 hour runs use one release binary and one clean commit, but
write to isolated directories. Running them concurrently shortens wall-clock
evaluation time without sharing TaskRun identity, event logs, projections,
ledgers, PIDs, or process output.

| Duration | Run record | Runtime directory | Initial status |
|---:|---|---|---|
| 12 hours | `docs/2026-08-17-eko-m5-soak-12h.md` | `.eko/soak/m5-12h` | Launch pending |
| 24 hours | `docs/2026-08-17-eko-m5-soak-24h.md` | `.eko/soak/m5-24h` | Launch pending |
| 48 hours | `docs/2026-08-17-eko-m5-soak-48h.md` | `.eko/soak/m5-48h` | Launch pending |

## Launch

From the `echo-agent-cli` repository on a clean worktree:

```bash
cargo build -p echo-agent-app-core --release \
  --example task_runtime_soak --locked --offline
./scripts/start-m5-soaks.sh
```

The launcher refuses a dirty worktree, a missing release binary, or an already
live PID for any target duration. It never removes or overwrites an existing
TaskRuntime ledger. The harness itself validates the current commit and rejects
an incompatible resume.

## Inspect

Each duration directory contains:

| File | Meaning |
|---|---|
| `ledger.json` | Atomically replaced progress, configuration, commit, metrics, failures, and final evidence |
| `process.log` | Process stderr plus the final pretty-printed ledger or fatal error |
| `process.pid` | Detached process identity captured by the launcher |
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
done
```

`status: passed` plus non-null `final_evidence` is the only success condition.
A stopped process with `running` or `interrupted` can resume using the same
binary, duration, output directory, and commit. `failed` requires a fix and a
new output directory for that duration; a longer run never hides the failure.

After all processes stop, copy the three final ledger summaries into
`docs/2026-08-17-eko-long-horizon-runtime-m5-evaluation.md`, run the final
repository gates, and only then mark M5 and the Runtime Goal complete.
