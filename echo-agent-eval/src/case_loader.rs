//! Eval case loader — loads YAML eval cases from directories.

use echo_agent::eval::{EvalCase, EvalConstraints, SuccessCriteria, grader::Assertion};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Load all eval cases from a directory.
pub fn load_cases(dir: &Path) -> Result<Vec<EvalCase>, String> {
    if !dir.exists() {
        return Err(format!("Cases directory not found: {}", dir.display()));
    }

    let mut cases = Vec::new();
    for entry in std::fs::read_dir(dir)
        .map_err(|e| format!("Failed to read directory {}: {}", dir.display(), e))?
    {
        let entry = entry.map_err(|e| format!("Dir entry error: {e}"))?;
        let path = entry.path();

        if path.is_dir() {
            // Recurse into subdirectories (domain folders)
            for sub_entry in std::fs::read_dir(&path)
                .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?
            {
                let sub_entry = sub_entry.map_err(|e| format!("Sub-dir entry error: {e}"))?;
                let sub_path = sub_entry.path();
                if sub_path.extension().and_then(|e| e.to_str()) == Some("yaml") {
                    match load_case_file(&sub_path) {
                        Ok(case) => cases.push(case),
                        Err(e) => tracing::warn!("Skipping {}: {e}", sub_path.display()),
                    }
                }
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some("yaml") {
            match load_case_file(&path) {
                Ok(case) => cases.push(case),
                Err(e) => tracing::warn!("Skipping {}: {e}", path.display()),
            }
        }
    }

    cases.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(cases)
}

/// Load eval cases filtered by domain.
pub fn load_cases_by_domain(dir: &Path, domains: &[&str]) -> Result<Vec<EvalCase>, String> {
    let all = load_cases(dir)?;
    if domains.is_empty() {
        return Ok(all);
    }
    Ok(all
        .into_iter()
        .filter(|c| {
            c.domain
                .as_ref()
                .map(|d| domains.contains(&d.as_str()))
                .unwrap_or(false)
        })
        .collect())
}

/// Get the default cases directory (relative to this crate's location).
pub fn default_cases_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("eval")
        .join("cases")
}

// ── Private ────────────────────────────────────────────────────────

#[derive(Debug, serde::Deserialize)]
struct YamlEvalCase {
    id: String,
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    domain: Option<String>,
    task: String,
    #[serde(default)]
    project_fixture: Option<String>,
    success_criteria: serde_yaml::Value,
    #[serde(default)]
    constraints: Option<YamlConstraints>,
}

#[derive(Debug, serde::Deserialize)]
struct YamlConstraints {
    #[serde(default)]
    max_files_changed: Option<usize>,
    #[serde(default)]
    max_tool_calls: Option<usize>,
    #[serde(default)]
    forbidden_paths: Vec<String>,
    #[serde(default)]
    required_read_before_edit: bool,
}

fn load_case_file(path: &Path) -> Result<EvalCase, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;

    let yaml: YamlEvalCase = serde_yaml::from_str(&content)
        .map_err(|e| format!("Failed to parse YAML in {}: {}", path.display(), e))?;

    let success_criteria = parse_criteria(&yaml.success_criteria)?;
    let constraints = yaml
        .constraints
        .map(|c| EvalConstraints {
            max_files_changed: c.max_files_changed,
            max_tool_calls: c.max_tool_calls,
            forbidden_paths: c.forbidden_paths,
            required_read_before_edit: c.required_read_before_edit,
        })
        .unwrap_or_default();

    // Resolve fixture path relative to cases dir
    let project_fixture = yaml.project_fixture.map(|p| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("eval")
            .join(p)
    });

    Ok(EvalCase {
        id: yaml.id,
        name: yaml.name,
        description: yaml.description,
        domain: yaml.domain,
        task: yaml.task,
        project_fixture,
        success_criteria,
        constraints,
    })
}

fn parse_criteria(value: &serde_yaml::Value) -> Result<SuccessCriteria, String> {
    match value {
        serde_yaml::Value::Mapping(map) => {
            if let Some(v) = map.get("test_pass") {
                let cmd = get_str_field(v, "command")
                    .ok_or_else(|| "test_pass requires 'command'".to_string())?;
                Ok(SuccessCriteria::TestPass { command: cmd })
            } else if let Some(v) = map.get("output_contains") {
                let substring = match v {
                    serde_yaml::Value::String(s) => s.clone(),
                    serde_yaml::Value::Mapping(m) => {
                        get_str_field(&serde_yaml::Value::Mapping(m.clone()), "substring")
                            .ok_or_else(|| {
                                "output_contains requires string or {{substring: ...}}".to_string()
                            })?
                    }
                    _ => return Err("output_contains must be a string or mapping".to_string()),
                };
                Ok(SuccessCriteria::OutputContains { substring })
            } else if let Some(v) = map.get("tool_used") {
                let tool_name = match v {
                    serde_yaml::Value::String(s) => s.clone(),
                    serde_yaml::Value::Mapping(m) => {
                        get_str_field(&serde_yaml::Value::Mapping(m.clone()), "tool_name")
                            .ok_or_else(|| {
                                "tool_used requires string or {{tool_name: ...}}".to_string()
                            })?
                    }
                    _ => return Err("tool_used must be a string or mapping".to_string()),
                };
                Ok(SuccessCriteria::ToolUsed { tool_name })
            } else if let Some(v) = map.get("tool_not_used") {
                let tool_name = match v {
                    serde_yaml::Value::String(s) => s.clone(),
                    _ => return Err("tool_not_used must be a string".to_string()),
                };
                Ok(SuccessCriteria::ToolNotUsed { tool_name })
            } else if let Some(v) = map.get("all_of") {
                let items = parse_criteria_list(v)?;
                Ok(SuccessCriteria::AllOf(items))
            } else if let Some(v) = map.get("any_of") {
                let items = parse_criteria_list(v)?;
                Ok(SuccessCriteria::AnyOf(items))
            } else if let Some(v) = map.get("llm_graded") {
                let assertions = parse_assertions(v)?;
                Ok(SuccessCriteria::LlmGraded { assertions })
            } else if let Some(v) = map.get("safety_check") {
                let forbidden = get_string_list(v, "forbidden_patterns").unwrap_or_default();
                let required = get_string_list(v, "required_patterns").unwrap_or_default();
                Ok(SuccessCriteria::SafetyCheck {
                    forbidden_patterns: forbidden,
                    required_patterns: required,
                })
            } else if let Some(v) = map.get("citation_valid") {
                let min_citations = get_usize_field(v, "min_citations").unwrap_or(1);
                let format = get_str_field(v, "format").unwrap_or_else(|| "any".to_string());
                Ok(SuccessCriteria::CitationValid {
                    min_citations,
                    format,
                })
            } else if let Some(v) = map.get("value_match") {
                let expected = parse_f64_map(v)?;
                let tolerance = get_f64_field(v, "tolerance").unwrap_or(0.05);
                Ok(SuccessCriteria::ValueMatch {
                    expected,
                    tolerance,
                })
            } else {
                Err(format!(
                    "Unknown criteria type. Keys: {:?}",
                    map.keys().filter_map(|k| k.as_str()).collect::<Vec<_>>()
                ))
            }
        }
        _ => Err(format!("Criteria must be a mapping, got: {:?}", value)),
    }
}

fn parse_criteria_list(value: &serde_yaml::Value) -> Result<Vec<SuccessCriteria>, String> {
    let seq = value
        .as_sequence()
        .ok_or_else(|| "Expected a list of criteria".to_string())?;
    seq.iter().map(parse_criteria).collect()
}

fn parse_assertions(value: &serde_yaml::Value) -> Result<Vec<Assertion>, String> {
    let assertions_val = match value {
        serde_yaml::Value::Mapping(m) => m
            .get("assertions")
            .ok_or_else(|| "llm_graded requires 'assertions' field".to_string())?,
        _ => return Err("llm_graded must be a mapping with 'assertions'".to_string()),
    };

    let seq = assertions_val
        .as_sequence()
        .ok_or_else(|| "'assertions' must be a list".to_string())?;

    let mut result = Vec::new();
    for item in seq {
        let id = get_str_field(item, "id").unwrap_or_default();
        let check = get_str_field(item, "check")
            .ok_or_else(|| "Assertion requires 'check' field".to_string())?;
        let expected = get_str_field(item, "expected").unwrap_or_default();
        result.push(Assertion {
            id,
            check,
            expected,
        });
    }
    Ok(result)
}

fn get_str_field(value: &serde_yaml::Value, key: &str) -> Option<String> {
    value
        .as_mapping()
        .and_then(|m| m.get(key))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn get_usize_field(value: &serde_yaml::Value, key: &str) -> Option<usize> {
    value
        .as_mapping()
        .and_then(|m| m.get(key))
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
}

fn get_f64_field(value: &serde_yaml::Value, key: &str) -> Option<f64> {
    value
        .as_mapping()
        .and_then(|m| m.get(key))
        .and_then(|v| v.as_f64())
}

fn get_string_list(value: &serde_yaml::Value, key: &str) -> Option<Vec<String>> {
    value.as_mapping().and_then(|m| {
        m.get(key).and_then(|v| {
            v.as_sequence().map(|seq| {
                seq.iter()
                    .filter_map(|item| item.as_str().map(|s| s.to_string()))
                    .collect()
            })
        })
    })
}

fn parse_f64_map(value: &serde_yaml::Value) -> Result<HashMap<String, f64>, String> {
    let m = value
        .as_mapping()
        .ok_or_else(|| "value_match expected must be a mapping".to_string())?;
    let expected_val = m
        .get("expected")
        .ok_or_else(|| "value_match requires 'expected' field".to_string())?;
    let expected_map = expected_val
        .as_mapping()
        .ok_or_else(|| "'expected' must be a mapping".to_string())?;

    let mut result = HashMap::new();
    for (k, v) in expected_map {
        let key = k
            .as_str()
            .ok_or_else(|| "expected keys must be strings".to_string())?;
        let val = v
            .as_f64()
            .ok_or_else(|| format!("expected value for '{key}' must be a number"))?;
        result.insert(key.to_string(), val);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_output_contains_string() {
        let yaml: serde_yaml::Value = serde_yaml::from_str(r#"output_contains: "hello""#).unwrap();
        let criteria = parse_criteria(&yaml).unwrap();
        match criteria {
            SuccessCriteria::OutputContains { substring } => assert_eq!(substring, "hello"),
            _ => panic!("Expected OutputContains"),
        }
    }

    #[test]
    fn test_parse_all_of() {
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            r#"all_of:
  - test_pass:
      command: "cargo test"
  - tool_used: "shell"
  - output_contains: "hello""#,
        )
        .unwrap();
        let criteria = parse_criteria(&yaml).unwrap();
        match criteria {
            SuccessCriteria::AllOf(items) => assert_eq!(items.len(), 3),
            _ => panic!("Expected AllOf"),
        }
    }

    #[test]
    fn test_parse_safety_check() {
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            r#"safety_check:
  forbidden_patterns:
    - "建议你服用"
  required_patterns:
    - "咨询医生""#,
        )
        .unwrap();
        let criteria = parse_criteria(&yaml).unwrap();
        match criteria {
            SuccessCriteria::SafetyCheck {
                forbidden_patterns,
                required_patterns,
            } => {
                assert_eq!(forbidden_patterns.len(), 1);
                assert_eq!(required_patterns.len(), 1);
            }
            _ => panic!("Expected SafetyCheck"),
        }
    }

    #[test]
    fn test_parse_citation_valid() {
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            r#"citation_valid:
  min_citations: 5
  format: "pmid""#,
        )
        .unwrap();
        let criteria = parse_criteria(&yaml).unwrap();
        match criteria {
            SuccessCriteria::CitationValid {
                min_citations,
                format,
            } => {
                assert_eq!(min_citations, 5);
                assert_eq!(format, "pmid");
            }
            _ => panic!("Expected CitationValid"),
        }
    }

    #[test]
    fn test_parse_value_match() {
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            r#"value_match:
  expected:
    mean_age: 33.5
    correlation: 0.72
  tolerance: 0.01"#,
        )
        .unwrap();
        let criteria = parse_criteria(&yaml).unwrap();
        match criteria {
            SuccessCriteria::ValueMatch {
                expected,
                tolerance,
            } => {
                assert_eq!(expected.len(), 2);
                assert!((expected["mean_age"] - 33.5).abs() < 0.001);
                assert!((tolerance - 0.01).abs() < 0.001);
            }
            _ => panic!("Expected ValueMatch"),
        }
    }

    #[test]
    fn test_parse_any_of() {
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            r#"any_of:
  - tool_used: "read_data"
  - tool_used: "shell"
  - tool_used: "read_file""#,
        )
        .unwrap();
        let criteria = parse_criteria(&yaml).unwrap();
        match criteria {
            SuccessCriteria::AnyOf(items) => assert_eq!(items.len(), 3),
            _ => panic!("Expected AnyOf"),
        }
    }
}
