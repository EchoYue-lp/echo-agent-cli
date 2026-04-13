//! 记忆管理 API
//!
//! 提供长期记忆的存储、检索、删除功能。
//!
//! # API 端点
//!
//! | 端点 | 方法 | 说明 |
//! |------|------|------|
//! | `/api/memory` | POST | 添加或更新记忆 |
//! | `/api/memory` | GET | 获取指定记忆 |
//! | `/api/memory/search` | POST | 搜索记忆 |
//! | `/api/memory/delete` | POST | 删除记忆 |
//! | `/api/memory/namespaces` | GET | 列出所有命名空间 |
//!
//! # 示例
//!
//! ```json
//! // POST /api/memory
//! {
//!   "namespace": "user/123",
//!   "key": "preference",
//!   "value": { "theme": "dark", "language": "zh-CN" }
//! }
//! ```

use axum::{
    debug_handler,
    extract::State,
    response::{IntoResponse, Response},
    Json,
};
use std::sync::Arc;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::WebError;
use crate::state::AppState;

// ── 请求类型 ─────────────────────────────────────────────────

/// 添加记忆请求
///
/// # 示例
///
/// ```json
/// {
///   "namespace": "user/123",
///   "key": "profile",
///   "value": { "name": "张三", "age": 25 }
/// }
/// ```
#[derive(Debug, Deserialize)]
pub struct AddMemoryRequest {
    /// 命名空间，支持多级（如 "user/123/preferences"）
    /// 默认为 "default"
    #[serde(default = "default_namespace")]
    pub namespace: String,
    /// 记忆唯一标识键
    pub key: String,
    /// 记忆内容，支持任意 JSON 值
    pub value: serde_json::Value,
}

fn default_namespace() -> String {
    "default".to_string()
}

/// 搜索记忆请求
///
/// 使用关键词在指定命名空间中搜索记忆。
#[derive(Debug, Deserialize)]
pub struct SearchMemoryRequest {
    /// 搜索的命名空间
    #[serde(default = "default_namespace")]
    pub namespace: String,
    /// 搜索关键词
    pub query: String,
    /// 返回结果数量限制，默认 10
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_limit() -> usize {
    10
}

/// 删除记忆请求
#[derive(Debug, Deserialize)]
pub struct DeleteMemoryRequest {
    /// 命名空间
    pub namespace: String,
    /// 记忆键
    pub key: String,
}

/// 获取记忆查询参数
#[derive(Debug, Deserialize)]
pub struct GetMemoryQuery {
    /// 命名空间
    pub namespace: String,
    /// 记忆键
    pub key: String,
}

// ── 响应类型 ─────────────────────────────────────────────────

/// 记忆项响应
#[derive(Debug, Serialize)]
pub struct MemoryItemResponse {
    /// 命名空间
    pub namespace: String,
    /// 记忆键
    pub key: String,
    /// 记忆内容
    pub value: serde_json::Value,
    /// 创建时间（Unix 时间戳，秒）
    pub created_at: u64,
    /// 最后更新时间（Unix 时间戳，秒）
    pub updated_at: u64,
    /// 搜索相关度分数（仅搜索结果有值）
    pub score: Option<f32>,
}

/// 添加记忆响应
#[derive(Debug, Serialize)]
pub struct AddMemoryResponse {
    /// 是否成功
    pub success: bool,
    /// 记忆键
    pub key: String,
    /// 结果消息
    pub message: String,
}

/// 搜索记忆响应
#[derive(Debug, Serialize)]
pub struct SearchMemoryResponse {
    /// 匹配的记忆列表
    pub items: Vec<MemoryItemResponse>,
    /// 总数量
    pub total: usize,
}

/// 删除记忆响应
#[derive(Debug, Serialize)]
pub struct DeleteMemoryResponse {
    /// 是否成功删除
    pub success: bool,
    /// 结果消息
    pub message: String,
}

/// 命名空间列表响应
#[derive(Debug, Serialize)]
pub struct NamespacesResponse {
    /// 命名空间列表（每个命名空间是一个字符串数组）
    pub namespaces: Vec<Vec<String>>,
}

// ── API 处理函数 ─────────────────────────────────────────────────

/// POST /api/memory - 添加或更新记忆
///
/// 将数据存储到长期记忆中，支持 upsert 语义。
///
/// # 请求体
///
/// ```json
/// {
///   "namespace": "user/123",
///   "key": "profile",
///   "value": { "name": "张三" }
/// }
/// ```
#[debug_handler]
pub async fn add_memory(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AddMemoryRequest>,
) -> Response {
    let agent = state.agent.lock().await;

    // 获取 Store
    let store = match agent.store() {
        Some(s) => s,
        None => {
            return WebError::Internal("Memory 未启用，请在配置中设置 enable_memory=true".to_string())
                .into_response();
        }
    };

    let namespace: Vec<&str> = req.namespace.split('/').collect();

    match store.put(&namespace, &req.key, req.value.clone()).await {
        Ok(()) => Json(AddMemoryResponse {
            success: true,
            key: req.key,
            message: "记忆已保存".to_string(),
        })
        .into_response(),
        Err(e) => WebError::Internal(format!("保存记忆失败: {}", e)).into_response(),
    }
}

/// GET /api/memory - 获取指定记忆
///
/// 通过命名空间和键精确获取记忆内容。
///
/// # 查询参数
///
/// - `namespace`: 命名空间
/// - `key`: 记忆键
#[debug_handler]
pub async fn get_memory(
    State(state): State<Arc<AppState>>,
    query: axum::extract::Query<GetMemoryQuery>,
) -> Response {
    let agent = state.agent.lock().await;

    let store = match agent.store() {
        Some(s) => s,
        None => {
            return WebError::Internal("Memory 未启用".to_string()).into_response();
        }
    };

    let namespace: Vec<&str> = query.namespace.split('/').collect();

    match store.get(&namespace, &query.key).await {
        Ok(Some(item)) => Json(MemoryItemResponse {
            namespace: item.namespace.join("/"),
            key: item.key,
            value: item.value,
            created_at: item.created_at,
            updated_at: item.updated_at,
            score: None,
        })
        .into_response(),
        Ok(None) => Json(json!({
            "error": format!("记忆 '{}' 不存在", query.key),
            "found": false
        }))
        .into_response(),
        Err(e) => WebError::Internal(format!("获取记忆失败: {}", e)).into_response(),
    }
}

/// POST /api/memory/search - 搜索记忆
///
/// 在指定命名空间中使用关键词搜索记忆。
/// 返回结果按相关度排序。
#[debug_handler]
pub async fn search_memory(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SearchMemoryRequest>,
) -> Response {
    let agent = state.agent.lock().await;

    let store = match agent.store() {
        Some(s) => s,
        None => {
            return WebError::Internal("Memory 未启用".to_string()).into_response();
        }
    };

    let namespace: Vec<&str> = req.namespace.split('/').collect();

    match store.search(&namespace, &req.query, req.limit).await {
        Ok(items) => {
            let total = items.len();
            let response_items: Vec<MemoryItemResponse> = items
                .into_iter()
                .map(|item| MemoryItemResponse {
                    namespace: item.namespace.join("/"),
                    key: item.key,
                    value: item.value,
                    created_at: item.created_at,
                    updated_at: item.updated_at,
                    score: item.score,
                })
                .collect();

            Json(SearchMemoryResponse {
                items: response_items,
                total,
            })
            .into_response()
        }
        Err(e) => WebError::Internal(format!("搜索记忆失败: {}", e)).into_response(),
    }
}

/// POST /api/memory/delete - 删除记忆
///
/// 删除指定命名空间和键的记忆。
#[debug_handler]
pub async fn delete_memory(
    State(state): State<Arc<AppState>>,
    Json(req): Json<DeleteMemoryRequest>,
) -> Response {
    let agent = state.agent.lock().await;

    let store = match agent.store() {
        Some(s) => s,
        None => {
            return WebError::Internal("Memory 未启用".to_string()).into_response();
        }
    };

    let namespace: Vec<&str> = req.namespace.split('/').collect();

    match store.delete(&namespace, &req.key).await {
        Ok(deleted) => Json(DeleteMemoryResponse {
            success: deleted,
            message: if deleted {
                format!("记忆 '{}' 已删除", req.key)
            } else {
                format!("记忆 '{}' 不存在", req.key)
            },
        })
        .into_response(),
        Err(e) => WebError::Internal(format!("删除记忆失败: {}", e)).into_response(),
    }
}

/// GET /api/memory/namespaces - 列出所有命名空间
///
/// 返回所有已存在的命名空间列表。
#[debug_handler]
pub async fn list_namespaces(
    State(state): State<Arc<AppState>>,
) -> Response {
    let agent = state.agent.lock().await;

    let store = match agent.store() {
        Some(s) => s,
        None => {
            return WebError::Internal("Memory 未启用".to_string()).into_response();
        }
    };

    match store.list_namespaces(None).await {
        Ok(namespaces) => Json(NamespacesResponse { namespaces }).into_response(),
        Err(e) => WebError::Internal(format!("获取命名空间失败: {}", e)).into_response(),
    }
}

// ── 单元测试 ─────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_memory_request_deserialize() {
        let json = r#"{
            "namespace": "user/123",
            "key": "profile",
            "value": {"name": "张三"}
        }"#;
        let req: AddMemoryRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.namespace, "user/123");
        assert_eq!(req.key, "profile");
        assert_eq!(req.value["name"], "张三");
    }

    #[test]
    fn test_add_memory_request_default_namespace() {
        let json = r#"{
            "key": "test",
            "value": "data"
        }"#;
        let req: AddMemoryRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.namespace, "default");
    }

    #[test]
    fn test_search_memory_request_deserialize() {
        let json = r#"{
            "namespace": "user/123",
            "query": "偏好设置",
            "limit": 20
        }"#;
        let req: SearchMemoryRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.namespace, "user/123");
        assert_eq!(req.query, "偏好设置");
        assert_eq!(req.limit, 20);
    }

    #[test]
    fn test_search_memory_request_defaults() {
        let json = r#"{
            "query": "test"
        }"#;
        let req: SearchMemoryRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.namespace, "default");
        assert_eq!(req.limit, 10);
    }

    #[test]
    fn test_memory_item_response_serialize() {
        let item = MemoryItemResponse {
            namespace: "user/123".to_string(),
            key: "profile".to_string(),
            value: serde_json::json!({"name": "张三"}),
            created_at: 1700000000,
            updated_at: 1700000100,
            score: Some(0.95),
        };
        let json = serde_json::to_string(&item).unwrap();
        assert!(json.contains("\"namespace\":\"user/123\""));
        assert!(json.contains("\"score\":0.95"));
    }

    #[test]
    fn test_add_memory_response_serialize() {
        let resp = AddMemoryResponse {
            success: true,
            key: "profile".to_string(),
            message: "记忆已保存".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"success\":true"));
    }

    #[test]
    fn test_delete_memory_response_serialize() {
        let resp = DeleteMemoryResponse {
            success: true,
            message: "记忆 'profile' 已删除".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"success\":true"));
    }
}