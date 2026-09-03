# Skill Management and Upstream Sync

## Overview

EKO SkillsHub installs and updates Skills from Git remotes. Built-in Skills are
updated with the product; user-installed Skills use explicit upstream sync.
The loader recursively scans `skills/**/SKILL.md`, and category depth does not
change Skill identity.

`/skills install` also recognizes an Agent Plugins 1.0 root `plugin.json`. It
reuses the framework manifest and Skill validators, preflights every Skill,
then atomically installs and enables the complete `skills/` face as one staged
directory. Each Git-installed Skill records its exact `skills/<name>` subdir
for later independent sync. Packages containing `mcp.json` are not yet handled
by the Skill installation entry point. An existing plugin target directory is
never overwritten without an ownership marker; users must explicitly uninstall
it first.

Built-in and user Skills share framework `SkillDocument` parsing. SkillsHub
owns installation, enablement, upstream records, and surface projection; it
does not own another frontmatter parser or activation runtime.

## Official SKILL.md Format

EKO accepts only the official agentskills.io frontmatter — no private
extension namespace:

```yaml
---
name: my-skill                  # required: kebab-case, 1-64 chars, equals dir name
description: >-                 # required: ≤1024 chars; what it does and when
  One-line description with routing keywords.   # routing is description-driven
license: MIT                    # optional
compatibility: Requires poppler # optional: environment needs, ≤500 chars
allowed-tools: shell read_file  # optional: space-separated string (not a list); omit if empty
metadata:                       # optional: string → string mapping
  category: methodology
  author: author-name
---
# Body — full instructions
```

- Skill files do not define Hooks. Hooks are application/plugin configuration;
  a frontmatter `hooks:` block is rejected as a non-standard field.
- "Discoverable in the catalog" and "runtime active" are different states:
  SkillsHub lists every shipped Skill, but only entries allowed by
  `enabled-skills.json` register descriptors and LLM routing candidates;
  Hooks remain application/plugin configuration.
- Validation gate: framework `validate_skill_dir` (the in-process equivalent
  of `skills-ref validate`); `cargo test -p echo-agent-app-core --lib
  skills_hub::catalog_gate` walks `skills/` asserting zero violations and
  `BUILTIN_SKILL_NAMES` parity with disk.

| Type | Location | Update method |
| --- | --- | --- |
| Built-in | `<echo-agent-cli>/skills/<category>/` | Product release |
| User-installed | `~/.eko/skills/` | Explicit check and atomic staging sync |

## Enablement State

`~/.eko/enabled-skills.json` is the sole durable enablement fact. Since the
2026-09 simplification (ADR 0036) it stores only the flat skill map
(`{category, enabled, baseline}`) written atomically; stale generation or
repair-debt fields from older files are ignored. Corrupt or unreadable files
fall back to the default active set (fail-open) with a warning log.

Every mutation (enable/disable/install/uninstall/sync/refresh) follows:

```text
lock extension mutation
  -> read enabled-skills.json
  -> mutate the entry
  -> atomic write
  -> reconcile all runtime targets (builtin dir + user/plugin skills)
  -> return Settled or Degraded
```

Within a crash window (file written, runtimes not yet reconciled) the next
skill operation or app start converges the state; no precise replay is kept.
Install receipts expose `installed_names` for either the single Skill or every
Skill installed and enabled from a plugin package.

## Upstream Sync

Git-installed Skills carry `.eko-skill-source.json` with repository URL,
subdirectory, revision, content hash, and sync time.

```bash
/skills check-updates
/skills check-updates paper-reader
/skills sync paper-reader
/skills sync all
/skills sync paper-reader --force
```

Sync clones into a same-filesystem staging directory, validates `SKILL.md`,
hashes content, and atomically replaces the installed Skill. Local changes are
not overwritten without explicit `--force`. The shared extension authority then
reconciles every runtime target and returns an immediate typed settlement.

## Local Constraints

Git sources must use HTTPS. Sync is explicit, uses the user's credentials, and
has a 120-second timeout. The local application does not automatically pull
from upstream.

## Skills with Dependencies

Python Skills can declare PEP 723 inline dependencies:

```python
#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["defusedxml", "lxml"]
# ///
```

System binaries and Python packages can be declared in `SKILL.md` metadata;
EKO probes and reports them but does not install them automatically.

All `metadata` values are strings. Instructions belong in the Markdown body,
and supporting files belong in the Skill directory; EKO does not use the old
top-level `version`, `author`, `tags`, `instructions`, or `resources` fields.
