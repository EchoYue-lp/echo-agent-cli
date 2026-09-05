---
name: plugin-creator
description: Create, scaffold, extend, and validate EKO Agent Plugins when the user asks for a plugin that packages skills, Subagents, hooks, MCP or LSP configuration, monitors, themes, output styles, or scripts.
allowed-tools: shell read_file apply_patch plugin_scaffold plugin_validate
metadata:
  category: methodology
  source: eko
  tags: plugin-authoring, plugin-validation
---

# Plugin Creator

Create EKO plugins against the protocol EKO actually loads. Do not copy another
host's private plugin layout.

## EKO Plugin Contract

An EKO plugin is an Agent Plugins 1.0 package with `plugin.json` at the plugin
root. In particular, do not create `.codex-plugin/plugin.json`.

```text
plugin-name/
|-- plugin.json
|-- skills/<skill-name>/SKILL.md
|-- agents/<name>.md
|-- hooks/hooks.yaml
|-- mcp.json
|-- lsp.yaml
|-- monitors.yaml
|-- themes/<name>.json
|-- output-styles/<name>.md
`-- scripts/
```

Components are optional and use these fixed locations. Create only the
components the request needs.

The root manifest starts with:

```json
{
  "$schema": "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json",
  "name": "plugin-name",
  "version": "0.1.0",
  "description": "What the plugin provides",
  "license": "MIT",
  "displayName": "Plugin Name",
  "defaultEnabled": true
}
```

Plugin names are 1-64 lowercase ASCII letters, digits, hyphens, or periods;
they start and end with an alphanumeric character and contain neither `--` nor
`..`. Use `Subagent` in EKO-owned names and prose, never `Worker`.

## Workflow

1. Inspect the destination and installed plugins before creating anything.
   Refuse to overwrite a non-empty target unless the user explicitly asks to
   update that same plugin.
2. Use `plugin_scaffold` for the base package. If its schema is not visible,
   find it with `tool_search`. The tool delegates to the same PluginRuntime
   scaffold as `/plugins init <directory> <name>`.
3. Remove example components the requested plugin does not need, then implement
   the requested components. A skill component follows the Agent Skills
   standard; use `skill-creator` guidance when authoring one.
4. Keep configuration values in the manifest `config` map. Mark secrets with
   `sensitive: true`; never put secret values in the manifest, logs, examples,
   or generated output.
5. Run `plugin_validate` so the existing PluginRuntime validator checks the
   manifest and all application components. Fix every reported error. The
   equivalent user command is `/plugins validate <directory>`; do not
   substitute a second, handwritten validator.
6. Re-read the final package, report its path and capabilities, and explain the
   explicit install or reload command. Do not install, enable, publish, or
   overwrite another plugin unless the user requested that action.

## Boundaries

- Framework-owned Agent Plugins parsing and preparation remain authoritative.
- EKO-specific themes, output styles, monitors, UI projection, install roots,
  and reload behavior remain application concerns.
- Third-party wire names may be preserved only at their adapter boundary;
  internal EKO concepts use EKO terminology.
- A plugin must not introduce SQLite into EKO. Local state uses the existing
  file or memory facilities.

## Quality Bar

- `plugin.json` and every referenced component parse successfully.
- Only requested components remain; no sample placeholders survive.
- Skill descriptions route precisely and supporting resources are reachable.
- Scripts have been executed against a representative input.
- Validation uses EKO's existing `/plugins validate` authority.
