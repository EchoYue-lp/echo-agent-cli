# ADR 0048: EKO 上下文压缩产品策略

## Status

accepted

## 背景与候选

框架已提供 canonical/protected context、按 token 选择近期 turn 的压缩器和带 focus/cancel
的手动压缩入口。EKO 已有 TaskRuntimeStore、goal/recovery projection 与 app-core
manual_compression。永久保留所有用户输入会破坏活动窗口的上界；另建 goal store 或在
应用复制 selector 会形成平行权威，因此复用现有机制。

参考框架 ADR 0081 中 Codex 的有界用户材料和重建初始上下文，以及 Pi 的近期 token
预算与工具组边界。EKO 本地个人助理的特有策略是保存原始 goal、用户 steer 和产品 journal，
这些能力保留在应用层。

## 决策

- EKO 默认 `compress_window: 0`，使用模型窗口 25%、至多 20K token 的近期原文预算。
  正数显式选择旧消息条数上限；`adaptive` 仍使用自身的 L1-L5 配置。
- `/compress`、`/compact`、TUI 与 GUI 使用同一已安装策略，不再宣称固定 6/12 条覆盖。
  各入口传入同一个可选 focus；GUI 提供压缩重点输入框，TUI 接受命令后的文本。
- app-core 持有 foreground/AgentPool/workspace I/O owner，取消传给框架并等待结果。
  变换前取消保留上下文；变换成功后即使 caller drop/cancel，journal safe point 仍完成。
- Goal contract 与 recovery capsule 来自唯一的 TaskRuntimeStore。journal 保留 user steer
  全文；最近四条的活动投影按时间顺序保留短原文或 400 字符预览与完整 SHA 的稳定引用。
  长原文沿原 PreparedUserTurn/InputResourceRef 落在 host 产品数据目录，缓存缺失/篡改
  只能从原 journal 再物化，不把尾部丢弃后宣称约束完整。它们不是新请求或新的
  canonical system policy，退出 run scope 时删除 projection。最近四条不代表全部有效要求。
- ConversationStore 保存完整 transcript，ContextManager 保存活动窗口，RuntimeStateStore
  保存恢复 checkpoint，TaskRuntimeStore 保存 goal/执行事实，Trace/ChatEventLog 保存诊断和事件。

## 影响与回滚

删除 app-core request 和前端中未生效的 `keep_messages` 参数。开发阶段不保留这一旧接口。
框架 Rust 构造器保留，EKO 正数窗口继续支持显式旧策略。回滚本切片恢复旧配置和 adapter，
不会删除会话历史或 TaskRuntime journal。框架先交付 main 后，应用才能合并 main。

## 验收

四类 surface focus 都产生一份 checkpoint 和一条 journal 事件；取消、caller drop 和 journal
失败保留原结算合同。真实压缩后 goal 和 steer 的末尾限制仍在，重复刷新不累积 projection。
Rust、GUI 与前端验证分别报告，不能把 mock 测试当作原生窗口或外部模型验收。

## 补充决策：必要原文不可达时保留可恢复任务

原 Framework required projector/final request 接缝负责通用错误传播。EKO 的原
TaskRuntime projector 使用它，把本机 artifact 读取策略留在应用：原 reader 在
snapshot 的 host 根下校验 SHA/文件，SSH 项目的 read_file 不承担本机产品数据读取。
真正的最终模型请求必须含 read_artifact；非空 allowed_tools 移除它时不能自动加回，
empty allowed_tools 仍为原 unrestricted 语义。物化、验证或读取资格失败不能只发送预览。

原 typed AgentFailure 的 code=required_context_unavailable 由同一 helper 分类，
不解析 message、不将 retryable 改 true。正式 PlanTask 原 exact-claim interruption/CAS
暂停任务和 Run；普通 GUI/IM 的原 RunTurn owner 用 NeedsInput 暂停原 Run。两条路径
保留原物理失败和 Goal，修复后通过原 expected-resume 恢复，retry 不增加。
Cancelled、旧 claim 与 ExecutionNotSettled 的原优先级保持，不新增 store/状态机。

仅内联全文会越窗；仅截预览会丢尾部；仅查工具注册表会漏本次 hard W。所选方案复用
原 journal、spill、read_artifact 与 TaskRuntime owner，不新建权限或错误共享旗。
行为模式参考框架 ADR0081 所列 LangChain 官方 middleware 显式 handler/异常传播，
本项目的 required 与暂停政策是本地工程推论，不声称外部提供同名 API。

验收须真实走超过8000 token的 ContextManager、冷重开/重复压缩、损坏缓存重建，
以及 execute_run 和 drive_chat 的暂停后恢复。SSH read_file-only 控制须证明最终工具
范围不变、模型/脚本/远端FileSystem均不调用。完整有效要求的分类、替换、范围、版本
及独立coverage仍需后续切片，不能由最近四条或 generic 模型零调用冒充完成。
