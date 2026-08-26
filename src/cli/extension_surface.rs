//! Thin text-surface adapter for the app-core Extension command authority.

use std::sync::Arc;

use echo_agent_app_core::extension_commands::{
    ExtensionCommandDispatcher, ExtensionCommandIdentity, ExtensionCommandReceipt, ExtensionKind,
    ExtensionRequestScope, parse_extension_command,
};
use echo_agent_app_core::state::AppState;

fn kind_for_root(root: &str) -> Option<ExtensionKind> {
    match root {
        "skills" => Some(ExtensionKind::Skills),
        "plugins" => Some(ExtensionKind::Plugins),
        "mcp" => Some(ExtensionKind::Mcp),
        "hooks" => Some(ExtensionKind::Hooks),
        "lsp" => Some(ExtensionKind::Lsp),
        "browser" => Some(ExtensionKind::Browser),
        _ => None,
    }
}

fn command_text(root: &str, arguments: &str) -> String {
    let arguments = arguments.trim();
    if arguments.is_empty() {
        format!("/{root}")
    } else {
        format!("/{root} {arguments}")
    }
}

async fn capture_scope(state: &AppState) -> Result<ExtensionRequestScope, String> {
    let product_data = state
        .current_product_data()
        .await
        .map_err(|error| error.to_string())?;
    ExtensionRequestScope::new(
        product_data.workspace_id(),
        product_data.generation(),
        None,
        None,
    )
    .map_err(|error| error.to_string())
}

/// Convert one text command into the shared typed request and pin the exact
/// workspace generation before dispatch. The app-core service owns parsing,
/// mutation admission, specialist execution, and settlement semantics.
pub(crate) async fn dispatch_extension_command(
    state: Option<&Arc<AppState>>,
    conversation_id: Option<&str>,
    root: &str,
    arguments: &str,
) -> ExtensionCommandReceipt {
    let identity = ExtensionCommandIdentity::random();
    let kind = kind_for_root(root).unwrap_or(ExtensionKind::Skills);
    let mut request =
        match parse_extension_command(&command_text(root, arguments), identity.clone()) {
            Ok(Some(request)) => request,
            Ok(None) => {
                return ExtensionCommandReceipt::failed(
                    kind,
                    identity,
                    "unresolved",
                    format!("Unsupported Extension command root '/{root}'"),
                );
            }
            Err(error) => {
                let scope = match state {
                    Some(state) => capture_scope(state).await.ok(),
                    None => None,
                };
                return match scope {
                    Some(scope) => ExtensionCommandReceipt::failed_scoped(
                        error.extension.unwrap_or(kind),
                        identity,
                        scope,
                        error.to_string(),
                    ),
                    None => ExtensionCommandReceipt::failed(
                        error.extension.unwrap_or(kind),
                        identity,
                        "unresolved",
                        error.to_string(),
                    ),
                };
            }
        };

    let Some(state) = state else {
        return ExtensionCommandReceipt::failed(
            request.kind(),
            request.identity(),
            "unresolved",
            "Extension control is unavailable during application bootstrap",
        );
    };
    let scope = match capture_scope(state).await {
        Ok(scope) => scope,
        Err(error) => {
            return ExtensionCommandReceipt::failed(
                request.kind(),
                request.identity(),
                "unresolved",
                error,
            );
        }
    };
    request.scope = Some(scope.clone());
    ExtensionCommandDispatcher::new(Arc::clone(state))
        .dispatch_for_scope(
            scope,
            request,
            conversation_id
                .filter(|value| !value.trim().is_empty())
                .unwrap_or("interactive-extension-control")
                .to_string(),
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_app_core::extension_commands::{
        ExtensionCommandReceipt, ExtensionCommandStatus, ExtensionMessageReceipt,
        ExtensionReceiptMeta,
    };

    #[test]
    fn text_surface_parser_claims_the_complete_extension_union() -> Result<(), String> {
        let commands = [
            ("skills", "sync all --force", ExtensionKind::Skills),
            ("plugins", "validate .", ExtensionKind::Plugins),
            ("mcp", "enable local", ExtensionKind::Mcp),
            ("hooks", "test PreToolUse *", ExtensionKind::Hooks),
            ("lsp", "restart rust", ExtensionKind::Lsp),
            ("browser", "tabs list", ExtensionKind::Browser),
        ];
        for (root, arguments, expected) in commands {
            let identity = ExtensionCommandIdentity::new("request-a", "operation-a")
                .map_err(|error| error.to_string())?;
            let request = parse_extension_command(&command_text(root, arguments), identity)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("/{root} was not claimed"))?;
            assert_eq!(request.kind(), expected);
            assert_eq!(request.operation_id, "operation-a");
        }
        Ok(())
    }

    fn browser_receipt(status: ExtensionCommandStatus) -> ExtensionCommandReceipt {
        ExtensionCommandReceipt::Browser {
            meta: ExtensionReceiptMeta {
                request_id: "request-a".to_string(),
                operation_id: "operation-a".to_string(),
                authority_scope: "workspace-a".to_string(),
                workspace_generation: "generation-a".to_string(),
                sender_id: None,
                sender_incarnation: None,
                status,
                error: (status == ExtensionCommandStatus::Degraded)
                    .then(|| "fanout incomplete".to_string()),
            },
            receipt: Some(ExtensionMessageReceipt {
                action: "status".to_string(),
                message: "browser projection".to_string(),
            }),
        }
    }

    #[test]
    fn text_receipt_rendering_preserves_committed_and_degraded_status() {
        let committed = browser_receipt(ExtensionCommandStatus::Committed).display_message();
        assert!(committed.contains("[COMMITTED]"));
        assert!(committed.contains("workspace_generation=generation-a"));
        assert!(committed.contains("operation_id=operation-a"));

        let degraded = browser_receipt(ExtensionCommandStatus::Degraded).display_message();
        assert!(degraded.contains("[DEGRADED]"));
        assert!(degraded.contains("error=fanout incomplete"));
    }
}
