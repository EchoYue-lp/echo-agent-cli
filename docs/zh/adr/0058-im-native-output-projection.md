# ADR 0058：IM 原生消息投影与原终态边界

## 状态

实施中；以定向验证和原生验收回执分别定资格。

## 背景

Framework 已提供 AssistantMessageBlock，含 block_id、request_id、sequence、phase、delta、complete、provider 及 signed/redacted/opaque reasoning。渠道过去只将 Token 合并为匿名文本，并忽略 FinalAnswer 正文。继续依赖兼容 Token 会丢失消息语义、重复输出正文，并无法区分候选与已确认回答。

## 候选方案

候选一是在渠道按 ThinkStart/End 或正文措辞重建类型和完成；拒绝，因为它建立第二份语义与结束判断。候选二是原样消费 Framework 事件，只在原呈现通道投影。

## 决策

采用候选二。通用类型、provider 归一化及校验／Stop／持久化归 Framework；EKO 渠道只拥有呈现策略。已有 ChannelManager、ChannelSurfaceSink、ChatEventLog、foreground Terminal(TurnOutcome)、scheduler occurrence 和 TaskRuntime 继续是原 owner。

## 数据流与约束

原生 AssistantMessage 原样经过有界 sink 和 journal，保块身份、请求、顺序、签名及 opaque 原件；当前渠道消费路径删除兼容 Token／Think arm、匿名 Token 合并缓存和对应旧事件夹具，不根据是否出现原生块恢复旧消息分支。原输入失败及 drain 状态继续走现有内部 Prompt 投影，不充当 provider 正文或终态。渠道不公开原生 reasoning，也不将它计作自然汇报；折叠与原文读取仍归原会话日志／GUI。

明确 Commentary／PartialAnswer 的文本按原顺序呈现；complete 仅为公开文字边界。Unknown 保留原件且不按措辞猜类型，同一块后继明确 Commentary 可以释放先前 Unknown 增量。当前 Framework 通用流存在 Unknown 文本后以空 complete 标记 PartialAnswer 的过渡接缝：渠道不把空标记倒推成已验证 final，最终正文只能取原 FinalAnswer 确认事件。provider／Framework 必须继续完成原生阶段归一化，不能用此薄投影冒称上游所有场景已验收。

Unknown 缓存绑定原 stream、turn、execution、request 和 block，不将同 stream 中的另一个 turn／execution 当成同一块。确认回答、取消或错误只清理原 execution 尚未公开的缓存并归还字节预算，不把缓存释放成正文。公开增量沿原字符安全阈值刷入有界脱敏投影，单个活动 execution 越界仍明确失败；已结束 execution 不持续占用后继额度。

Frame 后继新增的 ExecutionStatus 沿原有序通道显示为具名执行状态，标出原 execution 身份；它不算自然语言汇报，也不提供回答正文。Settled 只显示 Agent 执行已结算，不替代 TaskRun 完成、FinalAnswer 确认或 foreground Terminal；后继汇报、HITL 和取消继续消费同一通道。

FinalAnswer 是 Framework 原 Guard／Critic／Stop／持久化路径的受校验最终正文确认，合法运行时通知继续保留。渠道收到该事件即按原事件顺序呈现，不拖到后续 execution 的汇报或 HITL 之后。FinalAnswer 不证明物理结算完成；Agent 的 ExecutionStatus::Settled、原 driver EOF 与 physical 终态仍由原 owner 后继核对，不能以正文确认提前替代。原 foreground Terminal 继续独占渠道执行关闭，TaskRun 继续独占目标完成；发布确认回答不改变这两种状态。块 final／complete 或候选在失败、取消、观察未知、断流时不能补成确认回答。后继 execution 的取消不撤销前一次已确认回答，仍显示原取消事实。重复 journal 事件只去重呈现，不新增投递 store 或完成事实。

同一执行的正文确认后发生清理失败或未结算观察时，原已发布正文仍是正文事实，不能改称执行成功。候选没有通用确认时继续禁止发布；合法通用确认则保留恰一次正文，并保留原 EOF、失败／未结算身份和成功结果字段为空的收据。EKO 不为旧 raw-adapter 的正文／完成混用 oracle 新增缓冲或第二个结束判断。

错误回执或原 oneshot 丢失不能抢在 sink 已接受的正文之前结束呈现。renderer 收到错误时固定原有界队列的当前长度，仅按序读取这一有限前缀，再原样传播原错误。它不等待仍由原 owner 保持的 producer EOF，不继续吸收后继事件，也不生成 foreground Terminal 或把 ExecutionNotSettled 改为执行终态。正常完成／失败 outcome 继续沿原 driver EOF 规则关闭。

原生增量保原 envelope，使用原 2048 事件／512 KiB 的输入预算与 64 槽实时队列背压；原 512 条内部消息计数继续限制非原生控制／工具投影，原生块另由原事件／字节预算与实时队列约束；reader 正常排空时不能仅因第 513 个短原生增量而取消。输出总量仍不超过原 256 消息／256 KiB：普通输出保留 254 槽，另为确认回答和失败／取消各保留一槽，各 2 KiB。正常确认回答仍按普通可用额度完整分块；额度不足时仅发送有界确认预览及原会话完整结果入口提示，不能静默吞掉最终回答，也不能占掉后继取消／失败的保留槽。超过有限确认预览额度时明确返回投影错误，不伪造已投递。

cron 仍由 drive_existing_cron_run 读取 canonical TaskRunStatus；仅 Completed 允许 callback 成功。微信通知仍使用冻结目标和原 durable debt／receipt，不因一个模型回答或原生块结束重发通知。渠道平台 Accepted 与手机真正收到分别取证。

## 取舍与影响

原生产逻辑与新测试组成 RED，新 sink／renderer 组成 GREEN；这些是待执行验证，不能写成已通过。回归覆盖公开进度／工具顺序、候选隔离、确认正文只发一次、原 foreground 不提前关闭、跨 execution 顺序与后继取消、普通额度耗尽、513 个短增量／真实队列背压、Unknown 缓冲上限与跨 execution 隔离，以及 signed／redacted／opaque 三类原件经过真实文件 journal 关闭重开和重复回放。scheduler 另以 Pending／Running／Paused／Failed／Cancelled 五状态反控，验证未完成 TaskRun 不产生通知、调用 sender 或改变原 journal／冻结目标；原有 cron 重复 occurrence 与通知 debt 重开恢复测试继续保留。

没有新增 executor、scheduler、store、框架 API、SQLite、网络发送或崩溃重放政策。没有独立 GUI 渠道示例，examples 不新增；官网能力文案不变，由官网 owner 在正式合流后同步源指纹。真实 QQ／飞书私测、微信扫码／确认／取消／过期／重连／HITL／手机收件，以及隐藏／真正退出／重开仍需具名新版原生验收，不能由本地回放代替。

## 2026-10-29 native-only 消费后继

本后继只迁移 EKO 渠道消费，不改变 Framework 合理公开 API、send／poll owner、ACK／cursor、HITL 请求身份或物理结算协议。Generic FinalAnswer 继续来自原运行时，未附正文块时也按原 execution 身份去重呈现；provider final／complete 不可替代该事实。

新增超时组合回归使用原 ChannelHumanLoopProvider、ForegroundTurnControl 和 renderer：同 request_id 重用产生新的 response_target，超时旧 target 与 request_id 本身都不能回答新请求；请求超时和 ExecutionStatus 观察不释放原 foreground，原 owner 结算且 Terminal／driver 排空后才结束。测试只缩短单个 fixture 请求期限，生产 300 秒上限保持。

v2 三源及修正超时 oracle 后的 v3 测试由 EKO 唯一 writer 按函数合流，旧失败和旧候选保留。合流时的 GUI 五文件36项仅覆盖 mock 前端合同；此前 app-core31 中原 HITL owner 和 cron physical-send 各一项也只属于当时来源。当前定向资格见下节；真人收件、实机隐藏／退出／重开与完整迁移分别验收。

### 固定消费者的定向结果

在 Framework `f327f63e34b7b4fea30d1c34f8559431fd2228172a2d427d8f1eac5912ee75c4` 的 2831 文件冻结 WIP 输入上，app-core 的 `channels,test-utils` 消费者已取得原生 IM 24 项、HITL provider 11 项及 cron runner 17 项通过。原两个 HITL 身份／超时边界和无正文块的通用 FinalAnswer 去重均在该 IM 组中；cron 组包含 occurrence 与 Run 绑定、未完成时拒绝通知、原 sender future、同槽去重及通知债务重开恢复。另有六项原生命周期／渠道组合按准确函数分别通过。

后继同输入消费者同时纳入当前插件诊断增量，编译自然退出 0、零错误和零警告；Task 三项、ChatDriver 39 项、四项插件控制及受影响领域组 35 项均通过。pool 精确用例也单独通过，保留原 receipt 1→0、原 typed 工具成功结果和持久计划，并核对真实默认意图分类先于 ReAct、原 Critic 仍执行。领域组包含插件 sidecar 控制，组计数不能相加冒充不重复测试总数。原缺失终态负控精确保留最早的 `agent_stream` 失败，不让后置投影诊断覆盖原先因。

这些结果分别绑定原命令、源码摘要、原始日志和保全制品；原失败不删除或改称通过。它们不授予整个 Framework、后继协议适配器、GUI Rust／实际桌面、新 pin／main、真实 SSH／手机收件、性能 paint 或完整矩阵资格。最终 Framework 冻结来源上的后继消费和原生验收仍须单独安排。
