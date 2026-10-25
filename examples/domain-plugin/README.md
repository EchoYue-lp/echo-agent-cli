# Task-bound domain example

This is an EKO application plugin, using the existing plugin installation and
workspace discovery paths. The manifest intentionally omits `version`; EKO keeps
that value absent and binds the prepared content identity instead.

Select `domain_plugin: "evidence-method"` through the existing task tool or GUI
chat request. The method's artifact and semantic obligations require the original
`task_create` graph and `task_execute` completion gate. Each obligation must remain
on a real, non-skipped node. An empty `allowed_tools` adds no restriction; it does
not grant a tool that the Agent otherwise lacks.

The package uses existing built-in roles. A custom plugin role must come from this
package or its declared manifest dependency closure. Additional content used by
the method belongs in `inputs`; neither an entire directory nor external MCP
implementations are implicitly snapshotted. Credentials do not belong here.

The owning app-core `domain_plugin` regression parses these actual files and
checks their input digest. They join the ordinary Rust test chain when that chain
is run. The source also defines real agent_tool/background lease and prompt
projection regressions. The separate framework same-registration source still
requires integration; this EKO contribution has not yet compiled or run those
tests. No native, release or main qualification is claimed.
