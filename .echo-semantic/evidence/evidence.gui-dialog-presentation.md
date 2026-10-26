---
schema_version: 1
id: evidence.gui-dialog-presentation
kind: evidence
observed_at: 9bcc1fc052326acc13b4223e64a51fcc867f1ccf
source_refs:
  [
    web-frontend/src/components/chat/ChatPanel.tsx,
    web-frontend/src/components/chat/HitlDialog.tsx,
    web-frontend/src/components/chat/HitlDialog.test.tsx,
    echo-agent-app-core/src/tasks/task_runtime/file_store.rs,
    echo-agent-app-core/src/tasks/task_runtime/file_shadow.rs,
    echo-agent-app-core/src/tasks/task_runtime/store/runtime.rs,
    echo-agent-app-core/src/tasks/task_runtime/store/tests.rs,
    echo-agent-app-core/src/tasks/task_runtime/types.rs,
    src/tauri/commands/task_runtime.rs,
    web-frontend/src/api/endpoints.ts,
    web-frontend/src/components/layout/ContextPane.tsx,
    web-frontend/src/components/layout/ContextPane.test.tsx,
    web-frontend/src/components/task/TaskRuntimePanel.css,
    web-frontend/src/stores/taskRuntimeStore.test.ts,
    web-frontend/src/components/layout/RightRail.tsx,
    web-frontend/src/components/task/TaskRuntimePanel.tsx,
    web-frontend/src/components/task/TaskRuntimeList.tsx,
    web-frontend/src/components/task/TaskRuntimeList.test.tsx,
    web-frontend/src/components/common/Modal.tsx,
    web-frontend/src/stores/chatStore.ts,
    web-frontend/src/stores/taskRuntimeStore.ts,
    web-frontend/src/hooks/useTauriChat.ts,
    web-frontend/src/hooks/useTauriChat.test.tsx,
    docs/zh/adr/0053-im-cron-gui-control.md,
    docs/zh/guides/gui-hitl-dialog.md,
    docs/zh/guides/gui-task-runtime-panel.md,
  ]
supports: [rule.eko-framework-consumer-authorities]
limitations:
  [
    "以下原有五项英文限制仅描述历史 APP-only 增量；不描述本次 focused 结算：",
    APP UI integration only,
    No new Rust execution,
    No native Tauri or live IM acceptance,
    No full frontend311 rerun,
    No Plan07 or main closure,
    "当前仅有 epoch15 具名 focused Rust 与 Root 前端冻结来源证据，不续期旧切点",
    "未证明 native、真实微信或 SSH、完整 requirements 或 Applied、性能、全矩阵、main CI 与发布",
  ]
---

# APP GUI presentation integration

## 来源与范围

This application-only increment reuses the original FIFO request projection, immutable delivered HITL identity, three original Tauri reply hooks, canonical TaskRuntime/Subagent projections and shared Modal. The new exported React components are frontend module interfaces; they introduce no backend/framework API, wire protocol, store, scheduler or reply authority. Their source paths are explicitly covered by the existing EKO ownership Rule.

ChatPanel sends each original request.identity directly to the existing approval/input/selection callbacks. It does not reconstruct scope or response_target from the selected page, and its label follows the original request's conversation. Successful reply submission is acknowledged and removed by the original hook after its submitted/response_target check; canonical hitl_request_released events retain their existing exact-identity removal path. The dialog's local draft/busy/open state is presentation only. A real unit regression reuses request_id with a different target/incarnation, resolves the old reply late, and checks the successor's clean draft and exact original object delivery.

Task presentation preserves original task operations and identities, distinguishes execution completion from committed task completion, bounds the visible loaded cards, and keeps late action receipts attached to their original task. Neither button returns nor closed dialogs claim physical settlement.

## 支持的结论

The APP base is local recoverable checkpoint dee03328a12fc5fb1cf47d37252bbd090775e65d. The approved underlying GUI contribution is ac7950c412e8c94a21f89537616d37e898d53d5d, final11 manifest58b4d7a8dc42db711dec7873c0fa080c11c19c35028d768801af186761cff3b6 and independent product review52b8911ceab8dbbca56a25b3a1638d2e3e02eaa8034644cd26bdd91ee23da0ea. It is a hunk-wise presentation input, not a whole-branch merge or current APP wire qualification.

On this actual APP integration, 11 relevant Vitest files pass117 tests, zero failures/ignored; changed-file ESLint, formatter and the single TypeScript/Vite production build each exit0. Independent APP integration reviewer depth_source_review reports C0/I0/M0/action0 at review-input dbfaf2253789f927fe3115446709f51d1253c9fe0e4b6ec0100dcc2b7386bd29. Product11 bytes stay frozen while normal governance/docs closure adds this Evidence, two English guides, their parity entries and two existing Rule references. Original428 protected frontend files,306 Rust/build inputs, consumer08f993, cd05 lock and exact same-six policy stay unchanged.

Raw command outputs, receipts, source input hashes and the review are retained locally under .echo-semantic/local/gui-runtime-ui-integration/. The contribution's separate311-test run, later18-test wording increment and synthetic React screenshots retain their original source scopes; they are not relabeled as APP reruns or native Tauri/IM evidence.

Normal TaskSemantic failures for the mutable presentation candidate and missing public-component/rule coverage, plus docs parity's initially missing English pages, remain preserved. They are repaired through normal preflight and actual formal materials, never by disabling the checks. Final normal scope/results are recorded after execution.

## 已知缺口

Current Rust/build input hashes match the previously qualified consumer; no new Cargo, rustfmt, metadata generation, native or live IM/SSH operation occurs in this UI increment. Full matrices, real GUI/IM competition, performance, main/CI and release acceptance remain separate. API examples and website behavior are unchanged; the two bilingual guides document the accepted UI behavior without announcing an unmerged release.

## Task-list finite history increment

The task-list branch starts from EKO780. It extends the existing TaskRuntime read adapters with finite formal-run and event pages, shared provenance visibility and captured workspace/product-data generations. The same frontend store selects an exact historical run and preserves real continuation/error facts. Existing 20-card PlanTask display and shared Modal are reused. Typed control receipts distinguish no-op and accepted requests without publishing optimistic terminal states. No Framework, SDK, channel, input queue, scheduler or Task CRUD change is introduced.

Current local raw evidence is in ignored `.echo-semantic/local/task-list-dialog/`. The no-op receipt regression produced an actual AssertionError on the original store and passed at the same entry after preserving the receipt. A same-id/new-generation entry case also fails on the exact original ContextPane; the original bytes and exact restoration are retained. The first focused store/list batch passed 31 tests. These are narrow actual Node results; later counts and current source hashes belong to their own run receipts. The four new authority paging/visibility/cursor/repair/event tests are definitions and have not run without the coordinated Rust slot.

Rust compilation, genuine ts-rs generation, final Node source checks, legal high-data query/render measurements, native Tauri themes/windows, full workspace/main CI and website synchronization are still pending. Historical APP/UI results above retain their original snapshots and are not relabelled as the new branch's qualification. Broad Rule/Finding status remains unchanged.

The initial new four-file Node result was 53 passed with whole noEmit TypeScript, scoped lint and format0. Those exact bytes are retained in ignored `source-before-chat-isolation/`. A subsequent real consumer trace found the conflict Continue callback reading the selected `activeRun`, requiring the same-store chat/history separation. The follow-up preserves current chat fields, retains at most one historical presentation, and targets all detail actions explicitly. The exact Hook uses the original admitted workspace/run and a prompt-reference/original-view fence. New cross-history, chat-consumer and successor-prompt definitions have not executed during the coordinator's journal measurement quiet window; the earlier53/TS result is not relabelled as their qualification.

The post-isolation five-file focused batch passes 130 tests with zero failures/ignored, whole noEmit TypeScript0, eleven-file ESLint0 (zero warnings), and twelve-file formatter check0. Original Hook successor-prompt control fails with a real AssertionError (B becomes null) and succeeds after the exact Hook restoration; older formatting scopes remain separate. The two TaskRuntime display points now directly reuse the existing latestSubagentRunsByTask selector. A controlled startedAt-only order produces a real 1-versus-0 completion-count AssertionError on identical test bytes for that controlled before/after pair; restoring the canonical selector passes the same entry. No new attempt sorting algorithm or Subagent store is added. The initially failing compiler memo lint is preserved; reading the existing presentation through the original Zustand selector preserves memoization without disabling any rule.

These results still do not compile the five new real-authority paging tests, generate the two Rust DTO exports, qualify native Tauri/list performance, or close Plan13/performance-baseline. That combined Rust batch is prepared for the coordinator's next window and will save original results outside its exact owned target before cleanup. The current chat consumers remain unchanged; history is at most one additional presentation cache in the same store, and controls plus prompts use their original exact run identities.

The final task-source check initially reported one undeclared machine signal, newStateAuthority, after introducing the additional presentation snapshot. Normal incremental preflight now conservatively records that signal while explicitly retaining the original runtime/journal authority and one Zustand store; it does not assert a new task state authority. No checker, external tool, old Finding status or runtime policy was changed.

Source review of the existing projection_only_orphan_is_not_a_task_run control refined strict enumeration before Rust execution: the same parameterized loop preserves NotFound/no-journal orphan exclusion, but propagates actual I/O, malformed-journal and recovery failures. The new cold corruption regression replaces the initial over-strict missing-journal assumption; no new registry or authority is introduced. Docs parity first reported the old ADR label/language/manifest issues; precise label normalization, one English translation and two existing ADR-pair registrations repair them, preserving all prior entries and Domain qualification prose. The original failures and final68-pair/56-ADR success remain separate local receipts.

Plan13 review correction epoch2: preserve source9662/130 and original review findings. Added generation-required task controls through the original AppState captured-host lease (application layer); no Framework changes or new runtime authority. Five controlled UI cases produced four real assertion failures plus one pre-existing generation positive, then passed after the targeted store fixes; the initial unisolated fixture run is retained separately and is not counted as five behavior reds. Whole relevant Node result now 135/135 before final formatting, with final TS/lint/format qualification still recorded by their actual subsequent exits. Three new real AppState generation/owned-relink/control-lease definitions are not yet compiled or executed. Explicit current-run history and superseded busy ownership use the existing one store and one historical snapshot.

Final epoch2 frontend qualification: actual final five files136/136, whole noEmit TypeScript0, eleven changed-TS ESLint0/zero warnings, twelve-file Prettier0. Source includes the same-run replacement generation receipt/pending control (47 assertion red → 48 same case green). Old135/130 results retain their snapshots. Backend generation wrapper and three real AppState test definitions remain not compiled/not executed until the single owned Rust window. Framework actual canonical dependency is now frozen steer-safe-points/echo-agent@a1a30/treefe97085 after explicit Root alias reassignment; no Task Cargo/metadata/native reader existed at reassignment.

## 2026-10-27 当前 focused 范围与历史分界

本次 observed_at 绑定 CLI 可恢复提交 `9bcc1fc052326acc13b4223e64a51fcc867f1ccf`；Framework 源为 `69296a4dea1047be7fb91b7ab06a839ee57dda7e`。原观察 `source:23cc1e60947062f2c151779231f7b8ca5459761fd3461af16bc8e896c9c17a1b` 与上述 APP-only、117/311、task-list 53/130/135/136 等历史结果全部保留在各自来源范围。当前观察修复只意味着本文件26条 source_refs 可以从具名提交读回，不把历史未执行项倒签为已执行。

本次新增可支持的直接结论是原 GUI TaskRuntime 分页/控制适配在 epoch15 的真实定向 Rust 结果：`task_history_` 五项与 `task_control_generation_` 三项自然0、零失败/忽略和 warning；两项真实 DTO 导出及既有类型来源回执分别保留。整个 epoch15 含 channels 的68项、无 channels 的37项与生产/GUI caller 检查也有独立自然0、最终零编译 warning，但它们证明的是各自具体选择，不是把68项等同 GUI 分页测试。

Root 最终前端批次在冻结的465输入上实际75文件/580测试通过，TypeScript＋Tauri 模式 Vite build、全 ESLint（零 warning）与格式检查均自然0。该原批次使用其独立 snapshot，来源字节未改变；没有以旧 APP 结果或本次文档 schema 检查替代这份新回执。

原始依据保存在本仓 ignored 工程目录，属于回执位置而非正式 source_refs：

- `.echo-semantic/local/core-repair-20261027/epoch-15/batch-return.json`，SHA256 `835d282d42a7bb3b88d991d357df47a446ee7e54499bc345c99017fa8a3b472c`，保留68/37、调用方检查、导出与限界。
- `.echo-semantic/local/local-milestones-20261027/integration-checkpoint/frontend-verification.json`，SHA256 `47ffe815c29cef5c575143783d5f446066c9f2b8be0b281e465fe8864524bff7`，保留75文件/580测试与465输入及各自然出口。
- `.echo-semantic/local/core-repair-20261027/epoch-15/post-clean-receipt.json`，SHA256 `399c87c2c793c612549c272dccd87d6a6e3d73f7e41716ca925d478475364fc1`，准确 cargo clean 自然0、原 target 不存在；制品在清理前移到 target 外，reader与实际工作进程已结算。
- 同目录 `historical-qualification-corrections.json` 保留原编译/fixture失败、无效反例、19 warning、误报与明确撤销；本次不删除或重标原件。

这不是 native Tauri/真实微信扫码与消息/SSH验收，不证明完整有效 requirements、Drained→Applied 闭环、性能基准、全 workspace/全 feature 矩阵、精确 main CI 或发布。原宽 Rule/Finding 仍未因此关闭，外部 echo-semantic 合同修复继续排除；本次仅维护两份现有语义材料，不增加产品 API、示例或官网行为。
