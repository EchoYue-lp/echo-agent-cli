---
schema_version: 1
id: evidence.gui-dialog-presentation
kind: evidence
observed_at: source:23cc1e60947062f2c151779231f7b8ca5459761fd3461af16bc8e896c9c17a1b
source_refs:
  [
    web-frontend/src/components/chat/ChatPanel.tsx,
    web-frontend/src/components/chat/HitlDialog.tsx,
    web-frontend/src/components/chat/HitlDialog.test.tsx,
    web-frontend/src/components/layout/RightRail.tsx,
    web-frontend/src/components/task/TaskRuntimePanel.tsx,
    web-frontend/src/components/task/TaskRuntimeList.tsx,
    web-frontend/src/components/task/TaskRuntimeList.test.tsx,
    web-frontend/src/components/common/Modal.tsx,
    web-frontend/src/stores/chatStore.ts,
    web-frontend/src/stores/taskRuntimeStore.ts,
    web-frontend/src/hooks/useTauriChat.ts,
    docs/zh/adr/0053-im-cron-gui-control.md,
    docs/zh/guides/gui-hitl-dialog.md,
    docs/zh/guides/gui-task-runtime-panel.md,
  ]
supports: [rule.eko-framework-consumer-authorities]
limitations:
  [
    APP UI integration only,
    No new Rust execution,
    No native Tauri or live IM acceptance,
    No full frontend311 rerun,
    No Plan07 or main closure,
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
