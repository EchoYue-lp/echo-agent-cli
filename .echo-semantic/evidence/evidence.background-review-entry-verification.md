---
schema_version: 1
id: evidence.background-review-entry-verification
kind: evidence
observed_at: f57b90a2b0962fa5561f8839a3df70be1f261ffa
source_refs: [src/tauri/desktop.rs, echo-agent-app-core/src/runtime.rs, echo-agent-app-core/src/workspace/runtime.rs, echo-agent-app-core/src/evolution/review_integration.rs, echo-agent-app-core/src/evolution/background_review_owner.rs, echo-agent-app-core/src/state/tests.rs]
supports: [finding.background-review-entry-settlement, behavior.background-review-caller-owned-settlement]
limitations:
  - Remote CI and mainline delivery remain pending
  - No native window or real provider was driven
---

# Background Review entry verification

## 支持的结论

On main ff27188e, the existing Evolution tests passed 61/61. They did not cover
Tauri normal process exit or workspace-local recovery through bootstrap. Source
evidence from locked Tauri 2.11.2 proves Builder::run calls nonreturning App::run.
The real workspace bootstrap regression failed within its 15-second bound;
temporary boundary diagnostics identified generation mismatch followed by a
ProductData/command-flow rollback wait cycle. Temporary diagnostics were removed.

Separate actual red tests demonstrated an old Memory fingerprint being mistaken
for a new review write, recall telemetry alone causing that same attribution,
the seed primary missing a promoted rule after workspace target binding, and
same-project relinking closing both the new and recovery runtime's Review owner.
The explicit-project regression also failed through real bootstrap: selecting Q
loaded persisted workspace project P instead.
Fixture import/visibility errors and an interrupted exploratory run are not
counted as behavioral failure evidence.

## 来源与范围

After the final explicit-project increment, the integrated app-core focused
suite passed 94/94, exit 0, in
`review38-final-project-context-green-1791684284641.log` under the Git worktree
state directory. Its SHA-256 is
`7b4b46b71125100869e156a1b2bc7e15ae1f899b38149a44302e7cb508ad2067`.
It includes Evolution, bootstrap, lifecycle, workspace runtime, same-project link
and real P/Q bootstrap with six existing/future Agents. The desktop adapter
suite passed 7/7 in `/tmp/review38-gui-focused.log`, using the real application
lifecycle owner and TauriBridgeSupervisor for cancellation and blocked join.
The real handle/parked observer test passed caller-drop and shutdown cases with
durable Interrupted receipt, committed Memory, and no fabricated Outcome/Inbox.
Docs parity passed 59 pairs / 49 ADRs; diff checks passed on that candidate.

On the final source (including the behavior-preserving Clippy let-chain fix),
both strict Clippy commands, fmt/check, and app-core no-default check passed.
`cargo test --workspace --all-features --locked` exited 0 in
`review38-final-workspace-tests-1791684974954.log`: main app-core suite 1,611
passed / 9 ignored, CLI library 285 passed, and all integration/doctest summaries
reported zero failures. Nested subprocess summaries are not added to these
suite counts. The initial Clippy run found one collapsible-if warning; its
equivalent correction and the same command's successful retry are retained.
The conditional GUI binary check and GUI-feature tests also exited 0; the latter
reported 216 library tests, 1 Tauri entry test and 6 JSONL integration tests
passed, with 3 ignored doctests. Its receipt is
`review38-final-gui-tests-1791685657271.log`. All applicable local gates are green.

## 已知缺口

Two independent scoped reviewers returned pass with zero blocking findings for
the frozen source and the 94-test receipt. Their scopes and snapshot anchors are
recorded in audit.background-review-entry-independent-rereview. Remote CI and
delivery must still be recorded. Tauri restart/setup panic and complete
cleanup of early post-fork initialization errors are not proven by these tests.

历史观察归属：原 `source:2b2aadbb89e3fb339505cc4e42f53c0466f982f1310e5fc1c30720acaefa33c3` 经官方源码摘要函数与 Git `f57b90a2b0962fa5561f8839a3df70be1f261ffa` 精确对应；本次只恢复同一历史切点的可恢复版本锚，不代表当前源码重验。原 source_refs、锚点、roles 与状态保持。
