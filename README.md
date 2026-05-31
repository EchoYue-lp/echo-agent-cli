# Echo Agent CLI

Multi-mode AI agent CLI with Web UI.

## Quick Start

```bash
# Web mode (default)
echo-agent-cli

# CLI REPL
echo-agent-cli --cli

# Both web + CLI
echo-agent-cli --web --cli
```

## Modes

| Mode | Description |
|------|-------------|
| Web (default) | HTTP API + WebSocket on port 3000 |
| CLI REPL | Interactive REPL (`--cli`) |
| GUI | Tauri desktop app (`cargo tauri dev`) |

## Configuration

Create `echo-agent.yaml` or set env vars. See `echo-agent.yaml.example`.

## REST API

| Endpoint | Method | Description |
|----------|--------|-------------|
| `GET /api/health` | Health check |
| `POST /api/chat` | Chat |
| `GET /api/config` | Get config |
| `WS /ws` | WebSocket chat |

See full API docs in [API.md](API.md).