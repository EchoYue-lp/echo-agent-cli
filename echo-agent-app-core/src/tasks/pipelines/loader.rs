//! Pipeline 配置加载器
//!
//! 支持从 YAML/JSON 文件动态加载 Pipeline 定义，
//! 并编译为 Graph workflow。

use std::collections::HashMap;
use std::path::Path;

use echo_agent::workflow::{Graph, GraphBuilder, SharedAgent, SharedState};
use futures::future::BoxFuture;

/// Pipeline 定义
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PipelineDefinition {
    pub name: String,
    pub description: String,
    pub version: String,
    pub config: HashMap<String, ConfigSchema>,
    pub nodes: Vec<NodeDefinition>,
    pub edges: Vec<EdgeDefinition>,
    #[serde(default)]
    pub templates: HashMap<String, String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ConfigSchema {
    #[serde(rename = "type")]
    pub ty: String,
    pub required: Option<bool>,
    pub default: Option<serde_yaml::Value>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct NodeDefinition {
    pub id: String,
    #[serde(rename = "type", alias = "node_type")]
    pub node_type: String,
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub input: Option<HashMap<String, serde_yaml::Value>>,
    #[serde(default)]
    pub prompt_key: Option<String>,
    #[serde(default)]
    pub output_key: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct EdgeDefinition {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub condition: Option<String>,
}

/// Pipeline 加载器
pub struct PipelineLoader;

impl PipelineLoader {
    /// 从 YAML 文件加载 Pipeline 定义
    pub fn from_yaml(path: &Path) -> anyhow::Result<PipelineDefinition> {
        let content = std::fs::read_to_string(path)?;
        let def: PipelineDefinition = serde_yaml::from_str(&content)?;
        Ok(def)
    }

    /// 从 JSON 文件加载 Pipeline 定义
    pub fn from_json(path: &Path) -> anyhow::Result<PipelineDefinition> {
        let content = std::fs::read_to_string(path)?;
        let def: PipelineDefinition = serde_json::from_str(&content)?;
        Ok(def)
    }

    /// 将 Pipeline 定义编译为 Graph
    pub fn build_graph(def: &PipelineDefinition, agent: SharedAgent) -> anyhow::Result<Graph> {
        let mut builder = GraphBuilder::new(&def.name);
        let agent_ref = &agent;

        // 注册所有节点
        for node in &def.nodes {
            match node.node_type.as_str() {
                "function" => {
                    let node_id = node.id.clone();
                    builder = builder.add_function_node(&node.id, move |_state: &SharedState| -> BoxFuture<'_, Result<(), echo_agent::error::ReactError>> {
                        let id = node_id.clone();
                        Box::pin(async move {
                            tracing::debug!("Executing function node: {}", id);
                            // Function nodes are placeholder for now
                            // In production, these would dispatch to actual function handlers
                            Ok(())
                        })
                    });
                }
                "agent" => {
                    let prompt_key = node.prompt_key.clone().unwrap_or_default();
                    let output_key = node.output_key.clone().unwrap_or_default();
                    let agent_clone = agent_ref.clone();
                    builder = builder.add_shared_agent_node_with_mode(
                        &node.id,
                        agent_clone,
                        &prompt_key,
                        &output_key,
                        false,
                    );
                }
                _ => {
                    tracing::warn!("Unknown node type: {}", node.node_type);
                }
            }
        }

        // 注册所有边
        for edge in &def.edges {
            if let Some(_condition) = &edge.condition {
                // 有条件边（简化实现，实际应解析条件表达式）
                builder = builder.add_edge(&edge.from, &edge.to);
            } else {
                builder = builder.add_edge(&edge.from, &edge.to);
            }
        }

        // 设置入口和出口
        if let Some(first_node) = def.nodes.first() {
            builder = builder.set_entry(&first_node.id);
        }
        if let Some(last_node) = def.nodes.last() {
            builder = builder.set_finish(&last_node.id);
        }

        Ok(builder.build()?)
    }
}

/// 预定义的 Pipeline 目录
pub fn default_pipeline_dir() -> std::path::PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .join("pipelines")
}

/// 加载所有预定义 Pipeline
pub fn load_builtin_pipelines() -> Vec<PipelineDefinition> {
    let mut pipelines = Vec::new();

    // 尝试加载内置 Pipeline
    let builtin_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("tasks")
        .join("pipelines")
        .join("definitions");

    if let Ok(entries) = std::fs::read_dir(&builtin_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("yaml") {
                if let Ok(def) = PipelineLoader::from_yaml(&path) {
                    pipelines.push(def);
                }
            }
        }
    }

    pipelines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_research_pipeline() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("tasks")
            .join("pipelines")
            .join("definitions")
            .join("research.yaml");

        if path.exists() {
            let def = PipelineLoader::from_yaml(&path).unwrap();
            assert_eq!(def.name, "research");
            assert!(!def.nodes.is_empty());
            assert!(!def.edges.is_empty());
        }
    }
}
