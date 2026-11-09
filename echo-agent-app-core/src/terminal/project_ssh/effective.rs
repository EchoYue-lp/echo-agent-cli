//! System SSH configuration facts for one immutable project connection.
//! Only hostname/account/port leave this module; keys, credentials and the
//! remaining effective SSH configuration are neither logged nor persisted.

use super::{ProjectSshConnection, TerminalTarget};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};

const CONFIGURATION_BYTES: usize = 256 * 1024;
const CONFIGURATION_TIMEOUT: Duration = Duration::from_secs(10);

impl ProjectSshConnection {
    pub(super) async fn resolve_effective_target(self: &Arc<Self>) -> Result<(), String> {
        self.resolve_effective_target_with_config(None).await
    }

    // Production always delegates configuration selection to system OpenSSH.
    // A private explicit fixture config is used only by native-resolution tests.
    async fn resolve_effective_target_with_config(
        self: &Arc<Self>,
        fixture_config: Option<PathBuf>,
    ) -> Result<(), String> {
        if self.effective_target.get().is_some() {
            return Ok(());
        }
        let connection = Arc::clone(self);
        let operation = connection.operation()?;
        let mut physical = tokio::spawn(async move {
            let mut command = tokio::process::Command::new("ssh");
            command.args(["-G", "-T"]);
            if let Some(config) = fixture_config {
                command.arg("-F").arg(config);
            }
            let TerminalTarget::Ssh { host, user, port } = &connection.target else {
                return Err("project SSH configuration target is not remote".to_string());
            };
            if let Some(user) = user {
                command.arg("-l").arg(user);
            }
            if let Some(port) = port {
                command.arg("-p").arg(port.to_string());
            }
            command.arg("--").arg(host);
            let mut child = command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(false).spawn()
                .map_err(|error| format!("cannot resolve system SSH configuration; ensure OpenSSH is available: {error}"))?;
            let stdout = child.stdout.take();
            let stderr = child.stderr.take();
            let stdout = tokio::spawn(async move {
                match stdout {
                    Some(stdout) => read_configuration(stdout).await,
                    None => Err("system SSH configuration stdout is missing".to_string()),
                }
            });
            let stderr = tokio::spawn(async move {
                match stderr {
                    Some(stderr) => read_configuration(stderr).await.map(|_| ()),
                    None => Err("system SSH configuration stderr is missing".to_string()),
                }
            });
            let waited = match tokio::time::timeout(CONFIGURATION_TIMEOUT, child.wait()).await {
                Ok(result) => result,
                Err(_) => {
                    // This is our local, non-connecting `ssh -G` child. Killing
                    // it is not presented as a remote execution stop receipt.
                    if let Err(error) = child.start_kill() {
                        let stdout = tokio::spawn(async move {
                            stdout.await.map_err(|error| {
                                format!("SSH configuration stdout join failed: {error}")
                            })??;
                            Ok::<(), String>(())
                        });
                        let stderr = tokio::spawn(async move {
                            stderr.await.map_err(|error| {
                                format!("SSH configuration stderr join failed: {error}")
                            })??;
                            Ok::<(), String>(())
                        });
                        connection
                            .retain_child_with_resources(
                                child,
                                operation,
                                Vec::new(),
                                vec![stdout, stderr],
                            )
                            .await;
                        return Err(format!(
                            "SSH configuration timed out and original local child could not be stopped; owner retained: {error}"
                        ));
                    }
                    let waited = child.wait().await;
                    let _stdout = stdout.await;
                    let _stderr = stderr.await;
                    if let Err(error) = waited {
                        connection.retain_child(child, operation).await;
                        return Err(format!(
                            "SSH configuration timed out and original child wait failed; owner retained: {error}"
                        ));
                    }
                    return Err("system SSH configuration resolution timed out; project authentication was not started".to_string());
                }
            };
            let stdout = stdout.await;
            let stderr = stderr.await;
            let status = match waited {
                Ok(status) => status,
                Err(error) => {
                    connection.retain_child(child, operation).await;
                    return Err(format!(
                        "original SSH configuration child wait failed; owner retained: {error}"
                    ));
                }
            };
            drop(operation);
            if !status.success() {
                return Err(format!(
                    "system SSH configuration resolution failed ({status}); check the alias and SSH configuration in the GUI"
                ));
            }
            stderr.map_err(|error| {
                format!("SSH configuration diagnostic reader failed: {error}")
            })??;
            let output =
                stdout.map_err(|error| format!("SSH configuration reader failed: {error}"))??;
            let target = parse_effective_target(&output)?;
            if let Err(resolved) = connection.effective_target.set(target) {
                // A cancelled observer may retry while the same original local
                // resolver still finishes. It cannot replace an already-fixed
                // descriptor with newly changed alias configuration.
                connection.verify_effective_target(&resolved)?;
            }
            Ok(())
        });
        // The native resolver retains its own operation/child/pipes if an
        // observer leaves. Do not wait without bound in the project dialog.
        tokio::time::timeout(CONFIGURATION_TIMEOUT + Duration::from_secs(5), &mut physical)
            .await
            .map_err(|_| "SSH configuration resolution remains unsettled; original project owner retained for cleanup".to_string())?
            .map_err(|error| format!("SSH configuration observation failed: {error}"))?
    }
}

async fn read_configuration(mut reader: impl AsyncRead + Unpin) -> Result<Vec<u8>, String> {
    let mut retained = Vec::new();
    let mut overflow = false;
    let mut buffer = [0_u8; 8192];
    loop {
        let read = reader
            .read(&mut buffer)
            .await
            .map_err(|error| format!("cannot read system SSH configuration: {error}"))?;
        if read == 0 {
            break;
        }
        let available = CONFIGURATION_BYTES.saturating_sub(retained.len());
        overflow |= read > available;
        retained.extend(buffer.iter().take(read.min(available)).copied());
    }
    if overflow {
        return Err(
            "system SSH configuration exceeds the bounded resolution output budget".to_string(),
        );
    }
    Ok(retained)
}

fn parse_effective_target(output: &[u8]) -> Result<TerminalTarget, String> {
    let output = std::str::from_utf8(output)
        .map_err(|_| "system SSH configuration is not UTF-8".to_string())?;
    let mut hostname = None;
    let mut user = None;
    let mut port = None;
    for line in output.lines() {
        let Some((key, value)) = line.split_once(' ') else {
            continue;
        };
        let slot = match key {
            "hostname" => &mut hostname,
            "user" => &mut user,
            "port" => &mut port,
            _ => continue,
        };
        if slot.replace(value.to_string()).is_some() {
            return Err(format!(
                "system SSH configuration returned duplicate {key} facts"
            ));
        }
    }
    let host =
        hostname.ok_or_else(|| "system SSH configuration did not resolve hostname".to_string())?;
    let user = user
        .filter(|user| !user.is_empty())
        .ok_or_else(|| "system SSH configuration did not resolve account".to_string())?;
    let port = port
        .ok_or_else(|| "system SSH configuration did not resolve port".to_string())?
        .parse::<u16>()
        .map_err(|_| "system SSH configuration returned an invalid port".to_string())?;
    TerminalTarget::Ssh {
        host,
        user: Some(user),
        port: Some(port),
    }
    .normalized()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::TerminalService;

    #[test]
    fn invalid_or_duplicate_resolution_cannot_become_a_target() {
        assert!(
            parse_effective_target(b"hostname first\nhostname second\nuser echo\nport 22\n")
                .is_err()
        );
        assert!(parse_effective_target(b"hostname first\nuser echo\nport 0\n").is_err());
        assert!(parse_effective_target(b"hostname -option\nuser echo\nport 22\n").is_err());
        assert!(parse_effective_target(b"hostname first\nport 22\n").is_err());
        assert!(parse_effective_target(b"hostname \xff\nuser echo\nport 22\n").is_err());
    }

    #[tokio::test]
    async fn oversized_native_configuration_is_drained_without_unbounded_retention() {
        let bytes = vec![b'x'; CONFIGURATION_BYTES.saturating_add(1)];
        assert!(
            read_configuration(std::io::Cursor::new(bytes))
                .await
                .is_err()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn alias_change_keeps_the_original_connection_target_and_rejects_resume_mismatch()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = tempfile::tempdir()?;
        let config = fixture.path().join("ssh_config");
        std::fs::write(
            &config,
            "Host fixture-project\n HostName first.example.invalid\n User first_fixture\n Port 2201\n CanonicalizeHostname no\n ProxyJump fixture-jump\n IdentityFile /dev/null\n",
        )?;
        let service = TerminalService::new();
        let target = TerminalTarget::Ssh {
            host: "fixture-project".into(),
            user: None,
            port: None,
        };
        let first = ProjectSshConnection::prepare(&service, "first-draft", target.clone())?;
        first
            .resolve_effective_target_with_config(Some(config.clone()))
            .await?;
        let original = first.effective_target()?.clone();
        std::fs::write(
            &config,
            "Host fixture-project\n HostName second.example.invalid\n User second_fixture\n Port 2202\n CanonicalizeHostname no\n ProxyJump fixture-jump\n IdentityFile /dev/null\n",
        )?;
        let second = ProjectSshConnection::prepare(&service, "second-draft", target)?;
        second
            .resolve_effective_target_with_config(Some(config.clone()))
            .await?;
        assert!(second.verify_effective_target(&original).is_err());
        assert_eq!(first.effective_target()?, &original);
        // Exercise system precedence with the actual master argv, without
        // starting an SSH connection or reading the user's own config/keys.
        let output = tokio::process::Command::new("ssh")
            .args(["-G", "-T", "-F"])
            .arg(config)
            .args(first.arguments(true)?)
            .arg("--")
            .arg(first.destination()?)
            .output()
            .await?;
        assert!(output.status.success());
        assert_eq!(parse_effective_target(&output.stdout)?, original);
        assert!(
            std::str::from_utf8(&output.stdout)?
                .lines()
                .any(|line| line == "proxyjump fixture-jump")
        );
        Ok(())
    }
}
