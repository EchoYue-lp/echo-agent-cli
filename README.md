# Echo Agent CLI

Multi-mode CLI + Web server for the [Echo Agent](https://github.com/EchoYue-lp/echo-agent) AI framework.

## Quick Start

```bash
# Install
cargo install echo-agent-cli

# Default model is qwen-plus; set one of these keys
export DASHSCOPE_API_KEY=sk-...
# or
export QWEN_API_KEY=sk-...

# Web mode (default — starts a single-user local HTTP + WebSocket server)
echo-agent-cli

# CLI REPL mode
echo-agent-cli --cli

# TUI mode
echo-agent-cli --tui

# Use OpenAI instead of the default Qwen provider
export OPENAI_API_KEY=sk-...
echo-agent-cli --cli --model openai:gpt-4o-mini

# Both Web + CLI simultaneously
echo-agent-cli --web --cli
```

## Modes

Echo Agent CLI is designed as a single-user local application. Web, CLI, and TUI modes share one local agent session and are not intended for multi-tenant hosting.

| Mode | Flag | Description |
|------|------|-------------|
| Web | `--web` (default) | HTTP REST API + WebSocket on port 3000 |
| CLI | `--cli` | Interactive REPL with rich output |
| TUI | `--tui` | Terminal UI with chat/tools/context panels |
| Channels | `--channels` | QQ Bot + Feishu IM integration |

## CLI Options

```
echo-agent-cli [OPTIONS]

  --web              Start web server
  --cli              Start CLI REPL
  --tui              Start terminal UI
  --channels         Start IM channels (QQ/Feishu)
  --port <PORT>      Web server port [default: 3000]
  --host <HOST>      Web server host [default: 127.0.0.1]
  --model <MODEL>    Model name override
  --config <PATH>    Config file path
  --mcp-config <PATH> MCP config file path
  --project <DIR>    Project directory for context-aware mode
  --mode <MODE>      Agent mode (general/code/data/customer-service)
  --system-prompt <S> System prompt override
  --no-color         Disable colored output
```

## Configuration

Create `echo-agent.yaml` in the current directory or `~/.echo-agent/config.yaml` for application/runtime settings:

```yaml
model:
  # Default provider is Qwen/DashScope. Use DASHSCOPE_API_KEY or QWEN_API_KEY.
  name: qwen-plus
  temperature: 0.7
  max_tokens: 4096

agent:
  name: echo
  system_prompt: "You are a helpful assistant."
  max_iterations: 20
  enable_tools: true
  enable_memory: true
  token_limit: 32000

server:
  host: 127.0.0.1
  port: 3000

logging:
  level: info
```

For OpenAI-compatible usage, set `OPENAI_API_KEY` and configure:

```yaml
model:
  name: openai:gpt-4o-mini
```

Optional model registry configuration for aliases/custom endpoints belongs in `echo-agent-models.yaml` or `~/.echo-agent/models.yaml`:

```yaml
models:
  qwen3-max:
    provider: qwen
    api_key: ${DASHSCOPE_API_KEY}
```

`echo-agent.yaml` is the app config; `echo-agent-models.yaml` is the provider/model registry. Keeping them separate avoids app config being parsed as a model registry.

## REST API

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/chat` | POST | Blocking chat |
| `/api/history` | GET | Conversation history |
| `/api/session` | GET | Session state |
| `/api/session/reset` | POST | Reset session |
| `/api/session/checkpoint` | POST | Create state snapshot |
| `/api/tools` | GET | List tools |
| `/api/tools/:name` | GET/POST | Tool detail / enable/disable |
| `/api/memory` | GET/POST | Memory CRUD |
| `/api/memory/search` | POST | Search memory |
| `/api/skills` | GET | List skills |
| `/api/compress` | POST | Trigger context compression |
| `/api/extract` | POST | Structured extraction |
| `/api/mcp` | GET | List MCP servers |
| `/api/mcp/connect` | POST | Connect MCP server |
| `/api/mcp/health` | GET | MCP health status |
| `/api/config` | GET/PUT | Agent config |
| `/api/conversations` | GET/POST | Conversation persistence |
| `/api/sessions/search` | GET | Full-text session search |
| `/api/scheduler/tasks` | GET/POST | Cron task management |
| `/api/audit/logs` | GET/DELETE | Audit logs |
| `/api/permissions/mode` | GET/PUT | Permission mode |
| `/api/webhooks` | GET/POST | Webhook management |
| `/api/skills-hub` | GET | Skill marketplace |
| `/api/workflow` | GET/POST | Workflow management |
| `/api/sandbox/status` | GET | Sandbox status |
| `/api/health` | GET | Health check |
| `/api/health/deep` | GET | Deep health (LLM + MCP + DB) |
| `/ws/chat` | WS | Streaming chat |

## WebSocket Messages

### Client -> Server
```json
{"type": "message", "data": "Hello", "attachments": []}
{"type": "approval_response", "request_id": "...", "approved": true}
{"type": "input_response", "request_id": "...", "text": "..."}
{"type": "cancel"}
```

### Server -> Client
```json
{"type": "token", "data": "..."}
{"type": "thinking_start"}
{"type": "thinking_end", "prompt_tokens": 100, "completion_tokens": 50}
{"type": "tool_start", "name": "calculator", "args": {}}
{"type": "tool_result", "name": "calculator", "result": "42", "success": true}
{"type": "final_answer", "data": "..."}
{"type": "error", "message": "..."}
{"type": "chart", "spec": {}}
```

## License

MIT
