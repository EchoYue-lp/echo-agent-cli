# Skill Management and Upstream Sync

## Overview

EKO SkillsHub installs and updates Skills from Git remotes. Built-in Skills are
updated with the product; user-installed Skills use explicit upstream sync.
The loader recursively scans `skills/**/SKILL.md`, and category depth does not
change Skill identity.

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

## Durable Desired State

`~/.eko/enabled-skills.json` is the sole durable enablement fact. Version 2
stores the Skill map, baseline flags, desired and settled generations,
canonical content identity, bounded operation identities, and repair debt.
`ExtensionControlService` shares this state across GUI, TUI, CLI/JSONL, and
channels.

Accepted mutations follow this sequence:

```text
validate request + capture workspace generation
  -> canonicalize desired content
  -> detect duplicate/conflicting operation identity
  -> stage JSON beside enabled-skills.json
  -> atomic replace and sync parent directory
  -> publish committed desired generation
  -> fan out through specialist owners
  -> return Settled or Degraded
```

Durable write failures are pre-commit errors. Once the file is committed,
fanout failures return committed-but-degraded typed receipts and bounded repair
debt; memory rollback cannot pretend the commit did not happen.

## Idempotency and Repair

The same operation and command identity returns the original or reconstructed
receipt. The same operation with a different command identity is a typed
conflict. Identical content does not advance generation but retries targets that
have not converged. Older desired, workspace, or specialist generations cannot
overwrite newer ones. Repair is retried on startup, workspace load, and before
the next mutation; it is not a second store.

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
not overwritten without explicit `--force`. A content change advances desired
generation even if enablement is unchanged.

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
