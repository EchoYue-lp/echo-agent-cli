# ADR 0046: Fail Closed on EKO Plugin Generation Failure

## Status

Accepted

## Context

The latest framework [ADR 0012](https://github.com/EchoYue-lp/echo-agent/blob/main/docs/adr/0012-immutable-plugin-preparation.md) permits a prepared Plugin set with isolated component errors and forbids republishing a withdrawn generation to the same Agent. Its [ADR 0060](https://github.com/EchoYue-lp/echo-agent/blob/main/docs/adr/0060-plugin-lifecycle-reconcile-settlement.md) retains cleanup debt instead of overlapping callbacks. EKO's previous reload path attempted to restore the old prepared generation after publishing a newer candidate, which now fails its generation fence and can leave product projections claiming the old Plugin is active.

## Options

1. Republish the old immutable generation. Rejected: the framework correctly fences it as stale.
2. Extend the framework to synthesize a new generation from old package bytes. Rejected for this application task: it expands the generic contract and prolongs ambiguity about already executed lifecycle effects.
3. EKO rejects any preparation Error diagnostic and retires an activated-but-failed candidate into a new empty Plugin generation (chosen).

## Decision

- The framework owns immutable preparation, per-Agent publication identity, component cleanup receipts, and callback reconciliation. EKO owns the stricter all-components-required policy and coordinated product projections.
- A preparation Error diagnostic rejects the whole EKO reload, even when the framework marks healthy sibling components applicable. The diagnostic remains visible to the caller.
- If callback activation fails after candidate publication, EKO deactivates/shuts down callbacks, removes candidate monitors, unwires the exact framework receipt, unloads application components, releases MCP claims, retires cached AgentPool components with a fresh generation number, clears LSP/theme/style projections, and persists the retired preference state. Any cleanup failure is reported; it is not described as a successful rollback.
- User/project Skills outside the Plugin generation remain visible. Retired Plugin descriptors and Subagents are absent from primary, existing pooled, and future pooled Agents.

## Consequences

- A failed reload can leave the Plugin set empty until the user fixes the package and reloads. This is preferable to claiming that stale callbacks or descriptors were restored.
- The application is in development; no legacy generation compatibility path is retained.
- Framework examples and `echo-website` do not consume EKO's reload policy and need no update.
