# External Skill Management and Upstream Sync

## Content Boundary

EKO does not bundle Skills or load them from the source tree, Tauri resources,
or a default catalog. Product runtimes consume only independently installed
Skills under `~/.eko/skills/` and Skills supplied by plugin generations.
Repository `.agents/skills/` files guide EKO development and are not product
runtime content.

SkillsHub owns installation, enablement, removal, upstream records, and surface
projection for independent Skills. PluginRuntime owns complete plugin packages.
Both reuse the framework `SkillDocument`, manifest parser, and validators
without introducing another frontmatter parser or activation runtime.

`/skills install` accepts a single-Skill repository or an Agent Plugins 1.0
package with a root `plugin.json`. For a plugin package, it preflights the full
`skills/` face, then atomically stages, installs, and enables every Skill.
Packages containing `mcp.json` are not handled by the Skill installation entry
point. An existing target without an ownership marker is never overwritten and
must be explicitly uninstalled first.

## enabled-skills.json

`~/.eko/enabled-skills.json` is the sole durable enablement fact for external
Skills. The current version 3 schema is:

```json
{
  "version": 3,
  "skills": {
    "paper-reader": { "enabled": true },
    "my-local-skill": { "enabled": false }
  }
}
```

- `enabled` determines whether an installed external Skill enters an Agent runtime.
- A newly installed Skill is enabled; first startup creates no default entries.
- Obsolete category, baseline, generation, operation identity, content identity,
  and repair-debt fields are ignored while existing `enabled` choices remain.
- Corrupt or unreadable configuration falls back to an empty set with a warning.

Every mutation uses the same Extension authority:

```text
lock extension mutation
  -> read enabled-skills.json
  -> validate the external Skill and mutate its entry
  -> atomic write
  -> reconcile user Skills to every runtime target (plugin generations are separate)
  -> return Settled or Degraded
```

GUI, TUI, CLI/JSONL, and channels share this service. A runtime-target failure
does not roll back an already written file; the next Skill operation, app start,
or workspace load converges again. Typed receipts keep artifact results and
per-target runtime settlement distinct without retaining exact replay state.

## SKILL.md Format

EKO accepts only official agentskills.io frontmatter, with no private extension
namespace:

```yaml
---
name: my-skill
description: >-
  Describe what this Skill does and when it should be used.
license: MIT
compatibility: Requires poppler
allowed-tools: shell read_file
metadata:
  category: research
  author: author-name
---
# Full instructions
```

- `name` is 1-64 characters of kebab-case and matches the directory name.
- `description` is at most 1,024 characters and carries routing guidance.
- `allowed-tools` is a space-separated string, not a YAML list.
- `metadata` is a string-to-string map.
- Skill files do not define Hooks; Hooks belong to application/plugin configuration.
- Framework `validate_skill_dir` is the sole directory-validation authority.

## Upstream Sync

A Git-installed Skill carries `.eko-skill-source.json` with repository URL,
exact subdirectory, revision, content hash, and sync time. This record is not
part of `SKILL.md` and does not affect loading.

```bash
/skills check-updates
/skills check-updates paper-reader
/skills sync paper-reader
/skills sync all
/skills sync paper-reader --force
```

Sync clones into a same-filesystem staging directory, validates `SKILL.md`,
hashes the content, and atomically replaces the installed Skill. Local changes
are not overwritten without `--force`. Git sources must use HTTPS. Sync is
explicit, uses the user's credentials, times out after 120 seconds, and never
pulls automatically in the background.

## Dependency Declarations

Python Skills can use PEP 723 inline dependencies:

```python
#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["defusedxml", "lxml"]
# ///
```

System binary or Python package dependencies can use string-valued metadata:

```yaml
metadata:
  requires-binaries: "soffice, pdftoppm"
  requires-python-packages: "defusedxml, lxml"
```

EKO detects and reports dependencies but does not install them automatically.
