# EKO 记忆与自进化接缝收口

> 日期：2026-07-23  
> 范围：`echo-agent` 通用记忆/演进原语 + `echo-agent-cli` 产品接线  
> 结论：总体分层与人工审阅边界正确；本轮不重做架构，集中修复动态上下文、命名空间、工作区权威和重复写入接缝。

## 1. 业界参考与本项目取舍

- Claude Code 将项目指令和 auto memory 视为互补上下文；auto memory 可在会话中读写，加载有 200 行或 25 KiB 预算，并明确说明指令文件是 context 而不是 enforcement。参考：<https://code.claude.com/docs/en/memory>。
- Letta Dreaming 由后台 subagent 在用户消息数或上下文压缩等事件后整理 memory，主 Agent 不需要等待后台工作。参考：<https://docs.letta.com/configuration/memory/>。
- EKO 因此采用“文件是权威、上下文是可替换投影”的方式：工作区指令和 hot memory 仍保存在普通文件中，但不再永久烘焙进 boot-time system prompt。Dreaming 或人工 hot-layer 变更后，当前 Agent 和池化 Agent 立即刷新投影。
- 不新增 plan/approval/runtime 状态机，不引入后台 LLM 整理器。Dreaming 仍是确定性、低成本维护；语义合并、规则和技能激活仍经过现有 Review Inbox/人工操作。

## 2. 已实施修复

### P0：当前会话立即看到正确的工作区上下文

- `UnifiedMemory` 删除恒空 `MemoryContext.memories`、`hot_content` 缓存和无人调用的 `refresh_hot()`。
- user/project/local/AGENTS/hot memory 改为 `eko:instruction-context` 可替换投影；项目结构和 git 状态改为独立 `eko:project-context` 投影。
- boot、工作区切换、退出工作区、Dreaming hot promotion、人工 hot memory 新增/删除、规则晋升都会刷新当前 Agent；池内已有 Agent 同步刷新，未来新建 Agent 继承当前 `working_dir`。
- Dreaming 在启动稳定 60 秒后先执行一次，再按日执行；GUI、TUI、CLI 使用同一接线。

### P0/P1：唯一产品写入口与统一命名空间

- EKO 的 CLI `/remember`、TUI `/remember` 和 Tauri `add_memory` 全部调用 `MemoryLayerManager::write_memory`，写入 `ProjectFact + ExplicitSave` 类型化元数据。
- list/search/forget/delete 统一覆盖 hot + warm，并使用 `MemoryLayerManager::delete_memory`；EKO 产品面只暴露 `agent/memories`。
- 框架 raw Store tools 仍保留，作为没有安装 evolution layer 的框架消费者的合理降级能力；其默认 namespace 已统一为 `['agent', 'memories']`，且不会再覆盖已安装的 layered forget tool。

### P0/P1：工作区权威与路径一致性

- `ReactAgent` 可注入通用 `Curator`；skill telemetry 优先使用该实例。EKO bootstrap、工作区 rebind、退出工作区和池 Agent 都绑定共享 `ReviewIntegration.curator()`，不再把使用时间写入另一份全局状态。
- CLI/TUI `/memory-review` 复用运行时共享 `ReviewIntegration`，不再现场构造可能指向错误目录的新实例。
- GUI、TUI、CLI 的 evolution changelog 全部归一为 `evolution/change-log.jsonl`，Tauri 面板优先使用共享 integration 的当前目录。

### P1/P2：压缩事实去重与死代码清理

- 启发式 `StoreMemoryPromoter` 和 LLM `pre_compaction_flush` 共用经过 trim 的稳定 FNV content key；相同内容已存在时不重复写入，也不重置原有 metadata/telemetry。
- 修正相关 UTF-8 字符计数与截断，移除字节切片。
- 删除已被 `evolution` 完全取代的 `improve::background_review` re-export shim 和过时的“30 天 TTL”说明。

## 3. 审阅中未按原建议删除的部分

- `LegacyStoreRememberTool/RecallTool/ForgetTool/SearchMemoryTool` 是框架层对未安装 evolution layer 的合理公开选项，不按“EKO 不调用”判死。EKO 产品路径已经只走 layered 入口。
- `MemoryLayer::Cold` / `COLD_NAMESPACE` 是框架为三层复用方保留的公共选项；EKO 默认路径继续用 Warm + Archived，不新增 app 侧 cold 逻辑。
- `StalenessSuggested` 保持 analysis/proposal-only。Dreaming 的确定性归档和 MemoryReviewer 的人工建议职责不同，不自动应用建议，避免把语义维护变成隐藏写操作。
- EKO 当前 `create_memory_store_at` 使用普通 `FileStore`，没有默认启用向量库。Embedding/hybrid 继续作为框架可选能力，本轮不做无收益的存储重构。

## 4. 后续方向

- 先观察真实长会话中动态投影刷新、Dreaming 首次执行和工作区频繁切换的 telemetry。
- hot layer 已有 2,000 token 总预算；条目级长度、200 行/25 KiB 文件预算需结合真实膨胀数据再定，避免未经证据直接截断长期事实。
- 若未来引入更频繁的 idle/session-end Dreaming，优先复用现有确定性 pass 和刷新入口，不新增第二套后台 reviewer。

