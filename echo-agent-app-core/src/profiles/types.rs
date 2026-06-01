//! 配置档案类型定义

use chrono::Utc;
use serde::{Deserialize, Serialize};

/// 命名配置档案
///
/// 每个档案保存一组完整的 Agent 配置参数，
/// 存储在 `~/.echo-agent/profiles/<name>.json`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    /// 档案名称（唯一标识）
    pub name: String,
    /// 模型名称
    pub model: String,
    /// 系统提示词
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// 颜色主题
    #[serde(default = "default_theme")]
    pub theme: String,
    /// 输出格式
    #[serde(default = "default_output_format")]
    pub output_format: String,
    /// 最大迭代次数
    #[serde(default = "default_max_iterations")]
    pub max_iterations: usize,
    /// 温度参数 (0.0-2.0)
    #[serde(default)]
    pub temperature: Option<f64>,
    /// 最大 Token 限制
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// 是否为当前激活的档案
    #[serde(default)]
    pub active: bool,
    /// 创建时间
    pub created_at: String,
    /// 最后更新时间
    pub updated_at: String,
}

fn default_theme() -> String {
    "dark".to_string()
}

fn default_output_format() -> String {
    "text".to_string()
}

fn default_max_iterations() -> usize {
    0 // 0 = unlimited (no iteration limit)
}

impl Profile {
    /// 创建新档案
    pub fn new(name: &str, model: &str) -> Self {
        let now = Utc::now().to_rfc3339();
        Self {
            name: name.to_string(),
            model: model.to_string(),
            system_prompt: None,
            theme: default_theme(),
            output_format: default_output_format(),
            max_iterations: default_max_iterations(),
            temperature: None,
            max_tokens: None,
            active: false,
            created_at: now.clone(),
            updated_at: now,
        }
    }
}

/// 档案摘要（列表展示用）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileSummary {
    pub name: String,
    pub model: String,
    pub theme: String,
    pub active: bool,
    pub updated_at: String,
}
