//! `/diff` — Visual diff preview command.
//!
//! Shows colored diffs for file edits, file comparisons, or git changes.
//!
//! Usage:
//! - `/diff`              — Show git diff (unstaged + staged) with ANSI colors
//! - `/diff <file>`       — Show diff between `<file>` and its `.bak` backup (last edit)
//! - `/diff <f1> <f2>`   — Compare two files
//! - `/diff --git`        — Show full git diff
//! - `/diff --staged`     — Show staged (cached) diff
//! - `/diff --html <file>` — Output HTML diff (for web frontend)

use crate::cli::command::{CommandCategory, CommandContext, CommandOutcome, cmd};
use echo_agent_app_core::api::diff::{
    generate_unified_diff, parse_unified_diff, render_diff_ansi, render_diff_html,
};
use std::path::Path;

async fn cmd_diff(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    if args.is_empty() {
        // No args: show git diff with colors
        return show_git_diff(ctx, false).await;
    }

    let first = args[0];

    match first {
        "--git" | "-g" => show_git_diff(ctx, false).await,
        "--staged" | "--cached" => show_git_diff(ctx, true).await,
        "--help" | "-h" => {
            print_diff_help();
            CommandOutcome::Continue
        }
        "--html" => {
            // HTML output for web frontend
            let rest = &args[1..];
            show_diff_html(ctx, rest).await
        }
        _ => {
            // File-based diff
            if args.len() == 1 {
                // Single file: compare with .bak backup
                show_backup_diff(first).await
            } else if args.len() == 2 {
                // Two files: compare them
                show_file_diff(args[0], args[1]).await
            } else {
                println!("Usage: /diff [<file> | <file1> <file2> | --git | --staged]");
                CommandOutcome::Continue
            }
        }
    }
}

// ── Git diff ─────────────────────────────────────────────────────────────

async fn show_git_diff(ctx: &CommandContext, staged: bool) -> CommandOutcome {
    // Try to find project root from coding loop, or fall back to cwd
    let root = if let Some(ref cl) = ctx.coding_loop {
        cl.lock().await.project_root.clone()
    } else {
        std::env::current_dir().unwrap_or_default()
    };

    if !root.join(".git").exists() {
        println!("Not a git repository.");
        return CommandOutcome::Continue;
    }

    let mut git_args: Vec<&str> = vec!["diff"];
    if staged {
        git_args.push("--cached");
    }
    // Enable color output from git
    git_args.push("--color=always");

    let output = tokio::process::Command::new("git")
        .args(&git_args)
        .current_dir(&root)
        .output()
        .await;

    match output {
        Ok(o) if o.status.success() => {
            let diff_text = String::from_utf8_lossy(&o.stdout);
            if diff_text.trim().is_empty() {
                if staged {
                    println!("No staged changes.");
                } else {
                    println!("No unstaged changes.");
                }
            } else {
                let label = if staged { "Staged" } else { "Unstaged" };
                println!("\n\x1b[1m--- {} Changes ---\x1b[0m\n", label);
                // Git already outputs ANSI colors with --color=always
                print!("{}", diff_text);

                // Show stats summary
                let stat_args: Vec<&str> = if staged {
                    vec!["diff", "--cached", "--stat"]
                } else {
                    vec!["diff", "--stat"]
                };
                if let Ok(stat_out) = tokio::process::Command::new("git")
                    .args(&stat_args)
                    .current_dir(&root)
                    .output()
                    .await
                {
                    let stat_text = String::from_utf8_lossy(&stat_out.stdout);
                    if let Some(summary_line) = stat_text.lines().last() {
                        println!("\n\x1b[2m{}\x1b[0m", summary_line);
                    }
                }
            }
        }
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            println!("git diff failed: {}", err);
        }
        Err(e) => {
            println!("Failed to run git diff: {}", e);
        }
    }

    CommandOutcome::Continue
}

// ── Backup diff (single file) ────────────────────────────────────────────

async fn show_backup_diff(file_path: &str) -> CommandOutcome {
    let path = Path::new(file_path);
    let bak_path = std::path::PathBuf::from(format!("{}.bak", file_path));

    if !path.exists() {
        println!("File not found: {}", file_path);
        return CommandOutcome::Continue;
    }

    if !bak_path.exists() {
        println!("No backup file found: {}", bak_path.display());
        println!("Tip: Backup files (.bak) are created when the agent edits a file.");
        println!("     Use /diff --git to see all changes, or /diff <file1> <file2> to compare.");
        return CommandOutcome::Continue;
    }

    let old_content = match tokio::fs::read_to_string(&bak_path).await {
        Ok(c) => c,
        Err(e) => {
            println!("Failed to read backup file: {}", e);
            return CommandOutcome::Continue;
        }
    };

    let new_content = match tokio::fs::read_to_string(path).await {
        Ok(c) => c,
        Err(e) => {
            println!("Failed to read file: {}", e);
            return CommandOutcome::Continue;
        }
    };

    let diff = generate_unified_diff(file_path, &old_content, &new_content, 3);

    if diff.hunks.is_empty() {
        println!("No differences between {} and its backup.", file_path);
    } else {
        println!("\n\x1b[1m--- Diff: {} (vs .bak) ---\x1b[0m\n", file_path);
        print!("{}", render_diff_ansi(&diff));
    }

    CommandOutcome::Continue
}

// ── Two-file diff ────────────────────────────────────────────────────────

async fn show_file_diff(file1: &str, file2: &str) -> CommandOutcome {
    let path1 = Path::new(file1);
    let path2 = Path::new(file2);

    if !path1.exists() {
        println!("File not found: {}", file1);
        return CommandOutcome::Continue;
    }
    if !path2.exists() {
        println!("File not found: {}", file2);
        return CommandOutcome::Continue;
    }

    let content1 = match tokio::fs::read_to_string(path1).await {
        Ok(c) => c,
        Err(e) => {
            println!("Failed to read {}: {}", file1, e);
            return CommandOutcome::Continue;
        }
    };

    let content2 = match tokio::fs::read_to_string(path2).await {
        Ok(c) => c,
        Err(e) => {
            println!("Failed to read {}: {}", file2, e);
            return CommandOutcome::Continue;
        }
    };

    let label = format!("{} vs {}", file1, file2);
    let diff = generate_unified_diff(&label, &content1, &content2, 3);

    if diff.hunks.is_empty() {
        println!("Files are identical.");
    } else {
        println!("\n\x1b[1m--- Diff: {} vs {} ---\x1b[0m\n", file1, file2);
        print!("{}", render_diff_ansi(&diff));
    }

    CommandOutcome::Continue
}

// ── HTML output ──────────────────────────────────────────────────────────

async fn show_diff_html(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    if args.is_empty() {
        // Git diff as HTML
        let root = if let Some(ref cl) = ctx.coding_loop {
            cl.lock().await.project_root.clone()
        } else {
            std::env::current_dir().unwrap_or_default()
        };

        if !root.join(".git").exists() {
            println!("Not a git repository.");
            return CommandOutcome::Continue;
        }

        let output = tokio::process::Command::new("git")
            .args(["diff"])
            .current_dir(&root)
            .output()
            .await;

        match output {
            Ok(o) if o.status.success() => {
                let diff_text = String::from_utf8_lossy(&o.stdout);
                if diff_text.trim().is_empty() {
                    println!("No changes.");
                } else {
                    let diff = parse_unified_diff("git diff", &diff_text);
                    println!("{}", render_diff_html(&diff));
                }
            }
            _ => {
                println!("git diff failed.");
            }
        }
    } else if args.len() == 1 {
        // Single file: backup diff as HTML
        let file_path = args[0];
        let bak_path = format!("{}.bak", file_path);

        let Ok(old) = tokio::fs::read_to_string(&bak_path).await else {
            println!("No backup file: {}", bak_path);
            return CommandOutcome::Continue;
        };
        let Ok(new) = tokio::fs::read_to_string(file_path).await else {
            println!("File not found: {}", file_path);
            return CommandOutcome::Continue;
        };

        let diff = generate_unified_diff(file_path, &old, &new, 3);
        println!("{}", render_diff_html(&diff));
    } else {
        println!("Usage: /diff --html [<file>]");
    }

    CommandOutcome::Continue
}

// ── Help ─────────────────────────────────────────────────────────────────

fn print_diff_help() {
    println!("\n=== Diff Command ===\n");
    println!("  /diff                    Show git diff (unstaged changes)");
    println!("  /diff --git              Show git diff (unstaged changes)");
    println!("  /diff --staged           Show staged changes");
    println!("  /diff <file>             Compare file with its .bak backup");
    println!("  /diff <file1> <file2>    Compare two files");
    println!("  /diff --html [<file>]    Output HTML diff for web frontend");
    println!("  /diff --help             Show this help");
}

// ── Registration ─────────────────────────────────────────────────────────

cmd!(
    DiffCommand,
    "diff",
    CommandCategory::Coding,
    "Show visual diff (file edits, git changes, file comparison)",
    cmd_diff
);

pub fn register_all(registry: &mut crate::cli::command::CommandRegistry) {
    registry.register(std::sync::Arc::new(DiffCommand));
}
