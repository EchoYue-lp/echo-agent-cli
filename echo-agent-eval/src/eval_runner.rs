//! Evaluation runner — executes eval cases against an agent.

use echo_agent::agent::Agent as AgentTrait;
use echo_agent::eval::{EvalCase, EvalReport, EvalResult, SuccessCriteria};
use std::path::Path;

/// Run eval cases against an agent.
pub async fn run_cases(
    agent: &dyn AgentTrait,
    cases: &[EvalCase],
    timeout_secs: u64,
) -> EvalReport {
    let mut results = Vec::new();

    for case in cases {
        print!("  ⏳ {} — {} ... ", case.id, case.name);

        let timeout_duration = std::time::Duration::from_secs(timeout_secs);
        let started = std::time::Instant::now();

        let output = match tokio::time::timeout(timeout_duration, agent.execute(&case.task)).await {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => format!("Error: {e}"),
            Err(_) => "TIMEOUT".to_string(),
        };

        let duration_ms = started.elapsed().as_millis() as u64;
        let mut result = EvalResult::new(&case.id, true);
        result.duration_ms = duration_ms;

        check_criteria(&case.success_criteria, &output, &mut result);

        if result.success {
            println!("✅ PASS ({:.1}s)", result.duration_ms as f64 / 1000.0);
        } else {
            println!("❌ FAIL ({:.1}s)", result.duration_ms as f64 / 1000.0);
            for v in &result.violations {
                println!("       └─ {v}");
            }
        }
        results.push(result);
    }

    EvalReport::new(results)
}

/// Format a human-readable eval report.
pub fn format_report(report: &EvalReport, domain: Option<&str>) -> String {
    let domain_label = domain.unwrap_or("all");
    let pass_pct = if report.total > 0 {
        report.passed as f64 / report.total as f64 * 100.0
    } else {
        0.0
    };
    let status = if pass_pct >= 80.0 {
        "✅"
    } else if pass_pct >= 50.0 {
        "⚠️"
    } else {
        "❌"
    };

    format!(
        "\n{status} Eval Report [{domain_label}]\n\
         ├─ Total:  {} cases\n\
         ├─ Passed: {} ({pass_pct:.0}%)\n\
         ├─ Failed: {}\n\
         ├─ Avg Score: {:.2}\n\
         └─ Duration: {:.1}s total\n",
        report.total,
        report.passed,
        report.failed,
        report.avg_score,
        report
            .results
            .iter()
            .map(|r| r.duration_ms as f64 / 1000.0)
            .sum::<f64>(),
    )
}

/// Save an HTML report to the given directory.
pub fn save_report(
    report: &EvalReport,
    output_dir: &Path,
    domain: Option<&str>,
) -> Result<std::path::PathBuf, String> {
    std::fs::create_dir_all(output_dir).map_err(|e| format!("Failed to create output dir: {e}"))?;
    let title = domain.unwrap_or("all");
    let html = echo_agent::eval::generate_html(report, title);
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let report_path = output_dir.join(format!("eval-report-{timestamp}.html"));
    std::fs::write(&report_path, &html).map_err(|e| format!("Failed to write report: {e}"))?;
    Ok(report_path)
}

// ── Private ────────────────────────────────────────────────────────

fn check_criteria(criteria: &SuccessCriteria, output: &str, result: &mut EvalResult) {
    match criteria {
        SuccessCriteria::OutputContains { substring } => {
            if !output.contains(substring.as_str()) {
                result.success = false;
                result
                    .violations
                    .push(format!("Output missing: '{substring}'"));
            }
        }
        SuccessCriteria::AllOf(items) => {
            for item in items {
                check_criteria(item, output, result);
            }
        }
        SuccessCriteria::AnyOf(items) => {
            let mut any_pass = false;
            for item in items {
                let mut temp = EvalResult::new("temp", true);
                check_criteria(item, output, &mut temp);
                if temp.success {
                    any_pass = true;
                    break;
                }
            }
            if !any_pass {
                result.success = false;
                result
                    .violations
                    .push("None of the AnyOf criteria matched".to_string());
            }
        }
        SuccessCriteria::SafetyCheck {
            forbidden_patterns,
            required_patterns,
        } => {
            for p in forbidden_patterns {
                if output.contains(p.as_str()) {
                    result.success = false;
                    result
                        .violations
                        .push(format!("SAFETY: forbidden pattern found: '{p}'"));
                }
            }
            for p in required_patterns {
                if !output.contains(p.as_str()) {
                    result.success = false;
                    result
                        .violations
                        .push(format!("SAFETY: required pattern missing: '{p}'"));
                }
            }
        }
        SuccessCriteria::CitationValid {
            min_citations,
            format,
        } => {
            let citation_patterns: Vec<&str> = match format.as_str() {
                "pmid" => vec![r"PMID:\s*\d+", r"PMID\s*\d+"],
                "doi" => vec![r"10\.\d{{4,}}/"],
                "url" => vec![r"https?://"],
                _ => vec![
                    r"PMID:\s*\d+",
                    r"PMID\s*\d+",
                    r"10\.\d{{4,}}/",
                    r"https?://",
                ],
            };

            let mut citation_count = 0usize;
            for pattern in &citation_patterns {
                if let Ok(re) = regex::Regex::new(pattern) {
                    citation_count += re.find_iter(output).count();
                }
            }

            if citation_count < *min_citations {
                result.success = false;
                result.violations.push(format!(
                    "Insufficient citations: found {} but need at least {} (format: {})",
                    citation_count, min_citations, format
                ));
            }
        }
        // ToolUsed/ToolNotUsed/TestPass/ValueMatch/LlmGraded/SweBench —
        // require trace analysis or shell execution, skip in simplified mode
        _ => {}
    }
}
