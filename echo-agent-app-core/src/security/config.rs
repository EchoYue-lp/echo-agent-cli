//! 安全配置模块
//!
//! 控制认证、JWT、CORS、速率限制和请求追踪等安全相关功能。
//! 可通过环境变量设置（优先级：环境变量 > 默认值）。
//! 注意：安全配置属于应用层关注点，不纳入通用框架的 YAML 配置。
//!
//! 此模块还包含 JWT 令牌管理相关的类型。

use serde::{Deserialize, Serialize};

/// 安全配置
///
/// 控制认证、JWT、CORS、速率限制和请求追踪等安全相关功能。
/// 主要通过环境变量配置，也支持通过 API 热更新。
///
/// # 安全说明
///
/// - `auth_enabled` 为 `true` 时，所有 API 请求（除 `/api/health` 和 `/api/auth/login` 外）
///   都需要 Bearer Token 认证。
/// - 管理员用户名和密码通过 `ADMIN_USERNAME` 和 `ADMIN_PASSWORD` 环境变量配置，
///   两个都必须设置且**不得**为默认值。
/// - 认证未启用时（`auth_enabled = false`），登录接口返回错误，服务仅限本地内网使用。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct SecurityConfig {
    /// 是否启用 JWT 认证
    pub auth_enabled: bool,
    /// JWT 密钥（至少 32 字符）
    pub jwt_secret: String,
    /// JWT 令牌过期时间（小时）
    pub jwt_expiry_hours: u64,
    /// 允许的 CORS 来源列表
    pub cors_origins: Vec<String>,
    /// 速率限制（每分钟请求数），0 表示禁用
    pub rate_limit_requests_per_minute: u32,
    /// 是否启用请求 ID 追踪
    pub enable_request_id: bool,
    /// 管理员用户名（禁止默认值，必须通过环境变量配置）
    pub admin_username: String,
    /// 管理员密码（禁止默认值，必须通过环境变量配置）
    pub admin_password_hash: String,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            auth_enabled: false,
            jwt_secret: String::new(),
            jwt_expiry_hours: 24,
            cors_origins: vec!["http://localhost:5173".to_string()],
            rate_limit_requests_per_minute: 60,
            enable_request_id: true,
            admin_username: String::new(),
            admin_password_hash: String::new(),
        }
    }
}

impl SecurityConfig {
    /// 从环境变量加载配置（覆盖当前值）
    pub fn load_from_env(&mut self) {
        if let Ok(enabled) = std::env::var("AUTH_ENABLED") {
            self.auth_enabled = enabled.to_lowercase() == "true";
        }
        if let Ok(secret) = std::env::var("JWT_SECRET") {
            self.jwt_secret = secret;
        }
        if let Ok(expiry) = std::env::var("JWT_EXPIRY_HOURS")
            && let Ok(parsed) = expiry.parse()
        {
            self.jwt_expiry_hours = parsed;
        }
        if let Ok(origins) = std::env::var("CORS_ORIGINS") {
            self.cors_origins = origins
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
        if let Ok(limit) = std::env::var("RATE_LIMIT_REQUESTS_PER_MINUTE")
            && let Ok(parsed) = limit.parse()
        {
            self.rate_limit_requests_per_minute = parsed;
        }
        if let Ok(enabled) = std::env::var("ENABLE_REQUEST_ID") {
            self.enable_request_id = enabled.to_lowercase() == "true";
        }
        // 管理员凭据（必须从环境变量读取，禁止默认值）
        if let Ok(username) = std::env::var("ADMIN_USERNAME") {
            self.admin_username = username;
        }
        if let Ok(password) = std::env::var("ADMIN_PASSWORD") {
            // 先对默认密码发出警告（在哈希之前检查明文）
            if password == "admin" {
                tracing::warn!("检测到默认管理员密码，强烈建议使用强密码");
            }
            // 使用 bcrypt 哈希存储密码
            match bcrypt::hash(&password, bcrypt::DEFAULT_COST) {
                Ok(hashed) => {
                    self.admin_password_hash = hashed;
                }
                Err(e) => {
                    tracing::error!("管理员密码 bcrypt 哈希失败: {}", e);
                    // 回退：不更新哈希值（使用默认空值）
                }
            }
        }
    }

    /// 创建 SecurityConfig，先加载默认值，再加载环境变量覆盖
    pub fn from_env() -> Self {
        let mut config = Self::default();
        config.load_from_env();
        config
    }

    /// 验证配置是否有效
    pub fn validate(&self) -> Result<(), String> {
        if self.auth_enabled {
            if self.jwt_secret.is_empty() {
                return Err("jwt_secret 不能为空 (配置 AUTH_ENABLED=true 时)".to_string());
            }
            if self.jwt_secret.len() < 32 {
                return Err("jwt_secret 至少需要 32 字符".to_string());
            }
            if self.admin_username.is_empty() {
                return Err("ADMIN_USERNAME 不能为空 (配置 AUTH_ENABLED=true 时)".to_string());
            }
            if self.admin_password_hash.is_empty() {
                return Err("ADMIN_PASSWORD 不能为空 (配置 AUTH_ENABLED=true 时)".to_string());
            }
            if self.admin_username == "admin" {
                tracing::warn!("检测到默认管理员用户名 (admin)，强烈建议使用自定义凭据");
            }
        }
        Ok(())
    }
}

/// JWT 声明结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// 主题（用户ID）
    pub sub: String,
    /// 过期时间（Unix 时间戳）
    pub exp: usize,
    /// 签发时间（Unix 时间戳）
    pub iat: usize,
    /// 令牌类型
    pub typ: String,
}

impl Claims {
    /// 创建新的声明
    pub fn new(sub: String, expiry_hours: u64) -> Self {
        let now = chrono::Utc::now().timestamp() as usize;
        let exp = now + (expiry_hours as usize * 3600);

        Self {
            sub,
            exp,
            iat: now,
            typ: "access".to_string(),
        }
    }
}
