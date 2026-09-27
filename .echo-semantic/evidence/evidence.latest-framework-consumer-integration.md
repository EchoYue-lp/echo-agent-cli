---
schema_version: 1
id: evidence.latest-framework-consumer-integration
kind: evidence
observed_at: source:caa0bf81fcda8d046037a8a1065adc31b7bb0652059b4e4c8e975d8831e26fc0
source_refs:
  - docs/en/adr/0043-background-review-owner.md
  - docs/en/adr/0044-skill-mutation-integration.md
  - docs/en/adr/0045-managed-conversation-lifecycle.md
  - docs/en/adr/0046-plugin-generation-fail-closed.md
  - echo-agent-app-core/src/agent_pool/generation.rs
  - echo-agent-app-core/src/agent_pool/pool.rs
  - echo-agent-app-core/src/agent_pool/tests.rs
  - echo-agent-app-core/src/browser/mod.rs
  - echo-agent-app-core/src/chat_driver.rs
  - echo-agent-app-core/src/conversation_archive.rs
  - echo-agent-app-core/src/conversation_deletion.rs
  - echo-agent-app-core/src/managed_conversation.rs
  - echo-agent-app-core/src/evolution/background_review_owner.rs
  - echo-agent-app-core/src/evolution/evidence.rs
  - echo-agent-app-core/src/evolution/mod.rs
  - echo-agent-app-core/src/evolution/review_integration.rs
  - echo-agent-app-core/src/extension_control/policy.rs
  - echo-agent-app-core/src/extension_control/service.rs
  - echo-agent-app-core/src/extension_control/tests.rs
  - echo-agent-app-core/src/infra/tests.rs
  - echo-agent-app-core/src/plugin_runtime/publication.rs
  - echo-agent-app-core/src/plugin_runtime/runtime.rs
  - echo-agent-app-core/src/plugin_runtime/tests.rs
  - echo-agent-app-core/src/product_data_io.rs
  - echo-agent-app-core/src/reflection.rs
  - echo-agent-app-core/src/runtime.rs
  - echo-agent-app-core/src/scheduler/runner.rs
  - echo-agent-app-core/src/state/app_state.rs
  - echo-agent-app-core/src/state/reliability_contracts.rs
  - echo-agent-app-core/src/state/tests.rs
  - echo-agent-app-core/src/state/workspace.rs
  - echo-agent-app-core/src/structured_extraction.rs
  - echo-agent-app-core/src/tasks/service.rs
  - echo-agent-app-core/src/tasks/task_runtime/executor/dispatch.rs
  - echo-agent-app-core/src/tasks/task_runtime/executor/tests.rs
  - echo-agent-app-core/src/tasks/task_runtime/file_shadow.rs
  - echo-agent-app-core/src/tasks/task_runtime/memory_bridge.rs
  - echo-agent-app-core/src/tasks/task_runtime/register.rs
  - echo-agent-app-core/src/tasks/task_runtime/run_authority.rs
  - echo-agent-app-core/src/tasks/task_runtime/store/tests.rs
  - echo-agent-app-core/src/tasks/task_runtime/task_execute_tool.rs
  - echo-agent-app-core/src/workspace/runtime.rs
  - src/cli/channels.rs
  - src/cli/cmd_impls/all.rs
  - src/cli/cmd_impls/evolution.rs
  - src/cli/cmd_impls/session.rs
  - src/main.rs
  - src/tauri/commands/chat.rs
  - src/tauri/commands/conversations.rs
  - src/tauri/commands/memory.rs
  - src/tauri/commands/panels.rs
  - src/tauri/mod.rs
  - src/tui/events.rs
  - web-frontend/src/api/endpoints.ts
  - web-frontend/src/components/evolution/EvolutionPanel.tsx
supports: [behavior.background-review-caller-owned-settlement, rule.eko-framework-consumer-authorities]
limitations:
  - framework PR 174 已合并且签名有效；CLI 最终远端 CI、CLI/main 和顶层指针尚未结算
  - echo-agent Issue 38 仍 OPEN，应用主分支与最终 CI 尚未结算
  - 本地 GUI feature 与前端测试通过，真实 Tauri 窗口视觉验收尚未执行
---

# Latest framework consumer integration evidence

## 支持的结论

CLI 使用本工作区 framework 候选 `15c053c0f7c56fa0156466f876779fcc64e9c68b` 编译并运行；PR #174 已 squash merge 为签名有效的 framework main `1927a5fc6783770f97d2d95cb24600c2f4cddfba`，两个提交的 Git tree 相同。Background Review 的 lazy handle 在 poll 前写 admission，owner 持有 generation、监督取消和 shutdown；outcome、Inbox、terminal 按顺序持久化，重启重放或记录中断与 Memory 效果归因。GUI-only Side Conversation 的入口与非 GUI 边界仍按 ADR 0042。

## 来源与范围

代码和 ADR 覆盖 ReviewIntegration、FileEventJournal 收据、GUI/TUI/CLI Review、Skill authority、Managed conversation 导入/删除、跨工作区 handoff 持久准入、channel exact GC、Plugin 新代际失败关闭、Task claim 与 Scheduler 路径绑定。`Cargo.lock` 跟随 framework 候选依赖图；框架通用变更由独立 PR #174 承载。

## 验证

- `cargo fmt --all -- --check`：通过。
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`：通过。
- `cargo clippy --workspace --lib --bins --all-features --locked -- -D clippy::unwrap_used -D clippy::expect_used -D clippy::panic -D clippy::unreachable`：通过。
- `CARGO_INCREMENTAL=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_PROFILE_DEV_DEBUG=0 cargo test --workspace --all-features --locked --quiet`：通过；app-core 1594 通过、9 既有忽略，CLI 280/280、JSONL 子进程 6/6，其余集成与 doctest 无失败。
- `cargo check -p echo-agent-app-core --no-default-features --locked --quiet`：通过。
- `CARGO_INCREMENTAL=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_PROFILE_DEV_DEBUG=0 cargo check --no-default-features --features gui --bin echo-agent-tauri --locked` 与同 feature 的 `cargo test --locked --quiet`：通过；GUI 211/211、Tauri main 1/1、JSONL 子进程 6/6。
- `npx prettier --check 'src/**/*.{ts,tsx}'`、`npm test`（55 文件、283 项）、`npm run build`：通过；Vite 大 chunk 提示为非失败警告。
- Background Review 定向故障注入覆盖收据写入失败不 poll、真实 owner 的 Outcome 与 Terminal append 失败、Memory 写入后 observer 未完成即 shutdown、旧 key 归因未知、outcome 到 Inbox 之间重启补投影及跨 generation 拒绝；新增两项 supervisor fault seam 定向测试通过，独立复审确认无剩余实现阻断。

## 已知缺口

这些命令仅证明本地 CLI 候选工作树。Framework PR #174 的 7 项 CI 全绿，签名 squash 提交的 Git tree 与候选相同；CLI CI、CLI 与顶层 MR 合并后的 exact main 和 Issue #38 远端关闭尚未结算，不得将本证据读成 EKO 已交付主分支。
