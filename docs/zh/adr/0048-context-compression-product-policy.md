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
  canonical system policy，退出 run scope 时删除 projection。最近四条只是呈现预览；完整历史通过同journal派生来源入口按需读取，不产生独立有效要求账本。
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
范围不变、模型/脚本/远端FileSystem均不调用。必要历史和完整原文仍须由主模型在真实压缩/恢复后读取；最近四条预览不代表完整历史。
2026-10-27用户裁决已撤销独立意图分类与强制有效要求/计划/验收链，不再为这些旧方案建立后续切片。


## 2026-10-27 裁决：Steer 由主模型理解，已有 Goal 仅用户可改

用户明确：普通Steer由主模型结合历史理解并在执行中按需更新计划，不先调用独立模型，
也不强制串联“有效要求更新 → 计划调整 → 验收调整”。该裁决替换旧Plan14的独立理解和
强制传播承诺，旧原文修复、接纳、幂等、错误与恢复证据保留各自真实范围，不改称全部通过。

TaskRun.goal是必填字段，未建立该运行目标时复用原初始化设置初始Goal。已有Run的重试、
ensure_scope与create_run重入返回原目标；普通Steer接纳/消费和模型task_update不能改写已有
Goal、goal_revision或goal_hash。用户通过GUI目标编辑或IM `/task-goal` 显式修改，继续使用
原RunGoalActorSource、expected_goal_revision/CAS和安全点；用户目标变更的原版本失效规则保留。

用户“结束并删除当前目标”沿原结束/取消入口退出活动目标呈现，不向后端写空Goal或抹去
历史事实。普通执行取消同样按原生命周期结算并保留goal/revision/hash，不能把取消运行等同
修改或历史删除Goal而另加权限门控。原Stop、HITL、恢复与物理结算owner不变。

必要历史、完整长原文及末尾限制继续沿原journal、压缩投影与read_artifact读取；缺失或
校验失败时原required_context_unavailable暂停相关新副作用，原Goal和可恢复owner保留。
这里不新增GoalStore、要求解释器、第二份队列/状态机或结构化模型协议，也不裁剪通用框架/SDK API。

## 2026-10-27 可发现的完整原文入口

保留原Goal/Steer产品权限：普通追加输入只交主模型结合历史理解；已有Goal只由用户显式更新或结束，不引入独立意图模型或统一强制要求/计划/验收流程。压缩后可读原文不等于全部原文常驻上下文。

原TaskRuntime的原始输入来源族从同一个journal派生，原始接纳与投递事实分别保留。模型在有界恢复capsule中取得准确Run、journal identity及固定序号前缀的不可变引用，通过既有`read_artifact`按需分页恢复十三条以上输入前的原文及末尾限制。完整来源由原有界journal读取覆盖到切点，不能把近期4/8条或一个256事件结果页当作全集。

派生来源缓存可重建，不拥有Goal、计划、要求或输入生命周期状态。每个已发布文件按真实源内容和原journal代际稳定命名；无新来源的重复刷新保留inode/mtime，后继追加不覆盖旧文件。最终请求校验的是模型实际看到的固定前缀，因此并发新输入不会混入旧页；下一安全点刷新为新引用。旧引用的Run、代际、SHA和cursor仍由原reader核验。

关键来源读取、物化、校验或最终`read_artifact`资格缺失时保留原required-context失败→暂停→expected-resume链，不以降空历史、放宽工具范围或重放未知副作用替代恢复。原文文件固定落在本机产品data-root，SSH项目的FileSystem/read_file不能冒充本机原文读取。

验证必须检查真实压缩后的模型请求可发现入口，以及真实tool结果逐页进入后继模型调用并覆盖早期全文/末尾；fixture本地回读或把旧正文再次当当前输入不构成此资格。

### canonical长Goal与原文历史分别保持来源

当前长Goal沿原GoalContract接缝物化到host artifacts，并携带原Goal revision/hash及Run/journal代际；原tasks/objective仅归档。legacy无Accepted的长初始化目标及用户后来改过的长目标都可由原host read_artifact读取，不为它们添加伪输入或扩展raw来源事件类别，也不把新Goal写入旧raw前缀。短目标按原内联方式；实际已完整呈现的必要原文不因派生引用新增reader要求，NoTools/限定read_file正常正控继续成立。真正被省略的必要长Goal或早期原文缺reader则沿原暂停/expected-resume，不放宽W。

完整可见性的判断以最终实际模型请求为准：既有 GoalContract 能内联的边界与未启用 continuation 时的恢复预览边界分别沿用；预览会省略的 canonical Goal 发布 host 引用。当前 PreparedUserTurn 已完整内联的中段文字（超过预览长度但未到 spill 边界）保持 NoTools／read_file-only 正常执行。此前真实配对 read_artifact 调用和 Tool 结果若仍完整留在请求中，按分页字节范围、全文覆盖及原 SHA 核对后也算全文可见；缺页、重叠不一致或 SHA 不符不算。核对不会从本地文件补入缺失模型文字，也不把“文件存在”当作“模型已读取”。同一 Goal 的已发布引用资格仍绑定 Run、journal、revision 与 hash，不随当前投影显示模式改变。

### 分页消费者测试的真实封包边界

分页测试必须绑定实际模型请求里的 assistant 调用、原 ToolCall 和 ToolResult 的同一调用 ID。
`read_artifact` 的第一页省略可选 string 参数 `cursor`；发送 JSON null 会被原 schema 校验
拒绝，此时模型中的 `[Error]` 是错误结果，不构成原文页或原文截断的证据。

成功页按原 reader 在真实输出末尾附加的分页 notice 解包，不按 expected 字符数截取首部。
验证模型 Tool 文本与原 ToolResult 完整输出相同，再核对文件坐标、原 SHA、连续字节范围、
每页完整 UTF-8 内容与 SHA，并从实际模型页重构整个来源文件。只出现短 marker、fixture
本地可读或 sidecar preview 均不等于全文进入模型。验收继续使用原 8000-token 上限和
2000-token 页预算；本节约束验证方法，具体通过资格以对应实际命令与快照回执为准。
