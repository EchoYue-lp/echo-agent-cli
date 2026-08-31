# EKO Configuration Guide

EKO owns `EkoConfig` and the `~/.eko` data root in app-core. Runtime bootstrap
selects the provider-neutral fields used by the framework's `FrameworkConfig`,
typed `PermissionMode`, and explicit paths; the framework does not discover EKO
configuration files.

## Location and Priority

The main configuration is resolved in this order:

1. `--config <path>`
2. `EKO_CONFIG`
3. `./eko.yaml`
4. `~/.eko/config.yaml`

`EKO_DATA_DIR` overrides the complete product data root. GUI, TUI, CLI/JSONL,
and channels read the same `EkoConfig`; secret-bearing files use restrictive
local permissions.

## Model Configuration

Provider entries hold shared connection and authentication settings. Configured
models select their own protocol and input modalities:

```yaml
model:
  default_model_id: "gateway:main"

model_providers:
  gateway:
    name: "Team Gateway"
    base_url: "https://gateway.example/v1"
    api_key_env: "TEAM_LLM_KEY"
    requires_api_key: true
    default_api_protocol: "responses"

configured_models:
  - id: "gateway:main"
    display_name: "Main Model"
    provider: "gateway"
    model: "main-model"
    api_protocol: "responses"
    input_modalities: ["text", "image"]
    enabled: true
    max_tokens: 8192
    temperature: 0.7
    context_window: 396000
```

Supported protocols are `chat_completions`, `responses`, and `anthropic`.
Supported modalities are `text`, `image`, `audio`, and `video`; `text` remains
enabled for every model. Runtime selection uses the model's `api_protocol`, not
the provider default. See [Provider architecture](./architecture/providers.md).
The top-level `model` section owns only `default_model_id`; connection,
credentials, protocol, sampling, and context fields are never mirrored there;
unknown fields in this section are rejected.

The `agent` section deserializes directly into framework `AgentSettings`. EKO
supplies product defaults but does not maintain a parallel Agent settings DTO.

## Agent and Compression

| Field | Meaning |
| --- | --- |
| `max_iterations` | Maximum ReAct iterations for one turn; `0` means no product cap |
| `tool_timeout_ms` | Timeout for one tool call |
| `max_tool_output_tokens` | Context budget for one tool result; full output can be persisted as an artifact |
| `token_limit` | Explicit compression threshold; `0` uses model/application policy |
| `compress_strategy` | `summary`, `sliding`, or `adaptive` |
| `compress_window` | Number of recent messages retained during compression |
| `subagent_timeout_secs` | Default Subagent timeout; `0` means no timeout |

EKO layered workspace memory is file-backed. The application does not enable
SQLite stores.

## MCP, Browser, Hooks, and Webhooks

MCP configuration priority is `--mcp-config`, YAML `mcp.config_path`,
`MCP_CONFIG_PATH`, then `~/.eko/mcp.json`. User-configured MCP entries take
precedence over Plugin entries with the same name. Configuration mutations use
`ExtensionControlService` and `McpConfigRuntime` durable settlement.

Browser runtime can manage Chromium or a Chrome extension backend. Common
variables include `EKO_BROWSER_ENABLED`, `EKO_BROWSER_HEADLESS`,
`EKO_BROWSER_PROFILE_DIR`, `EKO_BROWSER_OUTPUT_DIR`, and
`EKO_BROWSER_EXTENSION_TOKEN`.

Hooks merge from `eko.yaml`, `~/.eko/hooks.yaml`, and the project `.eko` file.
Webhook secrets are used for HMAC-SHA256 signatures and are excluded from
ordinary event logs.

## Project Instructions

EKO reads the repository-standard root-to-working-directory chain of
`AGENTS.md` and `AGENTS.override.md`, then combines its own files:

- `~/.eko/user.md` for user-level preferences;
- `<project>/.eko/learned-rules.md` for RulePromoter output;
- `<cwd>/.eko/local.md` for machine/directory-specific instructions.

`.eko/AGENTS.md` is not a product instruction source and is never renamed into
`learned-rules.md`. See [ADR 0028](./adr/0028-current-product-schema-authority.md).

## Plugins and Skills

Plugins live in user or project plugin roots. User Skills live in
`~/.eko/skills/`; desired enablement is stored in
`~/.eko/enabled-skills.json`. Install, enable, disable, and upstream sync use
durable-first typed receipts. See [Skill operations](./operations/skill-sync.md).

## Channels and Environment

QQ and Feishu use the `channels` section and corresponding environment
variables. Channels are a complete Agent surface and share the same chat
driver, TaskRuntime, HITL, and memory authority as GUI and TUI.

| Variable | Purpose |
| --- | --- |
| `EKO_DATA_DIR` | Override the `~/.eko` data root |
| `EKO_CONFIG` | Main configuration file |
| `MODEL_NAME` | Default value for CLI `--model` |
| `MCP_CONFIG_PATH` | MCP configuration file |
| `EKO_UV_PATH` | `uv` path for analytics runtime |
| `RUST_LOG` | Rust log filter |
| `HTTP_PROXY` / `HTTPS_PROXY` | HTTP proxy |
