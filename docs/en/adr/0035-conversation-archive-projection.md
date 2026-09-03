# ADR 0035: Conversation Archive Projection

## Background

EKO needs archive and permanent-delete actions in the conversation list. The
reusable `echo-agent` `ConversationStore` owns transcript persistence and does
not have a product-specific visibility lifecycle.

## Decision

Keep transcript deletion on the existing application aggregate-delete path and
store archive state in an EKO-owned, workspace-scoped JSON projection under the
EKO data root. The projection is atomically written and shared by Tauri and TUI
commands. GUI list/search responses include `archived`; delete also removes the
marker on a best-effort basis because transcript deletion is authoritative.

## Alternatives considered

- Add `archived` to the framework `ConversationStore` contract: rejected because
  archive is EKO product/UI policy.
- Keep archive only in browser `localStorage`: rejected because GUI windows and
  TUI/channel surfaces would diverge.

## Consequences

Archive and restore are reversible visibility changes; permanent delete removes
the conversation aggregate and application-owned projections. A corrupt or
unavailable archive projection does not prevent startup. Archive mutation errors
remain observable, while failure to clear a marker after deletion is only a
warning because the transcript deletion has already committed.
