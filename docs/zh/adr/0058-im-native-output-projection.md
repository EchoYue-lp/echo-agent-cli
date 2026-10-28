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

原生 AssistantMessage 原样经过有界 sink 和 journal，保块身份、请求、顺序、签名及 opaque 原件；进入原生流后不重复发送兼容 Token。渠道不公开原生 reasoning，也不将它计作自然汇报；折叠与原文读取仍归原会话日志／GUI。

明确 Commentary／PartialAnswer 的文本按原顺序呈现；complete 仅为公开文字边界。Unknown 保留原件且不按措辞猜类型，同一块后继明确 Commentary 可以释放先前 Unknown 增量。当前 Framework 通用流存在 Unknown 文本后以空 complete 标记 PartialAnswer 的过渡接缝：渠道不把空标记倒推成已验证 final，最终正文只能取原 FinalAnswer 确认事件。provider／Framework 必须继续完成原生阶段归一化，不能用此薄投影冒称上游所有场景已验收。

Unknown 缓存绑定原 stream、turn、execution、request 和 block，不将同 stream 中的另一个 turn／execution 当成同一块。确认回答、取消或错误只清理原 execution 尚未公开的缓存并归还字节预算，不把缓存释放成正文。公开增量沿原字符安全阈值刷入有界脱敏投影，单个活动 execution 越界仍明确失败；已结束 execution 不持续占用后继额度。

Frame 后继新增的 ExecutionStatus 沿原有序通道显示为具名执行状态，标出原 execution 身份；它不算自然语言汇报，也不提供回答正文。Settled 只显示 Agent 执行已结算，不替代 TaskRun 完成、FinalAnswer 确认或 foreground Terminal；后继汇报、HITL 和取消继续消费同一通道。

FinalAnswer 是 Framework 原 Guard／Critic／Stop／持久化路径的受校验最终正文确认，合法运行时通知继续保留。渠道收到该事件即按原事件顺序呈现，不拖到后续 execution 的汇报或 HITL 之后。FinalAnswer 不证明物理结算完成；Agent 的 ExecutionStatus::Settled、原 driver EOF 与 physical 终态仍由原 owner 后继核对，不能以正文确认提前替代。原 foreground Terminal 继续独占渠道执行关闭，TaskRun 继续独占目标完成；发布确认回答不改变这两种状态。块 final／complete 或候选在失败、取消、观察未知、断流时不能补成确认回答。后继 execution 的取消不撤销前一次已确认回答，仍显示原取消事实。重复 journal 事件只去重呈现，不新增投递 store 或完成事实。

原生增量保原 envelope，使用原 2048 事件／512 KiB 的输入预算与 64 槽实时队列背压；旧 512 条合并消息计数只覆盖旧合并投影，不能在 reader 正常排空时取消第 513 个短原生增量。输出总量仍不超过原 256 消息／256 KiB：普通输出保留 254 槽，另为确认回答和失败／取消各保留一槽，各 2 KiB。正常确认回答仍按普通可用额度完整分块；额度不足时仅发送有界确认预览及原会话完整结果入口提示，不能静默吞掉最终回答，也不能占掉后继取消／失败的保留槽。超过有限确认预览额度时明确返回投影错误，不伪造已投递。

cron 仍由 drive_existing_cron_run 读取 canonical TaskRunStatus；仅 Completed 允许 callback 成功。微信通知仍使用冻结目标和原 durable debt／receipt，不因一个模型回答或原生块结束重发通知。渠道平台 Accepted 与手机真正收到分别取证。

## 取舍与影响

原生产逻辑与新测试组成 RED，新 sink／renderer 组成 GREEN；这些是待执行验证，不能写成已通过。回归覆盖公开进度／工具顺序、候选隔离、确认正文只发一次、原 foreground 不提前关闭、跨 execution 顺序与后继取消、普通额度耗尽、513 个短增量／真实队列背压、Unknown 缓冲上限与跨 execution 隔离，以及 signed／redacted／opaque 三类原件经过真实文件 journal 关闭重开和重复回放。scheduler 另以 Pending／Running／Paused／Failed／Cancelled 五状态反控，验证未完成 TaskRun 不产生通知、调用 sender 或改变原 journal／冻结目标；原有 cron 重复 occurrence 与通知 debt 重开恢复测试继续保留。

没有新增 executor、scheduler、store、框架 API、SQLite、网络发送或崩溃重放政策。没有独立 GUI 渠道示例，examples 不新增；官网能力文案不变，由官网 owner 在正式合流后同步源指纹。真实 QQ／飞书私测、微信扫码／确认／取消／过期／重连／HITL／手机收件，以及隐藏／真正退出／重开仍需具名新版原生验收，不能由本地回放代替。
