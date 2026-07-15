# EKO 记忆 & 自进化/自改善系统完整审计报告

> **审计日期**: 2026-07-01
> **范围**: echo-agent (框架) + echo-agent-cli (EKO 应用) 的全部记忆/进化/改善代码
> **对比参考**: Hermes Agent (Nous Research)
> **修订记录**:
> - 2026-07-15 v1.5: EKO 删除 EvalRunner、行为 fixture、`improve`/TrajectorySaver 产品链路；BackgroundReviewer 改为严格 JSON 候选且默认不保存；MemoryReview 默认关闭 session-end 与语义合并；Curator 归属 `evolution`。当前优化路线见 `docs/2026-07-15-self-evolution-review-and-roadmap.md`。
> - 2026-07-01 v1.4: 阶段 5（Critic 默认策略）、SkillPatcher apply_patch 已实施完成。阶段 4（Embedding/RAG）已决策：暂不支持（对标 Claude Code/Codex/Cursor/OpenClaw 均无 RAG）。迭代计划阶段 1-6 全部完成。
> - 2026-07-01 v1.3: 阶段 1（SkillTelemetry 写入端）、阶段 2（Dreaming 多模式对等）、阶段 3（Evolution hook fire 点）、阶段 6（技能来源 curator 边界）已实施完成
> - 2026-07-01 v1.2: 修正 EmbeddingStore 状态表述（框架自动检测可能在 `OPENAI_API_KEY` 存在时激活）；补充 Hermes bundled + `prune_builtins` 细节；Critic 修复方向补充架构约束说明；陷阱 #3/#4 标记为已过时；Evolution hook fire 点描述精确化
> - 2026-07-01 v1.1: 修正 SQLite/ConversationStore（EKO 使用 FileConversationStore 而非 SQLite，符合 AGENTS.md 硬约束）；RulePromoter namespace 死链已修；Dashboard 前端已接入；规则晋升已有 review gate；技能候选可视化已接入
> - 2026-07-01 v1.0: 初始版本
>
> **重要阅读提示**: 本文严格区分「框架层有」和「应用层实际接入」。echo-agent 是通用框架，echo-agent-cli (EKO) 是产品。2026-07-15 之前的行号和部分历史描述仅用于追溯；自进化当前决策以 `docs/2026-07-15-self-evolution-review-and-roadmap.md` 为准。

---

## 目录

1. [总体架构概览](#1-总体架构概览)
2. [记忆系统 (Memory)](#2-记忆系统-memory)
3. [自进化系统 (Self-Evolution)](#3-自进化系统-self-evolution)
4. [自改善系统 (Self-Improvement)](#4-自改善系统-self-improvement)
5. [技能系统](#5-技能系统)
6. [EKO 实际接入状态验证](#6-eko-实际接入状态验证)
7. [与 Hermes Agent 的对比分析](#7-与-hermes-agent-的对比分析)
8. [已知缺口与待修复项](#8-已知缺口与待修复项)

---

## 1. 总体架构概览

### 1.1 两层 + 两大模块

```
┌─────────────────────────────────────────────────────────────────────┐
│                     echo-agent-cli (应用层 EKO)                      │
│  ┌──────────────┐ ┌──────────────┐ ┌──────────────┐ ┌────────────┐ │
│  │ UnifiedMemory │ │  AutoMemory  │ │  RulePromoter │ │  Dashboard │ │
│  │ (指令加载)    │ │ (REPL退出)   │ │ (AGENTS.md)   │ │ (GUI面板)  │ │
│  └──────────────┘ └──────────────┘ └──────────────┘ └────────────┘ │
│  ┌──────────────┐ ┌──────────────┐ ┌──────────────┐                │
│  │ Reflection   │ │ /evolution   │ │ /memory-     │                │
│  │ (checkpoint) │ │ CLI命令群     │ │ review等      │                │
│  └──────────────┘ └──────────────┘ └──────────────┘                │
├─────────────────────────────────────────────────────────────────────┤
│                      echo-agent (框架层)                             │
│  ┌──────────────────────────────────────────────────────────────┐  │
│  │               evolution/ (17 文件, 无 feature gate)           │  │
│  │  triggers | dreaming | recall | layer | review | curator      │  │
│  │  auto_memory | health | patch | merge | security | audit      │  │
│  │  background_review | candidate | draft | runtime_integration  │  │
│  └──────────────────────────────────────────────────────────────┘  │
│  ┌──────────────────────────────────────────────────────────────┐  │
│  │          improve/ (框架可选, #[cfg(feature = "improve")])      │  │
│  │  trajectory | analyzer/eval_improvement/loop/generator(+eval) │  │
│  └──────────────────────────────────────────────────────────────┘  │
│  ┌──────────────────────────────────────────────────────────────┐  │
│  │  记忆基础设施: Store | ConversationStore | RuntimeStateStore  │  │
│  │  压缩: ContextManager + 6种压缩器 + MemoryPromoter            │  │
│  │  快照: SnapshotManager | AgentRunSnapshot                     │  │
│  │  向量: Embedder | EmbeddingStore | HttpEmbedder               │  │
│  │  RAG: rag_index | rag_search | rag_chunk_document             │  │
│  │  Critic: LlmCritic | verify_answer | tool_error_feedback      │  │
│  └──────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────┘
```

### 1.2 Feature 依赖

| Feature | 解锁内容 | 应用层是否接入 |
|---------|---------|:---:|
| (默认) | 全部 evolution/ 模块、ContextManager、SnapshotManager、全部记忆工具 | ✅ |
| `sqlite` | SqliteStore、SqliteConversationStore、SqliteRuntimeStateStore | ❌ EKO 明确不使用 SQLite（AGENTS.md 硬约束，使用 `FileConversationStore`）。`sqlite` feature 仅供框架其他复用方 |
| `improve` | 框架通用 TrajectorySaver；与 `eval` 同开时增加 Analyzer、ImprovementLoop、EvalDrivenImprovement、PromptGenerator | ❌ EKO 不启用；Curator/BackgroundReviewer 属于默认 `evolution` |
| `rag` | rag_index/search/chunk_document 工具 | ❌ 应用层未显式接入 |
| `semantic-memory` | **空 feature**（定义但无代码关联） | N/A |

---

## 2. 记忆系统 (Memory)

### 2.1 三层存储架构

| 层 | Trait | 文件路径 | 生命周期 | 存储后端 |
|----|-------|---------|---------|---------|
| **长期知识** | `Store` | `echo-core/src/memory/store.rs:182-257` | 跨 session 持久 | `FileStore` (默认) / `SqliteStore` / `InMemoryStore` / `EmbeddingStore` |
| **运行时恢复** | `RuntimeStateStore` | `echo-agent/src/state/mod.rs:154-193` | 崩溃恢复 | `SqliteRuntimeStateStore` (feature `sqlite`) |
| **历史投影** | `ConversationStore` | `echo-core/src/memory/conversation.rs:98-205` | GUI/TUI 历史面板 | `SqliteConversationStore` / `FileConversationStore` |

### 2.2 Store trait — 长期记忆核心

```rust
// echo-core/src/memory/store.rs:182-257
pub trait Store: Send + Sync {
    fn put(...)           // 写入/更新 KV
    fn get(...)           // 精确读取
    fn search(...)        // 关键词搜索
    fn search_with(...)   // 统一搜索 (Keyword/Semantic/Hybrid + RRF融合)
    fn delete(...)
    fn list_namespaces(...)
    fn list(...)
    fn prune_expired(...)
    fn dedup_by_content(...)
}
```

**四种实现**：

| 实现 | 文件 | 定位 | 应用层状态 |
|------|------|------|:---:|
| `InMemoryStore` | `echo-state/src/memory/store.rs:20` | 测试/短生命周期 | ✅ |
| `FileStore` | 同上 L208+ | 默认（`~/.echo-agent/store.json`，atomic write） | ✅ |
| `SqliteStore` | `echo-state/src/memory/sqlite_store.rs:60` | FTS5 全文索引 + 可选向量表 | ❌ 未接入 |
| `EmbeddingStore` | `echo-state/src/memory/embedding_store.rs:80` | 包装任意 Store + 余弦相似度语义检索 | ❌ 未接入 |

### 2.3 关键数据结构

**StoreItem** (`echo-core/src/memory/store.rs:14-39`)：namespace, key, value, created_at, updated_at, score, importance, last_accessed, expires_at

**SearchMode** (`echo-core/src/memory/store.rs:97-110`)：Keyword / Semantic / Hybrid { vector_weight }

**SearchQuery** (`echo-core/src/memory/store.rs:137-177`)：text, limit, mode，支持 RRF（Reciprocal Rank Fusion）混合检索评分

### 2.4 ConversationStore — 会话历史投影

- `Conversation` 结构体含 `title`、`summary`、`compressed_before_id`
- `StoredMessage` 含 role、content、attachments_json、tool_calls_json、tool_result_json
- `project_messages()` 将 LLM Message 转为 StoredMessage（`echo-state/src/memory/conversation.rs:12-49`）
- `is_internal_transcript_message()` 过滤系统通知（memory 召回头、verifier 反馈、hook 输出、压缩通知），不写入 ConversationStore（`echo-agent/src/agent/snapshot.rs:20-44`）
- **调用链**：`ReactAgent.run_core_loop` 结束 → `AgentRunSnapshot::save_transcript_projection()` → `ConversationStore`

### 2.5 RuntimeStateStore — 崩溃恢复

```rust
// echo-agent/src/state/mod.rs:154-193
pub trait RuntimeStateStore: Send + Sync {
    fn save_node(...)
    fn load_nodes(...)
    fn update_status(...)
    fn get_checkpoint(...)
    fn save_checkpoint(...)
    fn clear_conversation(...)
}
```
- 实现：`SqliteRuntimeStateStore`（feature `sqlite`）
- 与 `SnapshotManager` 不同：SnapshotManager 是**内存环形缓冲**（进程内回滚），RuntimeStateStore 是**持久化崩溃恢复**

### 2.6 SnapshotManager — 内存快照

```rust
// echo-state/src/memory/snapshot.rs:20-305
StateSnapshot { id, iteration, messages, metadata, created_at }
SnapshotPolicy: EveryIteration / EveryN(usize) / Manual
```
- **环形缓冲**，`capture()` 写入，`rollback()` / `rollback_to()` 回滚
- 每轮 ReAct 后自动 `auto_snapshot(iteration)`
- 配置：`ReactAgentBuilder::snapshot_policy(...)`

### 2.7 上下文压缩 (Context Compression)

**6 种压缩器**（全部位于 `echo-state/src/compression/`）：

| 压缩器 | 策略 | 状态 |
|--------|------|:---:|
| `SlidingWindowCompressor` | 保留最近 N 条消息 | 活 |
| `SummaryCompressor` | LLM 摘要旧消息 | 活 |
| `IncrementalSummaryCompressor` | 增量结构化摘要（10字段）+ 字段级合并 | 活 |
| `HybridCompressor` | 多阶段管道 + 短路 | 活 |
| `VisibilityHorizonCompressor` | 基于轮次的工具追踪压缩 | 活 |
| `AdaptiveCompressor` | L1-L5 级联 + AdaptiveTuner | 活 |

**核心类型**：
- `StructuredSummary`：10字段（goal, current_task, completed_actions, pending_tasks, decisions, files_touched, errors, tool_outputs_summary, user_preferences, next_step）+ `merge_with()` 字段级合并
- `CanonicalContext`：压缩后的系统提示词/规则/技能注入恢复
- `CompressionCheckpoint`：完整审计记录（strategy, covered_range, summary, retained/evicted/protected count, token_before/after 等）

**MemoryPromoter**：压缩驱逐的消息 → `StoreMemoryPromoter` → 提取关键事实 → 写入 Store（`MemorySource::L3Promotion`）

**CLI 命令**：
- `/compress` → `force_compress_with_focus(focus, 6)` 保留最近 6 条
- `/compact` → `force_compress_with_focus(focus, 12)` 保留最近 12 条
- `/context` → 显示 token 使用统计

### 2.8 向量嵌入 & RAG

| 组件 | 文件 | 应用层接入 |
|------|------|:---:|
| `Embedder` trait | `echo-core/src/memory/embedder.rs:10` | ⚠️ 框架自动检测 |
| `HttpEmbedder` | `echo-state/src/memory/embedder.rs:43`（OpenAI 兼容，默认 `text-embedding-3-small`） | ⚠️ 框架自动检测 |
| `EmbeddingStore` | 包装 Store + 余弦相似度 + RRF 融合 | ⚠️ 框架自动检测 |
| RAG 工具 | `echo-tools/src/rag.rs`（rag_index/search/chunk，feature `rag`） | ❌ 未显式接入 |
| LLM Cache | OpenAI/Anthropic 缓存适配器 + `cache_user_id` 分区 | ✅ |

> ⚠️ **精确表述**：EKO 应用层没有显式配置 embedding 专用环境变量（`EMBEDDING_API_KEY`、`EMBEDDING_MODEL` 等），但框架的 `wrap_with_embedding_store_if_available()`（`react/mod.rs:654-689`）会自动检测 `EMBEDDING_API_KEY` / `OPENAI_API_KEY` / `EMBEDDING_APIKEY` 三个环境变量。EKO 的 LLM provider 配置（`infra.rs:962`）确实会读 `OPENAI_API_KEY`——**因此如果用户配了 OpenAI 作为 LLM provider，embedding 会隐式激活**。这属于框架的隐式行为而非 EKO 的显式产品决策。EKO 没有在任何配置文件或 UI 中引导用户配置 embedding 专用变量。

### 2.9 记忆工具（8 个内置工具）

| 工具 | 路径 | 操作 | 状态 |
|------|------|------|:---:|
| `LegacyStoreRememberTool` | 旧 | `store.put(ns, key, value)` | 遗留 |
| `LayeredRememberTool` | **新** | `layer_manager.write_memory(key, content, meta)` | 活跃 |
| `RecallTool` | 旧 | `store.search(ns, query, limit)` | 遗留 |
| `LayeredRecallTool` | **新** | `layer_manager.search_layered(query, limit)` | 活跃 |
| `ForgetTool` | 旧 | `store.delete(ns, key)` | 遗留 |
| `LayeredForgetTool` | **新** | `layer_manager.delete_memory(key)` | 活跃 |
| `SearchMemoryTool` | 旧 | `store.search_with(ns, SearchQuery::hybrid(...))` | 遗留 |
| `LayeredSearchMemoryTool` | **新** | `layer_manager.search_layered(query, limit)` | 活跃 |

> 旧路径直接操作 Store，绕过安全审计和 change log。新路径走 `MemoryLayerManager` 统一 chokepoint。

### 2.10 UnifiedMemory — 应用层指令加载

- `echo-agent-app-core/src/unified_memory.rs:98-246`
- 加载 3 层 `.md` 指令文件（user.md / project.md / local.md），聚合成 system prompt 后缀
- **已知陷阱 #3**：历史 namespace `["agent","memories"]` 与运行时 `[agent_name,"memories"]` 不一致
- **已知陷阱 #4**：`AgentRuntime::new` 不调 `with_store(...)`，动态记忆不可用

---

## 3. 自进化系统 (Self-Evolution)

### 3.1 实时触发器检测 (TriggerDetector) ⚡ 每轮运行

**文件**: `echo-agent/src/evolution/triggers.rs`（733行）

| 触发器 | 检测模式 | 默认置信度 | 生成的 MemoryType |
|--------|---------|-----------|------------------|
| `UserCorrection` | "不是这样"/"不对"/"wrong"/"actually" 等 | 0.90 | UserPreference / DebuggingLesson |
| `ErrorResolution` | 工具失败→换方法成功 | 0.85 | ErrorResolution |
| `RepeatedWorkflow` | 同一工具序列 ≥3 次 | 0.75 | WorkflowPattern |
| `ExplicitSave` | `/remember` 命令 | 1.00 | UserPreference |

**调用链**（框架 react loop 自动触发，应用层通过安装 MemoryLayerManager 使其生效）：
```
react_loop.rs:700 → detect_and_write_memory_triggers(message)
  → TriggerContext { user_message, assistant_message, last_tool_failure, last_tool_success, ... }
  → TriggerDetector::detect() [同步模式匹配]
  → MemoryLayerManager::write_memory() [异步持久化 → 安全审计 → change log → warm store]
```

Tool 成功/失败记录在 `execution.rs:45-99` 的 `record_trigger_data` 中填充。

### 3.2 Dreaming — 定时自进化 ⏰

**文件**: `echo-agent/src/evolution/dreaming.rs`（302行）

**设计参考**：OpenClaw Dreaming（cron + 召回统计驱动晋升）

**逻辑**：每天一次扫描统一 namespace `["agent","memories"]`：
1. **Revive**：Archived 但高召回的记忆 → Active（G2：晋升前置条件）
2. **Promote**：高召回记忆（recall_count ≥ 5）→ Hot 层（MEMORY.md）
3. **Demote**：过期（>30天）低召回 Active → Archived

> ⚠️ **关键发现**：Dreaming 仅在 **Tauri 桌面端**接入（`desktop.rs:241` → `infra.rs:771 spawn_dreaming_task()`）。**CLI REPL 和 TUI 模式完全没有接入。** 如果你只用 CLI 或 TUI，Dreaming 完全不会运行。

### 3.3 BackgroundReviewer — LLM 对话审查 🧠

**文件**: `echo-agent/src/evolution/background_review.rs`（580行）

- 用户显式触发后，LLM 回放 transcript，识别可能长期有价值的信息
- system 指令与带 nonce 的不可信 transcript 分离；只接受严格 JSON 和精确证据引用
- 三个审查 prompt：`MEMORY_REVIEW_PROMPT`、`SKILL_REVIEW_PROMPT`、`COMBINED_REVIEW_PROMPT`
- 默认 proposal-only，不写长期记忆；框架复用方可显式开启高置信用户偏好 Draft 写入
- 单次最大输出 512 token，避免旧实现 2048 token 的浪费

**调用点**（已接入）：
- GUI 面板手动触发：`review_run`
- CLI 命令：`/review`
- TUI 命令：`/run-review`

### 3.4 MemoryRecaller — 统一复合评分召回

**文件**: `echo-agent/src/evolution/recall.rs`（104行）

**评分公式**: `S = 0.5 × sim + 0.3 × decay(age, 30d) + 0.2 × recall_weight`

- Superseded 条目被过滤
- `recall_count` **fire-and-forget 自增**（供 Dreaming 消费）
- **统一了** auto recall 路径和 tool recall 路径（此前两者读不同 namespace、用不同排序）

**调用链**（框架 react loop 自动触发）：
- 每轮上下文组装 → `context.rs:248 → recall_long_term_memories() → MemoryRecaller::recall()`
- Agent 调用 `recall` 工具 → `LayeredRecallTool → MemoryLayerManager::search_layered() → MemoryRecaller::recall()`

### 3.5 MemoryLayerManager — 两层记忆管理核心

**文件**: `echo-agent/src/evolution/layer.rs`（900+行）

```
┌──────────────────────────────────────────────────┐
│  Hot 层: .eko/MEMORY.md                          │
│  YAML frontmatter + markdown body                │
│  最大 2000 tokens，始终加载到 context             │
├──────────────────────────────────────────────────┤
│  Warm 层: Store KV, namespace ["agent","memories"]│
│  落点 {workspace.root}/.eko/memory/store.json     │
│  按需搜索召回（按 workspace 物理隔离）            │
│  Archived 状态也在此 (stage4 移除 Cold 层)        │
└──────────────────────────────────────────────────┘
```

**每次 `write_memory()` 调用路径**：
```
write_memory() → EvolutionSecurityGuard → audit log → warm store →
  write observer → promotion check → hot file (MEMORY.md)
```

被以下组件使用：TriggerDetector、BackgroundReviewer、AutoMemory、remember 工具、Dreaming、MemoryReview。**应用层已深度接入**（agent_pool、task_runtime、auto_memory、review、repl 等全部路径）。

### 3.6 MemoryReview — 过期/冲突/合并/归档

**文件**: `echo-agent/src/evolution/review.rs`（1000+行）

| 组件 | 功能 |
|------|------|
| `StalenessScorer` | `staleness = age×0.35 + low_usage×0.20 + instability×0.20 + contradiction×0.20 + source_weakness×0.05` |
| `ConflictDetector` | 同 topic+type 但不同 content hash → 冲突组 |
| `MemoryMerger` | 合并冲突组，supersede 冗余条目 |
| `SkillCandidateDetector` | 扫描 WorkflowPattern/DebuggingLesson → 技能候选 |
| `SkillDraftGenerator` | 候选 → SKILL.md 草稿 |

**触发路径**：
1. ⚪ **Session-end** — 能力保留但默认关闭，避免和 Dreaming 重复维护
2. ✅ **手动** — CLI/TUI/GUI 用户触发
3. ❌ **Write-count-triggered** — 已删除无效的每次写入 observer

默认 `max_merges_per_review = 0`，冲突只报告、不自动语义合并；低使用度改用真实 `recall_count`，不再误用 `revision_count`。

### 3.7 Curator — 技能生命周期管理

**文件**: `echo-agent/src/evolution/curator.rs`（600+行）

**生命周期状态机**: `Candidate → Draft → Active → Stale → Deprecated → Archived`

- 状态持久化到 `~/.echo-agent/curator_state.json`
- 属于默认 `evolution`，不依赖 `improve`
- CLI 命令群：`/skills`、`/skills promote`、`/skills list`
- **三端全部接入**（CLI/TUI/GUI）

> ⚠️ **关键缺陷**：Curator 仍按全局文件和 skill name 建索引，可能跨 workspace 冲突；技能加载器也未把 Curator lifecycle 当作权威过滤条件。当前它是旁路元数据，不是完整生命周期控制面。

### 3.8 RulePromoter — 记忆→规则晋升

**文件**: `echo-agent-cli/echo-agent-app-core/src/evolution/rule_promoter.rs`（277行）

- `scan_for_proposals()`：扫描 `WARM_NAMESPACE = ["agent","memories"]`（已修复旧 namespace 死链 `["agent","typed_memories"]`），筛选条件：`confidence ≥ 0.95`、类型为 `ProjectFact/WorkflowPattern/UserPreference`、`age ≥ 7天`
- `promote_rule()`：用户审核通过后才写入 `AGENTS.md`
- **已有 review gate**：前端 `scan_rule_proposals` → 用户审阅 → `promote_rule` 才写；CLI `/rule-promote` 同样流程。不是静默自改
- 使用 `EvolutionSecurityGuard` 做可信度校验
- **已接入**（应用层独有模块，被 `InstructionProvider` 使用，Tauri command + CLI 命令均接入）

### 3.9 辅助组件

| 组件 | 文件 | 功能 | 状态 |
|------|------|------|:---:|
| `EvolutionSecurityGuard` | `evolution/security.rs`（790行） | 密钥扫描、注入检测、输入可信度分级 | ✅ |
| `JsonlChangeLog` | `evolution/audit.rs` | 所有变更的 JSONL 审计日志，支持 rollback | ✅ |
| `EvolutionDashboard` | `echo-agent-app-core/src/evolution/dashboard.rs`（296行） | 记忆统计、技能健康概览、近期进化活动 | ✅ 已接入：Tauri command `get_evolution_dashboard` → 前端 `EvolutionPanel` 调用 `evolutionApi.dashboard()` |
| `MemoryScope` | `echo-core/src/memory/scope.rs` | User/Project/Repo/Task/Session/Run 6 级作用域 | ✅ |

---

## 4. 自改善系统 (Self-Improvement)

框架把演进核心与离线评测分开：BackgroundReviewer 与 Curator 位于默认 `evolution`；`TrajectorySaver` 是框架 `improve` 的可选离线导出；Analyzer / ImprovementLoop 还需同时启用 `eval`。EKO 不启用 `improve` 或 `eval`。

### 4.1 Analyzer — 静态 Run 分析

**文件**: `echo-agent/src/improve/analyzer.rs`（230行）

检测 6 种 `CritiqueIssue`：
- `WriteWithoutRead`：写文件但未先读取
- `ExcessiveRetries`：工具过度重试
- `ToolErrorPattern`：同一工具重复失败
- `ContextOverflow`：触发了压缩
- `MissingTool`：应该用但没用某个工具
- `ExcessiveToolCalls`：简单任务过多工具调用

生成 `ImprovementSuggestion`（PromptChange / PolicyChange / EvalGeneration）。这是框架可选能力，EKO 无 CLI 入口。

### 4.2 ImprovementLoop — 迭代提示优化

**文件**: `echo-agent/src/improve/loop.rs`（200行）

- evaluate → detect failures → improve → re-evaluate 循环
- 框架可选能力，EKO 无 CLI 入口

### 4.3 TrajectorySaver — 微调数据收集

**文件**: `echo-agent/src/improve/trajectory.rs`（200行）

- ShareGPT 格式 JSONL 导出，供确有微调数据需求的框架复用方显式调用
- EKO 已删除 `/trajectories`、GUI stats、REPL 自动保存和 `improve` feature
- 不把运行轨迹保存包装成“本地 agent 自改善闭环”

### 4.4 EvalDrivenImprovement / PromptGenerator

**文件**: `echo-agent/src/improve/eval_improvement.rs`、`generator.rs`

- `EvalDrivenImprovement`：统一入口，包装 ImprovementLoop + HTML 报告
- `PromptGenerator`：LLM 驱动提示优化
- 仅在框架同时启用 `improve` + `eval` 时编译；EKO 不启用

---

## 5. 技能系统

### 5.1 EKO 技能系统现状

| 维度 | 状态 |
|------|------|
| **存储格式** | `SKILL.md`（与 Hermes 同格式） |
| **加载策略** | 两条激活路径：LLM 工具调用 / IntentRouter，均是按需加载 |
| **自动生成** | `SkillCandidateDetector` 扫描 memory → `SkillDraftGenerator` 生成 SKILL.md → **需人工 promote** |
| **候选可视化** | ✅ 已接入：Tauri command `scan_skill_candidates` / `generate_skill_draft` / `activate_skill_draft` → 前端 `EvolutionPanel` 调用 `evolutionApi.scanSkillCandidates()` 等 |
| **生命周期管理** | `Curator`：Candidate → Draft → Active → Stale → Deprecated → Archived |
| **健康监控** | ❌ `SkillHealthMonitor` — CLI 命令存在但无遥测数据 |
| **自动修复** | ❌ `SkillPatcher` — 同上 |
| **技能合并** | ❌ `SkillMerger` — 同上 |

**已知陷阱 #2**：Skill 激活两条路径产物不对称。LLM 工具激活产物是 `Role::Tool` + `<skill_content>` XML 包装（受 `protected_marker` 保护）；IntentRouter 激活产物是 `Role::System` + 裸 `instructions`（不受保护）。

### 5.2 与 Hermes 技能来源管理的对比

详见 [§7.4](#74-技能来源差异化管理)。

---

## 6. EKO 实际接入状态验证

> **关键原则**：以下严格区分「框架层 echo-agent 有」和「应用层 echo-agent-cli 实际调用」。标注为"框架自动"的能力由框架 react loop 内部触发，应用层通过注入基础设施（MemoryLayerManager 等）使其生效。

| # | 能力 | 状态 | 接入方式 |
|---|------|:---:|---------|
| 1 | TriggerDetector | ✅ 框架自动 | 框架 react loop 自动触发，应用层通过安装 MemoryLayerManager 使其生效 |
| 2 | Dreaming | ⚠️ 仅桌面端 | `tauri/desktop.rs:241` → `infra.rs:771`。CLI 和 TUI 未接入 |
| 3 | MemoryRecaller | ✅ 框架自动 | 框架 react loop 自动触发（`context.rs:241/479/543`），每轮注入 context |
| 4 | BackgroundReviewer | ✅ 三端显式触发 | GUI、CLI `/review`、TUI `/run-review`；严格 JSON 候选，默认不保存 |
| 5 | MemoryReview/ReviewIntegration | ✅ 手动接入 | 默认不在 session-end 自动运行，默认不做语义合并 |
| 6 | AutoMemory | ✅ 已接入 | `repl.rs:181`、`/auto-memory` CLI 命令、Tauri panel 接口 |
| 7 | Reflection | ✅ 已接入 | `repl.rs:184`、`/reflect` CLI 命令、`runtime.rs:401` |
| 8 | MemoryLayerManager | ✅ 深度接入 | agent_pool、task_runtime、auto_memory、review、repl 等全部路径 |
| 9 | TypedMemory/MemoryMeta/MemoryType | ✅ 已接入 | memory_bridge、rule_promoter、review_integration、dashboard、panels |
| 10 | Curator | ⚠️ 已接入但非权威 | CLI/TUI/GUI 三端；全局 name key 且未控制 loader，需 workspace scope + loader 接线 |
| 11 | SkillHealthMonitor/Patcher/Merger | ⚠️ CLI 命令存在但无数据 | 底层 SkillTelemetryStore 无运行时写入端 |
| 12 | Critic/verify_answer | ❌ 未配置 | 框架有但应用层未配置。仅 `tool_error_feedback`（默认 true）自动生效 |
| 13 | ConversationStore | ✅ 已接入 | `FileConversationStore`（`conversation_file.rs:1`: "EKO is local — no SQLite"）。三端注入，符合 AGENTS.md 硬约束 |
| 14 | ContextManager/compress | ✅ 已接入 | CLI `/compress`/`/compact`、TUI、Tauri、自动压缩 |
| 15 | EmbeddingStore/RAG | ⚠️ 框架自动检测 | 框架 `wrap_with_embedding_store_if_available()` 检测 `OPENAI_API_KEY` 等。EKO 无显式配置，但用户配了 OpenAI key 时会隐式激活。RAG 工具未注册 |
| 16 | EvolutionSecurityGuard | ✅ 已接入 | `rule_promoter.rs` + MemoryLayerManager 内部 |
| 17 | TrajectorySaver | ❌ EKO 未接入 | 仅保留为 echo-agent 框架的可选显式导出 API |
| 18 | RulePromoter | ✅ 已接入 | 应用层独有模块 |

---

## 7. 与 Hermes Agent 的对比分析

### 7.1 记忆层结构映射

| Hermes 五层 | EKO 实际对应 | 差异程度 |
|------------|------------|:---:|
| **Tier 1: USER.md** (~500 tokens) | `UnifiedMemory` 加载 `user.md`（指令注入 system prompt） | 🟡 |
| **Tier 2: MEMORY.md** (~800 tokens) | `MemoryLayerManager` Hot 层 `MEMORY.md`（~2000 tokens） | 🟡 |
| **Tier 3: Skills** (按需加载) | `SkillRegistry` + `Curator`（三端接入） | 🟡 |
| **Tier 4: History** (SQLite FTS5) | `FileConversationStore`（**文件存储**，AGENTS.md 硬约束：EKO 不需要 SQLite） | 🔴 设计取舍不同 |
| **Tier 5: Semantic** (外部向量) | ⚠️ 框架自动检测（用户配了 `OPENAI_API_KEY` 时隐式激活），但非显式产品决策 | 🟡 |

### 7.2 热记忆层对比

| 维度 | Hermes | EKO |
|------|--------|-----|
| **分拆策略** | USER.md（你是谁）+ MEMORY.md（环境/项目事实），语义清晰 | `UnifiedMemory` 加载 3 文件（user/project/local.md），但只有 MEMORY.md 有 Hot 层管理 |
| **容量** | ~1300 tokens（USER 500 + MEMORY 800） | ~2000 tokens |
| **写入者** | Agent 自动整合，写满时合并/丢弃低价值信息 | `Dreaming` 定时扫描 + `MemoryLayerManager` 晋升/降级 |
| **会话一致性** | 修改仅下一会话生效 | 同样设计，通过 Hot 层同步机制保证 |
| **缓存友好** | 明确提到适配 Prefix Cache | LLM Cache 层有适配但未专门为 MEMORY.md 优化 |

**评价**：Hermes 的概念模型更简洁，USER/MEMORY 分拆让用户一眼理解。EKO 的 Hot 层更灵活（YAML 元数据 + 晋升/降级/revive 全生命周期），但概念复杂度更高。Hermes 的 ~1300 tokens 强约束更有利于记忆质量管控。

### 7.3 记忆生命周期管理对比

| 维度 | Hermes | EKO |
|------|--------|-----|
| **被动采集** | 依赖 agent 主动总结 | ✅ TriggerDetector 实时检测 + AutoMemory 关键词提取 |
| **自动晋升** | 写满时手动合并 | ⚠️ Dreaming 基于 recall_count，但仅桌面端运行 |
| **过期淘汰** | ❓ | ✅ StalenessScorer + 30天 stale → archive |
| **召回排序** | FTS 关键词 | ✅ 复合评分 S = 0.5×sim + 0.3×decay(age, 30d) + 0.2×recall_weight |
| **压缩** | 分层架构本身控制 | ✅ 分层 + 6种压缩器 + MemoryPromoter |
| **审计** | ❓ | ✅ JsonlChangeLog 完整审计 + rollback |

**EKO 的优势**：TriggerDetector 被动采集是真正的「越用越聪明」——不需要用户或 agent 主动说「记住」。复合评分排序让召回更精准。

**EKO 的劣势**：Dreaming 仅桌面端运行。向量检索默认不工作。概念体系过于复杂（MemoryLayerManager / Dreaming / MemoryPromoter / MemoryRecaller 等术语对普通用户不友好）。

### 7.4 技能来源差异化管理

这是两个系统在哲学层面的核心差异点。

**Hermes 的做法**：

```
技能来源三分，存储位置统一，通过 sidecar 文件区分：

~/.hermes/skills/
├── plan/               ← bundled    → .bundled_manifest 标记
├── github-skill/       ← hub 安装   → .hub/lock.json 标记
├── my-skill/           ← 用户创建   → 无标记（默认）
├── web-scraping/       ← agent 生成 → .usage.json 中 created_by="agent"

关键设计决策：谁触发创建 → 谁负责管理
  - background_review 触发的 → mark_agent_created → curator 自动管理
  - 用户前台主动触发的 → 不标记 → curator 永不动
  - bundled 技能 → 默认 curator 不管；curator.prune_builtins=true 时才纳入（仅清理，不创建）
  - hub 安装的 → 永不纳入 curator
```

**安全扫描三级信任**：
- `builtin`：官方可选技能，从不扫描
- `trusted`：openai/anthropics/huggingface/NVIDIA skills，从不扫描
- `community`：其他所有来源，76 种威胁 pattern 完整扫描

**EKO 的现状**：

| 维度 | Hermes | EKO |
|------|--------|-----|
| **统一存储** | `~/.hermes/skills/` 所有源一视同仁 | `SkillRegistry` + 外部 skill 文件 |
| **来源标记** | sidecar 外部元数据 | ❓ 需确认 `SkillDescriptor` 是否有来源字段 |
| **Agent 自生成 vs 用户创建的 curator 边界** | ✅ **核心设计**：background review 生成的才进 curator；用户创建的 curator 永不动 | ❌ 无明确设计决策。候选→草稿→人工 promote，但 promote 后 curator 是否自动管理？是否区分来源？ |
| **安全扫描** | 三级信任 + community 完整扫描 | `EvolutionSecurityGuard` 管 memory 不管 skill |
| **渐进式披露粒度** | Tier1: name+desc → Tier2: SKILL.md → Tier3: references | 两条激活路径且产物不对称（已知陷阱 #2） |
| **Bundled 更新保护** | 用户修改过的 bundled 技能更新时跳过保留 | ❓ |
| **Pinned 绕过自动淘汰** | `pinned: true` 标记 | ❓ |

**EKO 需要明确的关键决策**：
1. Agent 自生成技能是否应纳入 curator 自动管理？用户主动创建的技能是否应免于自动淘汰？
2. 技能来源信息（bundled/hub/agent/user）应存储在 SKILL.md frontmatter 内还是外部 sidecar 文件？
3. 如果支持第三方技能安装，是否需要安全扫描？

### 7.5 总体评分

| 维度 | Hermes | EKO | 说明 |
|------|:---:|:---:|------|
| **概念清晰度** | ⭐⭐⭐⭐⭐ | ⭐⭐⭐ | Hermes 五层命名直观；EKO 概念多且复杂 |
| **被动记忆采集** | ⭐⭐⭐ | ⭐⭐⭐⭐⭐ | TriggerDetector + AutoMemory 是真正的自动采集 |
| **记忆生命周期** | ⭐⭐ | ⭐⭐⭐⭐ | Dreaming + MemoryReview 更完整（但 Dreaming 仅桌面端） |
| **召回质量** | ⭐⭐⭐ | ⭐⭐⭐ | Hermes 有外部向量；EKO 有复合评分但无向量 |
| **上下文成本** | ⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | EKO 分层 + 6种压缩器更主动 |
| **技能自进化闭环** | ⭐⭐⭐⭐ | ⭐⭐ | Hermes 全自动；EKO 半自动 + 监控断层 |
| **历史检索** | ⭐⭐⭐⭐ | ⭐⭐ | Hermes SQLite FTS5 vs EKO 文件存储（AGENTS.md 硬约束：EKO 不需要 SQLite） |
| **语义检索** | ⭐⭐⭐⭐ | ⭐⭐ | Hermes 外部向量 vs EKO 隐式激活（非显式产品决策） |
| **隐私/本地化** | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | 两端都是本地优先 |
| **工程审计** | ❓ | ⭐⭐⭐⭐⭐ | JsonlChangeLog + SecurityGuard 很扎实 |

---

## 8. 已知缺口与待修复项

### 8.1 🔴 致命：SkillTelemetry 写入端缺失

**根因**：`echo-state/src/skill_telemetry.rs` 定义了完整的 `SkillExecutionRecord` / `SkillTelemetry` / `SkillTelemetryStore`，但运行时无任何 `record_execution` 调用。grep 结果仅有测试代码调用。

**影响**：以下 3 个组件形同虚设：

| 死组件 | 文件 | CLI 命令 | 症状 |
|--------|------|---------|------|
| `SkillHealthMonitor` | `evolution/health.rs` | `/skill-health` | 永远返回空 → Healthy |
| `SkillPatcher` | `evolution/patch.rs` | `/skill-patch` | 无故障模式可分析 |
| `SkillMerger` | `evolution/merge.rs` | `/skill-merge` | 相似度检测永远返回空 |

**修复方向**：在 `SkillRegistry::activate` 末尾或工具批结束时插入 `record_execution` 调用。

### 8.2 🔴 重要：Dreaming 仅桌面端接入

**现状**：`spawn_dreaming_task()` 仅在 `tauri/desktop.rs:241` 调用。CLI REPL 和 TUI 完全没有接入。

**影响**：非桌面端用户没有定时自进化（自动晋升/降级/revive）。

**修复方向**：在 `repl.rs` 和 `tui/mod.rs` 的初始化路径中同样 `spawn_dreaming_task()`。

### 8.3 🟡 中等：EmbeddingStore/RAG 未作为显式产品决策

**现状**：框架 `wrap_with_embedding_store_if_available()`（`react/mod.rs:654-689`）自动检测 `EMBEDDING_API_KEY` / `OPENAI_API_KEY` / `EMBEDDING_APIKEY`。EKO 的 LLM provider 配置（`infra.rs:962`）会读 `OPENAI_API_KEY`——因此用户配了 OpenAI 作为 LLM provider 时，embedding 会**隐式激活**。但 EKO 没有在配置文件或 UI 中引导用户配置 embedding 专用变量（`EMBEDDING_MODEL` 等），也没有文档说明这一行为。

**影响**：向量语义检索的可用性取决于用户是否碰巧配了 OpenAI key，而非 EKO 的显式产品决策。RAG 工具（`rag_index` 等）则完全未注册。

**修复方向**：要么显式接入（在配置中增加 embedding 配置项 + UI 引导），要么显式禁用（在应用层覆盖框架的自动检测），不应停留在隐式状态。

### 8.4 🟡 中等：Critic/verify_answer 未配置

**现状**：`Critic` trait、`LlmCritic`、`verify_answer` 在框架中完整存在，但应用层从未调用 `set_critic`（`react/mod.rs:1337`，`&mut self` 方法）。仅 `tool_error_feedback`（默认 true，`config.rs:219`）自动生效——它把工具失败反馈给 LLM 自修正，但不等同于 Critic 的「答案自检→不达标就重试」机制。

**影响**：EKO 没有 Critic 回路。

**修复方向**：`set_critic` 是 `&mut self` 方法，而 EKO 使用 `AgentHandle`（Arc 包裹）+ `AgentPool` 架构。直接调用需要获取可变引用，不能简单地在 builder 链中加一行。需要在 agent 构建阶段（`infra.rs` 的 `build_agent` 系列函数中）通过 builder 注入，或改造 `AgentPool` 支持 critic 的延迟注入。实际执行前需单独做方案设计。

### 8.5 🟡 中等：技能来源缺乏 curator 边界

见 [§7.4](#74-技能来源差异化管理)。当前无明确设计决策区分「agent 自生成技能」和「用户主动创建技能」的 curator 管理策略。

### 8.6 🟡 中等：已知陷阱汇总

（详见 `docs/system-deep-dive/07-cross-cutting.md`）

| # | 陷阱 | 简述 |
|---|------|------|
| 1 | `"plan"` 工具空缺 | `TOOL_PLAN` 常量声明但无任何生产路径注册 |
| 2 | Skill 激活路径分裂 | LLM 工具激活 vs IntentRouter 激活产物不对称 |
| 3 | ~~Namespace 不一致~~ | **已过时**：`UnifiedMemory` 的 `remember/recall/forget/list_memories` 方法已完全移除（`unified_memory.rs:1-8` 注释确认），不再使用任何 namespace。动态记忆由 `MemoryLayerManager` 统一管理 |
| 4 | ~~UnifiedMemory store=None~~ | **已过时**：`with_store()` 已移除，`UnifiedMemory` 现在只做指令加载（`.md` 文件 → system prompt），不持有 Store |
| 5 | skill_telemetry 无写入点 | 本报告 §8.1 |
| 6 | AgentRole 在 ReactAgent 无效 | 仅 TaskExecutor 分支 |
| 7 | protected_marker try_lock 静默跳过 | 并发时可能丢失注册 |
| 8 | compress_threshold_ratio 未接入 | `ContextManager::prepare()` 不读该配置 |
| 9 | SkillsHub CLI 各自 new() | 不共享 AppState 实例 |

### 8.7 🟢 低：遗留旧记忆工具路径

`LegacyStoreRememberTool` / `ForgetTool` / `RecallTool` / `SearchMemoryTool` 仍编译且可注册，但新路径走 `Layered*Tool` 系列。旧路径直接操作 Store，绕过安全审计。可考虑清理或统一到新路径。

### 8.8 🟢 低：废弃的 feature 和概念

- `semantic-memory` feature：Cargo.toml 中定义但为空，无代码关联
- `MemoryLayer::Cold` variant：保留在 enum 中但默认路径不使用（已合并到 Warm + Archived 状态）

### 8.9 当前真实缺口优先级（经代码复查修正后）

以下排序基于「已修掉误报（SQLite、RulePromoter、Dashboard、review gate、技能候选可视化均已接入）」后的剩余硬缺口：

1. **✅ ~~SkillTelemetry 生产写入端~~**（已完成）：在 `execution.rs::execute_with_pipeline` 的 4 个分支（成功/失败 soften/失败不 soften/pipeline 错误）中插入 `record_skill_telemetry` 调用，fire-and-forget 写入 `SkillTelemetryStore`。同时桥接 `curator.touch_skill` 刷新 `last_used_at`（阶段 6.1）。
2. **✅ ~~Dreaming 多模式对等~~**（已完成）：CLI REPL（`repl.rs` banner 后 spawn、session 结束前 cancel）、TUI（`tui/mod.rs` event loop 前 spawn、返回后 cancel）均已接入 `spawn_dreaming_task`。`run_cli_mode` 签名增加 `review_integration` 参数，`ReplConfig` 增加对应字段。
3. **✅ ~~Evolution hook fire 点~~**（已完成）：新增 `echo-agent-app-core/src/evolution/hook_fire.rs` 的 `fire_evolution_hook` helper。CLI `cmd_rule_promote` / `cmd_skill_merge` / `cmd_skill_patch` + Tauri `promote_rule` 成功后 fire 对应事件（RulePromoted / SkillMergeApplied / SkillPatchApplied）。`SkillPatchApplied` 在 SkillPatcher apply_patch 实现后已补齐。
4. **✅ ~~Embedding/RAG 产品化决策~~**（已决策：暂不支持）。EKO 定位为个人本地 Cowork agent，同类产品（Claude Code、Codex、Cursor、OpenClaw 等）均未使用 RAG。框架保留 EmbeddingStore/HttpEmbedder/RAG 能力供其他复用方，EKO 应用层不接入，保持纯关键词检索。
5. **✅ ~~Critic 默认策略~~**（已完成）：在 `infra.rs::create_agent` 中注入 `LlmCritic::new(model).with_pass_threshold(7.0)` + `config_mut().set_verifier_enabled(true)`。框架 `config.rs` 新增 `set_verifier_enabled/min_score/max_retries` 三个 setter。Critic 的 LLM 调用复用主 agent 的同一 model 配置，出错时 fail-open 不阻塞主流程。
6. **✅ ~~技能来源 curator 边界~~**（已完成）：`SkillMeta.agent_created` 字段 + `apply_transitions` 的 `!agent_created { continue }` 已天然提供边界。新增 `/skill-register`（标记 `agent_created=false`）、`/skill-pin`、`/skill-unpin` CLI 命令。telemetry → curator `last_used_at` 桥接已补。`SkillPatcher::apply_patch` 已实现 + `/skill-patch <name> apply <idx>` 命令已接入。

---

## 附录 A：关键文件索引

### 框架层 (echo-agent)

| 模块 | 文件路径 |
|------|---------|
| Store trait | `echo-core/src/memory/store.rs` |
| ConversationStore trait | `echo-core/src/memory/conversation.rs` |
| MemoryType/MemoryMeta | `echo-core/src/memory/types.rs` |
| MemoryScope | `echo-core/src/memory/scope.rs` |
| Embedder trait | `echo-core/src/memory/embedder.rs` |
| ContextCompressor trait | `echo-core/src/compression.rs` |
| InMemoryStore / FileStore | `echo-state/src/memory/store.rs` |
| SqliteStore | `echo-state/src/memory/sqlite_store.rs` |
| EmbeddingStore | `echo-state/src/memory/embedding_store.rs` |
| HttpEmbedder | `echo-state/src/memory/embedder.rs` |
| SqliteConversationStore | `echo-state/src/memory/sqlite_conversation.rs` |
| TypedMemoryStore | `echo-state/src/memory/typed_store.rs` |
| SkillTelemetry | `echo-state/src/skill_telemetry.rs` |
| SnapshotManager | `echo-state/src/memory/snapshot.rs` |
| ContextManager | `echo-state/src/compression/mod.rs` |
| 压缩器 | `echo-state/src/compression/compressor/*.rs` |
| SummaryVerifier | `echo-state/src/compression/verifier.rs` |
| RuntimeStateStore trait | `echo-agent/src/state/mod.rs` |
| SqliteRuntimeStateStore | `echo-agent/src/state/sqlite.rs` |
| AgentRunSnapshot | `echo-agent/src/agent/snapshot.rs` |
| MemorySubsystem | `echo-agent/src/agent/react/subsystems/memory.rs` |
| 记忆工具 | `echo-agent/src/tools/builtin/memory.rs` |
| StoreMemoryPromoter | `echo-agent/src/memory_promoter.rs` |
| RAG 工具 | `echo-tools/src/rag.rs` |
| evolution/mod.rs | `echo-agent/src/evolution/mod.rs`（入口，导出全部子模块） |
| TriggerDetector | `echo-agent/src/evolution/triggers.rs` |
| Dreaming | `echo-agent/src/evolution/dreaming.rs` |
| BackgroundReviewer | `echo-agent/src/evolution/background_review.rs` |
| MemoryRecaller | `echo-agent/src/evolution/recall.rs` |
| MemoryLayerManager | `echo-agent/src/evolution/layer.rs` |
| MemoryReviewer | `echo-agent/src/evolution/review.rs` |
| Curator | `echo-agent/src/evolution/curator.rs` |
| AutoMemory (框架) | `echo-agent/src/evolution/auto_memory.rs` |
| SkillHealthMonitor | `echo-agent/src/evolution/health.rs` |
| SkillPatcher | `echo-agent/src/evolution/patch.rs` |
| SkillMerger | `echo-agent/src/evolution/merge.rs` |
| SkillCandidateDetector | `echo-agent/src/evolution/candidate.rs` |
| SkillDraftGenerator | `echo-agent/src/evolution/draft.rs` |
| EvolutionSecurityGuard | `echo-agent/src/evolution/security.rs` |
| JsonlChangeLog | `echo-agent/src/evolution/audit.rs` |
| RuntimeIntegration | `echo-agent/src/evolution/runtime_integration.rs` |
| improve/mod.rs | `echo-agent/src/improve/mod.rs`（入口） |
| Analyzer | `echo-agent/src/improve/analyzer.rs` |
| TrajectorySaver | `echo-agent/src/improve/trajectory.rs` |
| ImprovementLoop | `echo-agent/src/improve/loop.rs` |
| EvalDrivenImprovement | `echo-agent/src/improve/eval_improvement.rs` |
| PromptGenerator | `echo-agent/src/improve/generator.rs` |
| Critic/LlmCritic | `echo-agent/src/agent/critic/` |
| verify_answer | `echo-agent/src/agent/react/run/phases/verify.rs` |

### 应用层 (echo-agent-cli)

| 模块 | 文件路径 |
|------|---------|
| UnifiedMemory | `echo-agent-app-core/src/unified_memory.rs` |
| AutoMemory (应用) | `echo-agent-app-core/src/auto_memory/mod.rs` |
| RulePromoter | `echo-agent-app-core/src/evolution/rule_promoter.rs` |
| ReviewIntegration | `echo-agent-app-core/src/evolution/review_integration.rs` |
| Dashboard | `echo-agent-app-core/src/evolution/dashboard.rs` |
| MemoryBridge | `echo-agent-app-core/src/tasks/task_runtime/memory_bridge.rs` |
| Reflection | `echo-agent-app-core/src/runtime.rs:401-443` |
| AgentPool (注入) | `echo-agent-app-core/src/agent_pool.rs` |
| Dreaming spawn | `echo-agent-app-core/src/infra.rs:771` |
| ConversationStore | `echo-agent-app-core/src/conversation_file.rs` |
| REPL hooks | `echo-agent-cli/src/cli/repl.rs` |
| CLI evolution 命令 | `echo-agent-cli/src/cli/cmd_impls/evolution.rs` |
| CLI context 命令 | `echo-agent-cli/src/cli/cmd_impls/context.rs` |
| CLI all 命令 | `echo-agent-cli/src/cli/cmd_impls/all.rs` |
| Tauri memory commands | `echo-agent-cli/src/tauri/commands/memory.rs` |
| Tauri panels | `echo-agent-cli/src/tauri/commands/panels.rs` |
| Tauri desktop 启动 | `echo-agent-cli/src/tauri/desktop.rs` |
| TUI events | `echo-agent-cli/src/tui/events.rs` |
| 配置文件 | `echo-agent-cli/config/echo-agent.yaml` |

### 文档

| 文档 | 路径 |
|------|------|
| 记忆系统深度 | `echo-agent-cli/docs/system-deep-dive/04-memory.md` |
| 压缩系统深度 | `echo-agent-cli/docs/system-deep-dive/05-compression.md` |
| 技能系统深度 | `echo-agent-cli/docs/system-deep-dive/06-skills.md` |
| 跨切面陷阱 | `echo-agent-cli/docs/system-deep-dive/07-cross-cutting.md` |
| 运行时架构审计 | `echo-agent-cli/docs/runtime-architecture-audit.md` |
| 框架记忆文档 | `echo-agent/docs/zh/03-memory.md` |
| 框架压缩文档 | `echo-agent/docs/zh/04-compression.md` |

---

## 附录 B：会话生命周期事件流

```
用户开启会话 (REPL/TUI/GUI)
  │
  ├─ spawn_dreaming_task() → 每日定时自进化（三端均已接入）
  │
  ├─ UnifiedMemory::load() → 加载 user.md/project.md/local.md → system prompt
  │
  ├─ MemoryRecaller::recall() → 复合评分召回 → 注入 context [框架自动]
  │
  ├─ [每轮 ReAct]
  │   ├─ record_trigger_data() → 记录工具成功/失败
  │   ├─ record_skill_telemetry() → 遥测写入 + curator touch_skill [框架自动]
  │   ├─ detect_and_write_memory_triggers() → 触发器检测 → EvidenceCandidate sink
  │   ├─ ContextManager::prepare() → 压缩判断 + MemoryPromoter
  │   ├─ auto_snapshot() → SnapshotManager::capture()
  │   ├─ tool_error_feedback → 工具失败反馈给 LLM 自修正
  │   └─ verify_answer → LlmCritic 自检 (score≥7.0 通过, 最多重试2次, fail-open)
  │
  └─ [会话结束]
      ├─ run_auto_memory_on_exit() → 关键词提取 → Review Inbox JSONL
      ├─ run_reflection_on_exit() → LLM 轻量反思 → memory 文件
      ├─ MemoryReview 默认不在退出时自动运行（用户显式触发）
      ├─ cancel dreaming_task → 停止后台自进化
      ├─ save_transcript_projection() → ConversationStore
      └─ 不自动保存微调 trajectory
```

---

> **文档维护**: 本报告基于 2026-07-01 代码库状态编写。代码变更后请更新对应章节，标注更新日期和变更内容。

---

## 附录 C：迭代计划

> 基于 §8.9 的缺口优先级，以下是为 EKO 记忆/自进化系统制定的分阶段迭代计划。
> 每个阶段是一个可独立交付的里程碑——完成后提交 + 更新本文档 + 更新 `docs/MASTER-PLAN.md`。

### 阶段 1：SkillTelemetry 生产写入端（P0 — 修复断裂的神经）

**目标**：让 `SkillHealthMonitor` / `SkillPatcher` / `SkillMerger` 三个僵尸组件获得真实数据。

**问题本质**：`echo-state/src/skill_telemetry.rs` 有完整的 `SkillExecutionRecord` / `SkillTelemetryStore` 读写 API，但运行时**无任何 `record_execution` 调用**。三个组件的 CLI 命令（`/skill-health`、`/skill-patch`、`/skill-merge`）永远读到空数据。

**关键决策（动手前必须先调研）**：

- 调研 Hermes 的 `skill_usage.py` 如何记录技能使用——它是 Python 的、基于文件 sidecar 的遥测；EKO 是 Rust 的、基于 `Store` trait 的遥测，两者形态不同但目标一致（记录成功/失败/频率/最近使用时间）。
- 确认写入点选择：`SkillRegistry::activate` 末尾 vs 工具批结束时。需要判断哪个位置能获取到「技能执行结果」。
- 确认归并维度：按 skill name 归并，还是按 skill + tool_call 归并。

**实施范围**：

| 步骤 | 内容 | 涉及文件 |
|------|------|---------|
| 1 | 在 skill 激活路径末尾插入 `record_execution` 调用 | `echo-agent/src/agent/react/` 或 `echo-execution/src/skills/` |
| 2 | 记录成功/失败/耗时/错误信息 | 同上 |
| 3 | 验证 `/skill-health` 能读到真实数据 | `echo-agent-cli/src/cli/cmd_impls/evolution.rs` |
| 4 | 验证 Dashboard 中技能健康概览有数据 | `echo-agent-app-core/src/evolution/dashboard.rs` |

**验收标准**：运行一次使用 skill 的对话后，`/skill-health` 显示非空遥测数据。

**风险**：写入路径不能阻塞主循环（fire-and-forget，类似 `MemoryRecaller` 的 `recall_count` 自增模式）。

---

### 阶段 2：Dreaming 多模式对等（P0 — TUI/CLI 接入）

**目标**：TUI 和 CLI REPL 也启动 `spawn_dreaming_task()`，符合 AGENTS.md「TUI 与 GUI 功能对等」硬约束。

**问题本质**：`spawn_dreaming_task()` 定义在 `infra.rs:771`，但只有 `tauri/desktop.rs:241` 调用了它。CLI REPL（`repl.rs`）和 TUI（`tui/mod.rs`）完全没有调用。

**实施范围**：

| 步骤 | 内容 | 涉及文件 |
|------|------|---------|
| 1 | 在 CLI REPL 初始化路径中调用 `spawn_dreaming_task` | `echo-agent-cli/src/cli/repl.rs` 或 `src/main.rs` |
| 2 | 在 TUI 初始化路径中调用 `spawn_dreaming_task` | `echo-agent-cli/src/tui/mod.rs` |
| 3 | 确保三端的 `ReviewIntegration` 实例一致（Dreaming 依赖它创建 layer_manager） | 可能需要调整 `infra.rs` 的 `spawn_dreaming_task` 签名 |

**验收标准**：CLI/TUI 模式下日志可见 "Dreaming pass completed"，且记忆能在非桌面端也被自动晋升/降级。

**注意**：Dreaming 的 interval 是 86400s（每天一次）+ 60s 初始延迟。验收时可能需要临时缩短 interval 或手动触发一次。

---

### 阶段 3：Evolution hook fire 点（P1 — 打通事件回路）

**目标**：让 `SkillPatchApplied` / `SkillMergeApplied` / `RulePromoted` 三个事件在实际操作发生时被 fire。

**问题本质**：三个事件在 `HookEvent` enum 中定义了（`echo-core/src/hooks/types.rs:131-135`），`context.rs:399-401` 有对应的 `HookContext::for_lifecycle` 分发逻辑，但**无任何代码调用 `fire_lifecycle_hook` 传入这些事件**。

**关键决策（动手前必须先调研）**：

- 调研 Hermes 的 curator 在执行 archive/consolidate 后是否有类似事件通知机制。
- 确认 fire 点：`SkillPatcher` 执行 patch 后 fire `SkillPatchApplied`、`SkillMerger` 执行合并后 fire `SkillMergeApplied`、`RulePromoter::promote_rule` 执行后 fire `RulePromoted`。
- 确认谁消费这些事件：目前没有已注册的 hook 消费者——fire 了但没人听。需要判断是否需要先设计消费者（如 Dashboard 自动刷新、change log 自动追加），还是先 fire 再后续接入。

**实施范围**：

| 步骤 | 内容 | 涉及文件 |
|------|------|---------|
| 1 | 在 `RulePromoter::promote_rule` 末尾 fire `RulePromoted` | `echo-agent-app-core/src/evolution/rule_promoter.rs` |
| 2 | 在 SkillPatcher / SkillMerger 执行后 fire 对应事件 | `echo-agent/src/evolution/patch.rs`、`merge.rs` 或应用层调用点 |
| 3 | 验证 fire 后 `run_lifecycle_hooks` 能被触发 | 测试 |

**验收标准**：执行 `/rule-promote` 后，日志可见 `RulePromoted` 事件被分发。

**前置依赖**：阶段 1（SkillPatcher/SkillMerger 需要有数据才会被实际调用）。

---

### 阶段 4：Embedding/RAG 产品化决策（P2 — 显式化）

**目标**：消除 embedding 的「隐式激活」状态——要么显式接入，要么显式禁用。

**问题本质**：框架 `wrap_with_embedding_store_if_available()` 自动检测 `OPENAI_API_KEY` 等环境变量。EKO 的 LLM provider 配置会读 `OPENAI_API_KEY`，因此用户配了 OpenAI 时 embedding **隐式激活**。这不是显式产品决策。

**关键决策（需要用户拍板）**：

- **选项 A：显式接入**。在 `echo-agent.yaml` 中增加 embedding 配置项（`embedding_model`、`embedding_base_url` 等），在 UI 中引导用户配置。语义检索成为正式能力。
- **选项 B：显式禁用**。在应用层覆盖框架的自动检测，确保 embedding 不会隐式激活。保持纯关键词检索。
- **选项 C：维持现状 + 文档化**。在配置文档中说明"配了 OpenAI key 就自动有 embedding"，不做代码改动。

**建议**：先做选项 C（文档化），等产品方向明确后再做 A 或 B。这不是技术问题，是产品决策。

---

### 阶段 5：Critic 默认策略（P2 — 成本/体验权衡）

**目标**：决定是否给 EKO 默认启用 Critic 回路。

**问题本质**：`set_critic` 是 `&mut self` 方法，EKO 使用 `AgentHandle`（Arc 包裹）+ `AgentPool` 架构。不能简单地在 builder 链中加一行。需要在 agent 构建阶段（`infra.rs` 的 `build_agent` 系列函数）通过 builder 注入。

**关键决策（动手前必须先调研）**：

- 调研 Claude Code 的 self-verification 机制——它是否在每次 final_answer 后都做 LLM 评审？还是只在特定条件下？
- 调研 Hermes 是否有类似 Critic 机制。
- 成本评估：每次 final_answer 多一次 LLM 调用（critique），延迟 + token 成本翻倍。
- 打扰性评估：如果 Critic 频繁拒绝答案，用户会感觉 agent "反应慢、改来改去"。
- 阈值设计：Critic 的 score 阈值设多少？过低无意义，过高频繁拒绝。

**实施范围（如果决定启用）**：

| 步骤 | 内容 | 涉及文件 |
|------|------|---------|
| 1 | 在 `build_agent` 系列函数中通过 builder 注入 `LlmCritic` | `echo-agent-app-core/src/infra.rs` |
| 2 | 增加配置项控制是否启用 Critic + 阈值 | `echo-agent.yaml`、`AgentConfig` |
| 3 | 验证 Critic 回路在 streaming/non-streaming 路径都生效 | `verify.rs` |

**验收标准**：启用后，agent 对明显错误的答案能自检并重试；对正确答案不频繁拒绝。

---

### 阶段 6：技能来源 curator 边界设计（P2 — 设计决策）

**目标**：明确 EKO 是否需要像 Hermes 那样区分「agent 自生成技能」和「用户主动创建技能」的 curator 管理策略。

**问题本质**：当前 EKO 的 `SkillCandidateDetector → SkillDraftGenerator → 人工 promote` 流程生成技能后，curator 是否自动管理它？是否区分来源？无明确设计决策。

**关键决策（动手前必须先调研）**：

- 仔细研究 Hermes 的 `_is_curator_managed_record` 逻辑：只有 `created_by == "agent"` 的技能才进 curator。用户手动创建的永不动。
- 评估 EKO 是否需要同样的边界：如果 curator 自动 archive 了用户主动创建的重要技能 → 信任崩塌。如果 curator 从不清理 agent 自动生成的低质量技能 → 技能目录变垃圾场。
- 评估 EKO 的 `SkillMeta` 结构体是否已有足够的来源字段（`source`、`status` 等），还是需要新增。

**实施范围（如果决定实施）**：

| 步骤 | 内容 | 涉及文件 |
|------|------|---------|
| 1 | 在 `SkillMeta` 或 sidecar 中增加来源标记（agent_created vs user_created） | `echo-agent/src/evolution/curator.rs` |
| 2 | curator 自动 transitions 只作用于 `agent_created` 技能 | `echo-agent/src/evolution/curator.rs` |
| 3 | 增加 `pinned` 机制让用户保护重要技能 | 同上 |
| 4 | CLI/UI 增加 pin/unpin 命令 | `echo-agent-cli/src/cli/cmd_impls/evolution.rs`、`panels.rs` |

**前置依赖**：阶段 1（SkillTelemetry 有数据后，curator 的自动 transitions 才有意义）。

---

### 阶段 7：遗留清理（P3 — 降低维护负担）

**目标**：清理已确认的死代码和过时概念。

| 步骤 | 内容 | 涉及文件 |
|------|------|---------|
| 1 | 评估旧记忆工具（`LegacyStoreRememberTool` 等 4 个）是否可删 | `echo-agent/src/tools/builtin/memory.rs` |
| 2 | 删除空的 `semantic-memory` feature | `echo-agent/Cargo.toml` |
| 3 | 评估 `MemoryLayer::Cold` variant 是否可删（保留为 pub API 但默认不使用） | `echo-agent/src/evolution/layer.rs` |
| 4 | 更新 `07-cross-cutting.md` 中已过时的陷阱 #3/#4 | `echo-agent-cli/docs/system-deep-dive/07-cross-cutting.md` |

**原则**：按 AGENTS.md「代码清理」规则——无需兼容，过时代码可直接删。但框架层 pub API 删除前需确认无其他复用方依赖。

---

### 迭代节奏建议

| 阶段 | 优先级 | 建议窗口 | 依赖 |
|------|--------|---------|------|
| 1. SkillTelemetry 写入端 | P0 | 新窗口 | 无 |
| 2. Dreaming 多模式对等 | P0 | 同窗口或新窗口 | 无 |
| 3. Evolution hook fire 点 | P1 | 新窗口 | 阶段 1 |
| 4. Embedding/RAG 产品化决策 | P2 | 不阻塞，产品决策 | 无 |
| 5. Critic 默认策略 | P2 | 不阻塞，需方案设计 | 无 |
| 6. 技能来源 curator 边界 | P2 | 新窗口 | 阶段 1 |
| 7. 遗留清理 | P3 | 低优先，随手做 | 无 |

> 阶段 1 和 2 互相独立，可以在不同窗口并行推进。阶段 3 和 6 依赖阶段 1。阶段 4/5 是产品决策，不阻塞技术工作。阶段 7 随时可做。
>
> 每个阶段完成后：(1) 提交 git；(2) 更新本文档对应章节的状态；(3) 更新 `docs/MASTER-PLAN.md`。
