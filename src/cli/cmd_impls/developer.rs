//! CLI adapters for the shared application developer commands.

use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, SlashCommand};
use echo_agent_app_core::developer_commands::DeveloperCommandRegistry;
use echo_agent_app_core::terminal::TerminalEvent;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;

struct DeveloperCommand {
    name: &'static str,
    description: &'static str,
}

impl SlashCommand for DeveloperCommand {
    fn name(&self) -> &'static str {
        self.name
    }

    fn aliases(&self) -> &'static [&'static str] {
        if self.name == "terminal" {
            &["term"]
        } else {
            &[]
        }
    }

    fn description(&self) -> &'static str {
        self.description
    }

    fn category(&self) -> CommandCategory {
        CommandCategory::Coding
    }

    fn run<'a>(
        &'a self,
        ctx: &'a CommandContext,
        args: &'a [&'a str],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = CommandOutcome> + Send + 'a>> {
        Box::pin(async move {
            if matches!(self.name, "lsp" | "browser") {
                let receipt = crate::cli::extension_surface::dispatch_extension_command(
                    ctx.app_state.as_ref(),
                    ctx.conversation_id.as_deref(),
                    self.name,
                    &args.join(" "),
                )
                .await;
                println!("{}", receipt.display_message());
                return CommandOutcome::Continue;
            }
            let Some(app_state) = ctx.app_state.as_ref() else {
                println!("Developer tools are unavailable during application bootstrap.");
                return CommandOutcome::Continue;
            };
            let events = app_state.terminal.subscribe();
            let registry =
                DeveloperCommandRegistry::new(app_state.terminal.clone(), Some(app_state.clone()))
                    .with_browser_conversation_id(
                        ctx.conversation_id
                            .as_deref()
                            .unwrap_or("cli-browser-control"),
                    );
            match registry.execute(self.name, args).await {
                Ok(output) => {
                    println!("{}", output.message);
                    if let Some(terminal_id) = output.attached_terminal {
                        observe_terminal(events, terminal_id);
                    }
                }
                Err(error) => println!("/{} failed: {error}", self.name),
            }
            CommandOutcome::Continue
        })
    }
}

fn observe_terminal(events: tokio::sync::broadcast::Receiver<TerminalEvent>, terminal_id: String) {
    tokio::spawn(async move {
        if let Err(error) =
            forward_terminal_events(events, terminal_id.clone(), tokio::io::stdout()).await
        {
            tracing::warn!(%terminal_id, %error, "CLI terminal observer stopped");
        }
    });
}

async fn forward_terminal_events<W>(
    mut events: tokio::sync::broadcast::Receiver<TerminalEvent>,
    terminal_id: String,
    mut output: W,
) -> Result<(), String>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    loop {
        match events.recv().await {
            Ok(TerminalEvent::Output { id, bytes }) if id == terminal_id => {
                output
                    .write_all(&bytes)
                    .await
                    .map_err(|error| format!("terminal output write failed: {error}"))?;
                output
                    .flush()
                    .await
                    .map_err(|error| format!("terminal output flush failed: {error}"))?;
            }
            Ok(TerminalEvent::Exited { id, .. }) if id == terminal_id => return Ok(()),
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                let notice = format!(
                    "\n[terminal output lagged by {skipped} event(s); subsequent output remains live]\n"
                );
                output
                    .write_all(notice.as_bytes())
                    .await
                    .map_err(|error| format!("terminal lag notice write failed: {error}"))?;
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return Ok(()),
        }
    }
}

pub fn register_all(registry: &mut crate::cli::command::CommandRegistry) {
    for descriptor in DeveloperCommandRegistry::commands() {
        registry.register(Arc::new(DeveloperCommand {
            name: descriptor.name,
            description: descriptor.summary,
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_registers_the_shared_management_catalog() {
        let mut registry = crate::cli::command::CommandRegistry::new();
        register_all(&mut registry);
        for descriptor in DeveloperCommandRegistry::commands() {
            assert!(registry.get(descriptor.name).is_some());
        }
        assert!(registry.get("term").is_some());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pre_dispatch_receiver_keeps_fast_terminal_output() -> Result<(), String> {
        use tokio::io::AsyncReadExt;

        let terminal = echo_agent_app_core::terminal::TerminalService::new();
        let receiver = terminal.subscribe();
        terminal
            .create("cli-fast".to_string(), None, 24, 80)
            .await?;
        terminal
            .write("cli-fast", b"printf cli-fast-output; exit\r")
            .await?;
        let (mut reader, writer) = tokio::io::duplex(64 * 1024);
        let forward = tokio::spawn(forward_terminal_events(
            receiver,
            "cli-fast".to_string(),
            writer,
        ));
        let mut bytes = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            reader.read_to_end(&mut bytes),
        )
        .await
        .map_err(|_| "CLI terminal observer did not settle".to_string())?
        .map_err(|error| error.to_string())?;
        forward
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert!(String::from_utf8_lossy(&bytes).contains("cli-fast-output"));
        Ok(())
    }
}
