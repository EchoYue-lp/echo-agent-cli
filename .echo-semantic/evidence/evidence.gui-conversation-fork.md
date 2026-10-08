---
schema_version: 1
id: evidence.gui-conversation-fork
kind: evidence
observed_at: 8bd2a6f08117cde61037b583000c71ab820e73ce
source_refs: [src/tauri/commands/conversations.rs, web-frontend/src/components/chat/ChatPanel.tsx, web-frontend/src/components/chat/MessageBubble.tsx, web-frontend/src/components/chat/MessageBubble.fork.test.tsx, web-frontend/src/stores/conversationStore.ts, web-frontend/src/stores/conversationStore.test.ts, web-frontend/src/api/endpoints.ts, web-frontend/src/api/endpoints.sideConversation.test.ts, docs/en/adr/0045-managed-conversation-lifecycle.md, docs/supreme/specs/side-conversation/design.md, echo-agent-app-core/src/state/tests.rs]
supports: [behavior.gui-conversation-fork, rule.conversation-collaboration-authorities]
limitations: [该证据绑定当前候选源码，尚非远端合并后的验收, 浏览器视觉检查不证明原生 Tauri 端到端运行]
---

# GUI Fork verification

## 支持的结论

完成回复下方的独立 Fork 调用同一 branch IPC 的 include_turn 模式。测试断言完整回合工具行保留、未来回合与内部消息排除、无 final reply 则拒绝、源消息 ID 重建；前端断言按钮调用、busy 禁用、非 Side 列表中同时保留源/目标、加载 canonical 回复、迟到选择不覆盖和错误保留原历史。

## 来源与范围

Tauri conversations.rs 的真实分叉 command、已有 snapshot helper 与 React MessageBubble/ChatPanel/conversationStore/API 的调用链。定向前端 4 文件 30 项通过，前端完整 56 文件 290 项、ESLint 和构建通过；后端 branch_prefix 定向 2 项、修复后的 cold-delivery 定向回归通过。

## 最终候选验证

- framework 依赖为远端 main `6fd66621e0671028d6aa94c69933b63f38e299cd`；CLI base 为 `6b162b6ab2aa13c8262fd6424ff089a692df70b5`，合入 main 检查返回 Already up to date。
- fmt check、workspace all-target/all-feature Clippy 与 lib/bins panic/unwrap/expect/unreachable Clippy 全部通过。
- workspace all-features：app-core 1594 通过、9 既有忽略；CLI 281、main 11、JSONL subprocess 6，以及集成测试和 doctest 无失败。
- app-core no-default check、GUI bin check 和 GUI tests 全部通过：GUI 212、Tauri main 1、JSONL subprocess 6，无失败。
- 前端 Prettier、ESLint、56 文件 290 项测试、TypeScript/Vite 构建通过；Vite 大 chunk 提示不是失败。
- 双语文档 parity 57 对/47 ADR、strict semantic snapshot/change-evidence、git diff whitespace 检查通过。临时 browser fixture 已删除，正式生成 TypeScript 契约未被覆盖。

## 已知缺口

候选测试不是远端 main 交付证据。真实 Tauri 原生窗口与模型供应商不由浏览器模拟检查证明。浏览器检查使用真实 MessageBubble、LeftSidebar 与 conversationStore，持久化 API 为本地 fixture，已确认点击后两个独立列表条目及新会话选中。

第一次 workspace 全量运行中，既有 cold-delivery 测试的 live-steer 阶段以固定睡眠争抢就绪时机，1593 通过、1 失败、9 忽略。定向重现空载通过；修复改为等待真实流式调用和持久 mailbox acceptance 才放行模型，保持原断言与超时预算，不修改生产流程或放宽判定。最终 workspace 全量复验通过。

Framework examples 和 echo-website 不消费这个 EKO 界面入口，无需修改。
