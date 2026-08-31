# Provider Architecture

Providers store connection and authentication details. Configured models store
the selected wire protocol, input modalities, and context parameters. The
application keeps provider and model configuration separate so one provider can
serve multiple models.

## Protocols and Capabilities

EKO supports Chat Completions, Responses, and Anthropic-compatible protocols.
Plain text is the default input modality; image, audio, and video are enabled
when the configured model advertises them. Provider adapters translate the
request and response wire format, while the application preserves the common
model contract.

Thinking profiles are normalized to the provider's supported levels, such as
`none`, `low`, `high`, and `max`. The central model-scope resolver owns this
translation instead of each surface inventing its own mapping.

## Configuration Ownership

GUI, TUI, CLI, JSONL, and channels share `model_providers`,
`configured_models`, and `default_model_id`. A user-supplied API key has
priority over a provider environment variable. Configuration updates return
typed receipts and are observed by the shared runtime rather than by a
surface-local cache.

`configured_models` owns model protocol, modalities, sampling, and context;
`model_providers` owns endpoint and credentials. The top-level `model` section
contains only `default_model_id`, and the single typed resolver never falls
back to mirrored or synthetic model fields.

Provider connections and model definitions are EKO product configuration. The
framework owns protocol primitives, request construction, streaming events,
and generic retry/cancellation contracts. EKO owns workspace policy,
credential precedence, model selection UI, and product-facing diagnostics.

The framework `LlmTimeouts` value is also the sole request/stream timeout
authority. EKO currently uses its typed defaults when constructing
`LlmConfig`; it does not read `ECHO_AGENT_STREAM_*`, maintain a timeout DTO, or
run a provider-specific SSE loop. Chat Completions, Responses, and Anthropic
therefore share identical first-chunk, idle, overall, cancellation, and UTF-8
framing behavior.

## Adding a Provider

An adapter must declare its protocol, supported modalities, context limits, and
thinking profile mapping. It must preserve typed errors and streaming
settlement, avoid leaking credentials into logs, and leave generic protocol
behavior in the framework. New configuration fields require synchronized
surface DTOs and documentation updates.

The canonical sources for provider-specific endpoints and model capabilities
are maintained in the application configuration reference. This page records
the ownership boundary, not a second provider catalog.
