# EKO Getting Started

## Prerequisites

- Rust 1.95 or newer
- Node.js 20.19+, 22.13+, or 24+ for GUI development and packaging
- Tauri 2 system dependencies for the target platform when using GUI
- At least one usable LLM provider and model

## Install Dependencies

```bash
cd echo-agent-cli
cargo fetch

cd web-frontend
npm install
```

TUI and JSONL users can skip frontend dependencies.

## Configure a Model

Create a provider and model in the GUI, or prepare `./eko.yaml` or
`~/.eko/config.yaml`:

```yaml
model:
  default_model_id: "my-provider:my-model"

model_providers:
  my-provider:
    name: "My Provider"
    base_url: "https://api.example.com/v1"
    api_key_env: "MY_PROVIDER_API_KEY"
    requires_api_key: true
    default_api_protocol: "responses"

configured_models:
  - id: "my-provider:my-model"
    display_name: "My Model"
    provider: "my-provider"
    model: "my-model"
    api_protocol: "responses"
    input_modalities: ["text"]
    enabled: true
```

```bash
export MY_PROVIDER_API_KEY="..."
```

See the [configuration guide](./configuration.md) for all fields.

## Run TUI

```bash
cargo run --bin echo-agent-cli
cargo run --bin echo-agent-cli -- --project /path/to/project
cargo run --bin echo-agent-cli -- --model my-provider:my-model
cargo run --bin echo-agent-cli -- --continue
cargo run --bin echo-agent-cli -- --resume <conversation-id>
```

TUI is a complete Agent surface with TaskRun, Subagent, HITL, MCP, Browser,
Plugin, Skill, memory, and attachments. Use `/help` for commands registered by
the current build.

## Run GUI

```bash
cargo gui-dev
cargo tauri dev -- --no-default-features --features gui --bin echo-agent-tauri
cargo gui-bundle
```

The Tauri bundle, rather than a raw binary, contains frontend resources and
platform metadata.

## JSONL

```bash
cargo run --bin echo-agent-cli -- --jsonl "Inspect the current project and summarize it"
```

stdout contains one canonical chat envelope per line; logs go to stderr or
files for script-friendly consumption.

## MCP, Browser, and Data

MCP defaults to `~/.eko/mcp.json`; precedence is `--mcp-config`, YAML
`mcp.config_path`, `MCP_CONFIG_PATH`, then the default file. Managed Browser
uses `@playwright/mcp` and requires Node/npm/npx. EKO stores conversations,
tasks, memory, artifacts, and traces under `~/.eko/` or the `EKO_DATA_DIR`
override. The application does not use SQLite.

## Next Steps

- [Feature reference](./features.md)
- [Architecture overview](./architecture/overview.md)
- [Skill operations](./operations/skill-sync.md)
