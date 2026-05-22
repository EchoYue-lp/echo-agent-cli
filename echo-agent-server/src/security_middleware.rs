//! 安全中间件

use crate::error::WebError;
use crate::state::AppState;
use axum::{
    extract::{Request, State},
    http::HeaderValue,
    middleware::Next,
    response::Response,
};
use std::sync::Arc;

/// 认证中间件
///
/// 检查请求是否包含有效的 JWT 令牌。
/// 如果认证未启用（auth_enabled=false），则跳过验证。
pub async fn require_auth(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Result<Response, WebError> {
    // 从安全配置读取认证开关和 JWT 密钥
    let sec_cfg = state.config.security_config.read().await;
    let (auth_enabled, jwt_secret) = (sec_cfg.auth_enabled, sec_cfg.jwt_secret.clone());

    // 检查认证是否启用
    if !auth_enabled {
        return Ok(next.run(request).await);
    }

    // 提取 Authorization 头
    let headers = request.headers();
    let auth_header = headers
        .get("Authorization")
        .and_then(|h| h.to_str().ok())
        .ok_or(WebError::Auth("缺少 Authorization 头".to_string()))?;

    // 提取令牌
    let token = crate::security::JwtManager::extract_token_from_header(auth_header)?;

    // 验证令牌（使用缓存的 JwtManager 避免重复构造密钥）
    let jwt_manager = state.get_or_create_jwt_manager(&jwt_secret).await;
    let _claims = jwt_manager.verify_token(&token)?;

    // 令牌验证成功，继续处理请求
    Ok(next.run(request).await)
}

/// 速率限制中间件
///
/// 基于客户端IP地址限制请求频率。
pub async fn rate_limit_middleware(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Result<Response, WebError> {
    // 从安全配置读取速率限制值
    let sec_cfg = state.config.security_config.read().await;
    let rate_limit_per_minute = sec_cfg.rate_limit_requests_per_minute;

    // 检查速率限制是否启用（0表示禁用）
    if rate_limit_per_minute == 0 {
        return Ok(next.run(request).await);
    }

    // 获取客户端IP地址
    // 优先从 socket peer 获取真实 IP，再检查代理头
    let client_ip = extract_client_ip(&request);

    // 检查速率限制
    if state.session.rate_limiter.check_key(&client_ip).is_err() {
        return Err(WebError::RateLimitExceeded);
    }

    Ok(next.run(request).await)
}

/// Extract client IP with proper proxy header handling.
///
/// Priority: socket peer IP > last value of X-Forwarded-For > X-Real-IP > CF-Connecting-IP.
/// For X-Forwarded-For, the last entry is the most trustworthy (set by the nearest proxy).
fn extract_client_ip(request: &Request) -> String {
    if let Some(forwarded) = request.headers().get("X-Forwarded-For") {
        if let Ok(val) = forwarded.to_str() {
            if let Some(last) = val.rsplit(',').next() {
                let trimmed = last.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
        }
    }

    request
        .headers()
        .get("X-Real-IP")
        .or_else(|| request.headers().get("CF-Connecting-IP"))
        .and_then(|h| h.to_str().ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| {
            request
                .extensions()
                .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
                .map(|ci| ci.0.ip().to_string())
                .unwrap_or_else(|| "unknown".to_string())
        })
}

/// 请求ID中间件
///
/// 为每个请求生成唯一ID，便于追踪和调试。
pub async fn request_id_middleware(request: Request, next: Next) -> Response {
    use uuid::Uuid;

    let request_id = Uuid::new_v4().to_string();

    // 将请求ID添加到请求扩展中，供后续处理使用
    let mut request = request;
    request.extensions_mut().insert(request_id.clone());

    // 处理请求
    let mut response = next.run(request).await;

    // 将请求ID添加到响应头中
    response.headers_mut().insert(
        "X-Request-ID",
        HeaderValue::from_str(&request_id).unwrap_or_else(|_| HeaderValue::from_static("unknown")),
    );

    response
}

/// CORS 配置创建函数
pub fn create_cors_layer(
    security_config: &crate::security::SecurityConfig,
) -> tower_http::cors::CorsLayer {
    use axum::http::{HeaderValue, Method};
    use tower_http::cors::{AllowOrigin, Any, CorsLayer};

    if security_config.cors_origins.is_empty() {
        // 如果没有配置来源，允许所有来源（开发环境）
        // 注意：`allow_origin(Any)` 与 `allow_credentials(true)` 互斥，
        // 浏览器会拒绝此类响应。因此开发模式下禁用 credentials。
        CorsLayer::new()
            .allow_origin(Any)
            .allow_methods([
                Method::GET,
                Method::POST,
                Method::PUT,
                Method::DELETE,
                Method::OPTIONS,
            ])
            .allow_headers(Any)
    } else {
        // 配置指定的来源（使用 AllowOrigin::list 支持多个 origin）
        let origins: Vec<HeaderValue> = security_config
            .cors_origins
            .iter()
            .filter_map(|o| HeaderValue::from_str(o).ok())
            .collect();

        CorsLayer::new()
            .allow_origin(AllowOrigin::list(origins))
            .allow_methods([
                Method::GET,
                Method::POST,
                Method::PUT,
                Method::DELETE,
                Method::OPTIONS,
            ])
            .allow_headers([
                axum::http::header::AUTHORIZATION,
                axum::http::header::CONTENT_TYPE,
            ])
            .allow_credentials(true)
    }
}
