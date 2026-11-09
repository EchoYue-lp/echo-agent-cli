---
schema_version: 1
id: asset.eko-console-product-entrypoints
kind: asset
title: 已具名批准退出的独立终端产品入口源
asset_type: entrypoint
status: deprecated
risk: high
observed_at: 080e0b698c664105f789f522779a985f93b4f71b
boundary_refs:
- boundary.eko-framework-consumer-settlement
behavior_refs: []
rule_refs:
- rule.eko-framework-consumer-authorities
evidence_refs:
- evidence.eko-console-source-exit-admission
finding_refs:
- finding.eko-console-source-exit-admission
candidate_refs: []
code_refs:
- echo-agent-app-core/src/hitl/repl_provider.rs
- echo-agent-app-core/src/hitl/tui_provider.rs
- src/cli/args.rs
- src/cli/channels.rs
- src/cli/cmd_impls/advanced.rs
- src/cli/cmd_impls/agent_router.rs
- src/cli/cmd_impls/all.rs
- src/cli/cmd_impls/analysis.rs
- src/cli/cmd_impls/coding.rs
- src/cli/cmd_impls/context.rs
- src/cli/cmd_impls/cron.rs
- src/cli/cmd_impls/developer.rs
- src/cli/cmd_impls/diff_cmd.rs
- src/cli/cmd_impls/evolution.rs
- src/cli/cmd_impls/extract.rs
- src/cli/cmd_impls/git.rs
- src/cli/cmd_impls/hooks.rs
- src/cli/cmd_impls/info.rs
- src/cli/cmd_impls/mod.rs
- src/cli/cmd_impls/observability.rs
- src/cli/cmd_impls/pipeline.rs
- src/cli/cmd_impls/pipelines.rs
- src/cli/cmd_impls/plugins.rs
- src/cli/cmd_impls/research.rs
- src/cli/cmd_impls/session.rs
- src/cli/cmd_impls/skills.rs
- src/cli/cmd_impls/tasks_ext.rs
- src/cli/cmd_impls/workflows.rs
- src/cli/cmd_impls/workspace.rs
- src/cli/command.rs
- src/cli/commands.rs
- src/cli/completion.rs
- src/cli/editor.rs
- src/cli/extension_surface.rs
- src/cli/git_ops.rs
- src/cli/jsonl.rs
- src/cli/keybindings.rs
- src/cli/mod.rs
- src/cli/modes.rs
- src/cli/repl.rs
- src/main.rs
- src/tui/clipboard.rs
- src/tui/commands.rs
- src/tui/events.rs
- src/tui/markdown.rs
- src/tui/mod.rs
- src/tui/ui.rs
- src/tui/widgets/chat.rs
- src/tui/widgets/input.rs
- src/tui/widgets/mod.rs
- src/tui/widgets/sidebar.rs
- src/tui/widgets/status_bar.rs
- src/tui/widgets/task_strip.rs
- tests/jsonl_subprocess.rs
consumer_refs:
- Cargo.toml
- src/lib.rs
---

# 独立终端产品入口的源码处置身份

## 资产身份

只覆盖清单中的54个独占源，不包括GUI内置PTY/SSH、Agent shell或独立SDK Host stdio。该资产记录退役输入与恢复身份，canonical保留owner为原app-core/GUI。当前真实操作步骤和是否已经物理删除见同Finding，不据deprecated标签推定运行时不可达。

## 来源与消费者

历史code_refs按Git删除基准HEAD记录旧入口身份；dirty-before精确原件另在本次Finding恢复包，不把HEAD替代当前WIP原件。54源已实际删除，当前产品主入口为GUI；动态资格仍待后继。

## 生命周期

本资产保留删除前身份与恢复引用，状态为deprecated；当前入口注册与共享能力接线由正式Cargo清单、src/lib.rs和原GUI/app-core路径决定。

## 候选关系

唯一保留权威是既有 `asset.eko-application-channel-owner`，不是新建替代wrapper或执行器。原源码准入Finding仅覆盖具名54叶与8个共享修改。

## 未知与限制

未取得本轮删后Rust、风险扫描或原生运行结果，不据此宣称整个退役目标完成；固定tag与62份原件只提供已核实的源码恢复依据。
