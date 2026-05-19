//! 指标收集模块
//!
//! 提供应用性能指标收集和 Prometheus 端点。

use axum::response::IntoResponse;
use axum::http::StatusCode;
use metrics_exporter_prometheus::PrometheusBuilder;
use std::sync::OnceLock;
use metrics::histogram;

/// 指标全局状态
static METRICS_HANDLE: OnceLock<metrics_exporter_prometheus::PrometheusHandle> = OnceLock::new();

/// 指标标签
pub mod labels {
    /// API 端点标签
    pub const ENDPOINT: &str = "endpoint";
    /// HTTP 方法标签
    pub const METHOD: &str = "method";
    /// HTTP 状态码标签
    pub const STATUS_CODE: &str = "status_code";
    /// 错误类型标签
    pub const ERROR_TYPE: &str = "error_type";
}

/// 初始化指标收集器
pub fn init_metrics() -> Result<(), Box<dyn std::error::Error>> {
    if METRICS_HANDLE.get().is_some() {
        return Ok(());
    }

    let builder = PrometheusBuilder::new();
    let handle = builder
        .install_recorder()
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;

    // 忽略设置结果，如果已经设置则返回Ok
    let _ = METRICS_HANDLE.set(handle);

    // 注册自定义指标
    register_custom_metrics();

    Ok(())
}

/// 注册自定义指标
fn register_custom_metrics() {
    // HTTP 请求计数器
    metrics::describe_counter!(
        "http_requests_total",
        metrics::Unit::Count,
        "Total number of HTTP requests"
    );

    // HTTP 请求持续时间直方图
    metrics::describe_histogram!(
        "http_request_duration_seconds",
        metrics::Unit::Seconds,
        "HTTP request duration in seconds"
    );

    // HTTP 响应大小直方图
    metrics::describe_histogram!(
        "http_response_size_bytes",
        metrics::Unit::Bytes,
        "HTTP response size in bytes"
    );

    // 错误计数器
    metrics::describe_counter!(
        "errors_total",
        metrics::Unit::Count,
        "Total number of errors"
    );

    // 活动连接数
    metrics::describe_gauge!(
        "active_connections",
        metrics::Unit::Count,
        "Number of active connections"
    );

    // JWT 令牌验证计数器
    metrics::describe_counter!(
        "jwt_tokens_validated_total",
        metrics::Unit::Count,
        "Total number of JWT tokens validated"
    );

    // 速率限制触发计数器
    metrics::describe_counter!(
        "rate_limit_hits_total",
        metrics::Unit::Count,
        "Total number of rate limit hits"
    );

    // 活跃会话数
    metrics::describe_gauge!(
        "active_sessions",
        metrics::Unit::Count,
        "Number of active sessions"
    );

    // Agent 工具调用计数器
    metrics::describe_counter!(
        "agent_tool_calls_total",
        metrics::Unit::Count,
        "Total number of agent tool calls"
    );

    // 内存使用量
    metrics::describe_gauge!(
        "memory_usage_bytes",
        metrics::Unit::Bytes,
        "Memory usage in bytes"
    );
}

/// 记录 HTTP 请求指标
pub fn record_http_request(
    endpoint: &str,
    method: &str,
    status_code: u16,
    duration_seconds: f64,
    response_size_bytes: Option<u64>,
) {
    // 将字符串引用转换为 String 以解决生命周期问题
    let endpoint = endpoint.to_string();
    let method = method.to_string();
    let status_code_str = status_code.to_string();

    // 增加请求计数器
    metrics::counter!(
        "http_requests_total",
        labels::ENDPOINT => endpoint.clone(),
        labels::METHOD => method.clone(),
        labels::STATUS_CODE => status_code_str
    ).increment(1);

    // 记录请求持续时间
    histogram!("http_request_duration_seconds", labels::ENDPOINT => endpoint.clone(), labels::METHOD => method.clone())
        .record(duration_seconds);

    // 记录响应大小（如果有）
    if let Some(size) = response_size_bytes {
        histogram!("http_response_size_bytes", labels::ENDPOINT => endpoint, labels::METHOD => method)
            .record(size as f64);
    }
}

/// 记录错误指标
pub fn record_error(error_type: &str, endpoint: Option<&str>) {
    // 将字符串引用转换为 String 以解决生命周期问题
    let error_type = error_type.to_string();
    let endpoint = endpoint.map(|s| s.to_string());

    let labels = if let Some(ep) = endpoint {
        vec![
            (labels::ERROR_TYPE, error_type),
            (labels::ENDPOINT, ep),
        ]
    } else {
        vec![(labels::ERROR_TYPE, error_type)]
    };

    metrics::counter!("errors_total", &labels).increment(1);
}

/// 记录 JWT 令牌验证
pub fn record_jwt_validation(success: bool) {
    let status = if success { "success" } else { "failure" };
    metrics::counter!("jwt_tokens_validated_total", "status" => status).increment(1);
}

/// 记录速率限制触发
pub fn record_rate_limit_hit() {
    metrics::counter!("rate_limit_hits_total").increment(1);
}

/// 更新活动连接数
pub fn update_active_connections(count: usize) {
    metrics::gauge!("active_connections").set(count as f64);
}

/// 更新活跃会话数
pub fn update_active_sessions(count: usize) {
    metrics::gauge!("active_sessions").set(count as f64);
}

/// 更新内存使用量
pub fn update_memory_usage(bytes: u64) {
    metrics::gauge!("memory_usage_bytes").set(bytes as f64);
}

/// HTTP 请求指标中间件
///
/// 自动记录每个 HTTP 请求的耗时、状态码、端点和方法。
pub async fn metrics_middleware(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let start = std::time::Instant::now();
    let method = request.method().to_string();
    let uri = request.uri().path().to_string();

    let response = next.run(request).await;

    let duration = start.elapsed().as_secs_f64();
    let status_code = response.status().as_u16();

    record_http_request(&uri, &method, status_code, duration, None);

    response
}

/// 处理指标端点请求
pub async fn handle_metrics() -> impl IntoResponse {
    match METRICS_HANDLE.get() {
        Some(handle) => {
            let metrics_data = handle.render();
            (StatusCode::OK, metrics_data)
        }
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Metrics not initialized".to_string(),
        ),
    }
}

