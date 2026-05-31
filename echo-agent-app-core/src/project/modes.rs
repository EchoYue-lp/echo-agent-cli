//! CLI-specific AgentMode presentation (Chinese names, icons, bilingual parsing).

pub use echo_agent::prelude::{AgentMode, LocalizedModeEngine, ModeEngine};

/// CLI-localized mode engine with Chinese support.
pub fn chinese_mode_engine() -> LocalizedModeEngine {
    LocalizedModeEngine::with_chinese()
}

/// Chinese display name for a mode.
pub fn display_name(mode: &AgentMode) -> &str {
    match mode {
        AgentMode::General => "通用",
        AgentMode::Coding => "编程",
        AgentMode::Research => "研究",
        AgentMode::Data => "数据",
        AgentMode::Writing => "写作",
        _ => mode.name(),  // fallback to English name
    }
}

/// Icon (emoji) for a mode.
pub fn icon(mode: &AgentMode) -> &'static str {
    match mode {
        AgentMode::General => "💬",
        AgentMode::Coding  => "💻",
        AgentMode::Research => "🔬",
        AgentMode::Data    => "📊",
        AgentMode::Writing => "✍️",
        _                  => "🤖",
    }
}

/// Bilingual mode parse: supports Chinese names, English names, and the
/// internal enum-name.
pub fn from_str(s: &str) -> Option<AgentMode> {
    // Use LocalizedModeEngine for bilingual support.
    LocalizedModeEngine::from_str(s).or_else(|| {
        // Fallback: try the English name / enum variant name
        AgentMode::from_name(s)
    })
}

/// Format a mode for CLI display, e.g. "💻 通用"
pub fn format_display(mode: &AgentMode) -> String {
    format!("{} {}", icon(mode), display_name(mode))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display_name() {
        assert_eq!(display_name(&AgentMode::Coding), "编程");
        assert_eq!(display_name(&AgentMode::Research), "研究");
    }

    #[test]
    fn test_icon() {
        assert_eq!(icon(&AgentMode::Coding), "💻");
    }

    #[test]
    fn test_format_display() {
        assert_eq!(&format_display(&AgentMode::Coding), "💻 编程");
    }

    #[test]
    fn test_from_str() {
        // Chinese names
        assert_eq!(from_str("编程"), Some(AgentMode::Coding));
        // English names
        assert_eq!(from_str("coding"), Some(AgentMode::Coding));
        // Bilingual support
        assert_eq!(from_str("研究"), Some(AgentMode::Research));
    }
}