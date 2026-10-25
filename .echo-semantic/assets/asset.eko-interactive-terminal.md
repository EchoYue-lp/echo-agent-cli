---
schema_version: 1
id: asset.eko-interactive-terminal
kind: asset
title: Existing EKO interactive terminal owner and view adapters
asset_type: state_authority
status: active
risk: high
observed_at: source:4726a37cc1350e66b30b281fd02e16cdd087cd7f44b58b28a0ba6ab09bfa0d46
boundary_refs: [boundary.eko-framework-consumer-settlement]
behavior_refs: []
rule_refs: [rule.eko-framework-consumer-authorities]
evidence_refs: [evidence.eko-ssh-source-checkpoint]
finding_refs: []
candidate_refs: []
code_refs:
- echo-agent-app-core/src/terminal.rs
- echo-agent-app-core/src/runtime.rs
- src/tauri/terminal.rs
- web-frontend/src/api/endpoints.ts
- web-frontend/src/components/terminal/Terminal.tsx
- web-frontend/src/components/terminal/TerminalDrawer.tsx
- web-frontend/src/components/terminal/terminal-io.ts
- docs/en/adr/0051-ssh-terminal-sessions.md
- docs/zh/adr/0051-ssh-terminal-sessions.md
consumer_refs:
- echo-agent-app-core/src/developer_commands.rs
- src/cli/channels.rs
- echo-agent-app-core/src/channel_runtime/outbound.rs
- src/cli/cmd_impls/developer.rs
- src/tui/events.rs
- web-frontend/src/api/endpoints.terminal.test.ts
- web-frontend/src/components/terminal/TerminalDrawer.test.tsx
- web-frontend/src/components/terminal/terminal-io.test.ts
---

# Existing EKO interactive terminal authority

## 资产身份

既有 app-core TerminalService/TerminalSession 是交互终端的会话、目标、准入和原 child/reader 资源 owner。本次扩展该原 owner 的 local/SSH target、admission 与 physical-settlement 状态，不新增执行器、store、任务状态机或第二个权威。系统 OpenSSH 处理协议、认证、用户配置和 host key；portable-pty 提供原 PTY/Child primitive。正式依据为两份 ADR0051。

TerminalViewState 是前端可观察信息；机器的 newStateAuthority 风险扫描会命中其 interface/type import，但不证明新增 owner。风险标记保守声明为 true，boundaryDecision.createsNew 仍为 false；不修改检测器或降低风险，也不把新增 Asset 当作运行时新机制。

## 来源与消费者

code_refs 中的原 TerminalService、ApplicationLifecycleOwner、Tauri terminal adapter 与 endpoints 是当前来源。Terminal.tsx 先就绪 xterm/listeners 后调用既有 create；TerminalDrawer.tsx 跨 tab/hide 保持会话观察；terminal-io.ts 只拥有 transient bounded queues 和有限观察时间，不是历史或 replay store。原 CLI/TUI/channel 只投影同一服务的退出、issue 与输出事实；DeveloperCommand 默认 create 仍为 Local。

id/pid/target 来自同一后端 Session；输出经原 PTY bytes/base64、raw Uint8Array 到 xterm。create 的本地 ssh spawn 不等于远端完成认证，界面与文本输出不能制造 Connected 或远端副作用完成证据。现有 HTTP 前端分支只保留本地请求及返回 id 兼容；未发现本次已验证的 HTTP terminal 后端，不新增 HTTP SSH 产品或模型远程工具。

## 生命周期

原 Session 保留 Child、reader、wait 结果和一个 settlement handle；从 child spawn 到原会话登记与 cleanup 启动之间没有可取消 await。成功移除同一 Session 和发布一次正常 Exited 前，必须得到原 Child wait 与 reader join；event bridge join 或 kill 返回不能替代这些证据。

观察超时、调用方取消、kill/wait/reader 错误保留原资源、目标与实际诊断。重试不能擦掉不可恢复的 reader 错误。原 ApplicationLifecycleOwner 在等待前封闭终端准入，未结算的 close_all 保留同一 owner。前端取消观察不丢原 create/close Promise，创建晚完成仅对其实际 Session 做关闭补偿。

输出/输入队列、scrollback 和 channel forwarding 有界，真实 lag/overflow 可见；不伪称完整历史/VT 恢复，不自动重发可能已部分处理的输入。关闭本地 SSH child 不证明远端进程组或业务副作用已停止。直接用户终端不受 Agent full-auto 门控，手工连接不授权模型自动执行。

## 候选关系

这是既有 rule.eko-framework-consumer-authorities 下原应用资源 owner 的具名资产与路径绑定，不替代其历史 Rule/Behavior/Finding。新增 source-specific Evidence 单独记录本轮真实依据和限制；原对象的状态、observed_at、旧裁决与失败记录均保持。

## 未知与限制

active 表示该原 owner 在生产入口可达，不表示完整产品已验收。原25个 focused Rust cases 只覆盖 ea4/2b/e4e；原 Node61files/314tests 只按当前425个前端输入精确不变复用。710a精六项依赖 metadata 资格不是编译/Clippy/native/远端资格，风险未修复或变为不可达。完整 Rust/GUI、真实 Tauri/SSH、其它平台、main/CI、发布及整个 Plan06 尚未完成。

## 2026-10-25 私有结算别名的后继观察

前一source观察为688b4e517f8a99abb3d08d6b7547d03bc9ef84216f708054829d385f788b27e5，d7fc原件、710风险资格与旧25case边界保留。当前仅在原terminal模块抽取私有透明TerminalSettlementSlot，两个引用点展开后精确恢复同一Arc/Mutex/Option/JoinHandle/Result类型，不增加owner、store、API或同步/生命周期行为。observed_at刷新只绑定该后继的当前源码；原40个历史语义对象及其status/观察不变。

原focusedClippy在两处type_complexity自然101，CLI库未到；失败raw及exactclean1.2GiB/0保留。当前未重试Clippy，不把别名候选、metadata或旧710扫描写成通过；新9568绑定的原六工具方法/两次fresh扫描和下一Clippy的真实结果分别以任务回执记录。Node425字节未变，产品文档/示例/官网无需纯私有别名迁移；完整GUI/native/真实远端和Plan06仍未验。

## 2026-10-25 focused Clippy 后继资格

前一私有别名修复来源观察source:9f547aecb5c081047bcf82867a21515d83d490d701aed53a9a2a1b312f026cbf与原d7/710/101历史保持。后继另获一条focused租约后，原两package/lib/all-feature Clippy完整自然0，两个lib的rmeta已保存哈希，未运行tests/bins/examples/Node/native/远端。此处只更新当前来源观察和已执行资格，不改变原TerminalService唯一owner、前端观察角色或既有风险接受。

observer的du临时文件竞争exit1、采样gap和同原Cargo组恢复另存，不把observer退出当compiler退出，也不声称连续RSS峰值。原命令完整raw/真实0/源guard/rmeta保存后，无targetreader才立即exactclean0移除5939files/1.2GiB，targetabsent并归还槽；资格不以留target等待review。完整矩阵、真实native/SSH、其它平台、main/CI和Plan06仍未资格。

## 2026-10-25 应用源码归位观察

当前可读 channel helper 路径随原实现归位 app-core/channel_runtime；原 bin 入口只转发。此路径修正不续期本文件任何历史 Rust、Node、风险接受或 native 资格；历史观察和具名旧快照结论保持原范围。当前合流尚未 Cargo 编译。

旧来源观察 `source:230d0c7af649f29cef2002109e67167479b0894c28537c90eefe2d0746336829` 保留为历史。当前 observed_at 只绑定归位后的可读来源，不表示旧验证在新合流上续期。
