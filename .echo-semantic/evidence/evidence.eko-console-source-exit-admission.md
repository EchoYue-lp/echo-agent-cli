---
schema_version: 1
id: evidence.eko-console-source-exit-admission
kind: evidence
observed_at: source:513d810144fb2c3348549b4734c9ae9275edd7cfbe1c152419b76db135f871f3
source_refs:
- Cargo.toml
- src/lib.rs
- docs/zh/adr/0050-gui-product-convergence.md
- echo-agent-app-core/src/channel_runtime/management.rs
- echo-agent-app-core/src/channel_runtime.rs
- src/tauri/desktop.rs
- echo-agent-app-core/src/auto_memory/mod.rs
- src/tauri/commands/panels.rs
- echo-agent-app-core/src/runtime.rs
- echo-agent-app-core/src/scheduler/runner.rs
- src/tauri/mod.rs
- echo-agent-app-core/src/terminal.rs
supports:
- rule.eko-framework-consumer-authorities
limitations:
- 仅源码删除准入与保留机制源码接线，不是被删CLI/TUI行为等价
- after_revision绑定正常官方task观察值；prepared after-view身份另存，两者均不代表动态运行结果
- 未执行删后Cargo/新锁风险/新F组合/native/手机/SSH/mainCI；这些仍在原Goal与Plan09/10，未从任何宽scope证据移除unknown
evidence_type: behavior_equivalence
before_revision: 080e0b698c664105f789f522779a985f93b4f71b
after_revision: task:de2c349e730acd6edf941eb289dae733d23681f58a7cf9a032309c9afd51b108
deleted_paths:
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
coverage:
- 独立源码删除准入：54叶范围与8共享hunk
- 保留canonical GUI/app-core owner源码接线
- 真实prepared after-view与原件恢复字节；不含动态行为场景
scenario_results:
  source-exact-approved-scope:
    status: matched
    source_refs:
    - Cargo.toml
    - src/lib.rs
    basis: 当前62 base/已审candidate逐SHA匹配，精确54专属叶+8共享修改，非按目录名扩删
  source-retained-original-owner-anchors:
    status: matched
    source_refs:
    - src/tauri/desktop.rs
    - echo-agent-app-core/src/auto_memory/mod.rs
    - src/tauri/commands/panels.rs
    - echo-agent-app-core/src/runtime.rs
    - echo-agent-app-core/src/scheduler/runner.rs
    - src/tauri/mod.rs
    - echo-agent-app-core/src/terminal.rs
    basis: 真实after-view中7个保留构造/身份/数据锚文件逐字节等于当前dirty-before；未改变或新建owner
  source-shared-hunks-and-failure-test-wiring:
    status: matched
    source_refs:
    - echo-agent-app-core/src/channel_runtime/management.rs
    - echo-agent-app-core/src/channel_runtime.rs
    - echo-agent-app-core/src/hitl/mod.rs
    basis: 独审逐hunk确认start/stop失败断言迁原GUI receipt/manager，dispatcher等保原实现；这里只是源码断言接线审查
  source-actual-after-view-and-restoration-bytes:
    status: matched
    source_refs:
    - Cargo.toml
    - src/lib.rs
    basis: 本SID真实完整source after-view apply0后62输出逐SHA匹配candidate；62原件独立备份回读、pre-retirement tag实核
command_results:
- command: git apply --check /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/.echo-semantic/local/parallel-executors-20261027/governance/retirement-successor-f5908-epoch18-20261028/eko/eko-retirement.patch
  exit_code: 0
- command: git apply --unsafe-paths --directory /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli/.echo-semantic/local/eko-successor-integration-epoch19-20261028/retirement62-after-view
    /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/.echo-semantic/local/parallel-executors-20261027/governance/retirement-successor-f5908-epoch18-20261028/eko/eko-retirement.patch
  exit_code: 0
- command: installed verify_semantic.source_digest current repo and read-only-index after-view observation
  exit_code: 0
---

# 仅源码范围的独立终端删除准入证据

## 来源与范围

本证据从创建起只覆盖四个明示源码场景，matched只表示实际源码/逐字节检查和独审成立，没有动态等价行被删除或改写。Git删除基准为当前HEAD；dirty-before另有官方source身份 source:1c989caccd71c85c2006b0211fb91a82f48bedbf7ec85bd89964b9ae5c896f75，不是声称工作区等于HEAD。after-view已真实生成并逐项回读，官方source身份为 source:d6973f82ff3b5949e4ba575abcfaaa015c4ab8defcc24a1beacaa3f026f08eae。

## 支持的结论

62原件清单、SHA、after-view、官方digest与命令原件由本SID任务回执保存；本地产物不是正式source_refs/实现权威。准备阶段的after-view记录与后续正式应用观察分别保留，不把准备检查冒充正式执行。源码审查只依据原ADR0050和既有真实owner，不新建替代执行器、store或状态权威。

## 已知缺口

保留GUI/IM/cron恢复、PTY、Agent shell、四桌面参数、SDK stdio的后继编译与运行验证仍独立未完成。原source402工具合同排除与阻塞保持，不修改工具或扩大排除。

## 正式源码采用后观察

精确62与同切点7文稿已正常record/check/apply后回读；当前实际官方source身份为 source:aadfae61ac52abdfa6f8888fd71f7218319cbce4bb98e18860d611b59f9e3c5a。与准备after-view的代码62相同，但Cargo.lock额外观测399509ee，来源未定且未接受新风险，旧3fb/新锁均保存。这里不把锁漂移解释为已运行Cargo或风险通过。after_revision将由正常官方task观察返回值绑定，原prepared after-view身份在原准备回执保持。

正常installed validate_task在实际正式源上返回 task:127563cb03be48dd5d03062dacee9426c74e0c77d1a09906e9571268e0c05560，此处只绑定after_revision，不写runtime digest、不消费错误。首次all errors完整保留，原source-after-view source:d6973f82ff3b5949e4ba575abcfaaa015c4ab8defcc24a1beacaa3f026f08eae 未改写为运行证据。

## 旧API回归接入后的身份续接

当前正常官方source观察为 source:513d810144fb2c3348549b4734c9ae9275edd7cfbe1c152419b76db135f871f3，task观察为 task:8ea63ee812dd2edb752b382a56ec4b4260cd401ab01d433834a46fb0e492399f；只接入两个旧API回归及格式整理，不改变上述54删除/8共享源码场景。官方校验错误原样归档；绑定身份不表示校验通过，也未新增动态matched。
