# GUI task list and details

The task entry opens the original right context pane. Run history lists finite pages of formal TaskRuns in the current Workspace; the sidebar’s recent/new task conversations remain separate. Choose a history row to inspect that exact run, or use “Current conversation” to follow the current conversation again. Ordinary chat turns do not become tasks merely because history is opened. No second task store is created.

## Reading task state

The header shows the goal, original run status, trigger mode, and latest update. “Committed complete” counts completed tasks in the Todo projection; “Execution complete” also considers the corresponding Subagent's successful completion record. The list makes the distinction visible when a Subagent finishes execution while its task remains under review or finalization. Tasks that have not expanded show progress as unrecorded rather than an inferred percentage.

The plan supports status filters and search by title, task ID, or role. Each page presents at most 20 cards. Filtering and pagination affect only the loaded projection, without changing backend queries or the task graph. Cards show recovery reasons requiring attention and the original summary; details show long titles in full. This presentation bound is not evidence that a large-data performance benchmark passed.

A card opens its description, dependencies, acceptance criteria, files, artifacts, attempts, timestamps, and run identities. Existing Subagent records link to their original details. TaskRun, PlanTask, and SubagentRun retain separate identities; cards do not copy ownership of run state. The run route remains diagnostic information in the details and is not presented as proof of a GUI, IM, or cron origin.

## Editing and actions

Pause, cancel, continue, retry, edit title, skip, and add task reuse the original `taskRuntimeStore` actions. Details use the shared Modal, with focus containment, Escape to close, and return to the trigger. Closing details does not approve, cancel, or skip a task.

Titles are edited in details. Failure preserves the draft and original error. Skipping requires explicit confirmation, and Goal acceptance follows its original records. Retry retains the original task/run state and attempt limits. A pending edit or skip confirmation cannot submit after a background update makes the task ineligible.

Pending actions prevent duplicate submissions and show that a request is being processed. On return, the original typed receipt distinguishes an accepted request, no-op and retry path, alongside the current state from the run record. A button click is not execution completion, physical cancellation settlement, or acceptance. Read and submission errors stay visible; the backend retains state authority.

“Run controls and acceptance” preserves the original goal editing, budgets, recovery settlement, Goal acceptance, and usage views. List, details, and receipts support the existing light/dark themes and narrow windows without new dependencies.

## Scheduled runs and Weixin result notifications (source candidate)

The scheduled-task page separates notification settings from execution state.
Settings bind the original task definition and the scanning user of the connected
Weixin account. A prior phone direct message supplies an explicit context
reference. Changes affect later admissions only; an active occurrence cannot be
retargeted to the currently displayed project or a replacement login.

History queries notification status only for a record with a resolved original
run identity. Task completion, notification admission, effect-start, and platform
Accepted/Rejected/Unknown are independent facts. “Weixin accepted the result
notification” does not establish phone receipt or read status. Runs without
notifications do not acquire guessed destinations. Content comes from original
TaskRuntime results and artifact references, not the webhook's finished label.
Notification failure does not rerun the task; Unknown warns against direct resend.

These source paths do not turn the current-conversation task panel into another
notification store. Compilation, native GUI/authentication/messages and main CI
remain unqualified. Queued Weixin replies now remain data in the original pump
route; the original owner drives output only after claim. This targets the old
32-slot/strict-cursor/HITL backlog counterexample. Greater-than-32 plus later
`/reply` pressure checks have not run. The complete Weixin/HITL Outcome is not
accepted. See
[configuration](../configuration.md#personal-weixin-source-candidate-compilation-and-native-checks-pending)
and [ADR 0054](../adr/0056-personal-weixin-channel.md).

## Validation boundary

This application presentation change follows [task state authority](../adr/0015-task-graph-status-authority.md) and [bounded query projections](../adr/0008-taskruntime-bounded-query-projections.md). It adds finite EKO-only run/event read page contracts on the original authority; Framework and SDK APIs remain unchanged. Behavior tests live in `TaskRuntimeList.test.tsx`, and existing task state and context panel tests remain applicable.

Component screenshots use explicitly synthetic task data and establish only the actual React layout and interactions. Complete performance benchmarks, native desktop windows, real IM/cron triggers, and final main integration are recorded separately. Screenshots do not replace those checks. API examples are unchanged, and the website continues describing delivered mainline capabilities without announcing unmerged visual changes.


## Run and event pages

Run pages are ordered by creation time and run ID. Status changes and refresh start a new live query; continuation does not promise a transaction snapshot. Errors retain the displayed page and allow explicit retry. The next-page control is shown only with the original continuation, and no invented total is displayed. Event history is read in finite pages; “Continue reading execution records” follows the original sequence until no unread tail remains. A partially loaded execution history remains labelled incomplete. These bounds do not establish a high-data or native performance result.

History details and current chat remain separate views. Reading an older failure does not turn the current chat into a failure; its Continue prompt and progress still refer to the original live task.

Task controls target the captured workspace generation as well as the selected run. If the project location was replaced, a stale action is rejected; reload the current run before retrying. Selecting the live run in history keeps that detail selected when chat advances to another run.

### Steer and Goal controls

The composer shows the durable Steer queue above the input. Editing a queued item uses one journal CAS: its `input_id` and queue position remain stable, its revision increases, and an already executing or stale item is rejected. The queue displays its authoritative phase and drained state; a safe-point attempt also shows retained or rejected on the same input row without changing backend authority. Goal editing uses the original TaskRuntime Goal revision/CAS. Saving while a run is active first requests pause, then shows safe-point waiting and update confirmation. The GUI observes the canonical paused run and visible work; the backend CAS remains responsible for proving that physical execution is quiescent. Only an explicit active-owner rejection is retried during a bounded 30-second observation; a conflict, failed read, or rejected update keeps the draft and reports the error. A successful response is shown as updated only after the canonical Goal text and next revision are readable. Saving does not automatically resume the run. Ending a Goal uses the existing cancellation path: an accepted request is distinguished from a canonical cancelled run, and an empty Goal is never written.

Goal controls bind the current workspace, conversation, workspace generation and run. Leaving that view retires its pending UI observer; a late response cannot clear another Goal draft or busy indicator, even after returning to the same run. Steer editing similarly preserves a failed draft and rejects a changed input identity. Inputs owned by an IM channel are labelled as channel-managed and cannot be dragged, edited, executed or removed from this GUI queue.

In a Steer editor, Enter saves and Escape closes the edit. These shortcuts wait until input-method composition is complete, so confirming a Chinese or Japanese candidate does not submit an unfinished edit.

The workspace creation dialog applies the same Enter protection to its name field across all three project sources. Confirming an input-method candidate does not create or switch workspaces.

An unsuccessful immediate-execution response means delivery is unconfirmed. Read the same row's original retained/rejected status and error before retrying; the response alone does not prove the input is waiting for a safe point.
