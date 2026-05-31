//! Prompt 模板引擎
//!
//! 支持从外部文件加载模板，运行时替换变量。

use std::collections::HashMap;
use std::path::Path;

/// 模板引擎
pub struct PromptTemplateEngine;

impl PromptTemplateEngine {
    /// 从文件加载模板
    pub fn load_template(path: &Path) -> anyhow::Result<String> {
        let content = std::fs::read_to_string(path)?;
        Ok(content)
    }

    /// 渲染模板（简单变量替换）
    pub fn render(template: &str, variables: &HashMap<String, String>) -> String {
        let mut result = template.to_string();
        for (key, value) in variables {
            let placeholder = format!("{{{}}}", key);
            result = result.replace(&placeholder, value);
        }
        result
    }

    /// 从文件加载并渲染
    pub fn render_from_file(
        path: &Path,
        variables: &HashMap<String, String>,
    ) -> anyhow::Result<String> {
        let template = Self::load_template(path)?;
        Ok(Self::render(&template, variables))
    }
}

/// 预定义模板路径
pub mod paths {
    use std::path::PathBuf;

    pub fn research_search() -> PathBuf {
        template_path("research", "search.tpl")
    }

    pub fn research_merge() -> PathBuf {
        template_path("research", "merge.tpl")
    }

    pub fn research_fetch() -> PathBuf {
        template_path("research", "fetch.tpl")
    }

    pub fn research_synthesize() -> PathBuf {
        template_path("research", "synthesize.tpl")
    }

    pub fn research_write() -> PathBuf {
        template_path("research", "write.tpl")
    }

    pub fn research_review() -> PathBuf {
        template_path("research", "review.tpl")
    }

    pub fn research_revise() -> PathBuf {
        template_path("research", "revise.tpl")
    }

    fn template_path(pipeline: &str, file: &str) -> PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("tasks")
            .join("pipelines")
            .join("templates")
            .join(pipeline)
            .join(file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_template() {
        let template = "Hello, {name}! You have {count} messages.";
        let mut variables = HashMap::new();
        variables.insert("name".to_string(), "Alice".to_string());
        variables.insert("count".to_string(), "5".to_string());

        let result = PromptTemplateEngine::render(template, &variables);
        assert_eq!(result, "Hello, Alice! You have 5 messages.");
    }
}
