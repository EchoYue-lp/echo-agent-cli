---
schema_version: 4
slug: side-conversation/gui-only
outcome:
  summary: EKO 仅在 GUI/Tauri 中交付可持久、可并发、可继续交互的一级 Side Conversation，同时保持
    TUI、CLI/JSONL 与 channel 的既有行为不变
  acceptance:
    - GUI 用户可从主对话创建、打开并持续使用多个一级 Side
      Conversation，主对话保持独立可用，重启后父子关系、模型选择、消息和可恢复状态一致
    - 主 Agent 与支线使用既有 AgentRouter 双向通信且默认不污染主 transcript；支线不能嵌套创建，完成结果不自动回写
    - TUI、CLI/JSONL 与 channel 不包含 Side Conversation 专用入口、事件、帮助文本或 wire
      contract，既有普通 conversation、Subagent 与 TUI /fork 行为保持不变
    - echo-agent-cli 所有适用 Rust、GUI、前端和契约门禁全部通过，顶层与应用 AGENTS.md、双语 ADR 和产品文档与实现一致
out_of_scope:
  - 为 TUI、CLI/JSONL 或 channel 增加 Side Conversation 专用交互或协议
  - 跨 Workspace Side Conversation、默认 worktree 隔离与自动合并代码
  - Side Conversation 嵌套创建或完成后自动总结、自动写回主 transcript
  - 修改 echo-agent 公共 API、引入 SQLite、新表、SideConversationStore、第二 mailbox 或第三聊天运行时
design_ref: docs/supreme/specs/side-conversation/design.md
design_sections:
  - ref: design.md § 目标行为
    digest: sha256:3dc04353d8a57688a352e5c3c269abe6f490a5ca9aae2762a3742691e761328d
  - ref: design.md § 范围与非目标
    digest: sha256:1442d17955def12d0b9174d6a78ffc0afbe15ef3676a4cbf5602a8de03adf84f
  - ref: design.md § 系统边界与分层
    digest: sha256:eabcdc22220ebc54d2fd5af30bc4d4b043fdeb9fcdd3453bc5b3f53265d18bb0
  - ref: design.md § 核心结构与数据流
    digest: sha256:aa3d8124bf5f7fef2d9fd257578a469942f819868e5ade3cd36669f63bed0c2e
  - ref: design.md § API 边界
    digest: sha256:f8cab52ec15435f1098bed7cdb596aa8578c9b563a9368a673e202b985099c84
  - ref: design.md § 异常与边界场景
    digest: sha256:4bfab9f865718fc4f7c55beebdff32737024a924fd87755808eaa6a4a0aef0c0
  - ref: design.md § 复用与实现约束
    digest: sha256:8815238c1859bd4874e431020c24649a7e0d03fb3bb5e61e19116ba693e6ea0f
  - ref: design.md § 验收标准
    digest: sha256:8393def20c718b5248bf789922db8104a9b503003f4f8460580601a51b981e51
delivery_ref: null
todos:
  - id: converge-gui-side-conversation-authority
    summary: 在 app-core 组合 ConversationStore、AgentGroup/AgentRouter、AgentPool
      与现有输入/删除权威，形成 GUI 使用的唯一一级 Side Conversation 创建、快照、关系、模型和恢复服务
    files:
      - echo-agent-app-core/src/side_conversation.rs
      - echo-agent-app-core/src/agent_router/
      - echo-agent-app-core/src/agent_pool/
      - echo-agent-app-core/src/state/app_state.rs
    acceptance:
      - app-core 测试证明幂等创建只复制 committed transcript、稳定投影
        leader/member、拒绝二级支线，持久化首轮失败的 prompt/error 并可幂等重试；通用 Agent group
        入口看不到也不能修改 Side group，父子删除与重启恢复不产生第二权威
  - id: expose-tauri-conversation-contract
    summary: 让 Tauri conversation create/list/get/send/delete/cancel/update/rename
      复用同一 app-core 服务，并使用 exact conversation/turn identity、durable frontier 与
      AgentRouter inbox
    files:
      - src/tauri/commands/conversations.rs
      - src/tauri/commands/chat.rs
      - src/tauri/mod.rs
    acceptance:
      - Tauri 与 app-core 契约测试证明支线与主对话可并发发送、排队和精确取消，模型覆盖只作用于 child，内部消息有 typed
        receipt 且默认不进入主 transcript，首轮可重试且删除 child 不改变 parent
  - id: deliver-gui-side-navigation
    summary: 在 GUI 主对话提供可发现的支线入口，并在左侧 Workspace/主对话树下呈现可恢复的支线条目；打开后复用完整 Agent 时间线和
      composer，支持模型选择、状态、未读、重命名、取消与删除
    files:
      - web-frontend/src/api/endpoints.ts
      - web-frontend/src/types/api.ts
      - web-frontend/src/stores/conversationStore.ts
      - web-frontend/src/components/layout/LeftSidebar.tsx
      - web-frontend/src/components/chat/ChatPanel.tsx
      - web-frontend/src/components/chat/
    acceptance:
      - 前端测试和 Playwright 截图覆盖创建
        prompt/模型、父子树、切换与重载、失败重试、并发状态、内部消息标签、无嵌套入口、删除确认、workspace 隔离和宽屏/移动布局无重叠
  - id: remove-non-gui-contract-and-close-gates
    summary: 删除已新增的 TUI、CLI/JSONL 与 channel Side Conversation 入口，恢复既有 TUI /fork
      语义，并同步规则、双语 ADR、产品文档、生成契约与适用验证
    files:
      - AGENTS.md
      - src/tui/
      - src/cli/
      - README.md
      - docs/en/adr/
      - docs/zh/adr/
      - docs/en/features.md
      - docs/zh/features.md
      - docs/doc-parity-manifest.json
      - echo-agent-app-core/src/chat_driver.rs
      - echo-agent-app-core/src/workspace/runtime.rs
      - web-frontend/src/generated/
    acceptance:
      - 仓库搜索确认非 GUI surface 没有 Side Conversation 专用入口、事件、帮助文本或 wire contract，TUI
        /fork 源码与基线一致，规则和双语文档一致；门禁暴露的 workspace shutdown debt 必须保持 sealed，Task
        execute receipt barrier 以真实事件为条件并仅使用宽裕死锁保护，所有适用合并门禁零失败
artifact_id: plan:dbcf2a7c-b540-4b47-aa46-0c7237626876
lifecycle: completed
---
