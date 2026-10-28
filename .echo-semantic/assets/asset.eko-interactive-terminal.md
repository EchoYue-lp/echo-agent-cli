---
schema_version: 1
id: asset.eko-interactive-terminal
kind: asset
title: Existing EKO interactive terminal owner and view adapters
asset_type: state_authority
status: active
risk: high
observed_at: source:f49ec6694fa4b4cf0f175f3a746b2a82a610bfd044d86e11e72f88763aa54c7c
boundary_refs: [boundary.eko-framework-consumer-settlement]
behavior_refs: []
rule_refs: [rule.eko-framework-consumer-authorities]
evidence_refs: [evidence.eko-ssh-source-checkpoint]
finding_refs: []
candidate_refs: []
code_refs:
- echo-agent-app-core/src/terminal.rs
- echo-agent-app-core/src/terminal/project_ssh.rs
- echo-agent-app-core/src/terminal/project_ssh/cas.rs
- echo-agent-app-core/src/terminal/project_ssh/cas.py
- echo-agent-app-core/src/terminal/project_ssh/cas_tests.py
- echo-agent-app-core/src/terminal/project_ssh/channel.rs
- echo-agent-app-core/src/terminal/project_ssh/reader.rs
- echo-agent-app-core/src/terminal/project_ssh/effective.rs
- echo-agent-app-core/src/terminal/project_ssh/sandbox.rs
- echo-agent-app-core/src/terminal/project_ssh/sftp.rs
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
- echo-agent-app-core/src/channel_runtime.rs
- echo-agent-app-core/src/channel_runtime/outbound.rs
- docs/zh/adr/0050-gui-product-convergence.md
- web-frontend/src/components/terminal/TerminalDrawer.tsx
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

## 项目连接源码扩展

本节前来源观察 `source:4726a37cc1350e66b30b281fd02e16cdd087cd7f44b58b28a0ba6ab09bfa0d46` 保留为历史；新 observed_at 仅绑定当前源码，不续期历史验证。

用户澄清侧栏任务是项目，原交互终端切片不代表远程项目完成。本次沿已批准的项目工作环境 Design 与 plan:986e9191-1a18-454b-a8b8-59ae18ad3bc7，在同一 TerminalService 资源 owner 内保存自有 foreground SSH master、标准 SFTP 通道与现有 SandboxExecutor 适配。Workspace 仍是项目/位置/版本权威，TaskRun、任务 journal、原 effect 与 physical-settlement 语义不由此资产取代。

GUI 先用原 caller-generated id 订阅认证 PTY，密码仅走原输入；同目标校验、真实 SSH control check 和远端 SFTP 目录检查不能由 prompt 或本机文件替代。draft 取消/关闭与原映射移交 Workspace 身份串行，旧 draft 不能关闭已发布项目。普通项目终端为同连接的 mux slave，关闭标签不关闭项目。私有 socket、ControlPersist=no、slave ProxyCommand=false/BatchMode=yes 约束真实连接归属与失效后的行为，不修改 host-key 策略，也不选择 foreign master。

通用文件接口由 Framework 拥有，EKO 标准 SFTP provider 只适配实际 I/O；原 checkpoint 算法和恢复格式经 with_backend 复用。写入响应不确定沿原 PartialSideEffect/Possible/VerifyThenRetry，不冒已撤销、不猜回滚或重放。物理调用和 SSH child/pipe 保留到原结算，child wait 失败留同一原件可重试。plain SSH 隔离为 none，实际 cwd 不被默认项目根覆盖；其它隔离/资源约束不冒本机合格证明。

此处只有源码、直接 rustfmt、差异与来源调查；新 Rust 测试定义未运行，未进行 Cargo/metadata、真实 SSH 网络或远端写入。依赖版本已只读核对官方 registry 的发布身份，不等于 lock、编译或风险 gate 通过。Unix multiplexing、真实 GUI 密码及完整项目操作、其它平台、main/CI/部署均未由本节取得资格；原全部失败与历史证据范围保留。

实际 endpoint 后继：原连接用系统 ssh -G 有界读取原生 Host/Match 解析事实，OnceLock 固定 hostname/user/port，复用 TerminalTarget 向唯一 Workspace 作者提供；不新增项目 DTO、manifest/store。raw alias 保留配置选择，master/slave 以确定 HostName/User/Port 和禁二次 canonicalize 防止配置竞态改投，字面百分号转义。跨重连实际 descriptor 不匹配必须由原项目位置/恢复流程处理。只对自有 ssh -G -F fixture 取得原生解析/alias 变化/固定 argv 及 ProxyJump 保留的真实结果，不连接网络、不读取用户配置或凭据；这不是新 Rust 模块已编译的证据。原 SandboxCommand 新增的 runtime-only guards/cancel 被实际物理 future 接收；wait/reader 未结算不释放其 owner，也不把本地 child 结束当远端 stop 证明。

项目终端通过同一 create 路径在 spawn 前接收原 host I/O guard（独立终端默认空向量），原资源 owner 到 child+reader 结算才释放，不是另建 Agent slot。共享 Tauri bridge/Workspace/退出次序由唯一项目集成作者合流；本源码不修改这些共享文件。原 lifecycle 需要在等待 host I/O proof 前实际关闭/观察终端，不能因新增 guard 导致自等。新增标准 SFTP create_dir 保 exclusive 目录创建，原 create_dir_all 不冒充它。

后继 CAS 复用原GUI限定合同：同侧车 flock 协作写者、锁内完整SHA256比较、唯一自有temp/权限/file-sync/replace/parent-sync/unlock；false不替换，Err/SSH响应丢失可已修改，沿原Possible/VerifyThenRetry不重放。固定一次性Python stdlib helper经原executor与结构化stdin执行，不部署daemon或新协议/任务owner。原生自有临时目录13fixture全通过，去锁/首块hash反例各被真实断言拒绝；不是远端或Rust资格。probe只查标准原语可用性，缺能力明确Unsupported。

同一FileSystem.open_read用真实SFTP File.read每次≤64KiB返回标准Read，供原CompletionGate哈希实现复用；不整件Vec、不另建摘要协议、不要求Python。原channel资源容器同时服务已有文件请求和reader，删除旧平行启动代码，file/client/child/pipe完成后才EOF，提前drop沿原blocking边界清理，I/O错停止不重放。标准SFTP无dev/inode/no-follow持目录identity，保独立guard Unsupported，受该保护的mutation前拒绝，不凭mode/mtime/realpath冒资格。


## v4 原 owner 的未知结算修复

v3 全 transport 独审 ed48bba3 的 I1/I2 由原 owner 收敛：execute 仍为 physical-settlement future，不新增裸调用有界返回合同或 Framework API。TaskRun/Cell 沿原 observer，GUI 由唯一项目作者原 ProductDataIoService bounded-v2 承接。真实 child spawn 后、首个 await 前，原 retained_children 容器持有同一 operation/guards/effective target；physical JoinLoss 或调用观察消失不全量 drop。255/缺远端 exit receipt 保留实际原资源并在同 owner progress 非终态等待，不由 local EOF/check/reconnect/alias/时间清除。当前没有自动找回丢失远端回执的事件，不增 daemon/store/假 task 或手工确认绕过。已知非255实际退出或标准请求完成但 Possible 内容不永久保活，真实完成项从同容器移除。

标准 SFTP 薄 error context 在 ToolFailure 映射前保 native response knowledge；失去实际 mutation response 且标准 close 无证明，保留同一原义务；服务器已回复、能力预检失败和完成后的内容不确定不混成停止未知。新自有本机进程回归仍仅源码定义，未 Cargo/网络 SSH/远端执行。v3/c2d0/ed48 原件、原始失败、本机 CAS/解析等历史资格维持原范围，不能续期到新源。


v5 原薄错误接缝修复 v4 独审 I3：request-mutating 决定是否存在丢失写响应，completed-mutation 决定整次递归操作的 Possible 副作用，两维不共用一个 bool。原递归删除/建父目录及 atomic-write 后续 metadata 沿局部真实已完成事实映射错误；只读请求不制造 stop debt，已执行 mutation 不能降为 None/RestoreThenRetry。新增 mutation 后 read fail 和从未 mutation 的受控本机对照，仍仅源码定义未执行。原 v4/审查 be4d8762 与旧 native/失败资格维持历史范围。


## 2026-10-29：退役后的前置路径导航维护

本节只维护结构化源码/消费者引用的导航，不重新观察或续期旧运行资格。原观察 `source:f49ec6694fa4b4cf0f175f3a746b2a82a610bfd044d86e11e72f88763aa54c7c`、上文历史原件、失败与限制、原 command/scenario/after revision 均保持原范围；本节不新增 matched、状态或 owner。

| 原历史路径 | 当前实现或退役决定导航 |
| --- | --- |
| `src/cli/channels.rs` | `echo-agent-app-core/src/channel_runtime.rs` |
| `src/cli/cmd_impls/developer.rs` | `docs/zh/adr/0050-gui-product-convergence.md` |
| `src/tui/events.rs` | `web-frontend/src/components/terminal/TerminalDrawer.tsx` |

旧路径身份与退出范围由既有退役资产/Finding和正式ADR0050/0057保留；源码历史可在 `pre-retirement-2026-10-24` 及后继具名Git保存点回读。新导航只指向现存原接管实现或退出决定，不能把旧observed_at解释成这些路径当时已经运行通过。unknown/needs_review、原动态/native/风险缺口不清；本候选没有执行Cargo或官方新Task校验，采用与真实来源绑定由EKO唯一writer按正常新Task合同完成。
