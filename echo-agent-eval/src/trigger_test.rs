//! Skill trigger accuracy testing.
//!
//! Tests keyword-based skill routing against a labeled dataset of trigger
//! queries. Computes precision, recall, and F1 per skill and overall.

use std::collections::HashMap;
use std::path::Path;

/// Result of a single trigger test case.
struct TestCaseResult {
    query: String,
    expected: String,
    actual: String,
    passed: bool,
    is_boundary: bool,
}

/// Run skill trigger accuracy test using a routing function.
///
/// `match_fn` takes a user query and returns the matched skill name (or None).
/// The product layer wraps its `SkillGateway` in this closure.
///
/// Loads test cases from `cases_dir/skill-trigger/001_trigger_batch.yaml`.
pub fn run_trigger_test<F>(match_fn: F, cases_dir: &Path, threshold: f64) -> Result<(), String>
where
    F: Fn(&str) -> Option<String>,
{
    let trigger_file = cases_dir
        .join("skill-trigger")
        .join("001_trigger_batch.yaml");

    if !trigger_file.exists() {
        return Err(format!(
            "Trigger test file not found: {}",
            trigger_file.display()
        ));
    }

    println!("\n🎯 Skill Trigger Accuracy Test");
    println!("   Threshold: F1 ≥ {threshold}");
    println!("   Source: {}", trigger_file.display());

    let content =
        std::fs::read_to_string(&trigger_file).map_err(|e| format!("Failed to read: {e}"))?;

    let yaml: serde_yaml::Value =
        serde_yaml::from_str(&content).map_err(|e| format!("Failed to parse YAML: {e}"))?;

    let triggers = yaml
        .get("triggers")
        .and_then(|v| v.as_sequence())
        .ok_or_else(|| "No 'triggers' field found in YAML".to_string())?;

    println!("\n   Running {} test cases...\n", triggers.len());

    let mut results: Vec<TestCaseResult> = Vec::new();
    let mut positive = 0;
    let mut negative = 0;
    let mut boundary = 0;

    for t in triggers {
        let expected = t
            .get("expected")
            .and_then(|v| v.as_str())
            .unwrap_or("none")
            .to_string();
        let query = t
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();
        let note = t.get("note").and_then(|v| v.as_str());
        let is_boundary = note.is_some();

        // Run the routing function
        let actual = match_fn(&query).unwrap_or_else(|| "none".to_string());

        let passed = actual == expected;

        if is_boundary {
            boundary += 1;
        } else if expected == "none" {
            negative += 1;
        } else {
            positive += 1;
        }

        let icon = if passed { "✅" } else { "❌" };
        let label = if is_boundary {
            "🔀 boundary"
        } else if expected == "none" {
            "🚫 negative"
        } else {
            "   positive"
        };

        if !passed {
            println!("   {icon} \"{query}\" → expected={expected}, actual={actual} [{label}]");
        }

        results.push(TestCaseResult {
            query,
            expected,
            actual,
            passed,
            is_boundary,
        });
    }

    // ── Compute metrics ──
    let (precision, recall, f1, per_skill) = compute_metrics(&results);

    // ── Per-skill breakdown ──
    println!("\n   Per-skill metrics:");
    println!("   {:<25} {:>6} {:>6} {:>6}", "Skill", "Prec", "Rec", "F1");
    println!("   {}", "─".repeat(47));
    for (skill, (p, r, f)) in &per_skill {
        println!("   {:<25} {:>6.2} {:>6.2} {:>6.2}", skill, p, r, f);
    }

    // ── Summary ──
    let passed_count = results.iter().filter(|r| r.passed).count();
    let total = results.len();
    println!("\n   Summary:");
    println!("   ├─ Positive triggers: {positive}");
    println!("   ├─ Negative triggers: {negative}");
    println!("   ├─ Boundary tests:    {boundary}");
    println!("   ├─ Passed:            {passed_count}/{total}");
    println!("   ├─ Precision:         {precision:.3}");
    println!("   ├─ Recall:            {recall:.3}");
    println!("   └─ F1:                {f1:.3}");

    if f1 >= threshold {
        println!("\n   ✅ PASS: F1 {f1:.3} ≥ {threshold}\n");
    } else {
        println!("\n   ❌ FAIL: F1 {f1:.3} < {threshold}\n");
    }

    Ok(())
}

/// Compute precision, recall, F1 overall and per-skill.
///
/// For trigger testing:
/// - **True Positive (TP)**: expected=skill_X, actual=skill_X
/// - **False Positive (FP)**: expected=none, actual=skill_X
/// - **False Negative (FN)**: expected=skill_X, actual=none or wrong skill
fn compute_metrics(results: &[TestCaseResult]) -> (f64, f64, f64, Vec<(String, (f64, f64, f64))>) {
    // Overall metrics (excluding boundary cases)
    let non_boundary: Vec<&TestCaseResult> = results.iter().filter(|r| !r.is_boundary).collect();

    let tp = non_boundary
        .iter()
        .filter(|r| r.expected != "none" && r.passed)
        .count();
    let fp = non_boundary
        .iter()
        .filter(|r| r.expected == "none" && r.actual != "none")
        .count();
    let fn_ = non_boundary
        .iter()
        .filter(|r| r.expected != "none" && !r.passed)
        .count();

    let precision = if tp + fp > 0 {
        tp as f64 / (tp + fp) as f64
    } else {
        0.0
    };
    let recall = if tp + fn_ > 0 {
        tp as f64 / (tp + fn_) as f64
    } else {
        0.0
    };
    let f1 = if precision + recall > 0.0 {
        2.0 * precision * recall / (precision + recall)
    } else {
        0.0
    };

    // Per-skill metrics
    let mut skill_stats: HashMap<String, (usize, usize, usize)> = HashMap::new(); // (tp, fp, fn)
    for r in &non_boundary {
        if r.expected != "none" {
            let entry = skill_stats.entry(r.expected.clone()).or_insert((0, 0, 0));
            if r.passed {
                entry.0 += 1;
            } else {
                entry.2 += 1; // FN for expected skill
            }
        }
        if r.actual != "none" && r.expected != r.actual {
            let entry = skill_stats.entry(r.actual.clone()).or_insert((0, 0, 0));
            entry.1 += 1; // FP for wrongly matched skill
        }
    }

    let mut per_skill: Vec<(String, (f64, f64, f64))> = skill_stats
        .into_iter()
        .map(|(skill, (tp, fp, fn_))| {
            let p = if tp + fp > 0 {
                tp as f64 / (tp + fp) as f64
            } else {
                0.0
            };
            let r = if tp + fn_ > 0 {
                tp as f64 / (tp + fn_) as f64
            } else {
                0.0
            };
            let f = if p + r > 0.0 {
                2.0 * p * r / (p + r)
            } else {
                0.0
            };
            (skill, (p, r, f))
        })
        .collect();
    per_skill.sort_by(|a, b| a.0.cmp(&b.0));

    (precision, recall, f1, per_skill)
}
