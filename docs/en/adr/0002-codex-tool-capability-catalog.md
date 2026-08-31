# ADR 0002: Codex Tool Capability Catalog

> Status: Proposed
>
> Date: 2026-08-24

## Context

The Codex app exposes layered host, collaboration, filesystem, execution, and
research capabilities. Their availability changes with host version, mode,
plugins, MCP, and permission policy, so a snapshot must not become an EKO API
promise.

## Decision

Keep the catalog as a reference snapshot. EKO may adopt stable product
semantics such as explicit tool identity, bounded results, exact lifecycle
receipts, and user-visible approval, but each capability must be implemented by
the existing framework or app-core owner. Do not copy a host-only tool list or
create surface-local registries.

## Consequences

Tool discovery remains dynamic and capability-driven. EKO documentation names
stable contracts and links to implementation evidence; it does not freeze
provider-specific names, counts, or hidden host behavior.
