# ADR 0060：TaskRuntime 子执行原生消息观察

## 状态

实施中。用户于本轮明确要求 EkoAgentTurnSink 接通 canonical 原生消费；静态、定向前端、真实 Rust、native 和 main CI 分别取得资格。

## 背景

TaskRuntime 主 Agent 执行 PlanTask 的路径经 unattended→EkoAgentTurnSink::for_primary_task→on_event，RunTurn 经 events→for_run。旧 sink 只映射 Token／ThinkStart／ThinkEnd、工具和 FinalAnswer；新的 AssistantMessage／ExecutionStatus 被忽略。旧 Subagent store 白名单与详情时间线也只接 thinking/token，因此最终文本存在不能证明原生进度、阶段、身份和状态已经支持。

## 候选方案

1. 把原生 phase 重新压回 thinking/token 字符串：丢失原 block／request／provider 来源、顺序和签名，且会重新依赖 Think 开关推断语义。
2. 新建 Subagent 消息 store、完成状态机或执行器：重复原 TaskRuntime 和 journal 权威。
3. 复用原结构化 Note 观察、ExecEvent、TaskRuntime journal 与现有 Subagent store，保留完整 canonical envelope，仅扩展投影与呈现。

## 决策

采用方案三。payload.kind 为 native_agent_event，保原 execution_id、agent 与完整 agent_envelope。使用原 TaskRuntimeOperation 在原 journal 提交 Note，再经原 ExecSink／ChatEventLog 送 live；重开从同一 TaskRuntime Note 及既有工具边界重建 Subagent 时间线。没有新增 RuntimeEventKind、生成枚举、store、executor 或 lifecycle。手写 frontend 类型复用已有 AgentEventEnvelope。旧生成的 JsonValue 联合遗漏 JSON null／boolean，无法表达原生签名元数据；因此 RuntimeTaskEvent.payload 按 ExecEvent 已有方式添加 ts-rs 的 unknown 注解，同步预期生成 RuntimeTaskEvent.ts。这只修正 TypeScript 对现有 JSON payload 的过窄描述，wire、Rust serde 与生成入口均不变；EKO主写须在原统一生成链复验，候选手工匹配不能替代生成资格。

sink 对已知TaskRuntime owner要求原 run／conversation／execution／turn完整匹配，缺失或错误身份拒绝。原提交锁内以256事件页查原journal的同一event_id，完整payload相同才复用，冲突报错；单sink缓存只减少重复读取，不是持久去重权威。重建sink后重送不重复写journal；此线性分页核对尚未取得性能资格。保存或投递失败仍走原错误／owner 结算，不将它解释成成功。AssistantMessage 的 phase 和 reasoning 直接取原块，签名、opaque、provider_source、request/block/sequence/delta 不压成普通 token；兼容 Token 不再与已采用的 native 重复输出。原 Think 历史分支保兼容读取，不用于解释新块。

## 取舍与影响

Note 仅表示观察，ExecutionStatus Settled、Cancelled、Failed 和 FinalAnswer 不改变 TaskRun／Subagent 的状态；最终任务成功仍由原 physical receipt 和 task lifecycle 确认。已有终态 hydration 后，尚未见过的较早canonical历史和原工具引用可按原TaskRuntime sequence补齐时间线；已见live事件补上原持久sequence后合流，不重开或翻转状态。同块连续增量只合成显示正文，原envelope列表完整保留；工具间隔不能跨过。坏消息体显示不可读，不因缺字段让面板崩溃；缺owner不借当前目标补配。当前实现只覆盖 EkoAgentTurnSink 可达的 TaskRuntime 原生路径，不冒称其它 Framework SubagentEvent carrier 已具备未验证的原生映射。

详情页按原事件顺序显示公开进度、原工具引用和后续文本；reasoning 单独折叠并保原始身份来源可读；final phase 候选隐藏。未标注阶段保原标签和原文，不猜完成；确认回答仍来自原运行时确认事件。工具详情尚未载入时只显示原 call_id 引用，不补造工具内容或运行结果。Plan13 操作、TaskRuntimePanel 和 taskRuntimeStore 不变。

代码同步本 ADR 和回归；没有改变通用 Framework／SDK API，没有独立应用面板 examples 可同步，故不新增示例。官网能力口径不变，正式合流后由官网 owner 核 source 指纹；不冒已通过 native。

## 验证边界

Rust 定义直接使用真实 EkoAgentTurnSink，检验 commentary→reasoning→tool→partial→final 的原事件顺序、重复输入不双投、完整 envelope 和签名经真实文件 journal 关闭重开、wrong owner 拒绝，以及 status/error/cancel 不改变原 TaskRun。没有把新 helper 缺失或编译错误计为产品 RED。

前端候选使用现有 store live/replay 和真实 SubagentDetailView：旧生产 RED、同测试 GREEN，保旧 fixtures，并单独检查失败终态重放／重复／来源与签名／候选隔离和工具顺序。Rust 尚未编译时明确留缺口；同 execution 确认后尾错、真实 Frame producer／native／IM／main CI 继续沿原完整 Goal 和各自精确证据验收。
