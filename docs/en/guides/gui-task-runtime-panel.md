# GUI task list and details

The conversation's task entry opens the right context panel. It shows the TaskRun loaded for the current conversation, using the original TaskRuntime queries, file records, and event projections. Opening the panel does not turn an ordinary chat turn into a task. This is not a separate directory of all historical tasks, and it adds no task store or server pagination protocol.

## Reading task state

The header shows the goal, original run status, trigger mode, and latest update. “Committed complete” counts completed tasks in the Todo projection; “Execution complete” also considers the corresponding Subagent's successful completion record. The list makes the distinction visible when a Subagent finishes execution while its task remains under review or finalization. Tasks that have not expanded show progress as unrecorded rather than an inferred percentage.

The plan supports status filters and search by title, task ID, or role. Each page presents at most 20 cards. Filtering and pagination affect only the loaded projection, without changing backend queries or the task graph. Cards show recovery reasons requiring attention and the original summary; details show long titles in full. This presentation bound is not evidence that a large-data performance benchmark passed.

A card opens its description, dependencies, acceptance criteria, files, artifacts, attempts, timestamps, and run identities. Existing Subagent records link to their original details. TaskRun, PlanTask, and SubagentRun retain separate identities; cards do not copy ownership of run state. The run route remains diagnostic information in the details and is not presented as proof of a GUI, IM, or cron origin.

## Editing and actions

Pause, cancel, continue, retry, edit title, skip, and add task reuse the original `taskRuntimeStore` actions. Details use the shared Modal, with focus containment, Escape to close, and return to the trigger. Closing details does not approve, cancel, or skip a task.

Titles are edited in details. Failure preserves the draft and original error. Skipping requires explicit confirmation, and Goal acceptance follows its original records. Retry retains the original task/run state and attempt limits. A pending edit or skip confirmation cannot submit after a background update makes the task ineligible.

Pending actions prevent duplicate submissions and show that a request is being processed. On return, the receipt reports only that the request returned and the current state from the original run record. A button click is not execution completion, physical cancellation settlement, or acceptance. Read and submission errors stay visible; the backend retains state authority.

“Run controls and acceptance” preserves the original goal editing, budgets, recovery settlement, Goal acceptance, and usage views. List, details, and receipts support the existing light/dark themes and narrow windows without new dependencies.

## Validation boundary

This application presentation change follows [task state authority](../adr/0015-task-graph-status-authority.md) and [bounded query projections](../adr/0008-taskruntime-bounded-query-projections.md). It adds no runtime architecture or public API. Behavior tests live in `TaskRuntimeList.test.tsx`, and existing task state and context panel tests remain applicable.

Component screenshots use explicitly synthetic task data and establish only the actual React layout and interactions. Complete performance benchmarks, native desktop windows, real IM/cron triggers, and final main integration are recorded separately. Screenshots do not replace those checks. API examples are unchanged, and the website continues describing delivered mainline capabilities without announcing unmerged visual changes.
