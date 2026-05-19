//! 安全模块
//!
//! 提供认证、授权、CORS、速率限制等安全功能。
//!
//! 安全配置直接在 CLI 层定义，不依赖框架库。

mod config;
mod jwt;
mod middleware;

pub use config::{SecurityConfig, Claims};
pub use jwt::JwtManager;
pub use middleware::{require_auth, rate_limit_middleware, request_id_middleware, create_cors_layer};