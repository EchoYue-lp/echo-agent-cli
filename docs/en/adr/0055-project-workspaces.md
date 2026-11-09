# ADR 0055: Project locations and Workspace execution

- Date: 2026-10-25
## Status

accepted

Task-branch qualification is separate from a main release.

## Context and alternatives

A Workspace is a project or engineering workspace; tasks and conversations belong to it. Creating a Workspace selects an existing local directory, creates one new local directory, or opens a project over SSH. Creating a task uses that Workspace instead of creating another folder. A standalone SSH tab does not route file tools, instructions, Git, isolation or evidence to a project.

We considered a terminal-only addition, a complete remote EKO/IDE service, and a local GUI/Agent loop with location-aware file and command transports. We choose the third option. It follows the consistent file/execution location principle of [VS Code Remote SSH](https://code.visualstudio.com/docs/remote/ssh) and the client-side location handling of [TRAMP remote processes](https://www.gnu.org/software/emacs/manual/html_node/tramp/Remote-processes.html), without introducing a remote IDE server. A [standard SFTP client](https://github.com/openssh-rust/openssh-sftp-client/tree/ad1b77f1d7b35fc39183bae9d68034e3d9f9e8e6) supplies the protocol; EKO does not implement SSH/SFTP framing or cryptography.

## Decision

- The original Workspace manifest and registry own location. `Workspace.root` remains local product data. `metadata.project_location` describes `Local { path }` or `Ssh { host, user, port, path }`; remote paths remain strings and never undergo local canonicalization. Old manifests resolve `project_root`, then `root`, without moving existing data.
- `project_location` preserves configuration/display information. `metadata.project_execution_location` records the verified effective SSH hostname/account/port and canonical remote directory in the same manifest. System `ssh -G` resolves the endpoint once, and the connection pins it. Reconnection must match that saved identity. The existing `project_root_revision` covers both values; no second registry or version store is introduced.
- Existing directories are linked in place. New directories use exclusive, single-directory creation. Failure rolls back only application-owned data and a newly created directory that is still empty. Removing a Workspace preserves project code, including legacy code beside product data.
- Each Workspace host owns its target. Primary and pooled Agents within it share the original ToolManager; different hosts receive independent providers. Each invocation still carries its actual cwd and isolated checkout. A provider must not replace that with a fixed project root.
- The framework supplies generic `FileSystem`, `SandboxExecutor` and original tool injection points. EKO owns Workspace locations, version policy, SSH authentication, GUI and local product-data policy.
- Project structure, gitignore, repository instructions, project Skills and Subagents read the selected target. User configuration, user Skills, memory, conversations, journals and local materialized artifacts keep their local owners.
- Required project context is prepared at a fallible entry before a model runs. Primary construction propagates failure. Subagents prepare after isolation supplies the invocation cwd, then delegate through the original invocation execution methods. A warning from an optional projector cannot stand in for stopping an invocation with unreadable required rules.
- Each primary turn retains its strictly prepared project values in the original turn registration. The existing EKO projector republishes them after Execute/reset or checkpoint restoration while preserving Domain, TaskRuntime, turn policy, hot memory and command watches. Ordinary turns without a TaskRun use that same turn owner. Parallel primary verifications use their existing execution IDs as turn IDs, keeping the original root message as the trigger. Fresh Subagents compose their own actual-cwd values with the original Domain-only renderer; they do not inherit the primary checkout. File preparation and Git commands retain the invocation's original resource guards through physical settlement.
- GUI project file actions and terminals capture Workspace ID and generation, then borrow the registered host's control/I/O receipt. Interactive terminals require neither an Agent execution slot nor a configured LLM/full-auto permission. The terminal retains the receipt through its original child and output-reader settlement, so relinking/deletion cannot claim the project is idle while that PTY remains active.
- The six non-native GUI file actions use an explicit 30-second observation on the existing ProductDataIo operation. The bound includes waiting for its original semaphore and returns a typed Unconfirmed observation of that same u64 operation; it is not a cancellation or remote execution deadline. Timeout or a dropped observer marks only that admitted id retained. Its detached physical task, permit and captured host receipt remain alive until the original physical completion settles it. A late failure remains in the original failure receipt. Local/global calls retain their previous waiting behavior. Ordinary sidebar focus changes keep the existing multi-host behavior; relinking, reconnecting and host retirement retain their original idle gates.
- A physically completed observed result carries that same original operation through its result envelope. Actual consumption acknowledges it; dropping an unconsumed buffered result records its failure before settling the same id. Successful channel send alone is not acknowledgement. A normally consumed business error is returned to its caller without being recorded as an unobserved shutdown failure.
- Authentication uses the existing PTY renderer inside the project dialog. Passwords never enter metadata, a persistent frontend store or model context. Successful creation transfers the original connection owner to the Workspace; cancellation closes only its draft. Authentication masters are separate from ordinary terminal tabs.
- Shutdown seals admission, closes and observes interactive PTYs before joining dependent host/I/O owners, and closes shared authentication masters only after their Agent/file consumers settle. Killing a local SSH process does not prove remote side effects stopped.

## Consequences

Plain SSH provides no evidence of a remote OS sandbox. Code execution, resource limits and isolation must use actual capabilities, without borrowing a local Seatbelt receipt. Cancellation, disconnection and writes that might already have reached the server preserve the original typed failure/unknown semantics. Dropping a future proves neither remote cancellation nor rollback; writes are not automatically replayed.

The original TaskRun/target revision freezes verified location identity and revision. Recovery and cross-Workspace execution reject mismatches. Git, hashes and evidence read the real target or an explicitly sourced, integrity-checked local materialization, never a local namesake.

## Qualification and documentation

Regressions cover old local data, source payloads, rollback, effective identity changes, actual checkout cwd, rule-read failure, connection handoff, physical terminal settlement, and bounded observation with controlled physical barriers, dropped observers and independent operation ids. The observer does not repair a backend that prematurely returns an uncertain result and drops physical resources; transport unknown-settlement qualification remains separate. Real remote authentication and a user-selected directory remain separate acceptance evidence.

This is an EKO GUI/IPC feature, not an additional framework SDK example or standalone CLI surface. Framework provider examples use that repository's own compilation chain. Website mirrors follow the original synchronization process after a real EKO source checkpoint; a task branch is not a released main revision.
