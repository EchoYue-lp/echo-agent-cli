//! Test execution and output parsing for the coding test/fix loop.

use super::detector::ProjectType;
use std::path::Path;

/// Result of running a test command.
#[derive(Debug, Clone)]
pub struct TestResult {
    pub command: String,
    pub exit_code: Option<i32>,
    pub passed: bool,
    pub failures: Vec<TestFailure>,
    pub raw_stdout: String,
    pub raw_stderr: String,
}

/// A single test failure with source location.
#[derive(Debug, Clone)]
pub struct TestFailure {
    pub test_name: String,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub message: String,
}

/// Run a test command in the given working directory.
pub async fn run_test_command(cmd: &str, cwd: &Path) -> std::io::Result<TestResult> {
    let output = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(cwd)
        .output()
        .await?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let exit_code = output.status.code();
    let passed = output.status.success();

    let failures = if passed {
        Vec::new()
    } else {
        parse_test_output(&stdout, &stderr, &ProjectType::detect(cwd))
    };

    Ok(TestResult {
        command: cmd.to_string(),
        exit_code,
        passed,
        failures,
        raw_stdout: stdout,
        raw_stderr: stderr,
    })
}

/// Run a lint command.
pub async fn run_lint_command(cmd: &str, cwd: &Path) -> std::io::Result<(bool, String, String)> {
    let output = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(cwd)
        .output()
        .await?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    Ok((output.status.success(), stdout, stderr))
}

/// Parse test output into structured failures.
pub fn parse_test_output(
    stdout: &str,
    stderr: &str,
    project_type: &ProjectType,
) -> Vec<TestFailure> {
    match project_type {
        ProjectType::Rust => parse_rust_test_output(stdout),
        _ => parse_generic_test_output(stdout, stderr),
    }
}

/// Parse `cargo test` output.
fn parse_rust_test_output(stdout: &str) -> Vec<TestFailure> {
    let mut failures = Vec::new();
    let mut current_test = String::new();
    let mut current_message = Vec::new();
    let mut in_failure = false;

    for line in stdout.lines() {
        if line.starts_with("test ") && line.contains("... FAILED") {
            if in_failure && !current_test.is_empty() {
                failures.push(TestFailure {
                    test_name: current_test.clone(),
                    file: None,
                    line: None,
                    message: current_message.join("\n"),
                });
            }
            current_test = line
                .trim_start_matches("test ")
                .split(" ...")
                .next()
                .unwrap_or("")
                .to_string();
            current_message = Vec::new();
            in_failure = true;
        } else if in_failure && line.contains("FAILED") {
            // end of this failure's output
            failures.push(TestFailure {
                test_name: current_test.clone(),
                file: None,
                line: None,
                message: current_message.join("\n"),
            });
            current_test = String::new();
            current_message = Vec::new();
            in_failure = false;
        } else if in_failure {
            current_message.push(line.to_string());
        }
    }

    // Handle last failure if any
    if in_failure && !current_test.is_empty() {
        failures.push(TestFailure {
            test_name: current_test,
            file: None,
            line: None,
            message: current_message.join("\n"),
        });
    }

    failures
}

/// Generic test failure parser (for non-Rust projects).
fn parse_generic_test_output(stdout: &str, stderr: &str) -> Vec<TestFailure> {
    let mut failures = Vec::new();
    let combined = format!("{stdout}\n{stderr}");

    // Look for common patterns: FAIL, Error, assertion, etc.
    for line in combined.lines() {
        let lower = line.to_lowercase();
        if lower.contains("fail") || lower.contains("error") || lower.contains("assert") {
            if let Some(pos) = line.find(':') {
                let file_part = &line[..pos];
                if let Some(file_path) = extract_file_path(file_part) {
                    failures.push(TestFailure {
                        test_name: line[pos + 1..].trim().to_string(),
                        file: Some(file_path),
                        line: None,
                        message: line.to_string(),
                    });
                    continue;
                }
            }
            failures.push(TestFailure {
                test_name: line.to_string(),
                file: None,
                line: None,
                message: line.to_string(),
            });
        }
    }

    failures
}

/// Try to extract a file path from a string like "src/main.rs:42".
fn extract_file_path(s: &str) -> Option<String> {
    let s = s.trim();
    if s.contains('/') && s.contains('.') {
        let parts: Vec<&str> = s.split(':').collect();
        if !parts.is_empty() {
            let candidate = parts[0];
            if candidate.contains('.') {
                return Some(candidate.to_string());
            }
        }
    }
    None
}

/// Format test failures as a prompt for the agent.
pub fn format_failures_as_prompt(failures: &[TestFailure]) -> String {
    if failures.is_empty() {
        return "All tests passed.".to_string();
    }
    let mut lines = vec![format!(
        "The following {} test(s) are failing:\n",
        failures.len()
    )];
    for f in failures {
        lines.push(format!("- {}", f.test_name));
        if let Some(ref file) = f.file {
            lines.push(format!("  File: {}", file));
        }
        if !f.message.is_empty() {
            lines.push(format!("  {}", f.message));
        }
    }
    lines.push("\nPlease fix the code so all tests pass.".to_string());
    lines.join("\n")
}
