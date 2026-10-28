# 当前 EKO 集成状态

本表记录本轮正式源码的权威路径与未完成收敛，不替代实际验证回执，也不表示产品发布完成。

| 范围 | 当前权威路径 | 仍需完成 |
| --- | --- | --- |
| 输入与跨会话观察 | 原 AgentRouter typed payload／原消息 ResourceLink；GUI 输入视图和目标导航消费原记录；当前 app-core 来源／真实调用及 GUI typed 输入上下文回归已通过 | 真实 DTO 与新版原生窗口验收 |
| 原生正文、思考和工具顺序 | 原 Message／ChatEventLog publication、canonical executionSteps；最终呈现保原 execution／request／两序号域与 event ID；当前资源组保留原发布与冷恢复门槛 | 新组合源重新捕获后端 DTO 并重放完整气泡，旧票不跨源码续期 |
| 六组工具页 | 原 ToolControl snapshot／revision，来源与分类正交，EKO required policy 与原只读工具上界；app-core 必需项拒绝与运行快照／只读上界 focused 已通过 | GUI 发布失败／权威回读及整应用验收 |
| 当前 Workspace 合同 | 显式 project_location，原 registry／generation，Git porcelain NUL 协议；当前 GUI cold／Fork／Worktree 链和 NUL／bare／重复身份回归已通过 | 注册／位置等完整 Rust 消费者和真实 SSH 验收 |
| 旧版本兼容表面 | 旧 round／字符串思考与 EKO legacy origin adapter 已退役；当前签名／加密协议数据仍走原 Message | 取消项保持退役身份，不记通过，不迁移或删除用户资料 |
| Fork 的最终回复准入 | 原 source publication／实际结算与 NativeHistoryImport checksum 仍是唯一依据；两处无 native 字段的历史放行早退已删除，原非流式能力保留 | 原两正控与纯文本反控已完成 tests-first RED→GREEN；当前资源组 25 通过、1 个原手工性能用例 ignored，conversation／Agent 输入／worktree 回归通过。手工性能、真实 SSH、原生 App 与完整矩阵仍另验 |

通用 journal、取消、CAS 和运行结算继续由 Framework 持有；应用没有第二套 store、调度器或结束判定。普通 Agent 输入的呈现、来源标识和右对齐不授予人类编辑、HITL、Goal 或任务状态写权。

实际源码、未提交增量和验证记录按原协作目录维护。Rust、桌面制品、真实外部服务、main CI、推送与发布分别验收；本状态表不晋升任何历史通过结果。

相关决定见 ADR0045、ADR0047、ADR0055、ADR0057 和 ADR0061。示例与官网仍按所属实现同步；内部呈现 DTO 不新增独立示例或产品入口。
