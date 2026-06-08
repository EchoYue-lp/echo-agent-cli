//! Shared quality score extraction utilities.
//!
//! Used by research, writing, and research-to-writing pipelines to extract
//! structured quality assessments from review text.

/// Structured quality assessment from a review stage.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct QualityAssessment {
    /// Overall quality score (0-100).
    #[serde(default = "default_quality_score")]
    pub quality_score: u32,
    /// Confidence in the assessment (0.0-1.0).
    #[serde(default = "default_confidence")]
    pub confidence: f64,
    /// Brief summary of the assessment.
    #[serde(default)]
    pub summary: String,
    /// Specific suggestions for improvement.
    #[serde(default)]
    pub suggestions: Vec<String>,
    /// Whether the output needs revision.
    #[serde(default)]
    pub needs_revision: bool,
}

fn default_quality_score() -> u32 {
    60
}
fn default_confidence() -> f64 {
    0.5
}

/// Extract structured quality assessment from review text.
///
/// Primary strategy: parse JSON code block from the review.
/// Fallback: heuristic regex scanning (legacy behavior).
pub fn extract_quality_assessment(review_text: &str) -> QualityAssessment {
    // Strategy 1: Extract fenced JSON block
    if let Some(json_str) = extract_json_block(review_text)
        && let Ok(assessment) = serde_json::from_str::<QualityAssessment>(&json_str)
    {
        return assessment;
    }

    // Strategy 2: Try parsing the entire text as JSON
    if let Ok(assessment) = serde_json::from_str::<QualityAssessment>(review_text.trim()) {
        return assessment;
    }

    // Strategy 3: Fallback to legacy regex extraction
    let score = extract_quality_score_legacy(review_text);
    QualityAssessment {
        quality_score: score,
        confidence: 0.3,
        summary: "Extracted via legacy regex".to_string(),
        suggestions: vec![],
        needs_revision: score < 70,
    }
}

/// Extract the quality score (0-100) from review text.
///
/// Tries structured JSON first, falls back to regex.
pub fn extract_quality_score(review_text: &str) -> u32 {
    extract_quality_assessment(review_text).quality_score
}

/// Returns the prompt suffix for structured quality assessment.
///
/// Append this to the review prompt to get JSON output.
pub fn quality_assessment_prompt() -> &'static str {
    r#"

IMPORTANT: After your review, output a JSON assessment block in this exact format:

```json
{
  "quality_score": <0-100>,
  "confidence": <0.0-1.0>,
  "summary": "<brief summary>",
  "suggestions": ["<suggestion 1>", "<suggestion 2>"],
  "needs_revision": <true/false>
}
```"#
}

/// Extract a JSON code block from markdown text.
pub fn extract_json_block(text: &str) -> Option<String> {
    // Look for ```json ... ``` or ```JSON ... ```
    let markers = ["```json", "```JSON"];
    for marker in &markers {
        if let Some(start_idx) = text.find(marker) {
            let after_marker = &text[start_idx + marker.len()..];
            if let Some(end_idx) = after_marker.find("```") {
                let json_str = after_marker[..end_idx].trim();
                return Some(json_str.to_string());
            }
        }
    }
    // Try bare ``` blocks
    if let Some(start_idx) = text.find("```") {
        let after = &text[start_idx + 3..];
        // Skip optional language tag
        let content_start = after.find('\n').map(|i| i + 1).unwrap_or(0);
        let content = &after[content_start..];
        if let Some(end_idx) = content.find("```") {
            let json_str = content[..end_idx].trim();
            // Only return if it looks like JSON
            if json_str.starts_with('{') {
                return Some(json_str.to_string());
            }
        }
    }
    None
}

/// Legacy regex-based quality score extraction (kept as fallback).
pub fn extract_quality_score_legacy(review_text: &str) -> u32 {
    // Primary: look for "QUALITY_SCORE: <number>" pattern
    for line in review_text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("QUALITY_SCORE:") {
            let rest = rest.trim();
            if let Ok(score) = rest.parse::<u32>() {
                return score.min(100);
            }
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(score) = digits.parse::<u32>() {
                return score.min(100);
            }
        }
    }

    // Fallback heuristic: look for "Score:" prefix
    for line in review_text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Score:")
            && let Ok(score) = rest.trim().parse::<u32>()
        {
            return score.min(100);
        }
    }

    // Fallback heuristic: look for "Quality Score" or "quality score" phrases
    for line in review_text.lines() {
        let lower = line.to_lowercase();
        if lower.contains("quality score")
            && let Some(pos) = lower.find("quality score")
        {
            let rest = &line[pos..];
            let digits: String = rest
                .chars()
                .skip_while(|c| !c.is_ascii_digit())
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if let Ok(score) = digits.parse::<u32>() {
                return score.min(100);
            }
        }
    }

    tracing::warn!("Could not extract quality score from review text; defaulting to 60");
    60
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_json_block() {
        let text = r#"Some review text
```json
{"quality_score": 85, "confidence": 0.9, "summary": "Good", "suggestions": [], "needs_revision": false}
```
More text"#;
        let json = extract_json_block(text).unwrap();
        assert!(json.contains("quality_score"));
    }

    #[test]
    fn test_extract_quality_score_legacy() {
        assert_eq!(extract_quality_score_legacy("QUALITY_SCORE: 85"), 85);
        assert_eq!(extract_quality_score_legacy("QUALITY_SCORE: 120"), 100);
        assert_eq!(extract_quality_score_legacy("Score: 72"), 72);
        assert_eq!(extract_quality_score_legacy("no score here"), 60);
    }

    #[test]
    fn test_extract_structured() {
        let text = r#"```json
{"quality_score": 90, "confidence": 0.95, "summary": "Excellent", "suggestions": ["minor typo"], "needs_revision": false}
```"#;
        let assessment = extract_quality_assessment(text);
        assert_eq!(assessment.quality_score, 90);
        assert!(!assessment.needs_revision);
    }
}
