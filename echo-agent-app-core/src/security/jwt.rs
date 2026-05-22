//! JWT 令牌管理

use crate::error::WebError;
use crate::security::Claims;
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};

/// JWT 管理器
#[derive(Clone)]
pub struct JwtManager {
    encoding_key: EncodingKey,
    decoding_key: DecodingKey,
    validation: Validation,
}

impl JwtManager {
    /// 创建新的 JWT 管理器
    pub fn new(secret: &str) -> Self {
        let encoding_key = EncodingKey::from_secret(secret.as_bytes());
        let decoding_key = DecodingKey::from_secret(secret.as_bytes());

        let mut validation = Validation::default();
        validation.validate_exp = true;
        validation.leeway = 60; // 60秒的宽容时间

        Self {
            encoding_key,
            decoding_key,
            validation,
        }
    }

    /// 生成 JWT 令牌
    pub fn generate_token(&self, claims: &Claims) -> Result<String, WebError> {
        encode(&Header::default(), claims, &self.encoding_key)
            .map_err(|e| WebError::Auth(format!("生成令牌失败: {}", e)))
    }

    /// 验证 JWT 令牌
    pub fn verify_token(&self, token: &str) -> Result<Claims, WebError> {
        let token_data =
            decode::<Claims>(token, &self.decoding_key, &self.validation).map_err(|e| {
                match e.kind() {
                    jsonwebtoken::errors::ErrorKind::ExpiredSignature => WebError::TokenExpired,
                    _ => WebError::Auth(format!("令牌验证失败: {}", e)),
                }
            })?;

        Ok(token_data.claims)
    }

    /// 从 Authorization 头提取令牌
    pub fn extract_token_from_header(auth_header: &str) -> Result<String, WebError> {
        if !auth_header.starts_with("Bearer ") {
            return Err(WebError::Auth(
                "Authorization 头格式无效，应为 'Bearer <token>'".to_string(),
            ));
        }

        let token = auth_header[7..].trim(); // 移除 "Bearer " 前缀
        if token.is_empty() {
            return Err(WebError::Auth("令牌不能为空".to_string()));
        }

        Ok(token.to_string())
    }
}
