---
schema_version: 1
id: finding.eko-console-source-exit-admission
kind: finding
type: consolidation_candidate
status: resolved
severity: high
primary_focus: contract_evidence
focus:
- state_authority
- time_lifecycle
boundary_ref: boundary.eko-framework-consumer-settlement
behavior_refs: []
rule_refs:
- rule.eko-framework-consumer-authorities
evidence_refs:
- evidence.eko-console-source-exit-admission
audit_refs:
- audit.eko-console-source-exit-admission
decision_refs:
- docs/zh/adr/0050-gui-product-convergence.md
repair_evidence_refs:
- evidence.eko-console-source-exit-admission
verification_evidence_refs:
- evidence.eko-console-source-exit-admission
rereview_audit_refs:
- audit.eko-console-source-exit-admission
discovered_at: source:1c989caccd71c85c2006b0211fb91a82f48bedbf7ec85bd89964b9ae5c896f75
decision: retire
candidate_asset_refs:
- asset.eko-console-product-entrypoints
- asset.eko-application-channel-owner
canonical_asset_ref: asset.eko-application-channel-owner
replacement_refs:
- asset.eko-application-channel-owner
delete_paths:
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
rollback_ref: pre-retirement-2026-10-24
---

# 独占入口源删除准入与保留机制源码接线

## 问题

独立终端入口的具名删除需要证明范围准确、共享owner保留且原件可恢复；这与整个退役目标的动态验收是不同范围。

## 处理记录

Root已明确批准精确EKO62：54独占文件删除、8共享hunk。resolved只结算本Finding从创建起明示的源码审查/退役裁决与删除准入，不结算Plan10、删后动态或整个终端退役目标。54个专属文件已按同SID正常repair record/check与精确patch从正式源删除，8个修改逐SHA匹配已审candidate；同切点7文稿已采用。这里仅增加真实源码操作事实，不代表删后Cargo/native完成，也不借旧LSP/helper resolved授权。

## 保留与恢复

canonical replacement是实际原GUI/app-core共享owner，不是新wrapper资产。GUI PTY/SSH、Agent shell、四DesktopArgs、SDK Host stdio和共享IM/cron/恢复/结算继续原路径。包名echo-agent-cli、identifier com.eko.desktop及用户/工作空间数据路径不改。62恢复原件与固定tag已实核，不覆盖其它用户WIP；修改集合为 Cargo.toml, config/eko.example.yaml, echo-agent-app-core/src/channel_runtime/management.rs, echo-agent-app-core/src/channel_runtime.rs, echo-agent-app-core/src/config.rs, echo-agent-app-core/src/hitl/mod.rs, echo-agent-app-core/src/infra/logging.rs, src/lib.rs。

## 触发条件与影响

只有批准清单内54独占源退出和8共享修改属于本Finding；若连带删除GUI/IM/cron共享能力或改变原owner，就超出本源码准入结论，须保留原Goal并继续修复。

## 证据

`evidence.eko-console-source-exit-admission`记录四个真实源码场景、官方身份与恢复检查；`audit.eko-console-source-exit-admission`绑定相同边界。两者均不提供删后动态GREEN。

## 未验收范围

Equivalence只限命名源码场景；无删后Cargo、锁/风险、新F组合、跨平台、native、手机、SSH或CI matched。原Goal/Plan09-10及原宽scope未知项保持开放，不因本有限source Finding resolved消失。正式采用后另正常绑定实际after/source gate与运行证据；原失败、旧App基线均保留。
