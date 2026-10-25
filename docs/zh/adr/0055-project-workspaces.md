# ADR 0055：项目位置与工作区执行环境

- 日期：2026-10-25
## 状态

accepted

任务分支实现与验证独立于 main 发布。

## 背景

工作空间、项目与工程是同一个 Workspace，任务和会话归属 Workspace。创建 Workspace 时可以选择已有本地文件夹、在指定父目录中新建文件夹，或打开 SSH 远程项目；新建任务使用选定的 Workspace，不再新建目录。单独打开 SSH 终端不会让文件工具、项目规则或 Agent 自动切换到远端。

原 `Workspace.root` 同时承载产品数据和默认执行目录。仅添加一个远程地址或 `project_root` 字段无法覆盖工具、上下文加载、Git、隔离和验收产物读取这些真实消费者。

## 候选方案

1. 只增加独立 SSH 终端：复用简单，但其它项目操作仍在本机，不能满足项目语义。
2. 在远端部署完整 EKO Agent/IDE 服务：增加远端安装、版本、配置与生命周期，不符合本轮本机 GUI/Agent 主循环目标。
3. 保留本机 GUI/Agent 主循环，以原工作区 host 绑定位置，文件使用标准 SFTP、命令使用系统 OpenSSH，复用原工具和生命周期。采用此方案。

参考 [VS Code Remote SSH](https://code.visualstudio.com/docs/remote/ssh) 的文件与执行位置一致原则，以及 [TRAMP 远程进程](https://www.gnu.org/software/emacs/manual/html_node/tramp/Remote-processes.html) 的位置感知客户端方式。本项目不复制 VS Code Server。传输复用 [标准 SFTP 客户端](https://github.com/openssh-rust/openssh-sftp-client/tree/ad1b77f1d7b35fc39183bae9d68034e3d9f9e8e6)，不自建 SSH/SFTP wire protocol。

## 决策

- 原 Workspace manifest/registry 是唯一位置权威。`root` 保持本机产品数据目录；新项目的代码目录单独保存在 `metadata.project_location`。旧 manifest 未写新字段时仍按原 `project_root`、`root` 顺序解析，不移动或删除用户目录。
- `ProjectLocation` 区分 `Local { path }` 和 `Ssh { host, user, port, path }`。SSH 路径保存为字符串，绝不交给本机 canonicalize。远端实际目录由 SFTP 与同一执行连接确认。
- 用户配置与执行身份分别保存在同一 manifest：`project_location` 用于配置/展示，`project_execution_location` 保存经过验证的实际 SSH hostname/user/port 和实际目录。系统 `ssh -G` 解析一次后，同一个连接固定有效目标；重新连接必须匹配已保存身份。原 `project_root_revision` 同时保护两项信息，不添加独立版本表或任务状态。
- 已有本地文件夹只关联；新建文件夹使用 exclusive 单目录创建，不覆盖已有目录。注册失败只回滚本次拥有的产品数据与仍为空的新目录。移除工作区只删除 EKO 数据，不删除项目代码。
- 同一 host 的 primary/pooled Agent 共享其原 ToolManager；不同 host 选择独立的文件提供者和执行器。提供者不固定每次 cwd，实际 invocation、readonly/writer checkout 的工作目录继续由原执行路径携带。
- 框架只提供通用 `FileSystem`、`SandboxExecutor` 和原工具注入接缝。Workspace、SSH 认证、位置版本、GUI 和产品数据策略仍归 EKO。
- 目录树、gitignore、项目指令、项目 Skill 与 Subagent 来源必须读取相同位置。用户配置、用户 Skill、记忆、会话、journal 和本机制品保留原本机权威。
- 项目规则在原可失败的模型准入前准备。主 Agent 初始化失败直接返回错误；Subagent 在隔离已给出实际 cwd 后，在原 invocation 执行入口准备，成功后才委托原 Agent。不能将可选 projector 的 warning 当成必需规则读取已中止。
- 主智能体每轮的已准备项目事实放在原 turn registration 中，由原 EKO projector 在 Execute/reset 或 checkpoint 恢复后重新发布，同时保留 Domain、TaskRuntime、turn policy、hot memory 与命令观察。没有 TaskRun 的普通调用也使用原 turn owner；并行主智能体验证以已有 execution ID 作为独立 turn ID，原根消息仍是触发来源。fresh Subagent 将自己的实际 cwd 事实与原 Domain-only renderer 组合，不继承主智能体 checkout。文件准备和 Git 命令持续携带原 invocation 资源 guards，直到物理结算。
- GUI 文件操作和项目终端捕获 workspace ID 与原 generation，在后端借原 host control/I/O receipt。人手终端不占 Agent 执行槽，不要求 LLM 或 full-auto。活动 PTY 的 receipt 保持到原 child/reader 结算，防止项目删除或重关联误判空闲。
- 六个非本机项目文件操作使用原 ProductDataIo 的显式 30 秒观察，包含等待原信号量的时间，返回同一 u64 operation 的类型化 Unconfirmed；这是等待反馈上限，不是取消或远端执行期限。超时或观察者被丢弃只把该原 id 标为 retained，原 detached 物理任务、permit 与 host receipt 保持到真实结算；迟到失败保留在原失败收据。本机/global 调用保持原等待方式。普通侧栏焦点切换保持原多 host 行为，位置重绑、重连和 host 退休仍受原 idle 门禁约束。
- 已物理完成的观察结果通过结果封装移交同一个原 operation；实际取出才确认交付，未读取的缓存结果被丢弃时先记录其错误再结算同 id。通道发送成功不等于已被观察。正常读取到的业务错误返回调用方，不记作未观察的退出失败。
- 认证通过创建弹窗的原 PTY 完成，密码不进入项目 metadata、持久化前端 store 或模型。成功后原连接 owner 移交给 Workspace；取消只关闭本次 draft。认证 master 与普通终端 tab 分开，关闭 tab不自动关闭项目连接。
- 退出时先封准入，按原终端 owner 关闭并观察用户 PTY，再等待依赖的 host/I/O 资源；共享认证 master 等 Agent/file 操作结算后才关闭。不用杀本地 SSH 子进程冒充远端副作用已停止。

## 影响

标准 SSH 可以执行远端命令，但不提供远端 OS sandbox 的证据。`run_code`、资源和隔离请求必须按真实能力决定，不能沿用本机 Seatbelt 收据。取消、断连和可能已写入的 SFTP 请求沿原 typed failure/unknown 语义处理，不自动重放写入，不将 future 被丢弃解释为远端已回滚。

TaskRun 在原 `RunCreated`/目标版本中绑定经验证的位置 identity 和 revision。恢复或跨工作区执行不匹配时明确拒绝；文件 hash、Git 和验收证据使用同一目标或带来源与完整性校验的本机物化制品，不能读本机同名文件代替远端产物。

## 验证与文档边界

回归覆盖本地旧数据、三来源 payload、失败回滚、有效位置变化、隔离 cwd、规则读取失败、连接移交、终端物理结算，以及带受控物理 barrier、丢失观察者和独立 operation id 的有界观察。观察入口不能修复后端提前返回未知错误并丢弃物理资源的问题，transport 未知结算仍须独立验证。真实远程认证与选定目录验收是独立证据，不由 mock 或端口连通替代。

这是 EKO GUI/IPC 的产品能力，不新增框架 SDK 示例或独立终端产品入口。正式 API 的通用文件提供者示例由框架自己的编译链验证；官网镜像必须在真实 EKO source checkpoint 后使用原同步机制，不把任务分支写成已发布 main。
