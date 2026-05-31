//! 路由构建与认证端点
//!
//! 提供 axum Router 构建、登录和健康检查处理函数。

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, HeaderValue},
    routing::{delete, get, post, put},
};
use std::sync::Arc;

use crate::state::AppState;
use crate::{metrics, routes, security, security_middleware, ws};

/// Constant-time byte comparison to prevent timing attacks.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut result: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        result |= x ^ y;
    }
    result == 0
}

/// 构建路由
pub async fn build_router(state: Arc<AppState>) -> Router {
    use axum::middleware;

    // 创建认证路由（公开）
    let auth_routes = Router::new()
        .route("/api/auth/login", post(handle_login))
        .route("/api/health", get(handle_health))
        .route("/api/health/deep", get(handle_deep_health))
        .route("/metrics", get(metrics::handle_metrics))
        .with_state(state.clone());

    // 创建受保护的路由（应用安全中间件）
    let protected_routes = Router::new()
        // ── 对话 API ─────────────────────────────────────────────
        .route("/api/chat", post(routes::chat::handle_chat))
        .route("/api/chat/stream", post(routes::chat::handle_chat_stream))
        .route("/api/history", get(routes::history::get_history))
        .route("/api/history/export", get(routes::history::export_history))
        // ── 会话 API ─────────────────────────────────────────────
        .route("/api/session", get(routes::session::get_session))
        .route("/api/session/reset", post(routes::session::reset_session))
        // ── 工具 API ─────────────────────────────────────────────
        .route("/api/tools/:name/enable", post(routes::tools::enable_tool))
        .route(
            "/api/tools/:name/disable",
            post(routes::tools::disable_tool),
        )
        .route("/api/tools/:name", get(routes::tools::get_tool))
        .route("/api/tools", get(routes::tools::list_tools))
        // ── Skill API ─────────────────────────────────────────────
        .route(
            "/api/skills/load",
            post(routes::skills::load_skills_from_dir),
        )
        .route("/api/skills/upload", post(routes::skills::upload_skills))
        .route("/api/skills/:name", get(routes::skills::get_skill))
        .route("/api/skills", get(routes::skills::list_skills))
        // ── MCP API ─────────────────────────────────────────────
        .route("/api/mcp/connect", post(routes::mcp::connect_mcp_server))
        .route(
            "/api/mcp/:name/disconnect",
            post(routes::mcp::disconnect_mcp_server),
        )
        .route("/api/mcp/health", get(routes::mcp::get_mcp_health))
        .route("/api/mcp/:name", get(routes::mcp::get_mcp_server))
        .route("/api/mcp", get(routes::mcp::list_mcp_servers))
        .route(
            "/api/mcp/config",
            get(routes::mcp::get_mcp_config).put(routes::mcp::update_mcp_config),
        )
        // ── 配置 API ─────────────────────────────────────────────
        .route(
            "/api/config",
            get(routes::config::get_config).put(routes::config::update_config),
        )
        .route(
            "/api/config/full",
            get(routes::config::get_full_config).put(routes::config::update_full_config),
        )
        .route(
            "/api/config/security/reload",
            post(routes::config::reload_security_config),
        )
        .route("/api/config/discover", get(routes::config::discover_config))
        // ── 上下文 API ─────────────────────────────────────────────
        .route("/api/context", get(routes::context::get_context))
        // ── 压缩 API ─────────────────────────────────────────────
        .route("/api/compress", post(routes::compress::compress))
        .route(
            "/api/compress/stats",
            get(routes::compress::get_compression_stats),
        )
        // ── 记忆 API ─────────────────────────────────────────────
        .route(
            "/api/memory",
            post(routes::memory::add_memory).get(routes::memory::get_memory),
        )
        .route("/api/memory/search", post(routes::memory::search_memory))
        .route("/api/memory/delete", post(routes::memory::delete_memory))
        .route(
            "/api/memory/namespaces",
            get(routes::memory::list_namespaces),
        )
        .route("/api/memory/list", get(routes::memory::list_memory))
        // ── 结构化输出 API ─────────────────────────────────────────────
        .route("/api/extract", post(routes::extract::extract))
        .route(
            "/api/extract/validate",
            post(routes::extract::validate_schema),
        )
        .route("/api/extract/examples", get(routes::extract::get_examples))
        // ── WebSocket 流式对话 ─────────────────────────────────────────────
        .route("/ws/chat", get(ws::handler::ws_chat_handler))
        // ── 权限 API ─────────────────────────────────────────────
        .route(
            "/api/permissions/mode",
            get(routes::permissions::get_permission_mode)
                .put(routes::permissions::set_permission_mode),
        )
        .route(
            "/api/permissions/rules",
            get(routes::permissions::list_permission_rules)
                .post(routes::permissions::add_permission_rule),
        )
        .route(
            "/api/permissions/rules/:name",
            delete(routes::permissions::remove_permission_rule),
        )
        // ── 工作流 API ─────────────────────────────────────────────
        .route(
            "/api/workflow",
            get(routes::workflow::list_workflows).post(routes::workflow::create_workflow),
        )
        .route(
            "/api/workflow/:id",
            get(routes::workflow::get_workflow).delete(routes::workflow::delete_workflow),
        )
        .route(
            "/api/workflow/:id/execute",
            post(routes::workflow::execute_workflow),
        )
        // ── 审计 API ─────────────────────────────────────────────
        .route(
            "/api/audit/logs",
            get(routes::audit::get_audit_logs).delete(routes::audit::clear_audit_logs),
        )
        .route("/api/audit/stats", get(routes::audit::get_audit_stats))
        // ── 沙箱 API ─────────────────────────────────────────────
        .route(
            "/api/sandbox/status",
            get(routes::sandbox::get_sandbox_status),
        )
        .route(
            "/api/sandbox/config",
            get(routes::sandbox::get_sandbox_config).put(routes::sandbox::update_sandbox_config),
        )
        .route(
            "/api/sandbox/execute",
            post(routes::sandbox::execute_in_sandbox),
        )
        // ── 会话搜索 API ─────────────────────────────────────────────
        .route(
            "/api/sessions/search",
            get(routes::session_search::search_sessions),
        )
        .route(
            "/api/sessions/reindex",
            post(routes::session_search::reindex_sessions),
        )
        // ── 定时任务 API ─────────────────────────────────────────────
        .route(
            "/api/scheduler/tasks",
            get(routes::scheduler::list_tasks).post(routes::scheduler::add_task),
        )
        .route(
            "/api/scheduler/tasks/:id/status",
            put(routes::scheduler::set_task_status),
        )
        .route(
            "/api/scheduler/tasks/:id/run",
            post(routes::scheduler::run_task),
        )
        .route(
            "/api/scheduler/tasks/:id",
            delete(routes::scheduler::remove_task),
        )
        // ── 后台任务 API ─────────────────────────────────────────────
        .route(
            "/api/tasks",
            get(routes::tasks::list_tasks).post(routes::tasks::submit_task),
        )
        .route("/api/tasks/:id", get(routes::tasks::get_task))
        .route("/api/tasks/:id/cancel", post(routes::tasks::cancel_task))
        .route("/api/tasks/:id/events", get(routes::tasks::task_events))
        // ── Webhook API ─────────────────────────────────────────────
        .route(
            "/api/webhooks",
            get(routes::webhooks::list_webhooks).post(routes::webhooks::add_webhook),
        )
        .route(
            "/api/webhooks/remove",
            post(routes::webhooks::remove_webhook),
        )
        .route("/api/webhooks/test", post(routes::webhooks::test_webhook))
        // ── Skills Hub API ─────────────────────────────────────────────
        .route("/api/skills-hub", get(routes::skills_hub::list_hub_skills))
        .route(
            "/api/skills-hub/search",
            get(routes::skills_hub::search_hub_skills),
        )
        .route(
            "/api/skills-hub/:name",
            get(routes::skills_hub::get_hub_skill),
        )
        .route(
            "/api/skills-hub/install/local",
            post(routes::skills_hub::install_local),
        )
        .route(
            "/api/skills-hub/install/git",
            post(routes::skills_hub::install_git),
        )
        .route(
            "/api/skills-hub/uninstall",
            post(routes::skills_hub::uninstall_skill),
        )
        .route(
            "/api/skills-hub/refresh",
            post(routes::skills_hub::refresh_hub),
        )
        // ── Plugin API ─────────────────────────────────────────────
        .route("/api/plugins", get(routes::plugins::list_plugins))
        .route(
            "/api/plugins/install",
            post(routes::plugins::install_plugin),
        )
        .route(
            "/api/plugins/uninstall",
            post(routes::plugins::uninstall_plugin),
        )
        .route(
            "/api/plugins/:name/enable",
            post(routes::plugins::enable_plugin),
        )
        .route(
            "/api/plugins/:name/disable",
            post(routes::plugins::disable_plugin),
        )
        .route("/api/plugins/:name", get(routes::plugins::get_plugin))
        .route(
            "/api/plugins/reload",
            post(routes::plugins::reload_plugins),
        )
        // ── 文件系统 API ─────────────────────────────────────────────
        .route("/api/files/list", get(routes::files::list_files))
        .route("/api/files/read", get(routes::files::read_file))
        .route("/api/files/diff", get(routes::files::diff_file))
        .route("/api/files/tree", get(routes::files::file_tree))
        .route("/api/files/browse", get(routes::files::browse_directories))
        // ── Trace 观测 API ─────────────────────────────────────────────
        .route("/api/trace/sessions", get(routes::trace::list_trace_sessions))
        .route("/api/trace/session/:id", get(routes::trace::get_trace_session))
        .route("/api/trace/stats", get(routes::trace::get_trace_stats))
        // ── Trace Events API ─────────────────────────────────────────
        .merge(routes::trace_events::trace_event_routes())
        // ── Terminal API ─────────────────────────────────────────────
        .merge(routes::terminal::terminal_routes())
        // ── Papers API ───────────────────────────────────────────────
        .merge(routes::papers::paper_routes())
        // ── Scratchpad API ─────────────────────────────────────────────
        .merge(routes::scratchpad::scratchpad_routes())
        // ── Decisions API ─────────────────────────────────────────────
        .merge(routes::decisions::decision_routes())
        // ── 自进化 API ───────────────────────────────────────────────
        .merge(routes::evolution::evolution_routes())
        // ── Provider API ─────────────────────────────────────────────
        .merge(routes::providers::provider_routes())
        // ── 工作区 API ─────────────────────────────────────────────
        .route(
            "/api/workspaces",
            get(routes::workspace::list_workspaces).post(routes::workspace::create_workspace),
        )
        .route(
            "/api/workspaces/current",
            get(routes::workspace::get_current_workspace),
        )
        .route(
            "/api/workspaces/migrate/audit",
            post(routes::workspace::audit_migration),
        )
        .route(
            "/api/workspaces/migrate",
            post(routes::workspace::execute_migration),
        )
        .route(
            "/api/workspaces/:id",
            get(routes::workspace::get_workspace).delete(routes::workspace::delete_workspace),
        )
        .route(
            "/api/workspaces/:id/switch",
            post(routes::workspace::switch_workspace),
        )
        .route(
            "/api/workspaces/:id/link",
            post(routes::workspace::link_project),
        )
        .route(
            "/api/workspaces/default-root/:name",
            get(routes::workspace::get_default_root),
        )
        // ── 会话快照 API ─────────────────────────────────────────────
        .route(
            "/api/session/checkpoint",
            post(routes::session::create_checkpoint),
        )
        .route(
            "/api/session/checkpoints",
            get(routes::session::list_checkpoints),
        )
        .route(
            "/api/session/restore/:snapshot_id",
            post(routes::session::restore_checkpoint),
        )
        // ── 对话历史持久化 API ─────────────────────────────────────────────
        .route(
            "/api/conversations",
            get(routes::conversations::list_conversations)
                .post(routes::conversations::save_conversation),
        )
        .route(
            "/api/conversations/:id",
            get(routes::conversations::get_conversation)
                .put(routes::conversations::update_conversation)
                .delete(routes::conversations::delete_conversation),
        )
        .route(
            "/api/conversations/:id/export",
            get(routes::conversations::export_conversation),
        )
        .route(
            "/api/conversations/:id/restore",
            post(routes::conversations::restore_conversation),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            security_middleware::require_auth,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            security_middleware::rate_limit_middleware,
        ))
        .with_state(state.clone());

    // 合并路由（Web 前端已移除，仅 API）
    let app = Router::new().merge(auth_routes).merge(protected_routes);

    // 应用全局中间件: 指标收集 + CORS配置
    let app = {
        let sec_cfg = state.config.security_config.read().await;
        let app = app.layer(middleware::from_fn(metrics::metrics_middleware));
        let app = if sec_cfg.enable_request_id {
            app.layer(middleware::from_fn(
                security_middleware::request_id_middleware,
            ))
        } else {
            app
        };
        let cors_layer = security_middleware::create_cors_layer(&sec_cfg);
        app.layer(cors_layer)
    };

    // 应用追踪层（过滤敏感头）
    let trace_layer = tower_http::trace::TraceLayer::new_for_http()
        .on_request(
            |request: &axum::http::Request<axum::body::Body>, _span: &tracing::Span| {
                let method = request.method();
                let uri = request.uri();
                let headers = request.headers();

                let mut filtered_headers = HeaderMap::new();
                let redacted_value = HeaderValue::from_static("<REDACTED>");
                for (name, value) in headers.iter() {
                    let header_name = name.as_str();
                    if header_name.eq_ignore_ascii_case("authorization")
                        || header_name.eq_ignore_ascii_case("cookie")
                        || header_name.eq_ignore_ascii_case("set-cookie")
                        || header_name.eq_ignore_ascii_case("proxy-authorization")
                        || header_name.eq_ignore_ascii_case("x-api-key")
                    {
                        filtered_headers.insert(name, redacted_value.clone());
                    } else {
                        filtered_headers.insert(name.clone(), value.clone());
                    }
                }

                tracing::debug!(
                    "请求开始: {} {} 头信息: {:?}",
                    method,
                    uri,
                    filtered_headers
                );
            },
        )
        .on_response(
            |response: &axum::http::Response<axum::body::Body>,
             latency: std::time::Duration,
             _span: &tracing::Span| {
                let status = response.status();
                let headers = response.headers();

                let mut filtered_headers = HeaderMap::new();
                for (name, value) in headers.iter() {
                    let header_name = name.as_str();
                    if header_name.eq_ignore_ascii_case("set-cookie")
                        || header_name.eq_ignore_ascii_case("authorization")
                        || header_name.eq_ignore_ascii_case("x-api-key")
                    {
                        filtered_headers.insert(name, HeaderValue::from_static("<REDACTED>"));
                    } else {
                        filtered_headers.insert(name.clone(), value.clone());
                    }
                }

                tracing::debug!(
                    "请求完成: 状态码={} 耗时={:?}ms 响应头: {:?}",
                    status,
                    latency.as_millis(),
                    filtered_headers
                );
            },
        );

    app.layer(trace_layer)
}

/// 处理登录请求
#[axum::debug_handler]
pub async fn handle_login(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<LoginRequest>,
) -> std::result::Result<Json<LoginResponse>, crate::error::WebError> {
    let sec_cfg = state.config.security_config.read().await;
    let (auth_enabled, jwt_expiry_hours, jwt_secret, username, password) = (
        sec_cfg.auth_enabled,
        sec_cfg.jwt_expiry_hours,
        sec_cfg.jwt_secret.clone(),
        sec_cfg.admin_username.clone(),
        sec_cfg.admin_password_hash.clone(),
    );

    if !auth_enabled {
        return Err(crate::error::WebError::Auth(
            "认证未启用，无法登录".to_string(),
        ));
    }

    if payload.username.is_empty() || payload.password.is_empty() {
        return Err(crate::error::WebError::Validation(
            "用户名和密码不能为空".to_string(),
        ));
    }

    let username_match = constant_time_eq(payload.username.as_bytes(), username.as_bytes());
    let password_match = bcrypt::verify(&payload.password, &password).unwrap_or(false);

    if !username_match || !password_match {
        tracing::warn!("登录失败: 用户名='{}'", payload.username);
        return Err(crate::error::WebError::Auth(
            "无效的用户名或密码".to_string(),
        ));
    }

    tracing::info!("用户 '{}' 登录成功", payload.username);

    let claims = security::Claims::new(payload.username, jwt_expiry_hours);
    let jwt_manager = security::JwtManager::new(&jwt_secret);
    let token = jwt_manager.generate_token(&claims)?;

    Ok(Json(LoginResponse {
        token,
        token_type: "Bearer".to_string(),
        expires_in: (jwt_expiry_hours * 3600),
    }))
}

/// 处理健康检查请求
pub async fn handle_health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
    })
}

/// 深度健康检查：验证 LLM、MCP、SQLite 等后端依赖
pub async fn handle_deep_health(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    // 并发检查各项依赖
    let (llm_ok, mcp_servers, db_ok) = tokio::join!(
        check_llm_connectivity(&state),
        check_mcp_health(&state),
        check_storage(&state),
    );

    let overall = llm_ok && db_ok;

    Json(serde_json::json!({
        "status": if overall { "healthy" } else { "degraded" },
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "checks": {
            "llm": { "ok": llm_ok, "detail": if llm_ok { "connected" } else { "unreachable" } },
            "mcp": {
                "ok": mcp_servers.iter().all(|(_, ok)| *ok),
                "servers": mcp_servers.iter().map(|(name, ok)| {
                    serde_json::json!({ "name": name, "ok": ok })
                }).collect::<Vec<_>>()
            },
            "storage": { "ok": db_ok, "detail": if db_ok { "available" } else { "unavailable" } }
        }
    }))
}

async fn check_llm_connectivity(state: &AppState) -> bool {
    let model_name = state
        .connection
        .agent
        .read(|a| a.config().get_model_name().to_string())
        .await;
    !model_name.is_empty()
}

async fn check_mcp_health(state: &AppState) -> Vec<(String, bool)> {
    let health = state.plugins.mcp_health.read().await;
    health
        .iter()
        .map(|(name, h)| (name.clone(), h.healthy))
        .collect()
}

async fn check_storage(state: &AppState) -> bool {
    state.storage.conversation_store.is_some()
}

/// 登录请求体
#[derive(serde::Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

/// 登录响应体
#[derive(serde::Serialize)]
pub struct LoginResponse {
    pub token: String,
    pub token_type: String,
    pub expires_in: u64,
}

/// 健康检查响应体
#[derive(serde::Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub timestamp: String,
}
