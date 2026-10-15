---
schema_version: 4
slug: cancellation-observation-consumer/plan
outcome:
  summary: EKO consumes framework B observations without false terminals or
    releasing original resources, and rejects unresolved same-owner execution
    across restart.
  acceptance:
    - Typed observation preserves exact run/execution identity and original
      resource ownership without terminal state/receipt mutation or automatic
      replay.
    - Task and analysis original authorities persist admission intent before
      work; restart and shutdown cannot silently release unresolved owner;
      genuine matching settlement clears once.
    - Normal receipts/public event shapes remain unchanged; scoped validation
      and independent review pass before Root delivery.
    - Aggregate observation preserves a reachable original continuation and
      actual normal work drain; exact flow/lease partitions and weak return
      edges prevent deadlock or self-retention.
out_of_scope:
  - New public Task/Analysis status, protocol/UI fields, second supervisor, new
    execution or reset/retry policy.
  - SDK/framework/website production writes, Cargo before resource lease,
    external delivery before Root authorization.
design_ref: docs/supreme/specs/cancellation-observation-consumer/design.md
design_sections:
  - ref: design.md § 系统边界与权威
    digest: sha256:d904d8d126f7129d29eaacb83aa78901f5362f7adc566830f0c6776d40336b2d
  - ref: design.md § 设计
    digest: sha256:2427b3dd757ba83392f3920d29fd9ce9cb987aeb1ca59e0cef9c3538fcb228d7
  - ref: design.md § 异常与边界
    digest: sha256:338dc201535e02f6d1cf8aca6fac9230cda7203de41645ec269edbbd3199169e
  - ref: design.md § 验收标准
    digest: sha256:09b892dc4e2152e7d6a2e7e4c62020e0029b27e5a871a09a626a84c4b8592db6
delivery_ref: null
todos:
  - id: preserve-typed-consumption
    summary: Consume fallible framework APIs through the original shared driver and
      Task physical owner, retaining typed observations before terminal
      classification.
    files:
      - echo-agent-app-core/src/chat_driver.rs
      - echo-agent-app-core/src/tasks/task_runtime/executor
      - echo-agent-app-core/src/foreground_turn.rs
      - echo-agent-app-core/src/tasks/task_runtime/continuation.rs
      - src/tui/events.rs
      - src/cli/repl.rs
      - src/cli/channels.rs
      - src/cli/modes.rs
      - src/tauri/commands/chat.rs
      - src/tauri/commands/task_runtime.rs
      - echo-agent-app-core/src/state/app_state.rs
      - echo-agent-app-core/src/run_driver.rs
      - echo-agent-app-core/src/tasks/service.rs
      - echo-agent-app-core/src/tasks/task_runtime/task_execute_tool.rs
      - echo-agent-app-core/src/tasks/task_runtime/task_tools.rs
      - echo-agent-app-core/src/conversation_input.rs
      - src/cli/channels/input_pump.rs
      - echo-agent-app-core/src/scheduler/runner.rs
    acceptance:
      - Observation cannot enter terminal fallback/resolution/retry;
        physical-owner success/failed/cancelled receipts retain original
        behavior and data.
      - All shared foreground callers retain observation through the last
        terminal emission decision, preserving busy/ingress/generation and
        preventing continuation replay; no surface-specific new status.
      - Cron end-to-end acceptance also requires the original framework
        occurrence authority to preserve typed unsettled callback ownership; the
        C adapter alone is not this proof.
  - id: retain-task-owner
    summary: Keep unknown original Task driver resources in the existing supervisor
      and FileTaskShadow journal, including restart and admission protection.
    files:
      - echo-agent-app-core/src/tasks/task_runtime/store/runtime.rs
      - echo-agent-app-core/src/tasks/task_runtime/store/workspace.rs
      - echo-agent-app-core/src/tasks/task_runtime/types.rs
      - echo-agent-app-core/src/tasks/task_runtime/event_rebuild.rs
      - echo-agent-app-core/src/tasks/task_runtime/store/tests.rs
      - echo-agent-app-core/src/tasks/task_runtime/store/journal.rs
      - echo-agent-app-core/src/tasks/task_runtime/review.rs
      - echo-agent-app-core/src/tasks/task_runtime/file_store.rs
      - echo-agent-app-core/src/tasks/task_runtime/file_shadow.rs
      - echo-agent-app-core/src/tasks/task_runtime/planner.rs
      - echo-agent-app-core/examples/task_runtime_soak.rs
    acceptance:
      - Unresolved exact intent blocks duplicate and boot recovery mutations,
        caller receives typed observation, shutdown reports uncertainty, and
        only original matching physical evidence releases resources.
      - Canonical TaskExecution result (including explicit null), exact claim
        and interruption request round-trip through the same journal/checkpoint
        and revision adapter; TaskSpec/Todo IPC is unchanged.
  - id: retain-analysis-owner
    summary: Retain original analysis execution/flow and atomic directory intent
      before side effects; preserve unknown through original owner control and
      restart.
    files:
      - echo-agent-app-core/src/analysis.rs
      - echo-agent-app-core/src/product_data_io.rs
      - src/tauri/commands/analysis.rs
    acceptance:
      - Unknown cannot write terminal run record, drop original pool/flow, clear
        outputs or restart execution; matching durable result safely clears
        exact fence, write failure rejects before effects.
  - id: retain-original-aggregate-owner
    summary: Continue the same application lifecycle owner through exact
      ordinary-work drain and nonterminal uncertainty without resource cycles or
      false final receipts.
    files:
      - echo-agent-app-core/src/runtime.rs
      - echo-agent-app-core/src/agent_pool/admission.rs
      - echo-agent-app-core/src/agent_pool/pool.rs
      - echo-agent-app-core/src/product_data_io.rs
      - echo-agent-app-core/src/workspace/runtime.rs
      - echo-agent-app-core/src/state/app_state.rs
      - echo-agent-app-core/src/state/workspace.rs
      - echo-agent-app-core/src/tasks/task_runtime/store/workspace.rs
      - echo-agent-app-core/src/tasks/task_runtime/store/runtime.rs
      - echo-agent-app-core/src/agent_pool/tests.rs
    acceptance:
      - The same Runtime/Services/Settlement/diagnostic root retains original
        owner/resources; actual ordinary flows/leases drain before uncertainty
        observation, with no permanent dummy task or unpolled-future storage.
      - Task supervisor weak resource indices and counter-only opaque workspace
        guards remove actual strong cycles while original Task/Tool services
        remain available.
      - Actual normal-plus-unknown aggregate, dropped/repeated observer,
        matching late proof and full normal shutdown controls pass; no unknown
        resource release or false final drain occurs.
      - After Runtime/Services drop, the existing typed
        ApplicationLifecycleError receipt alone retains the root; subsystem
        links are Weak and genuine cleanup plus receipt drop releases the Weak
        graph.
artifact_id: plan:74c5690b-6f61-4d70-82f5-a769dfab055a
lifecycle: ready
---
