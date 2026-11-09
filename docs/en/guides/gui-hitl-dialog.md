# GUI human confirmation dialog

EKO presents operation approvals, requests for input, and selections in one confirmation dialog. Its heading identifies the original request's conversation and content. Operation arguments and the original request ID can be expanded. Multiple requests follow the existing queue order, with subsequent items shown as pending.

“Later”, Escape, and the backdrop only hide the dialog; they do not approve, reject, or cancel a task. A pending entry remains above the conversation input. Reopening the same request preserves its draft. Expiry, cancellation, or a response from another surface follows the original request queue; the view does not infer a final result.

“Allow once”, “Reject”, “Suggest changes”, and “Allow this tool for the conversation” retain their existing response scopes. Input supports multiple lines and Chinese text. Enter inserts a newline; Command/Ctrl + Enter submits, except during IME composition. Selections use a radio option and a separate submit button, with optional instructions.

Buttons prevent duplicate submission while a reply is pending. Failure preserves the answer for retry. Successful submission acknowledges the reply; it does not mean that the task completed or that GUI won a simultaneous GUI/IM response. HumanLoop, request identity, and response settlement remain owned by the existing runtime and Tauri response path. Changing the selected page does not send the reply to a different conversation. A replacement request starts with an empty draft.

This change affects only EKO presentation, with no framework public API, SDK, tool, plugin, or example changes. The website describes product capabilities and has no corresponding dialog manual, so no website update is needed. The integration workspace owns real Tauri and simultaneous IM response acceptance; browser screenshots do not replace that acceptance.
