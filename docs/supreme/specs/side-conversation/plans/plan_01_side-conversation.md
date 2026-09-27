---
schema_version: 4
slug: side-conversation/end-to-end
outcome:
  summary: EKO 在不新增聊天运行时或持久化权威的前提下，跨 GUI、TUI、CLI/JSONL 与 channel
    交付可持久、可并发、可继续交互的一级 Side Conversation
  acceptance:
    - 用户可从主对话创建、打开并持续使用多个一级 Side Conversation，主对话保持独立可用，重启后父子关系、模型选择、消息和可恢复状态一致
    - 主 Agent 与支线使用既有 AgentRouter 双向通信且默认不污染主 transcript；支线不能嵌套创建，完成结果不自动回写
    - echo-agent-cli 所有适用 Rust、GUI、前端、契约与多 surface 门禁全部通过，正式双语 ADR 和产品文档与实现一致
out_of_scope:
  - 跨 Workspace Side Conversation、默认 worktree 隔离与自动合并代码
  - Side Conversation 嵌套创建或完成后自动总结、自动写回主 transcript
  - 修改 echo-agent 公共 API、引入 SQLite、新表、SideConversationStore、第二 mailbox 或第三聊天运行时
design_ref: docs/supreme/specs/side-conversation/design.md
design_sections:
  - ref: design.md § 目标行为
    digest: sha256:eb2a1048bc92280ccb2d146f9a2f8661587da1d4100c0274ddac5dc2023f9c93
  - ref: design.md § 系统边界与分层
    digest: sha256:6be0478ee25818f51c8d8a99259451a06b75ba3a2ae9a97e8e7d5e4275f97ee4
  - ref: design.md § 核心结构与数据流
    digest: sha256:3836afda74ce762ccbae097c1ce92f9f01987da2e0f288b788dbbf27f7766b81
  - ref: design.md § API 边界
    digest: sha256:dc6c23fa5c3ef4de7aab7d053cf2df3639f35087d6478715060db8730b2fd3c4
  - ref: design.md § 异常与边界场景
    digest: sha256:3ccdbf4c6457fb3eb4573b8c3cc7abf7d70151f474069ad7b197b428a859d7eb
  - ref: design.md § 复用与实现约束
    digest: sha256:8815238c1859bd4874e431020c24649a7e0d03fb3bb5e61e19116ba693e6ea0f
  - ref: design.md § 验收标准
    digest: sha256:0fdc0a90480b79962bf9140b1d3fa46c77ba0b47f423f0b9b55fc21db62b5693
delivery_ref: null
todos:
  - id: converge-side-conversation-authority
    summary: 在 app-core 组合 ConversationStore、AgentGroup/AgentRouter、AgentPool
      与现有输入/删除权威，形成唯一的一级 Side Conversation 创建、快照、关系、模型和恢复服务，并收敛 GUI branch 与 TUI
      fork 的重复语义
    files:
      - echo-agent-app-core/src/agent_control.rs
      - echo-agent-app-core/src/agent_router/
      - echo-agent-app-core/src/state/app_state.rs
      - echo-agent-app-core/src/conversation_input.rs
      - echo-agent-app-core/src/conversation_deletion.rs
      - src/tauri/commands/conversations.rs
      - src/tui/events.rs
    acceptance:
      - app-core 测试证明幂等创建只复制 committed transcript、稳定投影
        leader/member、拒绝二级支线，并在首轮失败、缺失 child 和重启恢复时返回可诊断回执且不产生第二权威
  - id: bind-conversation-operations-and-surfaces
    summary: 让 conversation create/list/get
      messages/send/delete/cancel/update/rename 语义通过同一 app-core 服务覆盖
      Tauri、TUI、CLI/JSONL 与 channel，复用 exact conversation/turn identity、durable
      frontier 和 AgentRouter inbox
    files:
      - src/tauri/commands/conversations.rs
      - src/tauri/commands/chat.rs
      - src/tauri/commands/agent_router.rs
      - src/tauri/mod.rs
      - src/tui/commands.rs
      - src/tui/events.rs
      - src/cli/
      - tests/jsonl_subprocess.rs
    acceptance:
      - 多 surface 契约测试证明支线与主对话可并发发送、排队和精确取消，模型覆盖只作用于 child，内部消息有 typed receipt
        且默认不进入主 transcript，删除 child 不改变 parent
  - id: deliver-side-conversation-navigation
    summary: 在主对话提供可发现的支线入口，并在左侧 Workspace/主对话树下呈现可恢复的支线条目；打开后复用完整 Agent 时间线和
      composer，支持模型选择、状态、未读、重命名、取消与删除
    files:
      - web-frontend/src/api/endpoints.ts
      - web-frontend/src/types/api.ts
      - web-frontend/src/stores/conversationStore.ts
      - web-frontend/src/components/layout/LeftSidebar.tsx
      - web-frontend/src/components/chat/ChatPanel.tsx
      - web-frontend/src/components/chat/
      - web-frontend/src/stores/
    acceptance:
      - 前端测试覆盖创建 prompt/模型、父子树、切换与重载、并发状态、隐藏内部消息、无嵌套入口、删除确认、workspace
        隔离和宽屏/紧凑/移动布局无重叠
  - id: close-contract-documentation-and-gates
    summary: 删除或归一被 Side Conversation 服务替代的 fork/branch 状态权威，补齐双语
      ADR、产品文档、生成契约与适用验证，并明确 examples 和 website 的同步结论
    files:
      - docs/en/adr/
      - docs/zh/adr/
      - docs/en/
      - docs/zh/
      - docs/doc-parity-manifest.json
      - echo-agent-app-core/src/
      - src/
      - web-frontend/src/
    acceptance:
      - 仓库搜索与测试确认不存在平行 Side Conversation store、复制器、父子索引、非 Subagent 执行角色术语或长期旧
        fork 主路径；双语 ADR/文档契约一致，examples/website 不适用时有明确依据，所有适用合并门禁零失败
artifact_id: plan:235d48d7-0360-4213-9e38-19b8bae26f9c
lifecycle: abandoned
---
