//! Git 子命令组
//!
//! `/git` 统一管理所有 Git 相关操作，替代旧的 `/diff`、`/commit`、`/git-log` 等平铺命令。

use crate::cli::command::{
    CommandCategory, CommandContext, CommandOutcome, SlashCommand, SubCommandDef,
};
use std::future::Future;
use std::pin::Pin;

pub struct GitCommand;

impl SlashCommand for GitCommand {
    fn name(&self) -> &'static str {
        "git"
    }
    fn aliases(&self) -> &'static [&'static str] {
        &["g"]
    }
    fn description(&self) -> &'static str {
        "Git operations (status/log/diff/commit/blame/undo/pr/patch)"
    }
    fn category(&self) -> CommandCategory {
        CommandCategory::Coding
    }

    fn subcommands(&self) -> Vec<SubCommandDef> {
        vec![
            SubCommandDef {
                name: "status",
                aliases: &["s", "st"],
                description: "Show git status",
            },
            SubCommandDef {
                name: "log",
                aliases: &["l"],
                description: "Show git log",
            },
            SubCommandDef {
                name: "diff",
                aliases: &["d"],
                description: "Show staged/unstaged diff",
            },
            SubCommandDef {
                name: "commit",
                aliases: &["c"],
                description: "Stage and commit changes",
            },
            SubCommandDef {
                name: "blame",
                aliases: &["b"],
                description: "Git blame for a file",
            },
            SubCommandDef {
                name: "undo",
                aliases: &[],
                description: "Revert last commit",
            },
            SubCommandDef {
                name: "pr",
                aliases: &[],
                description: "Generate PR summary",
            },
            SubCommandDef {
                name: "patch",
                aliases: &[],
                description: "Generate patch",
            },
        ]
    }

    fn run<'a>(
        &'a self,
        ctx: &'a CommandContext,
        args: &'a [&'a str],
    ) -> Pin<Box<dyn Future<Output = CommandOutcome> + Send + 'a>> {
        Box::pin(async move {
            let subcommand = args.first().copied().unwrap_or("status");
            let sub_args = if args.is_empty() { args } else { &args[1..] };

            match subcommand {
                "status" | "s" | "st" => git_status(ctx, sub_args).await,
                "log" | "l" => git_log(ctx, sub_args).await,
                "diff" | "d" => git_diff(ctx, sub_args).await,
                "commit" | "c" => git_commit(ctx, sub_args).await,
                "blame" | "b" => git_blame(ctx, sub_args).await,
                "undo" => git_undo(ctx, sub_args).await,
                "pr" => git_pr(ctx, sub_args).await,
                "patch" => git_patch(ctx, sub_args).await,
                "help" | "--help" | "-h" => {
                    print_git_help();
                    CommandOutcome::Continue
                }
                _ => {
                    println!("Unknown git subcommand: {subcommand}");
                    print_git_help();
                    CommandOutcome::Continue
                }
            }
        })
    }
}

// ── Help ──────────────────────────────────────────────────────────────

fn print_git_help() {
    println!("\n=== Git Subcommands ===\n");
    println!("  /git status [s,st]     Show git status");
    println!("  /git log [l]           Show recent git log");
    println!("  /git diff [d]          Show staged/unstaged diff");
    println!("  /git commit [c]        Stage and commit changes");
    println!("  /git blame [b] <file>  Git blame for a file");
    println!("  /git undo              Revert the last commit");
    println!("  /git pr [base]         Generate PR summary");
    println!("  /git patch [path]      Generate patch file");
    println!("  /git help              Show this help");
}

// ── status ────────────────────────────────────────────────────────────

async fn git_status(ctx: &CommandContext, _args: &[&str]) -> CommandOutcome {
    if let Some(ref cl) = ctx.coding_loop {
        let root = { cl.lock().await.project_root.clone() };
        if !root.join(".git").exists() {
            println!("Not a git repo.");
            return CommandOutcome::Continue;
        }

        let status = tokio::process::Command::new("git")
            .args(["status", "--short"])
            .current_dir(&root)
            .output()
            .await;
        if let Ok(o) = status {
            let out = String::from_utf8_lossy(&o.stdout);
            println!("\n--- Git Status ---");
            if out.trim().is_empty() {
                println!("  Working tree clean.");
            } else {
                println!("{out}");
            }
        }
    } else {
        println!("Coding mode not active.");
    }
    CommandOutcome::Continue
}

// ── log ───────────────────────────────────────────────────────────────

async fn git_log(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    if let Some(ref cl) = ctx.coding_loop {
        let root = { cl.lock().await.project_root.clone() };
        if !root.join(".git").exists() {
            println!("Not a git repo.");
            return CommandOutcome::Continue;
        }

        let count_str = args.first().copied().unwrap_or("20");
        // Validate count is a positive integer to prevent argument injection
        let count: u32 = match count_str.parse::<u32>() {
            Ok(n) if n > 0 && n <= 10000 => n,
            Ok(0) => {
                println!("Git log count must be a positive integer.");
                return CommandOutcome::Continue;
            }
            Ok(n) => {
                // Cap at 10000 to prevent excessive output
                println!("Git log count capped at 10000 (requested {}).", n);
                10000
            }
            Err(_) => {
                println!("Invalid git log count: '{count_str}'. Must be a positive integer.");
                return CommandOutcome::Continue;
            }
        };
        let log = tokio::process::Command::new("git")
            .args(["log", "--oneline", &format!("-{}", count)])
            .current_dir(&root)
            .output()
            .await;
        if let Ok(o) = log {
            let out = String::from_utf8_lossy(&o.stdout);
            println!("\n--- Git Log (last {count}) ---");
            if out.trim().is_empty() {
                println!("  No commits.");
            } else {
                println!("{out}");
            }
        }
    } else {
        println!("Coding mode not active.");
    }
    CommandOutcome::Continue
}

// ── diff ──────────────────────────────────────────────────────────────

async fn git_diff(ctx: &CommandContext, _args: &[&str]) -> CommandOutcome {
    if let Some(ref cl) = ctx.coding_loop {
        let root = { cl.lock().await.project_root.clone() };
        println!("\n--- Changes ---");
        if root.join(".git").exists() {
            for (label, args) in &[
                ("Staged", &["diff", "--cached", "--stat"] as &[&str]),
                ("Unstaged", &["diff", "--stat"]),
            ] {
                if let Ok(o) = tokio::process::Command::new("git")
                    .args(*args)
                    .current_dir(&root)
                    .output()
                    .await
                {
                    let s = String::from_utf8_lossy(&o.stdout);
                    if !s.trim().is_empty() {
                        println!("\n--- {} ---\n{s}", label);
                    }
                }
            }
        }
    } else {
        println!("Coding mode not active.");
    }
    CommandOutcome::Continue
}

// ── commit ────────────────────────────────────────────────────────────

async fn git_commit(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    if let Some(ref cl) = ctx.coding_loop {
        let root = { cl.lock().await.project_root.clone() };
        if !root.join(".git").exists() {
            println!("Not a git repo.");
            return CommandOutcome::Continue;
        }

        // Check for changes
        let status = tokio::process::Command::new("git")
            .args(["status", "--short"])
            .current_dir(&root)
            .output()
            .await;
        if let Ok(s) = status {
            let out = String::from_utf8_lossy(&s.stdout);
            if out.trim().is_empty() {
                println!("Nothing to commit.");
                return CommandOutcome::Continue;
            }

            // Get commit message
            let msg = if !args.is_empty() {
                args.join(" ")
            } else {
                // Auto-generate message from changed files
                let files: Vec<&str> = out.lines().filter_map(|l| l.get(3..)).collect();
                format!("Update: {}", files.join(", "))
            };

            // Stage all changes
            let add = tokio::process::Command::new("git")
                .args(["add", "-A"])
                .current_dir(&root)
                .output()
                .await;
            if let Err(e) = add {
                println!("git add failed: {e}");
                return CommandOutcome::Continue;
            }

            // Commit
            let commit = tokio::process::Command::new("git")
                .args(["commit", "-m", &msg])
                .current_dir(&root)
                .output()
                .await;
            match commit {
                Ok(o) if o.status.success() => {
                    let hash_out = tokio::process::Command::new("git")
                        .args(["rev-parse", "--short", "HEAD"])
                        .current_dir(&root)
                        .output()
                        .await;
                    let hash = hash_out
                        .map(|h| String::from_utf8_lossy(&h.stdout).trim().to_string())
                        .unwrap_or_default();
                    println!("Committed {} — {}", hash, msg);
                }
                Ok(o) => {
                    println!("Commit failed: {}", String::from_utf8_lossy(&o.stderr));
                }
                Err(e) => println!("git commit failed: {e}"),
            }
        }
    } else {
        println!("Coding mode not active.");
    }
    CommandOutcome::Continue
}

// ── blame ─────────────────────────────────────────────────────────────

async fn git_blame(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    if let Some(ref cl) = ctx.coding_loop {
        let root = { cl.lock().await.project_root.clone() };
        if !root.join(".git").exists() {
            println!("Not a git repo.");
            return CommandOutcome::Continue;
        }

        let file = match args.first() {
            Some(f) => *f,
            None => {
                println!("Usage: /git blame <file>");
                return CommandOutcome::Continue;
            }
        };

        let blame = tokio::process::Command::new("git")
            .args(["blame", "--line-porcelain", file])
            .current_dir(&root)
            .output()
            .await;
        match blame {
            Ok(o) if o.status.success() => {
                let out = String::from_utf8_lossy(&o.stdout);
                // Parse porcelain format: extract author, date, line
                let mut current_author = String::new();
                let mut current_date = String::new();
                let mut results = Vec::new();
                for line in out.lines() {
                    if let Some(author) = line.strip_prefix("author ") {
                        current_author = author.to_string();
                    } else if let Some(date) = line.strip_prefix("author-time ") {
                        if let Ok(ts) = date.parse::<i64>() {
                            current_date = chrono::NaiveDateTime::from_timestamp_opt(ts, 0)
                                .map(|d| d.format("%Y-%m-%d").to_string())
                                .unwrap_or_else(|| date.to_string());
                        }
                    } else if line.starts_with('\t') {
                        let content = &line[1..];
                        results.push(format!(
                            "{:<10} {:<12} {}",
                            current_date, current_author, content
                        ));
                    }
                }
                println!("\n--- Git Blame: {file} ---");
                let display_count = 50.min(results.len());
                for r in results.iter().take(display_count) {
                    println!("{r}");
                }
                if results.len() > display_count {
                    println!("... ({} more lines)", results.len() - display_count);
                }
            }
            Ok(o) => println!("git blame failed: {}", String::from_utf8_lossy(&o.stderr)),
            Err(e) => println!("git blame failed: {e}"),
        }
    } else {
        println!("Coding mode not active.");
    }
    CommandOutcome::Continue
}

// ── undo ──────────────────────────────────────────────────────────────

async fn git_undo(ctx: &CommandContext, _args: &[&str]) -> CommandOutcome {
    if let Some(ref cl) = ctx.coding_loop {
        let root = { cl.lock().await.project_root.clone() };
        if !root.join(".git").exists() {
            println!("Not a git repo.");
            return CommandOutcome::Continue;
        }

        // Get last commit info
        let log = tokio::process::Command::new("git")
            .args(["log", "--oneline", "-1"])
            .current_dir(&root)
            .output()
            .await;
        if let Ok(o) = log {
            let last = String::from_utf8_lossy(&o.stdout).trim().to_string();
            println!("Reverting: {last}");
        }

        let revert = tokio::process::Command::new("git")
            .args(["revert", "HEAD", "--no-edit"])
            .current_dir(&root)
            .output()
            .await;
        match revert {
            Ok(o) if o.status.success() => println!("Reverted last commit."),
            Ok(o) => println!("Revert failed: {}", String::from_utf8_lossy(&o.stderr)),
            Err(e) => println!("git revert failed: {e}"),
        }
    } else {
        println!("Coding mode not active.");
    }
    CommandOutcome::Continue
}

// ── pr ────────────────────────────────────────────────────────────────

async fn git_pr(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    if let Some(ref cl) = ctx.coding_loop {
        let root = { cl.lock().await.project_root.clone() };
        if !root.join(".git").exists() {
            println!("Not a git repo.");
            return CommandOutcome::Continue;
        }

        let branch = args.first().copied().unwrap_or("main");

        // Get commits since branch
        let log = tokio::process::Command::new("git")
            .args(["log", "--oneline", &format!("{}..HEAD", branch)])
            .current_dir(&root)
            .output()
            .await;

        // Get diff stat
        let diff = tokio::process::Command::new("git")
            .args(["diff", "--stat", branch])
            .current_dir(&root)
            .output()
            .await;

        // Get changed files
        let files = tokio::process::Command::new("git")
            .args(["diff", "--name-only", branch])
            .current_dir(&root)
            .output()
            .await;

        println!("\n=== PR Summary ===\n");

        let mut commit_count = 0;
        if let Ok(o) = log {
            let commits = String::from_utf8_lossy(&o.stdout);
            commit_count = commits.lines().count();
            println!("Commits ({commit_count}):");
            if commits.trim().is_empty() {
                println!("  No commits since {branch}");
            } else {
                println!("{commits}");
            }
        }

        if let Ok(o) = files {
            let file_list = String::from_utf8_lossy(&o.stdout);
            let file_count = file_list.lines().count();
            println!("\nChanged files ({file_count}):");
            for f in file_list.lines().take(30) {
                println!("  {f}");
            }
        }

        if let Ok(o) = diff {
            let stat = String::from_utf8_lossy(&o.stdout);
            if !stat.trim().is_empty() {
                println!("\nDiff stat:\n{stat}");
            }
        }

        // Try to use gh CLI if available
        if commit_count > 0 {
            println!("\n--- Create PR ---");

            let gh_check = tokio::process::Command::new("gh")
                .arg("--version")
                .output()
                .await;

            if gh_check.is_ok() {
                let branch_output = tokio::process::Command::new("git")
                    .args(["rev-parse", "--abbrev-ref", "HEAD"])
                    .current_dir(&root)
                    .output()
                    .await;

                if let Ok(output) = branch_output {
                    let current_branch = String::from_utf8_lossy(&output.stdout).trim().to_string();

                    println!("  To create a PR with gh CLI:");
                    println!(
                        "    gh pr create --base {} --head {} --title \"Your PR title\"",
                        branch, current_branch
                    );
                    println!("\n  Or interactively:");
                    println!("    gh pr create");
                    println!("\n  Tip: Push your branch first if not already pushed:");
                    println!("    git push -u origin {}", current_branch);
                }
            } else {
                println!("  gh CLI not found. Install it from: https://cli.github.com/");
                println!("\n  Manual steps:");
                println!("    1. Push your branch: git push -u origin <your-branch>");
                println!("    2. Go to your repo on GitHub/GitLab");
                println!("    3. Click 'New Pull Request'");
            }
        } else {
            println!("\nTip: Use /git commit to stage and commit, then push to create the PR.");
        }
    } else {
        println!("Coding mode not active.");
    }
    CommandOutcome::Continue
}

// ── patch ─────────────────────────────────────────────────────────────

async fn git_patch(ctx: &CommandContext, args: &[&str]) -> CommandOutcome {
    if let Some(ref cl) = ctx.coding_loop {
        let root = { cl.lock().await.project_root.clone() };
        if !root.join(".git").exists() {
            println!("Not a git repo.");
            return CommandOutcome::Continue;
        }

        let output_path = args.first().copied().unwrap_or("changes.patch");

        let patch = tokio::process::Command::new("git")
            .args(["format-patch", "-1", "HEAD", "--stdout"])
            .current_dir(&root)
            .output()
            .await;

        match patch {
            Ok(o) if !o.stdout.is_empty() => {
                let content = String::from_utf8_lossy(&o.stdout);
                match tokio::fs::write(output_path, content.as_bytes()).await {
                    Ok(_) => println!("Patch written to: {output_path}"),
                    Err(e) => println!("Failed to write patch: {e}"),
                }
            }
            _ => {
                // No commits yet, generate diff as patch
                let diff = tokio::process::Command::new("git")
                    .args(["diff"])
                    .current_dir(&root)
                    .output()
                    .await;
                if let Ok(o) = diff {
                    if o.stdout.is_empty() {
                        println!("No changes to patch.");
                    } else {
                        match tokio::fs::write(output_path, &o.stdout).await {
                            Ok(_) => println!("Diff patch written to: {output_path}"),
                            Err(e) => println!("Failed to write patch: {e}"),
                        }
                    }
                }
            }
        }
    } else {
        println!("Coding mode not active.");
    }
    CommandOutcome::Continue
}

// ── Register ──────────────────────────────────────────────────────────

pub fn register_all(registry: &mut crate::cli::command::CommandRegistry) {
    registry.register(std::sync::Arc::new(GitCommand));
}
