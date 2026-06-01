//! 工作区模板
//!
//! 预配置的工作区脚手架，快速初始化特定类型的项目。

use std::fs;

use super::layout::WorkspaceLayout;
use super::{Workspace, WorkspaceKind};

/// 工作区模板类型。
#[derive(Debug, Clone, Copy)]
pub enum WorkspaceTemplate {
    /// 学术论文工作区
    ResearchPaper,
    /// 数据分析工作区
    DataProject,
    /// 代码项目工作区
    CodingProject,
}

impl WorkspaceTemplate {
    /// 从字符串解析模板类型。
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "research" | "research-paper" | "paper" => Some(Self::ResearchPaper),
            "data" | "data-project" | "analysis" => Some(Self::DataProject),
            "code" | "coding" | "coding-project" => Some(Self::CodingProject),
            _ => None,
        }
    }

    /// 模板名称。
    pub fn name(&self) -> &str {
        match self {
            Self::ResearchPaper => "Research Paper",
            Self::DataProject => "Data Project",
            Self::CodingProject => "Coding Project",
        }
    }

    /// 对应的工作区类型。
    pub fn workspace_kind(&self) -> WorkspaceKind {
        match self {
            Self::ResearchPaper => WorkspaceKind::Research { topics: vec![] },
            Self::DataProject => WorkspaceKind::DataAnalysis { datasets: vec![] },
            Self::CodingProject => WorkspaceKind::Code { repo_url: None },
        }
    }

    /// 应用模板到工作区。
    pub fn apply(&self, workspace: &Workspace) -> anyhow::Result<()> {
        let root = &workspace.root;

        // Ensure standard dirs exist first
        WorkspaceLayout::ensure_dirs(root)?;

        match self {
            Self::ResearchPaper => {
                // Create research-specific structure
                fs::create_dir_all(root.join("papers/pdf"))?;
                fs::create_dir_all(root.join("papers/notes"))?;
                fs::create_dir_all(root.join("references"))?;
                fs::create_dir_all(root.join("drafts"))?;

                // Create LaTeX skeleton
                let main_tex = r#"\documentclass[11pt,a4paper]{article}
\usepackage[utf8]{inputenc}
\usepackage{amsmath,amssymb}
\usepackage{graphicx}
\usepackage{hyperref}
\usepackage{biblatex}
\addbibresource{references.bib}

\title{Paper Title}
\author{Author Name}
\date{\today}

\begin{document}
\maketitle
\begin{abstract}
Your abstract here.
\end{abstract}

\section{Introduction}

\section{Related Work}

\section{Methodology}

\section{Experiments}

\section{Conclusion}

\printbibliography
\end{document}
"#;
                fs::write(root.join("drafts/main.tex"), main_tex)?;

                // Create empty BibTeX
                fs::write(
                    root.join("references/references.bib"),
                    "% Add your references here\n",
                )?;

                // Create research notes template
                let notes = "# Research Notes\n\n## Key Questions\n\n- \n\n## Reading Log\n\n| Paper | Key Finding | Relevance |\n|-------|-------------|----------|\n| | | |\n";
                fs::write(root.join("papers/notes/reading-log.md"), notes)?;

                tracing::info!(workspace = %workspace.id, "Applied research paper template");
            }

            Self::DataProject => {
                fs::create_dir_all(root.join("data/raw"))?;
                fs::create_dir_all(root.join("data/processed"))?;
                fs::create_dir_all(root.join("notebooks"))?;
                fs::create_dir_all(root.join("output/charts"))?;
                fs::create_dir_all(root.join("output/reports"))?;

                let analysis_template = "# Data Analysis\n\n## Objective\n\nDescribe your analysis goal.\n\n## Dataset\n\n- Source:\n- Size:\n- Format:\n\n## Steps\n\n1. Load data\n2. Explore & profile\n3. Clean & transform\n4. Analyze\n5. Visualize\n6. Summarize\n";
                fs::write(root.join("notebooks/analysis.md"), analysis_template)?;

                tracing::info!(workspace = %workspace.id, "Applied data project template");
            }

            Self::CodingProject => {
                fs::create_dir_all(root.join("docs"))?;
                fs::create_dir_all(root.join("scripts"))?;

                let project_notes = "# Project Notes\n\n## Architecture\n\nDescribe the project architecture.\n\n## TODO\n\n- [ ] \n\n## Decisions\n\n| Decision | Rationale | Date |\n|----------|-----------|------|\n| | | |\n";
                fs::write(root.join("docs/notes.md"), project_notes)?;

                tracing::info!(workspace = %workspace.id, "Applied coding project template");
            }
        }

        Ok(())
    }
}
