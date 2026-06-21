//! Complex-task input classifier.
//!
//! Produces cheap deterministic signals for the TaskRuntime router.
//!
//! The product route decision lives in [`super::router`]: it asks an LLM first,
//! reconciles that semantic verdict with deterministic safety signals, and can
//! apply historical user feedback. This module is deliberately smaller: it
//! provides a zero-cost fallback and transparent signals for traces/tests.
//!
//! We deliberately do NOT reuse the framework's `IntentRouter`: its `Intent`
//! enum has no "complex task" variant and its thresholding is tuned for
//! skill routing.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::signals::{
    ACTION_VERB_CUES, COMPLEX_TASK_CUES, MULTI_TARGET_CUES, PROFILE_CUES, contains_any,
};
use super::types::DomainProfile;

/// Heuristic verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Complexity {
    /// Simple question / single-step answer — stays on the normal chat path.
    Simple,
    /// Complex, multi-step task — enters the TaskRuntime.
    Complex,
    /// Heuristics are inconclusive; ask the LLM (if available) or fall back
    /// to `Simple` to avoid over-triggering the runtime.
    Maybe,
}

/// Result returned to the caller. `Simple` and `Complex` are terminal;
/// `Maybe` is resolved by the orchestrator using an optional LLM classifier
/// or by defaulting to `Simple`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, rename = "TaskRuntimeClassification")]
pub struct Classification {
    pub complexity: ComplexityLabel,
    /// The domain profile inferred from the message, or `General`.
    pub inferred_profile: DomainProfile,
    /// Human-readable reason for the verdict (shown in trace / debug).
    pub reason: String,
    /// Matched heuristic signals, for transparency in the UI.
    pub signals: Vec<String>,
}

/// serde-friendly label (Complexity is unit-like and not serialized directly).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "ComplexityLabel")]
pub enum ComplexityLabel {
    Simple,
    Complex,
}

impl From<Complexity> for ComplexityLabel {
    fn from(c: Complexity) -> Self {
        match c {
            Complexity::Simple | Complexity::Maybe => ComplexityLabel::Simple,
            Complexity::Complex => ComplexityLabel::Complex,
        }
    }
}

/// Deterministic, dependency-free classifier. Construct once (cheap), call
/// [`HeuristicClassifier::classify`] per message.
pub struct HeuristicClassifier;

impl Default for HeuristicClassifier {
    fn default() -> Self {
        Self::new()
    }
}

impl HeuristicClassifier {
    pub fn new() -> Self {
        Self
    }

    /// Classify a single user message. Pure function — safe to call from
    /// sync context. Does not touch the LLM.
    pub fn classify(&self, message: &str) -> Classification {
        let lower = message.to_lowercase();
        let mut signals: Vec<String> = Vec::new();

        // 1. Keyword triggers.
        for kw in COMPLEX_TASK_CUES {
            if lower.contains(kw) {
                signals.push(format!("keyword:{kw}"));
            }
        }

        // 2. Plurality / multi-target cues: "files", "modules", "issues",
        //    "bugs" + an action verb nearby.
        let lower_ascii = message.to_ascii_lowercase();
        let has_multi = contains_any(&lower, MULTI_TARGET_CUES);
        let has_verb = contains_any(&lower_ascii, ACTION_VERB_CUES);
        if has_multi && has_verb {
            signals.push("multi_target+verb".into());
        }

        // 3. Length + question-shape heuristic: a very long message that is
        //    NOT a question is more likely a task brief.
        let char_count = message.chars().count();
        let looks_like_question =
            message.trim_end().ends_with('?') || message.trim_end().ends_with('？');
        if char_count > 280 && !looks_like_question {
            signals.push(format!("long_brief:{char_count}"));
        }

        // 4. Profile inference.
        let mut inferred_profile = DomainProfile::General;
        for (kw, profile) in PROFILE_CUES {
            if lower.contains(kw) {
                inferred_profile = *profile;
                signals.push(format!("profile:{kw}"));
                break; // first match wins; user can override in GUI
            }
        }

        let complexity = if signals.is_empty() {
            Complexity::Simple
        } else {
            // Any positive signal → complex. The plan calls for erring toward
            // the runtime for complex work; false-positives just produce a
            // plan the user can decline.
            Complexity::Complex
        };

        Classification {
            complexity: complexity.into(),
            inferred_profile,
            reason: if signals.is_empty() {
                "no complex-task signals".into()
            } else {
                format!("matched {}", signals.join(", "))
            },
            signals,
        }
    }
}

/// A second-opinion classifier backed by a single JSON-mode LLM call.
///
/// The current heuristic layer is decisive (it never returns `Maybe`), so
/// this trait is wired but unused in the default path. It exists as the
/// extension point for softer triggering: when we want ambiguous inputs to
/// get a second opinion rather than defaulting to `Simple`, we add an
/// `LlmComplexityClassifier` impl and consult it from the orchestrator.
///
/// This is a trait so the planner can accept either a real LLM-backed impl
/// or a test stub. The concrete LLM impl lives in `planner.rs` next to the
/// plan generator so all LLM I/O is colocated.
#[async_trait::async_trait]
pub trait ComplexityClassifier: Send + Sync {
    async fn classify(&self, message: &str) -> Classification;
}

#[async_trait::async_trait]
impl ComplexityClassifier for HeuristicClassifier {
    async fn classify(&self, message: &str) -> Classification {
        HeuristicClassifier::classify(self, message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h() -> HeuristicClassifier {
        HeuristicClassifier::new()
    }

    #[test]
    fn simple_questions_stay_simple() {
        for msg in [
            "What is a Closure in Rust?",
            "今天天气怎么样",
            "explain async/await",
            "hi",
        ] {
            let c = h().classify(msg);
            assert_eq!(c.complexity, ComplexityLabel::Simple, "msg={msg}");
            assert!(c.signals.is_empty());
        }
    }

    #[test]
    fn explicit_review_keywords_trigger_complex() {
        for msg in [
            "对整个项目做一次全面 review",
            "do a full review of the codebase",
            "深入排查这个模块的 bug",
            "分析项目架构",
            "帮我分析这个代码库",
            "analyze the project architecture",
            "run a codebase analysis",
        ] {
            let c = h().classify(msg);
            assert_eq!(c.complexity, ComplexityLabel::Complex, "msg={msg}");
            assert!(!c.signals.is_empty());
        }
    }

    #[test]
    fn multi_target_plus_verb_triggers_complex() {
        let c = h().classify("Please review multiple files in the src directory");
        assert_eq!(c.complexity, ComplexityLabel::Complex);
        assert!(c.signals.iter().any(|s| s.contains("multi_target")));
    }

    #[test]
    fn long_task_brief_without_question_mark_is_complex() {
        let brief = "I need to redesign the authentication flow. \
            The current system uses session cookies but we want to move to JWT \
            with refresh tokens. There are several modules involved and we need \
            to update the middleware, the login endpoint, and the frontend client.";
        let c = h().classify(brief);
        assert_eq!(c.complexity, ComplexityLabel::Complex);
    }

    #[test]
    fn profile_inference_picks_a_starting_domain() {
        assert_eq!(
            h().classify("search arxiv for recent LLM papers")
                .inferred_profile,
            DomainProfile::AcademicResearch
        );
        assert_eq!(
            h().classify("run cargo check and fix the errors")
                .inferred_profile,
            DomainProfile::AiCoding
        );
        assert_eq!(
            h().classify("find pubmed guidelines on hypertension")
                .inferred_profile,
            DomainProfile::MedicalResearch
        );
        assert_eq!(
            h().classify("EDA on this dataset please").inferred_profile,
            DomainProfile::DataAnalysis
        );
        // No domain cue → General. ("refactor" is intentionally treated as an
        // AiCoding signal, so we use domain-neutral wording here.)
        assert_eq!(
            h().classify("Help me organize my weekend trip")
                .inferred_profile,
            DomainProfile::General
        );
    }

    #[tokio::test]
    async fn heuristic_satisfies_complexity_classifier_trait() {
        let c: Box<dyn ComplexityClassifier> = Box::new(HeuristicClassifier::new());
        let r = c.classify("全面优化整个项目").await;
        assert_eq!(r.complexity, ComplexityLabel::Complex);
    }
}
