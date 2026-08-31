# ADR 0029: Framework-Native Agent and Model Configuration

## Status

Accepted

## Context

`EkoConfig` stored an `AgentYamlConfig` with the same fields as framework
`AgentSettings`, then copied every field into a new framework value. Its
top-level `model` section also stored provider, model, credentials, endpoint,
protocol, sampling values, and context window beside the canonical
`configured_models` and `model_providers` collections. Selecting a default
model rewrote these mirror fields, and runtime resolution could synthesize a
model from them when no configured model existed.

The duplicate values made configuration ordering implicit and allowed stale
credentials or model parameters to remain authoritative after the selected
model changed. They also violated the framework-native value rule established
by framework ADR 0021 and EKO ADR 0027.

## Decision

1. `EkoConfig.agent` stores framework `AgentSettings` directly. EKO supplies
   product-specific defaults through `EkoConfig::default`; partial serialized
   settings are structurally merged onto those defaults before deserializing
   the same framework type. EKO does not define a format-named or
   framework-shaped Agent settings type.
2. The top-level `model` value is `ModelSelectionConfig` and contains only
   `default_model_id`; unknown mirror fields are rejected during deserialization.
3. `ConfiguredModel` is the sole owner of model id, provider reference,
   protocol, modalities, sampling parameters, and context window.
   `ModelProviderConfig` is the sole owner of endpoint and credentials.
4. `resolve_runtime_model(config, selector)` is the only runtime resolver. It
   returns `Result<ModelRuntimeConfig, ModelSelectionError>` and never creates
   a synthetic model from missing or stale fields.
5. `FrameworkConfig` is built from the selected `ConfiguredModel` plus the
   existing framework `AgentSettings`. Secrets remain in `LlmConfig` creation
   and are not copied into `FrameworkConfig`.
6. Full-config model updates modify the selected `ConfiguredModel` directly.
   Selecting or deleting a default changes only `default_model_id`.
7. Runtime mirror fields are deleted from the tracked schema and maintained
   `config/eko.example.yaml`. Ignored/local configuration files remain
   user-owned and are not modified by repository cleanup.

## Alternatives Considered

1. Rename `AgentYamlConfig` but retain field copying. Rejected because it keeps
   a second framework-shaped value.
2. Keep model mirrors as a cache. Rejected because ordinary config fields have
   no generation or invalidation contract and were already being read as
   fallback authority.
3. Return an empty/synthetic runtime model when unconfigured. Rejected because
   callers must distinguish not-configured, disabled, unknown, and ambiguous
   selection before constructing a provider client.

## Consequences

- Agent configuration crosses the application/framework boundary without a
  conversion adapter.
- Model selection, provider credentials, and runtime validation have one
  explicit source each.
- GUI, TUI, CLI/JSONL, channels, Cron, and future pooled Agents consume the same
  typed resolver and error contract.
- The YAML schema changes during development; no compatibility mirror remains.
