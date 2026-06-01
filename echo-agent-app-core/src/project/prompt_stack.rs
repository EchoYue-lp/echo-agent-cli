//! Prompt stacking architecture
//!
//! The system prompt is composed of stacked layers rather than full replacement:
//! - Base: core Agent capabilities (tool usage, safety rules, etc.)
//! - Domain Overlay: mode-specific domain knowledge and workflows
//! - Context Blocks: dynamic context (project info, memory, etc.)

use std::collections::BTreeMap;

/// A context block — a dynamically insertable/replaceable prompt section.
#[derive(Debug, Clone)]
pub struct ContextBlock {
    /// Tag used for identification and replacement.
    pub tag: String,
    /// Content.
    pub content: String,
    /// Priority (higher values appear first).
    pub priority: u32,
}

/// Prompt stacking stack.
///
/// The final system prompt is produced via the `render()` method.
pub struct PromptStack {
    /// Base prompt (Agent core capabilities, always preserved).
    base: String,
    /// Domain overlay (mode-specific prompt).
    domain_overlay: String,
    /// Dynamic context blocks (sorted by priority).
    context_blocks: BTreeMap<String, ContextBlock>,
}

impl PromptStack {
    pub fn new(base: String) -> Self {
        Self {
            base,
            domain_overlay: String::new(),
            context_blocks: BTreeMap::new(),
        }
    }

    /// Create from an existing full prompt (treats entire content as base).
    pub fn from_existing(prompt: &str) -> Self {
        Self::new(prompt.to_string())
    }

    /// Set the domain overlay (called during mode switches).
    pub fn set_domain_overlay(&mut self, overlay: String) {
        self.domain_overlay = overlay;
    }

    /// Get the current domain overlay.
    pub fn domain_overlay(&self) -> &str {
        &self.domain_overlay
    }

    /// Add or replace a context block.
    pub fn set_block(&mut self, block: ContextBlock) {
        self.context_blocks.insert(block.tag.clone(), block);
    }

    /// Remove a context block.
    pub fn remove_block(&mut self, tag: &str) {
        self.context_blocks.remove(tag);
    }

    /// Render the final system prompt.
    ///
    /// Structure:
    /// ```text
    /// {base}
    ///
    /// ---
    /// {domain_overlay}
    ///
    /// ---
    /// {context_block_1}  (priority high)
    /// {context_block_2}
    /// ...                 (priority low)
    /// ```
    pub fn render(&self) -> String {
        let mut parts = Vec::new();

        // 1. Base prompt (always first)
        if !self.base.is_empty() {
            parts.push(self.base.clone());
        }

        // 2. Domain overlay
        if !self.domain_overlay.is_empty() {
            parts.push(self.domain_overlay.clone());
        }

        // 3. Context blocks sorted by priority (descending)
        let mut blocks: Vec<&ContextBlock> = self.context_blocks.values().collect();
        blocks.sort_by(|a, b| b.priority.cmp(&a.priority));
        for block in blocks {
            if !block.content.is_empty() {
                parts.push(block.content.clone());
            }
        }

        parts.join("\n\n---\n\n")
    }
}

impl Default for PromptStack {
    fn default() -> Self {
        Self::new(String::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_basic() {
        let stack = PromptStack::new("You are a helpful assistant.".into());
        assert_eq!(stack.render(), "You are a helpful assistant.");
    }

    #[test]
    fn test_render_with_overlay() {
        let mut stack = PromptStack::new("Base prompt".into());
        stack.set_domain_overlay("Coding mode rules".into());
        let rendered = stack.render();
        assert!(rendered.contains("Base prompt"));
        assert!(rendered.contains("Coding mode rules"));
    }

    #[test]
    fn test_render_with_context_blocks() {
        let mut stack = PromptStack::new("Base".into());
        stack.set_block(ContextBlock {
            tag: "project".into(),
            content: "Project: echo-agent".into(),
            priority: 10,
        });
        stack.set_block(ContextBlock {
            tag: "memory".into(),
            content: "Remember: user prefers Rust".into(),
            priority: 5,
        });
        let rendered = stack.render();
        // Project block should come before memory (higher priority)
        let project_pos = rendered.find("Project:").unwrap();
        let memory_pos = rendered.find("Remember:").unwrap();
        assert!(project_pos < memory_pos);
    }

    #[test]
    fn test_replace_block() {
        let mut stack = PromptStack::new("Base".into());
        stack.set_block(ContextBlock {
            tag: "mode".into(),
            content: "General mode".into(),
            priority: 10,
        });
        stack.set_block(ContextBlock {
            tag: "mode".into(),
            content: "Coding mode".into(),
            priority: 10,
        });
        let rendered = stack.render();
        assert!(rendered.contains("Coding mode"));
        assert!(!rendered.contains("General mode"));
    }
}
