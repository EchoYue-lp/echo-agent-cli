//! LaTeX 导出器
//!
//! 将 Markdown 内容转换为 LaTeX 格式。

/// LaTeX 导出器。
pub struct LatexExporter;

impl LatexExporter {
    /// 将 Markdown 内容转换为 LaTeX。
    pub fn markdown_to_latex(markdown: &str) -> String {
        let mut latex = String::new();

        // Document preamble
        latex.push_str(r#"\documentclass[11pt,a4paper]{article}
\usepackage[utf8]{inputenc}
\usepackage[T1]{fontenc}
\usepackage{amsmath,amssymb}
\usepackage{graphicx}
\usepackage{hyperref}
\usepackage{biblatex}
\usepackage{geometry}
\geometry{margin=1in}

\begin{document}

"#);

        // Convert markdown to LaTeX
        for line in markdown.lines() {
            let converted = Self::convert_line(line);
            latex.push_str(&converted);
            latex.push('\n');
        }

        latex.push_str("\n\\end{document}\n");
        latex
    }

    /// 转换单行 Markdown 到 LaTeX。
    fn convert_line(line: &str) -> String {
        let trimmed = line.trim();

        // Headers
        if let Some(rest) = trimmed.strip_prefix("# ") {
            return format!("\\section{{{}}}", Self::escape_latex(rest));
        }
        if let Some(rest) = trimmed.strip_prefix("## ") {
            return format!("\\subsection{{{}}}", Self::escape_latex(rest));
        }
        if let Some(rest) = trimmed.strip_prefix("### ") {
            return format!("\\subsubsection{{{}}}", Self::escape_latex(rest));
        }

        // Horizontal rule
        if trimmed == "---" || trimmed == "***" {
            return "\\hrulefill".to_string();
        }

        // Step 1: Escape LaTeX special chars FIRST (before adding LaTeX commands)
        let escaped = Self::escape_latex(line);

        // Step 2: Apply inline formatting on the escaped text
        // Bold: **text** -> \textbf{text}
        let result = Self::replace_pattern(&escaped, "**", "\\textbf{", "}");
        // Italic: *text* -> \textit{text}  (but not inside \textbf)
        let result = Self::replace_pattern(&result, "*", "\\textit{", "}");
        // Inline code: `text` -> \texttt{text}
        let result = Self::replace_pattern(&result, "`", "\\texttt{", "}");

        // Step 3: Citations: [[N]] -> \cite{refN}
        // Note: [ and ] are NOT LaTeX special chars, so they survive escaping
        let mut output = String::new();
        let mut chars = result.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '[' {
                if chars.peek() == Some(&'[') {
                    chars.next(); // consume second [
                    let mut num = String::new();
                    while let Some(&nc) = chars.peek() {
                        if nc == ']' { break; }
                        num.push(nc);
                        chars.next();
                    }
                    chars.next(); // ]
                    if chars.peek() == Some(&']') {
                        chars.next(); // second ]
                        output.push_str(&format!("\\cite{{ref{}}}", num));
                        continue;
                    }
                    // Not a citation — put back the brackets
                    output.push('[');
                    output.push('[');
                    output.push_str(&num);
                    output.push(']');
                    continue;
                }
            }
            output.push(c);
        }

        output
    }

    /// 替换成对标记。
    fn replace_pattern(text: &str, marker: &str, open: &str, close: &str) -> String {
        let parts: Vec<&str> = text.split(marker).collect();
        if parts.len() < 3 {
            return text.to_string();
        }
        let mut result = String::new();
        let mut in_marker = false;
        for (i, part) in parts.iter().enumerate() {
            if i > 0 {
                if in_marker {
                    result.push_str(close);
                } else {
                    result.push_str(open);
                }
                in_marker = !in_marker;
            }
            result.push_str(part);
        }
        result
    }

    /// 转义 LaTeX 特殊字符。
    fn escape_latex(text: &str) -> String {
        Self::escape_latex_inline(text)
    }

    fn escape_latex_inline(text: &str) -> String {
        text.replace('&', "\\&")
            .replace('%', "\\%")
            .replace('$', "\\$")
            .replace('#', "\\#")
            .replace('_', "\\_")
            .replace('{', "\\{")
            .replace('}', "\\}")
    }

    /// 从论文引用列表生成 BibTeX。
    pub fn generate_bibtex(references: &[(String, String, String, u32)]) -> String {
        // (id, title, authors, year)
        let mut bib = String::new();
        for (id, title, authors, year) in references {
            bib.push_str(&format!(
                "@article{{ref{},\n  title = {{{}}},\n  author = {{{}}},\n  year = {{{}}}\n}}\n\n",
                id, title, authors, year
            ));
        }
        bib
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_headers() {
        assert_eq!(LatexExporter::convert_line("# Introduction"), "\\section{Introduction}");
        assert_eq!(LatexExporter::convert_line("## Methods"), "\\subsection{Methods}");
    }

    #[test]
    fn test_citations() {
        let result = LatexExporter::convert_line("As shown in [[1]] and [[2]].");
        assert!(result.contains("\\cite{ref1}"));
        assert!(result.contains("\\cite{ref2}"));
    }

    #[test]
    fn test_escape() {
        let result = LatexExporter::convert_line("Use $100 & 50% off");
        assert!(result.contains("\\$"));
        assert!(result.contains("\\&"));
        assert!(result.contains("\\%"));
    }

    #[test]
    fn test_full_document() {
        let md = "# Title\n\nSome text with **bold**.\n\n## Section\n\nMore text.";
        let latex = LatexExporter::markdown_to_latex(md);
        assert!(latex.contains("\\documentclass"));
        assert!(latex.contains("\\section{Title}"));
        assert!(latex.contains("\\textbf{bold}"));
        assert!(latex.contains("\\end{document}"));
    }
}
