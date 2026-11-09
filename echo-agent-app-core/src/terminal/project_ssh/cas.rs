//! One controlled server operation for the original cooperating-writer CAS.
//! The script is fixed; file paths and bytes travel only in structured stdin.

use super::generations::ConnectionSource;
use super::remote_path;
use super::sftp::remote_failure;
#[cfg(test)]
use super::sftp::unavailable;
use base64::Engine;
use echo_agent::error::{Result, ToolError};
use echo_agent::sandbox::{SandboxCommand, SandboxExecutor};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

const HELPER: &str = include_str!("cas.py");
const MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
const PROBE: &str = "if ! command -v python3 >/dev/null 2>&1; then printf '%s\\n' '{\"result\":\"unsupported\"}'; exit 0; fi; exec python3 -I -S -B -c \"$1\" --probe";

#[derive(Serialize)]
struct Request<'a> {
    path: &'a str,
    expected_sha256: String,
    replacement_base64: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ErrorPhase {
    CreateParent,
    OpenLock,
    Lock,
    Read,
    TemporaryOpen,
    Permissions,
    Write,
    FileSync,
    TemporaryClose,
    Replace,
    DirectorySync,
    Unlock,
    CloseLock,
}

#[derive(Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
enum Reply {
    Available,
    Unsupported,
    Invalid,
    Conflict,
    Replaced,
    Error {
        phase: ErrorPhase,
        errno: Option<i32>,
        may_have_changed: bool,
        cleanup_errno: Option<i32>,
    },
}

pub(super) async fn compare_and_swap(
    source: ConnectionSource,
    directory: PathBuf,
    path: PathBuf,
    expected: &[u8],
    replacement: &[u8],
) -> Result<bool> {
    let request = request(&path, expected, replacement)?;
    let sandbox = super::sandbox::ProjectSshSandbox::with_source(source);
    // This is a read-only capability observation through the same original
    // connection. The mutating helper rechecks availability itself as well;
    // the probe is not a qualification receipt for that server/filesystem.
    let capability = sandbox
        .execute(
            SandboxCommand::program(
                "/bin/sh",
                vec![
                    "-c".to_string(),
                    PROBE.to_string(),
                    "eko-file-cas-probe".to_string(),
                    HELPER.to_string(),
                ],
            )
            .with_working_dir(&directory),
        )
        .await?;
    if !capability.success() || capability.output_truncated {
        return Err(unsupported());
    }
    match serde_json::from_str::<Reply>(&capability.stdout) {
        Ok(Reply::Available) => {}
        _ => return Err(unsupported()),
    }
    // Isolated Python ignores project/user module search paths and never
    // writes bytecode. A project file named json.py must not replace the
    // standard primitives used for data protection in this fixed operation.
    let result = sandbox
        .execute(
            SandboxCommand::program(
                "python3",
                vec![
                    "-I".to_string(),
                    "-S".to_string(),
                    "-B".to_string(),
                    "-c".to_string(),
                    HELPER.to_string(),
                ],
            )
            .with_working_dir(directory)
            .with_stdin(request),
        )
        .await?;
    if !result.success() || result.output_truncated {
        return Err(remote_failure(
            "remote CAS did not return its completed structured result; replacement may already have happened, verify before retry",
            true,
        ));
    }
    completed_reply(&result.stdout)
}

fn request(path: &std::path::Path, expected: &[u8], replacement: &[u8]) -> Result<String> {
    if replacement.len() > MAX_REQUEST_BYTES {
        return Err(invalid(
            "remote CAS input exceeds the bounded helper request budget",
        ));
    }
    let path = remote_path(path).map_err(|error| invalid(&error))?;
    let request = serde_json::to_string(&Request {
        path,
        expected_sha256: hex::encode(Sha256::digest(expected)),
        replacement_base64: base64::engine::general_purpose::STANDARD.encode(replacement),
    })
    .map_err(|_| invalid("remote CAS input could not be encoded"))?;
    // This is an encoded wire-byte budget, not a character preview/truncation.
    if request.len() > MAX_REQUEST_BYTES {
        return Err(invalid(
            "encoded remote CAS input exceeds the bounded helper request budget",
        ));
    }
    Ok(request)
}

fn completed_reply(output: &str) -> Result<bool> {
    match serde_json::from_str::<Reply>(output) {
        Ok(Reply::Replaced) => Ok(true),
        Ok(Reply::Conflict) => Ok(false),
        Ok(Reply::Unsupported) => Err(unsupported()),
        Ok(Reply::Invalid) => Err(invalid(
            "remote CAS rejected its structured input before acquiring the file lock",
        )),
        Ok(Reply::Error {
            phase,
            errno,
            may_have_changed,
            cleanup_errno,
        }) => {
            // Even a pre-rename failure can leave sidecar/temporary effects.
            // Never translate an I/O error into a false conflict/non-write.
            Err(remote_failure(
                format!(
                    "remote CAS failed during {phase:?} (errno {errno:?}, target may have changed: {may_have_changed}, cleanup errno {cleanup_errno:?}); verify before retry"
                ),
                true,
            ))
        }
        _ => Err(remote_failure(
            "remote CAS result was invalid or incomplete; replacement may already have happened, verify before retry",
            true,
        )),
    }
}

fn unsupported() -> echo_agent::error::ReactError {
    std::io::Error::new(std::io::ErrorKind::Unsupported, "remote file CAS requires available Python3 POSIX flock, atomic replace and fsync primitives; no local or read-then-rename fallback").into()
}

fn invalid(message: &str) -> echo_agent::error::ReactError {
    ToolError::InvalidParameter {
        name: "file_compare_and_swap".to_string(),
        message: message.to_string(),
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::tools::{ToolFailure, ToolRecoveryAction, ToolSideEffect};

    #[test]
    fn conflict_is_false_but_post_replace_error_is_a_possible_effect() -> Result<()> {
        assert!(!completed_reply("{\"result\":\"conflict\"}")?);
        let error = completed_reply("{\"result\":\"error\",\"phase\":\"directory_sync\",\"errno\":5,\"may_have_changed\":true}")
            .err().ok_or_else(|| unavailable("post-replace failure was incorrectly successful"))?;
        let failure = ToolFailure::from_error(&error, false);
        assert_eq!(failure.side_effect, ToolSideEffect::Possible);
        assert_eq!(failure.recovery, ToolRecoveryAction::VerifyThenRetry);
        assert!(!failure.allows_automatic_retry());
        Ok(())
    }

    #[test]
    fn missing_capability_does_not_fall_back_to_a_read_and_rename() {
        let error = completed_reply("{\"result\":\"unsupported\"}").err();
        assert!(
            matches!(error, Some(echo_agent::error::ReactError::Io(error)) if error.kind() == std::io::ErrorKind::Unsupported)
        );
    }

    #[test]
    fn binary_input_is_only_stdin_data_and_hash_covers_complete_expected_bytes() -> Result<()> {
        let expected = "中文 🦀\0完整内容".as_bytes();
        let replacement = b"'\n$(touch should-not-run)\0";
        let payload = request(
            std::path::Path::new("/remote/it's 项目/file"),
            expected,
            replacement,
        )?;
        let decoded: serde_json::Value =
            serde_json::from_str(&payload).map_err(|_| invalid("test payload JSON is invalid"))?;
        assert_eq!(
            decoded
                .get("expected_sha256")
                .and_then(serde_json::Value::as_str),
            Some(hex::encode(Sha256::digest(expected)).as_str())
        );
        assert!(!HELPER.contains("should-not-run"));
        assert!(!PROBE.contains("should-not-run"));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn original_fixed_helper_passes_native_cooperating_writer_fixtures()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/terminal/project_ssh/cas_tests.py");
        let result = std::process::Command::new("python3")
            .args(["-I", "-S", "-B"])
            .arg(fixtures)
            .output()?;
        if !result.status.success() {
            return Err(std::io::Error::other(format!(
                "native CAS fixture failed: {}",
                String::from_utf8_lossy(&result.stderr)
            ))
            .into());
        }
        Ok(())
    }
}
