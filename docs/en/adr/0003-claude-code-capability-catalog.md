# ADR 0003: Claude Code Capability Catalog

> Status: Reference Snapshot (historical snapshot, not retro-edited as the
> catalog evolves; the current EKO skill catalog state is
> [ADR 0033](0033-skill-catalog-contraction-and-official-frontmatter.md))
>
> Date: 2026-08-24

## Context

The supplied Claude Code session listed built-in tools, Subagent types, Skills,
and plugins. Availability depends on account, version, project configuration,
experiments, and extensions.

## Decision

Retain the catalog as an industry reference, not as a frozen compatibility
contract. EKO may use the stable ideas of explicit Subagent targets, shared
Skills, bounded tool results, and durable task identity. EKO owns its own
policy, receipts, and application adapters.

## Consequences

Changes in Claude Code counts or names do not require EKO schema changes. Any
adopted capability must reuse the single framework/app authority and preserve
TUI, GUI, CLI/JSONL, and channel parity.
