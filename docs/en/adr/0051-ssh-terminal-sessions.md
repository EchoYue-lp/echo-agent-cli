# ADR 0051: Explicit SSH targets and original terminal session settlement

## Status

accepted

## Context

Approved Plan06 (plan:08c22af5-243e-4460-a70c-2c5e2ec186b6, SHA 7f0a96b913a019d2623283ea711f89a93daa2d60a1e86b4635f72797081624c0) continues from EKO 2b52251 and framework e4e5039. TerminalService already owns PTYs, but GUI creation precedes observation, tab/hide changes unmount observers, byte chunks are decoded separately, and close removes a session after clone_killer. In portable-pty 0.9.0, the Unix clone only sends SIGHUP; the original Child::kill adds forced termination after the HUP grace period. Original child wait and reader join remain necessary.

## Alternatives

1. System OpenSSH inside original create, ready frontend observers before creation, and retained original Session resources/settlement. Selected.
2. A new SSH library, daemon, executor/TaskRun, backend reserve/start or replay store. Outside the approved boundary.
3. Connected animation or shell prompt as login evidence, or kill return as Closed evidence. Rejected.

## Decision

System OpenSSH owns protocol, authentication, config, agent/key and host-key policy; portable-pty owns PTY/Child primitives. EKO TerminalService/TerminalSession remains the only session, target, admission and resource authority; Tauri only adapts. Framework, SDK pin, Agent ShellTool permissions and TaskRuntime do not change. Manual GUI connection does not authorize model execution; the model currently exposes no TerminalService tool.

Creation remains one existing create call. Target is local or structured ssh (host/alias, optional user/port), passed as independent system ssh argv. No shell command concatenation, unspecified-config override, weaker host-key option, credential storage, hardcoded server or local fallback. Spawn proves only a local process start.

A tab mounts xterm, output/exit/gap observers and bounded serial queues before create. All tabs remain mounted across selection/hide. Raw PTY Uint8Array goes directly to xterm; write callbacks release in-flight budget. Input is also serial and bounded. Real broadcast lag or overflow is visible and does not imply full history/VT replay. Every asynchronous setup step handles cancellation and cleans only its own instance. A dispatched create retains its original Promise; cancellation compensates the exact completed creation with close, retaining a retryable tab on failure.

The original Session retains the original Child, output reader JoinHandle and one settlement-task handle. Close requests settlement through original Child::kill/try_wait/wait and a real reader join. Only complete proof permits exact-instance removal and one exit event. Callers borrow the wait handle; cancellation or bounded observation timeout does not abort settlement or take an unfinished handle. Kill/wait/reader failures retain resources/target for retry and cannot publish normal Closed; retry cannot erase an irrecoverable reader error. Direct-child cleanup does not prove process-group or remote-side-effect termination.

The existing ApplicationLifecycleOwner synchronously closes terminal admission. close_all shares the creation boundary; incomplete terminal settlement retains the same application owner for retry. Event-bridge join is not PTY reader join. Actual id/pid/target and close values are reflected in manual TS contracts; invented cwd/created_at fields disappear.

## Consequences

Source/test preparation is not delivered runtime acceptance. This phase has no Cargo/native/remote lease. Rust, native windows, real authentication/remote interaction, full matrices and main/CI remain unverified until execution. An old GUI binary cannot qualify the new protocol. Only the coordinated acceptance phase uses the user's real target; product/test fixtures use generic targets.

The existing examples/lh6_product_soak.rs has no terminal/SSH consumer and needs no source migration. Any later example change must enter the original compilation chain. The coordinator mirrors only qualified exact sources to the website; no deployment here. Changed Rust inputs do not inherit the ace3 risk fingerprint automatically; later independently reassess the same exact six risks without acceptance expansion.

## Implementation references and validation

The installed OpenSSH 10.3p1 official ssh(1) manual describes destination, -l, -p and interactive TTY; user SSH config is not read. Locked portable-pty 0.9.0 lib.rs/unix.rs supplies exact Child kill/wait and EIO-to-EOF behavior. The official xterm API accepts raw Uint8Array and signals asynchronous write completion through its callback.

References: [OpenSSH ssh(1)](https://man.openbsd.org/ssh.1), [portable-pty 0.9.0 Child](https://docs.rs/portable-pty/0.9.0/portable_pty/trait.Child.html), [xterm Terminal API](https://xtermjs.org/docs/api/terminal/classes/terminal/). This follows the explicit-target/system-SSH pattern of VS Code Remote SSH while implementing only the approved interactive terminal.

Plan06 acceptance covers Node ordering/cleanup/raw UTF-8/tab/hide/queue/failed-close counterexamples; later Rust tests cover a HUP-ignoring direct child, wait/reader/cancellation/retry and shutdown admission. Actual Tauri/remote interaction remains separate evidence; unexecuted cases remain incomplete.
