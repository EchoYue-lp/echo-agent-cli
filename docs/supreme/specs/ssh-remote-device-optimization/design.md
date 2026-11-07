---
title: 免安装 SSH 运维的连接代际恢复
artifact: design
carrier: markdown
---

## 决策与目标

2026-11-07 用户确认“补齐连接代际设计并继续完整修复”：同一个 Workspace 保留旧命令的
未知结果和原资源，新认证连接供后续查询使用。继续系统 OpenSSH/SFTP，不部署专属远端
服务，不增加第二套任务存储，不自动重放启动、重启或停止命令。

该设计补齐当前免安装 SSH 运维闭环的恢复缺口，不改变原 Plan 的 Outcome。原启动性能、
查询取证、精确目标控制、有界日志、进度和真实 GUI/SSH 验收继续成立。当前已构建候选
保留其局部通过资格，但还未实现本设计，不作为完整恢复场景的通过证据。

## 问题与现状

当前 `ProjectSshConnection` 是一条物理 OpenSSH 主连接的 owner。工具及 Workspace Host
持有它的不可变 Arc。命令返回 SSH255 时缺失原远端退出凭据，原 operation 和 guards
保留 unknown；这是正确行为。普通产品 guard 还持有 Workspace control receipt。

应用重连却先要求 Workspace 空闲、退休 Host、关闭旧连接，最后才发布新 draft。旧 unknown
使空闲/关闭都不能成功，主连接已断时同 Workspace 无法恢复查询。只放宽空闲检查、清除
guard、覆盖旧map或将新状态查询当旧退出证明，都会丢失原生命周期责任。

已确认代码路径和原生组件证据见本次调查记录；完整 Workspace 回归与真实 SSH 仍须另验。
并非所有255都失去后续查询能力：若原 master 仍健康，原连接仍可承载后续请求。

## 分层与复用

Framework 的 FileSystem、FileSystemReadSession、SandboxExecutor、调用资源 guard、
TaskRuntime、命令 cell、错误分类与重试继续复用，不新增框架 API。应用 TerminalService
拥有连接代际、认证交接和关闭；Workspace Host 保持唯一的本机数据、TaskRuntime、AgentPool
和已验证项目位置，不因为通信恢复重新开 store 或替换 Host。

单一 Workspace SSH owner 内保留当前物理连接及仍需结算的旧物理连接。当前连接的选择
只有一个权威；不并行维护第二份设备注册表、任务状态机、恢复调度器或持久化数据库。
物理连接身份仍由原控制目录派生的 generation 标识；它不是 Workspace 产品数据 generation。

## 调用与代际边界

现有项目文件和命令适配器引用同一个 Workspace SSH owner，在每次新请求准入时捕获
当前物理连接 Arc。捕获后直到该请求实际结算，始终使用这个 Arc：

- 一个 shell/SandboxCommand 包含其启动、超时、取消观察、管道和原资源；创建执行 future
  时即捕获，不到真正 spawn 时再重新选择连接。应用适配器显式覆盖 stream 入口，
  避免框架默认缓冲 stream 到首次轮询时才调用 execute；各 limits/cancel 入口同样捕获。
- 一次普通 FileSystem 方法包含该方法内部的全部 SFTP/命令步骤、重试和结果收集。
  checkpoint、原子写和 CAS 等复合方法整个方法使用同一物理快照。
- 一个 FileSystemReadSession 的全部准备读取固定到开始时的代际；返回的流式 reader
  和已开文件固定到原连接直到 finish/drop 结算。cache namespace 反映对应物理代际。
- 一次方法内部不解析后继连接；新的一次独立工具/文件请求可以在重连成功后准入新代际，
  即使它属于同一 TaskRun。重连不触发这次调用，不重新执行旧调用，也不使原任务成功。

这一区分是物理请求边界，既不改变 TaskRun 的身份或结束权威，也不把一次任务包装为新的
连接状态机。跨多步使用的数据仍必须遵守原读写、CAS 和目标身份校验，不能因同目标就
假定远端文件未变化。

捕获当前代际及建立请求 lifetime 必须在同一个 owner 的短发布临界区完成。
该 lifetime 与原物理 operation/guard 各司其职：观察 future、准备会话或 reader 仍持有
请求 lifetime 时，退休关闭不得封闭该代际，复合方法的后继子步骤才能继续原连接。
发布后 root 的新准入只选择后继代际；旧 lifetime 的最后持有者释放后，才允许启动原代际
关闭观察。关闭仍须等待物理资源结算，不能用请求观察已结束代替原远端停止证明。

## 重连与发布

继续沿原 GUI 临时认证 draft、原 SSH 配置解析、目录验证、应用 Workspace transition
及产品 generation 校验。用户输入密码及主机指纹确认保持原 GUI 路径。

候选必须核对原配置目标以及实际 host/user/port/绝对目录，不能改变项目位置。认证和目录
探测完成不代表已发布。发布时再次验证 draft 的实际物理身份、master 仍有效且未 closing、
服务准入仍开放、Workspace 产品 generation 及预期旧物理 generation 未变化。

在原创建/转交串行边界和同一个 owner 的短临界区中，原子替换“后续请求的当前代际”，
同时把旧连接保留为该 owner 的历史未结算代际、撤销 draft 原键。所有可能失败的验证
在该可见切点前完成；发布之后不以旧连接关闭失败回滚到一个已断开的旧代际。

发布只修改通信代际，不要求 active_controls 或未知运行归零，不退休/重建 Workspace Host、
不修改当前 UI 焦点、不打开新的本机 TaskRuntime/store，不迁移原请求和 guard。
原 Host、AgentPool 和 TaskRuntime 继续通过相同应用适配器，在后续请求捕获新连接。

并发重连在进入原owned等待队列前固定预期物理代际（含尚未连接的None），等待结束后不得
重新取current作为预期值；发布时做 compare-and-publish，第二个过期候选必须失败并保留自己的
认证/关闭责任；不得静默覆盖较新的有效连接。用户取消/关闭 draft 与发布沿原所有权串行
核对：发布前取消可阻止交接，发布后的旧键不能关闭已发布或另一代际的 Workspace 连接。

## 旧资源与退出责任

owner不再向新准入返回旧连接；已有请求、准备会话、reader、原进程和 unknown 继续持有
原 owner。旧请求的内部子步骤可以继续原代际，不因重连而被隐式取消。被发布为当前连接的
后继不接管这些物理句柄，也不清除其未知状态。

退休代际可以在具有真实空闲、通道关闭和本机资源结算证据后回收；失败保留同一个实例
以供后续观察。未知旧代际不得因为超过时间、SSH EOF、重连成功或新查询成功就被回收。
重复恢复不制造失去管理的旧 master；整个集合沿现有 TerminalService 的关闭与应用退出
路径统一观察和报告。该集合不是远端任务结果数据库。

Workspace删除/位置重关联仍要求原本机数据与执行生命周期的完整空闲证明；连接恢复使用
自己的通信发布条件。真正退出应用时封闭所有代际的新请求，再在有界观察内关闭能证明
空闲的资源；仍未知的原操作以原 generation 和原错误报告，不把本机进程退出描述为远端停止。
关闭观察失败时保留所有未结算实例；若应用没有进入整体 shutdown，只重新开放当前有效
代际的后续请求，旧退休代际不会重新成为当前连接。

一次 owner 关闭对当前与全部退休代际共用原有关闭观察预算的同一个绝对 deadline，
包括等待请求lifetime释放、SFTP车道排队/关闭、原物理操作与master结算。
不能对每个代际串行重新计满5秒，使重复重连把退出等待放大为N倍。
观察到期只结束该观察；真实关闭任务及全部未结算代际继续由原owner持有。

## 用户可见结果

重连成功仅表示新连接已认证并核对到相同目标，可继续查询。原工具失败、已发布阶段发现、
保存告警和原结果入口继续呈现；有旧 unknown 时明确说明其结果仍未确认。当前目标状态
和原控制操作是否执行分别陈述，不产生新的业务结论或伪造完成。

沿已有 transition receipt/错误呈现和日志投影补充必要说明，不新建设备管理控制台或要求
用户操作连接代际。普通新操作继续原权限/HITL；用户终端不增加 full-auto 门槛。

## 参考与取舍

[OpenSSH ControlMaster](https://man.openbsd.org/ssh_config#ControlMaster)共享网络连接并复用
session；恢复新网络连接不提供原命令退出凭据。因此保持显式认证/目标核对与旧请求身份，
不启用隐式fallback冒充产品恢复。

moss-agent `41856a8590e31d2134fc9cf075ec7319c4d0b7ee` 的 SSH controller 和设备 binding
区分连接 attachment/generation 与设备身份。Eclip 借鉴这一边界，但将实现限定在已有
SSH Workspace 的通信适配，不采用其远端服务、HTTP设备协议和SQLite操作账本。

[Ansible异步状态](https://github.com/ansible/ansible/blob/devel/lib/ansible/modules/async_status.py)
依赖远端 job ID/结果文件才能查询原结果。本次只恢复后续查询，旧命令继续unknown，因而
不增加远端receipt、专用目录或执行服务。未来若要保证原退出结果可找回，应作为独立能力
设计，不能由本次连接恢复隐含承诺。

未采用重建Workspace Host：会重开TaskRuntime或改写原执行依赖。未采用共享可变工具指针
在执行中解析：会让原请求跨代际。选择在现有适配器入口捕获不可变物理快照，新增最小的
应用内代际集合，继续复用标准SFTP、OpenSSH、Arc和原资源结算机制，无新增依赖。

## 验收标准

1. 经真实 Workspace 重连主路径，旧master已断、原255/unknown与guard仍持有时，认证
   同目标draft能成为后续请求的连接；新查询有新generation证据，旧operation/guard不释放。
2. shell future在发布前创建但发布后轮询、已开SFTP请求、准备会话/reader及复合checkpoint/CAS
   在重连竞态下均不会中途转向新代际；后续独立请求使用新代际，旧控制调用计数不增加。
3. 错主机/账号/端口/目录、过期Workspace generation、过期物理generation、draft关闭、
   并发重连、service shutdown和用户取消分别有反控，失败不改变当前连接或丢失任何owner。
4. 原TaskRuntime/store/Host身份、运行/工具/日志身份保持；新目标状态不替代原命令结果。
   删除与整体退出仍报告原未知资源，关闭失败后有效当前连接可继续查询。
5. 已结算退休连接能回收，未知退休连接继续可达；多次重连、冷/热消息准备及通道计数
   不回归重复读取、逐文件开关和无界关闭观察。
6. 独立审查与当前必要回归通过后构建新候选。真实SSH/模型/GUI、原32.5秒同条件对照、
   原三问、隔离启停/日志及故障恢复各自留证，不由本地fixture或旧包替代。
