//! Eval subcommand handler.
//!
//! ```bash
//! echo-agent-cli eval ./evals/coding/
//! echo-agent-cli eval ./evals/coding/fix_compile_error.json --json
//! ```
//!
//! Only `.json` eval case files are supported.

use crate::agent_handle::AgentHandle;
use echo_agent::eval::{EvalCase, EvalReport, EvalRunner};
use std::path::Path;
use std::sync::Arc;

/// Run eval cases from a directory or single file.
pub async fn run_eval(agent: AgentHandle, path: &str, json_output: bool) -> anyhow::Result<()> {
    let path = Path::new(path);
    let cases = if path.is_dir() {
        load_cases_from_dir(path)?
    } else {
        vec![load_case_from_file(path)?]
    };

    if cases.is_empty() {
        println!("No eval cases found in {}", path.display());
        return Ok(());
    }

    // Extract RunStore from agent so trace metrics are populated (C1).
    let run_store: Option<Arc<dyn echo_agent::trace::RunStore>> =
        agent.read(|a| a.run_store.clone()).await;

    println!("Running {} eval case(s)...\n", cases.len());

    let workspace = std::env::temp_dir().join(format!("echo_eval_{}", uuid::Uuid::new_v4()));
    let mut runner = EvalRunner::new(workspace);
    if let Some(store) = run_store {
        runner = runner.with_run_store(store);
    }
    let runner = Arc::new(runner);

    let mut results = Vec::new();
    for case in &cases {
        print!("  {} ... ", case.name);
        let _ = std::io::Write::flush(&mut std::io::stdout());

        let runner = runner.clone();
        let case = case.clone();
        let result = agent
            .read_async(|a| {
                let runner = runner.clone();
                Box::pin(async move { runner.run(&case, a).await })
            })
            .await;

        if result.success {
            println!("PASS (score: {:.2})", result.score);
        } else {
            println!("FAIL (score: {:.2})", result.score);
            for v in &result.violations {
                println!("    violation: {v}");
            }
        }
        results.push(result);
    }

    let report = EvalReport::new(results);
    println!();
    println!("---");
    println!(
        "Results: {}/{} passed, avg score: {:.2}",
        report.passed, report.total, report.avg_score
    );

    if json_output {
        println!();
        println!("{}", serde_json::to_string_pretty(&report)?);
    }

    // Clean up workspace
    let ws_root = runner.workspace_root.clone();
    drop(runner);
    let _ = std::fs::remove_dir_all(ws_root);

    Ok(())
}

/// Load all YAML eval cases from a directory (recursive into subdirs).
fn load_cases_from_dir(dir: &Path) -> anyhow::Result<Vec<EvalCase>> {
    let mut cases = Vec::new();
    load_cases_recursive(dir, &mut cases)?;
    Ok(cases)
}

fn load_cases_recursive(dir: &Path, cases: &mut Vec<EvalCase>) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            load_cases_recursive(&path, cases)?;
        } else if let Some(ext) = path.extension()
            && ext == "json"
        {
            match load_case_from_file(&path) {
                Ok(case) => cases.push(case),
                Err(e) => eprintln!("Warning: skipping {}: {e}", path.display()),
            }
        }
    }
    Ok(())
}

/// Load a single eval case from a JSON file (only .json is supported).
fn load_case_from_file(path: &Path) -> anyhow::Result<EvalCase> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if ext != "json" {
        return Err(anyhow::anyhow!(
            "Unsupported format '{}' in {}. Only .json eval cases are supported.",
            ext,
            path.display()
        ));
    }
    let content = std::fs::read_to_string(path)?;
    let case: EvalCase = serde_json::from_str(&content)
        .map_err(|e| anyhow::anyhow!("Failed to parse {}: {e}", path.display()))?;
    Ok(case)
}
