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

- 原 Workspace manifest/registry 是唯一位置权威。`root` 保持本机产品数据目录；新项目的代码目录单独保存在 `metadata.project_location`。当前 manifest 的 `metadata.project_location` 必填；默认 create/create_at 也显式记录 Local(root)，不按旧 `project_root` 或产品目录推断缺失位置。缺字段拒读，原文件保留，不自动迁移。
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
- 创建弹窗用密码掩码表单完成认证，不展示黑色终端。认证仍由原系统 OpenSSH PTY、ControlMaster 和私有 control socket 拥有；表单只在原 SSH 已请求密码后，通过既有终端写入 IPC 暂时发送一次，随后清除本次密码。密码不进入 argv、env、项目 metadata、日志、持久化前端 store 或模型；未知写入不自动重发。主机指纹沿 OpenSSH 原提示明确交用户确认。
- “测试连接”只核对相同目标的原 `ssh -O check` 结果，不读取项目目录；目录可以稍后填写。点击创建/进入时才在同一 owner 上校验目录，`ssh-test`、`.` 和 `~/ssh-test` 按远程用户 HOME 解析，绝对路径保持绝对语义；命令 `pwd -P`、SFTP canonical path 与实际工作目录必须一致，读取失败不创建项目。
- 成功后原连接 owner 移交给 Workspace；取消只关闭本次 draft。认证 master 与普通终端 tab 分开，关闭 tab 不自动关闭项目连接。普通终端保留 xterm 的键盘输入；外层可访问文本框获得焦点时转交给实际输入区。
- 登录成功后清空未使用的口令并禁用密码编辑；换密码先断开重测，不能复用旧连接宣称新凭据通过。只读目录验证在 GUI 最多观察 30 秒，超时保留原验证 Promise 和物理 owner，同一 pending 请求只允许显式重试观察；旧观察迟到不自动创建。创建 IPC 的未知结果仍沿原合同处理。视图退役后的关闭失败会明确提示资源仍被应用持有，由原退出流程重试，不声称已有同 ID 的 GUI 关闭重试入口。
- 退出时先封准入，按原终端 owner 关闭并观察用户 PTY，再等待依赖的 host/I/O 资源；共享认证 master 等 Agent/file 操作结算后才关闭。不用杀本地 SSH 子进程冒充远端副作用已停止。

## 影响

标准 SSH 可以执行远端命令，但不提供远端 OS sandbox 的证据。`run_code`、资源和隔离请求必须按真实能力决定，不能沿用本机 Seatbelt 收据。取消、断连和可能已写入的 SFTP 请求沿原 typed failure/unknown 语义处理，不自动重放写入，不将 future 被丢弃解释为远端已回滚。

TaskRun 在原 `RunCreated`/目标版本中绑定经验证的位置 identity 和 revision。恢复或跨工作区执行不匹配时明确拒绝；文件 hash、Git 和验收证据使用同一目标或带来源与完整性校验的本机物化制品，不能读本机同名文件代替远端产物。

## 验证与文档边界

回归覆盖当前必填位置、新建与同版冷载、三来源 payload、失败回滚、有效位置变化、隔离 cwd、规则读取失败、连接移交、终端物理结算，以及带受控物理 barrier、丢失观察者和独立 operation id 的有界观察。观察入口不能修复后端提前返回未知错误并丢弃物理资源的问题，transport 未知结算仍须独立验证。真实远程认证与选定目录验收是独立证据，不由 mock 或端口连通替代。

这是 EKO GUI/IPC 的产品能力，不新增框架 SDK 示例或独立终端产品入口。正式 API 的通用文件提供者示例由框架自己的编译链验证；官网镜像必须在真实 EKO source checkpoint 后使用原同步机制，不把任务分支写成已发布 main。

### 密码表单的成熟实现依据与取舍（2026-10-26）

[OpenSSH `ssh(1)`](https://man.openbsd.org/ssh.1) 区分终端读取与 `SSH_ASKPASS` 图形询问；[VS Code Remote SSH 故障排查](https://code.visualstudio.com/docs/remote/troubleshooting)也明确将等待认证提示与连接完成分开。EKO 已有原 PTY/ControlMaster、二进制终端写入和连接移交，因此保留这一权威，改用掩码表单呈现，不再增加 askpass helper 或第二套连接 owner。表单是应用界面策略，登录成功仍由原 ControlMaster 证明，不能从密码提示、按钮点击或本地进程启动推断。

旧黑终端反例与新交互分别记录：浏览器真实 Modal/xterm 下，外层文本框聚焦时公开 probe 没有写入，直接聚焦 helper 输入区后产生写入；这证明一个输入可达性缺口，不倒推用户每次真实失败的原因。新表单/生命周期前端回归、原 PTY 无回显控制、目录解析和真实 SSH 验收按各自证据结算，源码或 mock 通过不代表 `ubuntu@111.229.245.175` 的真实认证已通过。

## 回复分叉的目标绑定（2026-10-27）

新 Worktree 是原 Git owner 创建的代码目录，不自行成为另一套 Workspace 权威。原 registry 登记目标后，分叉按返回的 product_data_generation 获取 scoped runtime；目标删除/重开或位置代际变化时拒绝旧回执。文件、Agent 命令、Git、终端和产物始终从同一个 execution location 解析。

用户直接触发的 GUI Git 原语沿既有 `project_run_git` 分派：本地项目复用原 native 进程路径，SSH 项目保留原 environment 与连接 owner，不在远程失败时回退本机。Agent 自动任务继续使用原项目 executor 与沙箱；这里不改变工作树算法、operation、merge lock 或清理权威。

自动打开分叉也必须把 expected generation 传入原工作空间切换准入，在同一个 transition 写锁内复用原 generation 校验，不能只核对前端缓存。目标重新登记或被删除时，拒绝旧选择而不接入后继目标。GUI 在这种切换被原生准入接受前保持来源投影；拒绝后查询原 current 事实，确认来源未变时保持原消息和文件绑定。资源列表从原 registry 重读，不从迟到回执重新登记已删除条目。

来源和目标各持自己的 SSH 认证 owner。准备与真正移交之间出现 draft 替换时，原创建/重连用刚才实际验证的 Arc 核对；已有来源 workspace 的连接不能作为可转移 draft。该检查防止框架 bug 误迁移或误关闭本地用户正在使用的连接，不给用户主动认证增加权限模式门控。

用户关闭 Fork 弹窗只退役观察。整段原 owned flow 在真实操作返回后，负责结算已证明属于本请求、仍未移交的独立 draft；认证 key、物理 Arc、有效目标和单一登记均须吻合。已移交后的旧 key 无项不关闭目标，key 被替换或双登记则保留并报告，绝不按未经验证的来源 key 清理。未知操作未返回时不提前关闭，清理失败与原错误一起进入原 supervisor 收据。

代码 HEAD-only 与历史资源保全分别处理：未提交代码留源，目标恢复所需的已知产品资源逐 bytes/大小/SHA 复制。来源关闭或独立清理后，目标仍由原 reader 读取自己的副本，不扩大 reader 根。部分创建/复制失败保留真实资源，从既有列表和 managed import 继续检查，不自动重建或删除用户代码。真实 SSH/终端矩阵与本机资源测试分开验收。

恢复提示依据实际 receipt：已有 Workspace、conversation 或 created／observed Worktree 时提示相应列表；只有分支残留且没有可打开对象时，不承诺列表存在目标。界面保留原错误与核对重试入口，不据 stderr 创建 pending store、认领或删除 live ref。旧包的失败与后继源码、集成和原生阳性结果分别保留。


## 2026-10-28：开发阶段唯一位置合同

用户明确本项目不承担旧版本兼容。位置改为当前必填类型，当前 registry 的默认、本地目录与已验证 SSH 创建入口先提供显式位置，再取消旧 manifest 的缺字段推断；不改变原 registry、revision、execution identity 或连接 owner。当前测试夹具随新合同明确位置，旧 manifest 自动推断的验收退役，不以 skip 或兼容分支维持。

前端 Workspace 类型同步必填位置，显示与项目终端直接读取该位置；verified SSH identity 仍用于实际执行展示，原用户配置保持独立。仅历史格式不再接纳，同版 inspect/reopen、代际校验、SSH 认证与 namespace、索引恢复和删除时保用户代码继续验收。Git Worktree 只解析实际生产者使用的 `--porcelain -z`，保留中文、emoji 和换行路径；非 NUL 旧文本或缺终止符的输入不构成可观察列表事实。


## 2026-11-06：免安装 SSH 运维的应用策略

日常运维沿既有 SSH Workspace、shell、文件与 command cell 完成，不新增远端 Eclip 服务、专用运维工具或任务状态机。SSH 运维要求与项目结构共用原 `eko:project-context` 投影，由初始工厂和 invocation 准备调用同一应用私有组装函数；`PreparedProjectContext` 保留本轮值，在 Execute/reset 后沿原 projector 发布。下一次本地绑定用同一投影槽替换或清除 SSH 内容，不把远端策略留在其它会话或全局静态前缀。

executor 的 `ssh` 名称仅选择模型行为要求，不证明认证、主机身份、权限或实际执行结果；这些继续由原 Workspace generation、位置校验、文件/命令适配器、权限和 TaskRuntime 负责。普通聊天不因此增加远端探测。原本地 refresh 的本地合同继续保留，真实 SSH 主路径使用既有可失败的文件提供者准备入口。完整初始化快照供 reflection 等消费者读取，不等同于一次模型准入；产品首轮应沿 factory→drive_chat 验证，不能以绕过 turn registration 的裸 chat 代替。

查询先回答用户范围，区分服务、进程和容器的计数与健康证据，说明采样时间和未知项；证据足够后收敛。控制需要原管理器或可靠身份与明确启动方式，控制后有界复查，不把 exit0 当健康。日志默认最近200行并受字节上限约束；跟踪只观察服务。断线或响应丢失时保留结果未知，重新查询不能证明上次命令是否执行，不自动重放控制操作。该策略描述目标行为，实际服务查询、控制、日志及故障表现仍须经原工具/TaskRun、真实模型、SSH 和 GUI 场景分别验收，提示词接线不等于完整运维交付。

## 2026-11-07：SSH 命令包装的输入与观察边界

远端命令的POSIX异步列表须从父shell预先保存的独立fd继承原stdin；只在后台命令上写`<&0`会在dash中复制已经置空的fd0。父shell先保存fd3，负载从fd3取输入后关闭副本，父shell也立即关闭副本，避免泄漏描述符。看门狗对子进程sleep承担回收责任，收到停止请求后终止并wait该原child，parent再wait看门狗。不能仅关闭外层subshell或重定向管道来隐藏残留；payload的stdout、stderr和正常退出码仍应保留。目标timer以秒为粒度时向上取整，避免早于原Duration发送信号；本机仍使用原精确观察预算。

取消或本机预算到达后，观察沿既有CLOSE_OBSERVATION_TIMEOUT有界返回；原物理任务、child与invocation guards继续由原连接持有，不abort或清除unknown。等待原物理观察丢失后的结算也受同一观察期限约束。原SSH非255退出已经确认时，结果观察失败与停止未知分别表达；未知副作用仍不自动重放。

None的旧POSIX看门狗只监督原直接PID；用户Shell脚本的后代可能继续存活并持有输出管道，不能把124或直接PID退出当成整组结束。原观察预算到期须有界返回Unknown并保留原物理owner，直到原wait及管道实际结算。直接PID期限测试必须使用exec负载，Shell后代负控单独检查Unknown及资源保留；不能靠改成exec掩盖后代仍存活的事实。需要整组物理结束证明的Some继续使用[ADR0064](0064-background-cell-receipt-ownership.md)的同一原owned-group控制器；本修正不增加None的Python运行时前置。

OpenSSH的退出状态可以直接来自payload。124可能是程序自然返回，也可能与看门狗有关，不能据此声称“实际被杀掉”；timed_out只表示本机观察预算到达，取消也不证明被操作服务停止。需要目标状态时沿原管理器或工具再查，退出原因缺证据就保留未知。

该边界参考[POSIX异步列表与输入重定向](https://pubs.opengroup.org/onlinepubs/9799919799/utilities/V3_chap02.html)、[OpenSSH退出状态](https://man.openbsd.org/ssh#EXIT_STATUS)，并对照本机moss-agent/dev `41856a8590e3`的`spawn_process_settled`：取消后仍收集原child，终止后等待communicate。复用的是所有权与回收模式，不部署其远端服务、操作库或设备协议。本地夹具与脚本测量不替代真实SSH、GUI及App验收。

## 2026-11-03：默认本地创建与已有 SSH 的重连入口

新建本地文件夹未选择父文件夹、且新目录名称字段留空时，GUI 直接复用原 `source=None/root=None` 创建路径：`WorkspaceRegistry::create` 决定默认目录与 `Local(workspace.root)`，此模式项目与本机数据共处默认 root。界面显示原 `get_default_root(name)` 返回的真实目标，名称尚未填入时不强制用户先选择父目录。项目位置与本机数据 root 分别记录，但默认或显式选择的路径都可能物理重合，不保证物理分离；不能把元数据 root 当成自定义项目父目录。

目录名称留空仅以工作空间名称作为默认值；用户实际输入任何非空目录名称，即使与显示名称相同，仍按显式输入处理。显式父文件夹保留原输入；父文件夹留空时由原 Registry 的 `base_dir` 字段提供默认父目录，再沿原 `NewLocal` 单组件校验、`PathBuf::join`、独占 `create_dir`、同一 manifest/index 发布与失败后的自有空目录回滚。目录名不经 `WorkspaceId` 清洗或 64 字符截断，大小写、空格和 Unicode 保留；文件系统自身的大小写/冲突规则不被规避。已有目录模式继续要求有效路径。创建失败保留草稿和错误，不覆盖既有项目或自动重发。默认目标异步读回只更新展示，不写入手输字段；切名称、切模式或关闭后旧响应退役。

持久化 SSH Workspace 只保存目标与位置，不证明新应用进程已经持有可用认证连接。侧栏已有工作空间及远端最近任务切换失败时，复用原工作空间弹窗的重连分支，携带 workspace ID 与 generation 作为界面导航意图，给出中文恢复提示并保留原错误。密码/私钥口令仍由用户在原掩码表单输入，不缓存、读取旧认证或借另一个 owner。目标认证与同一远程目录验证成功后，原 `switchTo(connection_id, expected_generation)` 才尝试进入；取消、迟到或 generation 被替换不得发起新的切换或重建工作空间。界面意图不成为连接、注册表或执行状态的新权威。

本轮前端正式定向 5 个 suite 共 90 项通过，类型检查、ESLint 零警告及格式检查均自然 0。受控完整 app-core crate 使用同两条真实临时目录测试：原 Registry 空父目录行为实际 1 项失败／自然 101（os error 2），保留控制实际 1 项通过／自然 0；仅切换固定修复后，两项各选中 1 项并全部通过、无忽略。两阶段真实 Cargo NoRun 均自然 0、编译 W0/E0，实际 lib test 程序及完整指纹和依赖元数据已保留；同一 GREEN 来源执行定向 `cargo clippy -p echo-agent-app-core --lib --tests --no-default-features --offline --locked -- -D warnings` 自然 0、W0/E0。此受控本地 Registry／文件系统资格不代替实际 SSH/PTY、GUI、新 App 用户手验或发布，正式联合 66a502 来源的 fresh 风险 gate 仍为自然 1、资格缺失，不能借测试绿冒风险 gate 0。测试不访问原用户工作空间或真实服务器，也不操作当前运行包、窗口、认证或 SSH 全局配置。现有公开 API/SDK/examples 未新增；官网没有新增能力说明，后续只按最终合格来源同步。

## 2026-11-07：免安装 SSH 的通信代际恢复

背景：旧SSH master断开、原命令255且结果未知时，原operation和Workspace guard必须保留。
此前重连复用删除前idle/Host退休与旧连接close，导致原unknown阻止同Workspace恢复查询。

决策：TaskRuntime、Host、AgentPool、本机store和项目位置不因通信恢复替换。
TerminalService在同一个项目owner内保存current及仍需结算的retired物理连接；新认证draft
再次核对原host/user/port/目录和预期generation后原子发布，只影响后续新准入请求。
原调用、guard及未知结果不转交，不自动重放启停，不以新目标状态证明原命令成功。

文件方法、命令future（包括缓冲stream）在入口捕获同一个物理快照与request lifetime。
准备session、reader和复合checkpoint/CAS的内部步骤复用该快照。发布封闭旧代际的新准入，
已有快照的子步骤继续原连接；最后持有者释放后，原owner有界观察可回收的退休连接。
旧unknown继续依赖原物理结算证据，不因超时、EOF、重连或本机观察结束而清除。

关闭draft必须同时验证逻辑owner准入与物理master；已发布Workspace或包含退休连接的owner
不能作为独立draft转交。关闭由原owner保存实际结算任务，观察者取消不丢关闭责任。
所有current/retired共用一个绝对关闭观察期限；有未确认资源时保留原实例和错误。
GUI沿现有transition回执说明“新连接可用，旧操作仍未知”，不另增设备控制台或任务状态。

没有采用关闭检查放宽或覆盖map：会丢原资源责任。没有重建Host：会重开本机权威或使旧请求
改绑。没有引入远端server或结果账本：本轮只恢复后续查询，找回原退出结果不在能力承诺内。
机制限定在应用SSH适配；框架trait、工具schema、权限与恢复权威继续复用。
唯一设计入口为docs/supreme/specs/ssh-remote-device-optimization/design.md；实际验证与真实SSH、
GUI/模型及重新构建App仍须分别取得证据，本节不是已完成回执。
