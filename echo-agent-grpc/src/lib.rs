//! gRPC/HTTP API for Echo Agent
//!
//! Provides language-agnostic API bindings for the Echo Agent framework.

use echo_agent::agent::Agent;
use futures::Stream;
use std::pin::Pin;
use tonic::{Request, Response, Status};

pub mod agent {
    tonic::include_proto!("echoagent");
}

use agent::{
    agent_service_server::{AgentService, AgentServiceServer},
    ChatStreamChunk, ChatStreamRequest, ExecuteRequest, ExecuteResponse, StatusRequest,
    StatusResponse,
};

/// gRPC Agent 服务实现
pub struct AgentGrpcService {
    // 这里可以持有 AgentHandle 或其他状态
}

impl AgentGrpcService {
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for AgentGrpcService {
    fn default() -> Self {
        Self::new()
    }
}

#[tonic::async_trait]
impl AgentService for AgentGrpcService {
    async fn execute(
        &self,
        request: Request<ExecuteRequest>,
    ) -> Result<Response<ExecuteResponse>, Status> {
        let req = request.into_inner();
        // 这里应该调用实际的 Agent 执行逻辑
        // 简化实现：直接返回模拟结果
        let response = ExecuteResponse {
            result: format!("Executed task: {}", req.task),
            iterations: 1,
            success: true,
        };
        Ok(Response::new(response))
    }

    type ChatStreamStream =
        Pin<Box<dyn Stream<Item = Result<ChatStreamChunk, Status>> + Send + 'static>>;

    async fn chat_stream(
        &self,
        request: Request<ChatStreamRequest>,
    ) -> Result<Response<Self::ChatStreamStream>, Status> {
        let _req = request.into_inner();
        // 创建流式响应
        let stream = Box::pin(async_stream::stream! {
            // 模拟流式输出
            yield Ok(ChatStreamChunk {
                event: Some(agent::chat_stream_chunk::Event::Token(
                    agent::TokenChunk {
                        data: "Hello ".to_string(),
                    },
                )),
            });
            yield Ok(ChatStreamChunk {
                event: Some(agent::chat_stream_chunk::Event::Token(
                    agent::TokenChunk {
                        data: "from gRPC!".to_string(),
                    },
                )),
            });
            yield Ok(ChatStreamChunk {
                event: Some(agent::chat_stream_chunk::Event::FinalAnswer(
                    agent::FinalAnswerEvent {
                        data: "Hello from gRPC!".to_string(),
                    },
                )),
            });
        });

        Ok(Response::new(stream))
    }

    async fn get_status(
        &self,
        _request: Request<StatusRequest>,
    ) -> Result<Response<StatusResponse>, Status> {
        let response = StatusResponse {
            version: env!("CARGO_PKG_VERSION").to_string(),
            status: "running".to_string(),
            uptime_seconds: 0,
        };
        Ok(Response::new(response))
    }
}

/// 启动 gRPC 服务
pub async fn serve(addr: &str) -> Result<(), Box<dyn std::error::Error>> {
    let service = AgentGrpcService::new();
    let grpc = tonic::transport::Server::builder()
        .add_service(AgentServiceServer::new(service))
        .serve(addr.parse()?);

    tracing::info!("gRPC server listening on {}", addr);
    grpc.await?;
    Ok(())
}

// 导出类型
pub use agent::*;
