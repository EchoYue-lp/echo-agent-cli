//! REPL HITL Provider — stdin/stdout human-in-the-loop for CLI mode.
//!
//! Prints approval/input requests to stdout and reads responses from stdin.
//! Similar to `ConsoleHumanLoopProvider` but integrated with the REPL's
//! output formatting.

use echo_agent::human_loop::{
    ApprovalScope, HumanLoopKind, HumanLoopProvider, HumanLoopRequest, HumanLoopResponse,
};
use futures::future::BoxFuture;

/// REPL-based HumanLoopProvider that uses stdin/stdout.
pub struct ReplHumanLoopProvider;

impl ReplHumanLoopProvider {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ReplHumanLoopProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl HumanLoopProvider for ReplHumanLoopProvider {
    fn request(
        &self,
        req: HumanLoopRequest,
    ) -> BoxFuture<'_, Result<HumanLoopResponse, echo_agent::error::ReactError>> {
        Box::pin(async move {
            match req.kind {
                HumanLoopKind::Approval => handle_approval(&req),
                HumanLoopKind::Input => handle_input(&req),
                HumanLoopKind::Selection => handle_selection(&req),
            }
        })
    }
}

fn handle_approval(
    req: &HumanLoopRequest,
) -> Result<HumanLoopResponse, echo_agent::error::ReactError> {
    println!("\n╔══════════════════════════════════════════════════════╗");
    println!("║              TOOL APPROVAL REQUIRED                ║");
    println!("╚══════════════════════════════════════════════════════╝");

    if let Some(ref tool_name) = req.tool_name {
        println!("  Tool: {}", tool_name);
    }
    if let Some(ref risk) = req.risk_level {
        println!("  Risk: {:?}", risk);
    }
    println!("  {}", req.prompt);

    if let Some(ref args) = req.args {
        println!("\n  Arguments:");
        let formatted = serde_json::to_string_pretty(args).unwrap_or_default();
        for line in formatted.lines() {
            println!("    {}", line);
        }
    }

    println!("\n  [y] 同意  [n] 拒绝  [m] 修改意见  [a] 本次会话全部同意");
    println!("  Choice: ");

    // Read from stdin (blocking — acceptable for REPL mode)
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .map_err(|e| echo_agent::error::ReactError::Other(format!("stdin read error: {e}")))?;

    let choice = input.trim().to_lowercase();
    match choice.as_str() {
        "y" | "yes" | "" => Ok(HumanLoopResponse::Approved),
        "a" | "all" => Ok(HumanLoopResponse::ApprovedWithScope {
            scope: ApprovalScope::SessionAllTools,
        }),
        "n" | "no" => {
            println!("  请输入拒绝原因（直接回车跳过）: ");
            let mut reason = String::new();
            std::io::stdin().read_line(&mut reason).map_err(|e| {
                echo_agent::error::ReactError::Other(format!("stdin read error: {e}"))
            })?;
            let reason = reason.trim().to_string();
            Ok(HumanLoopResponse::Rejected {
                reason: if reason.is_empty() {
                    Some("User rejected".to_string())
                } else {
                    Some(reason)
                },
            })
        }
        "m" | "modify" => {
            println!("  请输入修改意见（Agent 将据此调整方案）: ");
            let mut feedback = String::new();
            std::io::stdin().read_line(&mut feedback).map_err(|e| {
                echo_agent::error::ReactError::Other(format!("stdin read error: {e}"))
            })?;
            let feedback = feedback.trim().to_string();
            Ok(HumanLoopResponse::Rejected {
                reason: Some(if feedback.is_empty() {
                    "用户要求修改".to_string()
                } else {
                    format!("用户修改意见: {}", feedback)
                }),
            })
        }
        _ => Ok(HumanLoopResponse::Rejected {
            reason: Some("User rejected".to_string()),
        }),
    }
}

fn handle_input(
    req: &HumanLoopRequest,
) -> Result<HumanLoopResponse, echo_agent::error::ReactError> {
    println!("\n╔══════════════════════════════════════════╗");
    println!("║         INPUT REQUESTED                  ║");
    println!("╚══════════════════════════════════════════╝");
    println!("  {}", req.prompt);
    println!("\n  > ");

    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .map_err(|e| echo_agent::error::ReactError::Other(format!("stdin read error: {e}")))?;

    let text = input.trim().to_string();
    if text.is_empty() {
        Ok(HumanLoopResponse::Rejected {
            reason: Some("Empty input".to_string()),
        })
    } else {
        Ok(HumanLoopResponse::Text(text))
    }
}

fn handle_selection(
    req: &HumanLoopRequest,
) -> Result<HumanLoopResponse, echo_agent::error::ReactError> {
    println!("\n╔══════════════════════════════════════════╗");
    println!("║         SELECTION REQUIRED               ║");
    println!("╚══════════════════════════════════════════╝");
    println!("  {}", req.prompt);

    if let Some(ref options) = req.options {
        println!("\n  Options:");
        for (i, opt) in options.iter().enumerate() {
            println!("    [{}] {}", i + 1, opt);
        }
    }
    println!("\n  Choice (number or text): ");

    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .map_err(|e| echo_agent::error::ReactError::Other(format!("stdin read error: {e}")))?;

    let trimmed = input.trim();

    // Try to parse as number, otherwise use as-is
    let selection = if let Ok(idx) = trimmed.parse::<usize>() {
        if let Some(ref options) = req.options {
            if idx >= 1 && idx <= options.len() {
                options[idx - 1].clone()
            } else {
                trimmed.to_string()
            }
        } else {
            trimmed.to_string()
        }
    } else {
        trimmed.to_string()
    };

    Ok(HumanLoopResponse::Selection {
        selection,
        instructions: None,
    })
}
