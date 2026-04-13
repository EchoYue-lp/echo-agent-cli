# Web CLI

基于 echo-agent 框架的 Web 终端服务。

## 功能

- 阻塞式对话 API
- WebSocket 流式对话
- 工具管理 API
- MCP 服务端管理 API
- 配置管理 API
- 人工介入支持

## 快速开始

1. 复制配置文件：
```bash
cp .env.example .env
```

2. 编辑 `.env` 文件，配置模型和服务地址。

3. 运行服务：
```bash
cargo run --package web-cli
```

4. 访问 API：
- 健康检查: http://localhost:3000/api/session
- WebSocket: ws://localhost:3000/ws/chat

## API 端点

### 对话
- `POST /api/chat` - 阻塞式对话
- `WS /ws/chat` - 流式对话

### 会话
- `GET /api/session` - 获取会话状态
- `POST /api/session/reset` - 重置会话

### 工具
- `GET /api/tools` - 列出工具
- `GET /api/tools/{name}` - 获取工具详情
- `POST /api/tools/{name}/enable` - 启用工具
- `POST /api/tools/{name}/disable` - 禁用工具

### MCP
- `GET /api/mcp` - 列出 MCP 服务端
- `POST /api/mcp/connect` - 连接 MCP 服务端
- `GET /api/mcp/{name}` - 获取 MCP 服务端详情

### 配置
- `GET /api/config` - 获取配置
- `PUT /api/config` - 更新配置

## WebSocket 消息格式

### 客户端 -> 服务端

```json
// 发送消息
{"type": "message", "data": "你好"}

// 审批响应
{"type": "approval_response", "request_id": "xxx", "approved": true, "reason": null}

// 输入响应
{"type": "input_response", "request_id": "xxx", "text": "用户输入"}

// 取消
{"type": "cancel"}
```

### 服务端 -> 客户端

```json
// Token 片段
{"type": "token", "data": "你"}

// 工具开始
{"type": "tool_start", "name": "calculator", "args": {}}

// 工具结果
{"type": "tool_result", "name": "calculator", "result": "42", "success": true}

// 最终答案
{"type": "final_answer", "data": "答案是 42"}

// 错误
{"type": "error", "message": "错误信息"}

// 审批请求
{"type": "approval_request", "request_id": "xxx", "tool_name": "shell", "args": {}, "prompt": "需要审批"}

// 输入请求
{"type": "input_request", "request_id": "xxx", "prompt": "请输入"}
```