# EKO Long-Horizon Runtime M5 Evaluation

> Date: 2026-08-17
> Runtime Goal: M0-M5 implementation
> Authority: `events.jsonl`; `checkpoint.json` is a discardable projection
> Status: M5a checkpoint/performance `3e409d0` and M5b harness/fault matrix `82d8eda` complete; real 12/24/48-hour soak active

## Checkpoint Contract

`checkpoint.json` is compact machine-readable JSON containing `schema_version`,
`run_id`, `seq`, `event_byte_offset`,
`state_hash`, and the serializable event-fold state. `state_hash` covers schema,
run identity, seq, and recursively key-sorted state JSON; it excludes the byte
offset because that field is only a suffix locator.

The read path accepts a checkpoint only when schema, run identity, seq, offset,
suffix continuity, and hash all validate. Any failure discards the cache and
rebuilds from the complete event log. Event append is fsync'd before projection
and checkpoint replacement; a later write failure returns
`CommittedProjectionDegraded { seq, detail }`, so callers know the authority
already advanced and must not replay an external command as a new fact. Snapshot
reads first validate that the checkpoint has no durable event suffix; a process
kill after event fsync but before projection replacement therefore triggers the
same event fold instead of returning stale state.

## Performance Fixture

Fixture: 1,000 completed RunTurns, 10,000 persisted runtime events, 100
compactions, then one warm continuation event. Release command:

```bash
cargo test -p echo-agent-app-core --release \
  benchmark_checkpoint_1k_turns_10k_events_100_compactions \
  --offline -- --ignored --nocapture
```

Environment: Darwin 25.5.0 arm64, Apple M1 Pro, Rust/Cargo 1.97.1. Five
consecutive runs used the same release binary. Values are milliseconds.

| Metric | Median | Worst | Merge threshold |
|---|---:|---:|---:|
| Full 10k event read + parse + fold | 7.847 | 8.082 | <= 150 |
| Valid checkpoint read + suffix fold | 0.794 | 0.890 | <= 10 |
| One event append + suffix fold + fsync projections | 24.240 | 28.576 | <= 50 |
| Validated `get_run` snapshot read | 0.916 | 1.005 | <= 2 |

The median warm/full ratio is 9.88x and every recorded sample exceeded 9x. The
fixed regression floor is 5x so ordinary host jitter does not turn this
microbenchmark into a flaky gate. The warm append folded exactly one event. The
compact checkpoint was 36,416 bytes versus about 3,680,984 event bytes (0.99%);
the fixed size gates are <= 128 KiB and < 10% of the event log for this fixture.

Absolute thresholds are intentionally fixed from this first stable baseline.
Later implementations may tighten them; they must not widen them after a
regression merely to make the benchmark pass.

An earlier draft reported 37.4x and used a 10x ratio gate, but that comparison
included projection fsync in the full path and excluded it from the warm path.
The corrected fixture measures `read + parse + fold` on both sides. Its first
run exposed the invalid threshold rather than being waived; the table above is
the five-sample baseline from the corrected final binary. The remaining
synthetic Note events carry a fixed representative runtime-diagnostic payload
instead of an empty padding object.

## Automated Fault Matrix

| Fault | Authoritative regression coverage | Required result |
|---|---|---|
| Provider transient/5xx equivalent | `provider_retry_schedule_rebuilds_and_counts_across_fingerprints`; `provider_retry_claim_waits_then_success_clears_schedule`; `fifth_provider_failure_atomically_pauses_and_explicit_resume_resets_retry` | persisted backoff, exact admission, typed pause at limits |
| Process kill/power-loss boundary | `missing_or_corrupt_snapshots_rebuild_from_event_authority` (includes durable event before projection replacement); `boot_recovery_closes_orphan_turn_and_records_pause_reason`; `boot_recovery_closes_orphan_cell_without_replaying_it`; `boot_recovery_terminalizes_replay_safe_orphan_subagent_without_blocker`; `mutating_in_doubt_subagent_blocks_resume_until_user_decides` | rebuild authority, no stale snapshot, no fabricated completion, no unsafe replay |
| Disk write/partial checkpoint | `torn_tail_is_ignored_then_repaired_before_append`; `corruption_before_the_tail_still_fails_closed`; `corrupt_checkpoint_is_discarded_and_rebuilt_from_events`; `checkpoint_schema_hash_and_offset_mismatches_fall_back`; `durable_event_reports_typed_projection_degradation` | append failure does not advance authority; committed event reports degraded projection; bad cache is discarded |
| HITL suspended/owner missing | `boot_auto_resume_admission_rejects_missing_owner_workspace_and_unsafe_boundary`; TUI `consecutive_hitl_inputs_advance_the_front_immediately`; `cancelled_hitl_front_exposes_the_next_request_on_input` | attended run remains paused until an owner explicitly continues |
| Subagent/cell terminal race | `cell_terminal_and_defer_race_cannot_leave_lost_wakeup`; exact-attempt Subagent control suite; framework CommandCell waiter/retention suite | no lost wakeup, cross-attempt delivery, or Running zombie |
| 100 compactions/Goal drift | `one_hundred_turns_and_compactions_replay_without_double_accounting` | exact usage/compaction accounting and unchanged Goal SHA-256 |

The table identifies the canonical tests rather than duplicating fault logic in
a second harness. The complete matrix was executed on 2026-08-17 after commit
`82d8eda`, with all commands run `--locked --offline` and zero failures:

```bash
# echo-agent-cli
cargo test -p echo-agent-app-core --all-features provider_retry
cargo test -p echo-agent-app-core --all-features boot_recovery
cargo test -p echo-agent-app-core --all-features checkpoint
cargo test -p echo-agent-app-core --all-features \
  tasks::task_runtime::subagent_control::tests
cargo test -p echo-agent-app-core --all-features \
  cell_terminal_and_defer_race_cannot_leave_lost_wakeup
cargo test -p echo-agent-app-core --all-features \
  one_hundred_turns_and_compactions_replay_without_double_accounting

# echo-agent
cargo test -p echo_orchestration --all-features command_cell::tests
cargo test -p echo_agent --all-features subagent::control::tests
cargo test -p echo_agent --all-features controlled_
```

Exact single-test commands additionally covered provider limit pause/reset,
corrupt snapshot rebuild, unsafe Subagent recovery blocker, torn event tail,
mid-log corruption, committed projection degradation, boot auto-resume admission,
and both TUI HITL queue transitions. Results were application provider 4,
BootRecovery 8, snapshot/unsafe-boundary 2, checkpoint 5, disk 3, HITL 3,
Subagent control 5, cell race 1, Goal-drift 1; framework CommandCell 24,
Subagent mailbox 5, and controlled executor 2.

## Soak Harness Contract

`echo-agent-app-core/examples/task_runtime_soak.rs` is the committed M5b runner.
It uses only public production TaskRuntime APIs and the sole file event store.
It accepts exactly 12, 24, or 48 hours, records active monotonic time in an
atomically fsync'd ledger, drives one RunTurn every 30 seconds, executes a real
boot-recovery cycle every 120 completed turns, and validates event continuity,
checkpoint/full-fold/snapshot equality, Goal and Plan hashes, budgets, active
facts, and blockers on every cycle. The deterministic local provider avoids
external availability as a confounder; provider/network failures are covered by
the canonical matrix above.

Release invocation for each sequential gate:

```bash
cargo run -p echo-agent-app-core --release \
  --example task_runtime_soak --locked --offline -- --hours 12
```

Replace `12` with `24` and then `48` only after the preceding ledger passes.
The runner rejects a dirty worktree and pins the current commit and configuration
into each ledger.

## Real Soak Ledger

Soaks are sequential gates. A failure restarts the same duration after the fix;
it cannot be skipped in favor of a longer run.

| Duration | Commit | Configuration/provider | Events/compactions/recoveries | Failure fingerprints | Final evidence | Status |
|---:|---|---|---|---|---|---|
| 12 hours | - | - | - | - | - | Pending |
| 24 hours | - | - | - | - | - | Pending |
| 48 hours | - | - | - | - | - | Pending |
