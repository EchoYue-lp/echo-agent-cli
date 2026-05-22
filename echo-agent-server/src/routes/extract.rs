//! 结构化输出 API
//!
//! 使用 LLM 从自然语言中提取结构化 JSON 数据。
//!
//! # 背景
//!
//! 很多场景需要从非结构化文本中提取结构化信息：
//! - 从简历中提取联系方式
//! - 从发票文本中提取金额、日期、供应商
//! - 从会议记录中提取待办事项
//!
//! # API 端点
//!
//! | 端点 | 方法 | 说明 |
//! |------|------|------|
//! | `/api/extract` | POST | 执行结构化提取 |
//! | `/api/extract/validate` | POST | 验证 JSON Schema |
//! | `/api/extract/examples` | GET | 获取示例 Schema |
//!
//! # 示例
//!
//! ```json
//! // POST /api/extract
//! {
//!   "input": "张三，28岁，邮箱 zhangsan@example.com",
//!   "schema": {
//!     "type": "object",
//!     "properties": {
//!       "name": { "type": "string" },
//!       "age": { "type": "integer" },
//!       "email": { "type": "string" }
//!     },
//!     "required": ["name", "age", "email"]
//!   }
//! }
//! ```

use axum::{
    Json, debug_handler,
    extract::State,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

use crate::error::WebError;
use crate::state::AppState;

// ── 请求类型 ─────────────────────────────────────────────────

/// 结构化提取请求
///
/// # 示例
///
/// ```json
/// {
///   "input": "张三，28岁",
///   "schema": {
///     "type": "object",
///     "properties": {
///       "name": { "type": "string" },
///       "age": { "type": "integer" }
///     }
///   },
///   "schema_name": "person"
/// }
/// ```
#[derive(Debug, Deserialize)]
pub struct ExtractRequest {
    /// 用户输入的自然语言文本
    pub input: String,
    /// JSON Schema 定义输出结构
    pub schema: serde_json::Value,
    /// Schema 名称，用于日志和调试
    #[serde(default = "default_schema_name")]
    pub schema_name: String,
}

fn default_schema_name() -> String {
    "response".to_string()
}

/// Schema 验证请求
#[derive(Debug, Deserialize)]
pub struct ValidateSchemaRequest {
    /// JSON Schema 定义
    pub schema: serde_json::Value,
}

// ── 响应类型 ─────────────────────────────────────────────────

/// 结构化提取响应
#[derive(Debug, Serialize)]
pub struct ExtractResponse {
    /// 是否成功
    pub success: bool,
    /// 提取的结构化数据
    pub data: serde_json::Value,
}

/// Schema 验证响应
#[derive(Debug, Serialize)]
pub struct ValidateSchemaResponse {
    /// 是否有效
    pub valid: bool,
    /// 错误信息列表
    pub errors: Vec<String>,
}

// ── API 处理函数 ─────────────────────────────────────────────────

/// POST /api/extract - 执行结构化提取
///
/// 使用 LLM 从自然语言中提取结构化数据。
///
/// # 请求体
///
/// - `input`: 自然语言输入
/// - `schema`: JSON Schema 定义输出结构
/// - `schema_name`: 可选的 schema 名称
///
/// # 响应
///
/// ```json
/// {
///   "success": true,
///   "data": {
///     "name": "张三",
///     "age": 28,
///     "email": "zhangsan@example.com"
///   }
/// }
/// ```
#[debug_handler]
pub async fn extract(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ExtractRequest>,
) -> Response {
    use echo_agent::prelude::*;

    let response_format = ResponseFormat::json_schema(&req.schema_name, req.schema.clone());

    // 执行提取
    match state
        .connection
        .agent
        .read_async(|agent| {
            Box::pin(async move { agent.extract_json(&req.input, response_format).await })
        })
        .await
    {
        Ok(value) => Json(ExtractResponse {
            success: true,
            data: value,
        })
        .into_response(),
        Err(e) => {
            tracing::warn!("结构化输出失败: {}", e);
            WebError::Internal(format!("结构化输出失败: {}", e)).into_response()
        }
    }
}

/// POST /api/extract/validate - 验证 JSON Schema
///
/// 验证提供的 Schema 是否符合基本规范。
///
/// # 响应
///
/// ```json
/// {
///   "valid": true,
///   "errors": []
/// }
/// ```
#[debug_handler]
pub async fn validate_schema(Json(req): Json<ValidateSchemaRequest>) -> Response {
    let mut errors = Vec::new();

    // 检查 schema 是否是有效的 JSON Schema
    if let Some(obj) = req.schema.as_object() {
        if !obj.contains_key("type") {
            errors.push("Schema 缺少 'type' 字段".to_string());
        }
    } else {
        errors.push("Schema 必须是一个对象".to_string());
    }

    let valid = errors.is_empty();

    Json(ValidateSchemaResponse { valid, errors }).into_response()
}

/// GET /api/extract/examples - 获取结构化输出示例
///
/// 返回一些常见场景的 JSON Schema 示例。
#[debug_handler]
pub async fn get_examples() -> Response {
    Json(json!({
        "examples": [
            {
                "name": "用户信息提取",
                "description": "从文本中提取用户基本信息",
                "input": "我叫张三，今年 25 岁，邮箱是 zhangsan@example.com",
                "schema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "用户姓名" },
                        "age": { "type": "integer", "description": "用户年龄" },
                        "email": { "type": "string", "description": "用户邮箱" }
                    },
                    "required": ["name", "age", "email"]
                }
            },
            {
                "name": "发票信息提取",
                "description": "从发票文本中提取关键信息",
                "input": "发票编号 INV-2024-001，金额 1250.00 元，日期 2024-03-15，供应商是北京科技有限公司",
                "schema": {
                    "type": "object",
                    "properties": {
                        "invoice_number": { "type": "string", "description": "发票编号" },
                        "amount": { "type": "number", "description": "金额" },
                        "date": { "type": "string", "description": "日期" },
                        "vendor": { "type": "string", "description": "供应商" }
                    },
                    "required": ["invoice_number", "amount", "date", "vendor"]
                }
            },
            {
                "name": "任务列表提取",
                "description": "从会议记录中提取待办事项",
                "input": "明天需要完成：1. 写周报 2. 开项目会议 3. 代码审查",
                "schema": {
                    "type": "object",
                    "properties": {
                        "date": { "type": "string", "description": "日期" },
                        "tasks": {
                            "type": "array",
                            "description": "任务列表",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "order": { "type": "integer", "description": "序号" },
                                    "task": { "type": "string", "description": "任务内容" }
                                }
                            }
                        }
                    }
                }
            }
        ]
    }))
    .into_response()
}

// ── 单元测试 ─────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_request_deserialize() {
        let json = r#"{
            "input": "张三，28岁",
            "schema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "age": { "type": "integer" }
                }
            },
            "schema_name": "person"
        }"#;
        let req: ExtractRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.input, "张三，28岁");
        assert_eq!(req.schema_name, "person");
    }

    #[test]
    fn test_extract_request_default_schema_name() {
        let json = r#"{
            "input": "test",
            "schema": {"type": "object"}
        }"#;
        let req: ExtractRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.schema_name, "response");
    }

    #[test]
    fn test_extract_response_serialize() {
        let resp = ExtractResponse {
            success: true,
            data: serde_json::json!({"name": "张三", "age": 28}),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"success\":true"));
        assert!(json.contains("\"name\":\"张三\""));
    }

    #[test]
    fn test_validate_schema_response_serialize() {
        let resp = ValidateSchemaResponse {
            valid: true,
            errors: vec![],
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"valid\":true"));
    }

    #[test]
    fn test_validate_schema_response_with_errors() {
        let resp = ValidateSchemaResponse {
            valid: false,
            errors: vec!["Schema 缺少 'type' 字段".to_string()],
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"valid\":false"));
        assert!(json.contains("缺少 'type'"));
    }

    #[test]
    fn test_schema_validation_valid() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"}
            }
        });
        let obj = schema.as_object().unwrap();
        assert!(obj.contains_key("type"));
    }

    #[test]
    fn test_schema_validation_missing_type() {
        let schema = serde_json::json!({
            "properties": {
                "name": {"type": "string"}
            }
        });
        let obj = schema.as_object().unwrap();
        assert!(!obj.contains_key("type"));
    }
}
