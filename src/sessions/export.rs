//! 会话导出器
//!
//! 支持导出为 JSON、Markdown、HTML 格式。

use std::fs;
use std::path::Path;

use super::types::Session;

/// 会话导出器
pub struct SessionExporter;

impl SessionExporter {
    /// 导出为 JSON 文件
    pub fn to_json(session: &Session, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(session)?;
        fs::write(path, json)?;
        Ok(())
    }

    /// 导出为 JSON 字符串
    pub fn to_json_string(session: &Session) -> anyhow::Result<String> {
        Ok(serde_json::to_string_pretty(session)?)
    }

    /// 导出为 Markdown 文件
    pub fn to_markdown(session: &Session, path: &Path) -> anyhow::Result<()> {
        let md = Self::build_markdown(session);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, md)?;
        Ok(())
    }

    /// 导出为 Markdown 字符串
    pub fn to_markdown_string(session: &Session) -> String {
        Self::build_markdown(session)
    }

    /// 导出为 HTML 文件
    pub fn to_html(session: &Session, path: &Path) -> anyhow::Result<()> {
        let html = Self::build_html(session);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, html)?;
        Ok(())
    }

    /// 导出为 HTML 字符串
    pub fn to_html_string(session: &Session) -> String {
        Self::build_html(session)
    }

    // ── 内部构建 ────────────────────────────────────────

    fn build_markdown(session: &Session) -> String {
        let mut md = String::new();
        md.push_str(&format!("# 会话: {}\n\n", session.name));
        md.push_str(&format!("- **ID**: {}\n", session.id));
        md.push_str(&format!("- **模型**: {}\n", session.model));
        if let Some(ref branch) = session.branch {
            md.push_str(&format!("- **分支**: {}\n", branch));
        }
        if let Some(ref parent) = session.parent_id {
            md.push_str(&format!("- **父会话**: {}\n", parent));
        }
        md.push_str(&format!("- **消息数**: {}\n", session.message_count));
        md.push_str(&format!("- **创建时间**: {}\n", session.created_at));
        md.push_str(&format!("- **更新时间**: {}\n\n", session.updated_at));
        md.push_str("---\n\n");

        for msg in &session.messages {
            let role_icon = match msg.role.as_str() {
                "user" => "👤 **You**",
                "assistant" => "🤖 **Assistant**",
                "system" => "⚙️ **System**",
                "tool" => "🔧 **Tool**",
                _ => "💬",
            };
            md.push_str(&format!("### {}\n\n", role_icon));
            if let Some(ref content) = msg.content {
                md.push_str(content);
                md.push_str("\n\n");
            }
            if let Some(ref calls) = msg.tool_calls {
                md.push_str("**工具调用:**\n");
                for tc in calls {
                    md.push_str(&format!("- `{}`({})\n", tc.name, tc.arguments));
                }
                md.push('\n');
            }
            md.push_str("---\n\n");
        }

        md
    }

    fn build_html(session: &Session) -> String {
        let mut html = String::new();
        html.push_str("<!DOCTYPE html>\n<html lang=\"zh-CN\">\n<head>\n");
        html.push_str("<meta charset=\"UTF-8\">\n");
        html.push_str(&format!("<title>会话: {}</title>\n", escape_html(&session.name)));
        html.push_str(
            "<style>
body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif; max-width: 800px; margin: 0 auto; padding: 2rem; background: #f8f9fa; color: #212529; }
.header { background: white; border-radius: 12px; padding: 1.5rem; margin-bottom: 1.5rem; box-shadow: 0 1px 3px rgba(0,0,0,0.1); }
.header h1 { margin: 0 0 0.5rem 0; font-size: 1.5rem; }
.meta { color: #6c757d; font-size: 0.875rem; }
.msg { background: white; border-radius: 12px; padding: 1.5rem; margin-bottom: 1rem; box-shadow: 0 1px 3px rgba(0,0,0,0.1); }
.msg.user { border-left: 4px solid #4263eb; }
.msg.assistant { border-left: 4px solid #20c997; }
.msg.system { border-left: 4px solid #868e96; }
.msg .role { font-weight: 600; margin-bottom: 0.5rem; }
.msg .content { line-height: 1.6; white-space: pre-wrap; }
.tool-calls { margin-top: 0.75rem; padding: 0.75rem; background: #f1f3f5; border-radius: 8px; font-size: 0.875rem; }
</style>\n",
        );
        html.push_str("</head>\n<body>\n");

        html.push_str("<div class=\"header\">\n");
        html.push_str(&format!("<h1>会话: {}</h1>\n", escape_html(&session.name)));
        html.push_str("<div class=\"meta\">\n");
        html.push_str(&format!("<div>模型: {}</div>\n", escape_html(&session.model)));
        html.push_str(&format!("<div>消息数: {}</div>\n", session.message_count));
        html.push_str(&format!("<div>创建: {}</div>\n", session.created_at));
        html.push_str("</div>\n</div>\n");

        for msg in &session.messages {
            html.push_str(&format!("<div class=\"msg {}\">\n", msg.role));
            html.push_str(&format!("<div class=\"role\">{}</div>\n", escape_html(&msg.role)));
            if let Some(ref content) = msg.content {
                html.push_str(&format!(
                    "<div class=\"content\">{}</div>\n",
                    escape_html(content)
                ));
            }
            if let Some(ref calls) = msg.tool_calls {
                html.push_str("<div class=\"tool-calls\">\n");
                for tc in calls {
                    html.push_str(&format!(
                        "<div>🔧 <code>{}</code>({})</div>\n",
                        escape_html(&tc.name),
                        escape_html(&tc.arguments)
                    ));
                }
                html.push_str("</div>\n");
            }
            html.push_str("</div>\n");
        }

        html.push_str("</body>\n</html>\n");
        html
    }
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::types::SessionMessage;

    #[test]
    fn test_export_markdown_string() {
        let mut session = Session::new("test", "qwen-plus");
        session.messages.push(SessionMessage {
            role: "user".into(),
            content: Some("Hello".into()),
            tool_calls: None,
        });

        let md = SessionExporter::to_markdown_string(&session);
        assert!(md.contains("# 会话: test"));
        assert!(md.contains("Hello"));
    }

    #[test]
    fn test_export_html_string() {
        let session = Session::new("test", "qwen-plus");
        let html = SessionExporter::to_html_string(&session);
        assert!(html.contains("<!DOCTYPE html>"));
        assert!(html.contains("test"));
    }

    #[test]
    fn test_escape_html() {
        assert_eq!(escape_html("<script>"), "&lt;script&gt;");
        assert_eq!(escape_html("a & b"), "a &amp; b");
    }
}
