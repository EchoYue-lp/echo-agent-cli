# ADR 0051：显式 SSH 目标与原终端会话结算

## 状态

已采纳

## 背景

已批准 Plan06（plan:08c22af5-243e-4460-a70c-2c5e2ec186b6，SHA 7f0a96b913a019d2623283ea711f89a93daa2d60a1e86b4635f72797081624c0）从 EKO 2b52251 与框架 e4e5039 接续。现有 TerminalService 已拥有 PTY，但 GUI 先 create 后监听、切 tab/隐藏卸载、逐块解码 UTF-8，close 仅调用 clone_killer 后立刻移除。portable-pty 0.9.0 的 Unix clone_killer 只发 SIGHUP；原 Child::kill 才有 HUP 宽限后强终止，且仍需原 wait 与 reader join。

## 候选方案

1. 原 create 内使用系统 OpenSSH；前端先就绪再创建，原 Session 保留所有资源与结算任务。采用。
2. 新 SSH 协议库、daemon、独立执行器/TaskRun 或后端 reserve/start、输出重放 store。超出原终端切片边界；下文已批准的项目扩展使用标准 SFTP 适配，不新增任务或会话权威。
3. 用 Connected 动画或 shell prompt 判断登录，或 kill 返回即发布 Closed。不构成认证或物理结算证据，拒绝。

## 决策

通用 SSH 协议、认证、config/agent/key/host-key 仍归系统 OpenSSH；PTY/Child 原语归 portable-pty。EKO 的 TerminalService/TerminalSession 仍是唯一会话与目标/准入/资源权威，Tauri 只转发。框架、SDK pin、Agent ShellTool 权限与 TaskRuntime 不变。GUI 手工连接不授权模型自动执行；当前模型不注册 TerminalService 工具。

创建仅有既有 create 一步：target 为 local 或结构化 ssh（host/alias、可选 user/port），参数直接给系统 ssh，不经 shell 拼接、不覆盖用户未填写的配置、不添加弱化 host-key 的参数。程序缺失明确失败，不回退本地 shell。产品不保存凭据，也不硬编码验收服务器；启动仅证明本地进程已启动。

每个 tab 先挂载 xterm、输出/退出/缺口观察及有界串行写队列，ready 后才 create。所有 tab 跨切换和隐藏持续挂载。PTY raw Uint8Array 直接交 xterm，其写回调释放在途预算；输入同样串行且有界。真实广播 lag/队列溢出可见，不伪称完整历史或 VT 重放。每个异步 setup 步骤检查取消并清理自己的实例；已发出的 create 保留原 Promise，取消后精确补偿 close，失败保留可重试 tab。

原 Session 保存原 Child、输出 reader 的 JoinHandle 和一个结算任务句柄。关闭设置原会话请求，结算任务使用原 Child::kill/try_wait/wait 并实际 join 原 reader，全部证明后才按实例身份移除并发布一次退出。调用者只借用等待句柄；取消或有界观察超时不 abort 原结算、不取走活句柄。kill/wait/reader 失败保留原资源和目标供重试，不能发正常 Closed。不可恢复的 reader 错误不得靠重试抹掉。直接 child 回收不表示进程组或远端副作用全部停止。

ApplicationLifecycleOwner 的原 shutdown 阶段同步关闭终端准入；close_all 与 create 共用既有创建边界。终端未结算使同一应用 owner 继续 retained 重试，事件 bridge 的 join 不替代 PTY reader join。创建/list 的实际 id/pid/target 与 close 实值同步到手写 TS 合同，删除虚构 cwd/created_at。

## 影响

源码与测试阶段不等于已交付运行能力。当前无 Cargo/native/远端租约；Rust、原生窗口、真实认证与远端交互、完整矩阵和 main/CI 都待实际验证。不复用旧 GUI binary 作为新协议验收。现有真实目标仅在协调验收阶段由用户提供，产品配置/测试使用通用目标。

existing examples/lh6_product_soak.rs 不调用终端/SSH，无源码迁移；若后续变更示例须纳入原编译链。官网由协调者按已验收精确来源镜像，本任务不部署。新 Rust 字节不自动继承 ace3 风险指纹，后续按原六项精确风险独立复评，不扩大接受范围。

## 实现依据与验证

本机 OpenSSH 10.3p1 的官方 ssh(1) 说明 destination、-l、-p 与交互 TTY；不读取用户 SSH config。portable-pty 锁定 0.9.0 的 lib.rs/unix.rs 是 Child kill/wait 与 EIO→EOF 的精确源码依据。xterm 官方 API 明确接受原始 Uint8Array，并通过 write callback 表示异步处理完成。

参考：[OpenSSH ssh(1)](https://man.openbsd.org/ssh.1)、[portable-pty 0.9.0 Child](https://docs.rs/portable-pty/0.9.0/portable_pty/trait.Child.html)、[xterm Terminal API](https://xtermjs.org/docs/api/terminal/classes/terminal/)。与 VS Code Remote SSH 的系统 SSH/明确目标模式一致，只实现已批准的交互终端，不实现远程 IDE。

验收沿 Plan06：Node 反例覆盖首输出、setup/cleanup、raw UTF-8、多 tab/隐藏、队列和 close 失败；Rust 后续覆盖忽略 HUP 的原 child、wait/reader、取消/重试和 shutdown 准入；真实 Tauri/远端交互单独取证，未执行项保持未完成。

## 项目连接扩展

用户已明确侧栏任务相当于项目，创建来源是现有本地目录、新建本地目录或远程项目。本扩展以项目工作环境设计和 Plan `plan:986e9191-1a18-454b-a8b8-59ae18ad3bc7` 为准，不再把独立终端验收当作远程项目验收。Workspace 仍拥有项目及位置，TaskRun 仍拥有执行事实；本 ADR 只负责原终端/SSH 资源适配。

系统 OpenSSH 在原 PTY 中以前台认证 master 运行。GUI 先确定 session id 并订阅，再启动进程，避免丢失首个密码或 host-key 提示。项目认证用密码掩码表单呈现，不再展示黑色终端；密码只在原进程已请求时通过既有写入 IPC 送入原 PTY 并清空，不进入 argv、env、项目元数据、模型上下文或日志。提示只用于决定何时请求输入或展示指纹，不能证明登录成功。真实 `ssh -O check` 状态用于独立测试登录；创建时相对目录按远程 HOME 解析，再用原连接的 SFTP canonicalize/metadata 和命令 cwd 交叉验证，不检查本机同名 PathBuf。

每个连接使用应用本次拥有的私有 control socket，明确 `ControlPersist=no`。普通项目终端、结构化执行和 SFTP 共用该 socket；slave 设置 `ProxyCommand=false` 和 `BatchMode=yes`，在检查与实际使用之间 master 消失时也不得偷偷新建连接。host key 校验不关闭。创建成功在原服务边界内把同一连接映射从 draft owner 移交 Workspace 身份；取消/关闭与移交串行，旧 draft 不能误关已发布项目。

认证前由系统 `ssh -G` 原生求值 Host/Match，读取有界输出中的实际 hostname/account/port。原连接仅固定一次这些事实，复用 TerminalTarget 形状提供读取；持久化、位置 revision 和恢复比较只由 Workspace 拥有。raw alias 仍选择原 SSH config、IdentityFile、agent 和 ProxyJump 行为，同时用命令行 HostName/User/Port 及禁止再次 canonicalize 固定解析结果，防止解析到 A 后配置变化使 master 偷偷连 B。已解析主机名的字面百分号在再次 HostName 展开前转义。新连接实际目标改变必须显式处理项目位置，不能自动恢复；配置错误或超时不启动认证、不把完整配置写日志，也不假称已连接。

认证 master 不是普通终端 tab。创建成功仅卸载认证视图，原服务继续持有 master；普通项目终端是 mux slave，关 tab 不关整个项目。创建失败明确关闭本次 draft owner。项目关闭先封闭新请求，观察文件/命令/slave 的实际资源，再只关闭自有 master。原 child wait 失败保留同一 child 供重试。应用退出沿原生命周期，不对用户已有 master 执行泛化的 `ssh -O exit`。

`openssh-sftp-client` 在本次 system-SSH subsystem 流上实现标准 SFTP，不启用其可选 `openssh` 连接库。已通过官方 registry API 独立确认 0.15.10 实际发布、未 yanked、MIT、MSRV 1.64。取消 future 不撤销已发送请求：继续观察原操作资源、read/flush 和 child 结算。写入失败沿原结构化“可能副作用/核验后再试”合同，不让 patch rollback 当作确定未写。原子替换要求服务器提供 posix-rename；原框架 checkpoint 格式经位置适配继续复用，未装配 checkpoint 是错误，不冒充“非 Git”。

既有 SandboxExecutor 适配真实远端 cwd、环境和结构化程序参数；plain SSH 的隔离等级是 `none`，本机 Seatbelt 不构成远端隔离证据。不支持的隔离/资源约束明确返回能力错误。取消或超时不会杀本地 SSH child 后声称远端已停止：原物理执行和两条输出 pipe 继续观察，已实际完成但副作用不确定时保留原 typed failure；丢失远端停止证明时则连同原资源保持非终态。缓冲执行不宣称实时流。

原 SandboxCommand 的 runtime-only resource guards 和取消 token 在 spawn 前进入同一个 SSH 物理调用，不序列化为新 wire contract。wait 失败时同连接保留原 child 和原 guards，待 pipe 实际 join 后才能成功关闭。取消的有界非终态观察仍由原 command-cell/Task owner 提供，适配器不制造终态 Cancelled，也不另起执行 owner。

本地和 SSH 项目终端也在 spawn 前接收原 host I/O lifetime guard，TerminalResources 持有到原 child/reader 实际结算，取消 GUI 观察不释放。这是 I/O lease，不是 Agent/LLM 槽位。原应用退出顺序必须先关闭/观察这些终端，再等待 host I/O lifetime，保留同一收据和 busy proof，避免自己等待自己。

原 begin_shutdown 封闭创建后，close_interactive_sessions 只关闭/观察同一 sessions 中的 human 本地/mux 会话，保留共享项目认证 master。原生命周期随后结算 Agent/文件工作，再沿 close_all 关闭 idle master。两者复用同一会话表和原 child/reader 结算，不新增终端 registry 或退出状态机。

## 远端文件 CAS 与有界读取

GUI 原内容 revision 合同使用持久侧车 `.<filename>.lock`，遵循同协议的进程在同一锁内完成完整读取/SHA-256 比较和替换；普通 atomic_write 或外部编辑器不会自动参与。这不是 inode identity，也不是对所有外部写者的事务。SSH FileSystem 通过固定的一次性 Python stdlib 程序实现同一限定操作：同名侧车 POSIX flock、锁内完整流式内容哈希、自有唯一同目录临时文件、原权限保留、file fsync、原子 replace、parent fsync，再 unlock。冲突返回 false 且不替换目标；错误可能发生在替换之后，继续保留可能副作用。侧车不删除，临时文件撞名不把他人的原件当本次清理对象。

固定代码交给原 executor，路径、expected digest 和 base64 新内容只通过有界结构化 stdin 传递；不在服务器安装代码、不建 daemon 或新 Agent 工具，不打印输入或异常全文。隔离 Python 只使用标准原语。同连接上的只读 probe 检查可用性，真实 helper 再核一次；缺能力明确 Unsupported。probe 成功不代表该服务器/文件系统已验收。取消或 SSH 响应丢失不自动重放写入，也不报告虚假回滚。

标准 FileSystem.open_read 返回真实 SFTP file handle 上的 reader，每次请求至多 64 KiB，与制品总大小无关，不预读全件，也不依赖 Python。原 CompletionGate/哈希算法消费该 reader；读取失败即停止，不猜 offset 重放；成功返回 EOF 前观察原 file、client tasks、child 与 pipe 结算。普通文件请求和 reader 共享原 channel 启动/结算路径，提前 drop 沿同 blocking caller 与连接 guard 清理，不新增 runtime、线程或 owner。

标准 SFTP 属性没有原 directory guard 所需的稳定 dev/inode 和 no-follow 持有目录 identity，该独立能力继续 Unsupported；需要该保护的生命周期 mutation 必须在副作用前拒绝，不能用 mode/mtime/realpath 替代。普通读/列目录/Git 或 CAS 不代表目录 identity 保护已合格。

固定 helper 已在自有本机临时目录通过 13 个原生 fixture，覆盖不同进程竞争、其它协作写者、完整/尾部内容比较、binary/中文、权限和 rename/fsync/unlock 后错误。故意去锁或只 hash 首块各产生预期断言失败。这不代表 Cargo/Rust、真实 SFTP、远端 Python 原语、GUI 写入或真实服务器已验收；fixture 同时纳入 app-core 后续测试链。现有 examples 与 SDK accepted 合同无新增 CAS 消费者；官网按合格集成同步，不部署。

依据：[Python flock](https://docs.python.org/3/library/fcntl.html#fcntl.flock)、[Python 原子替换](https://docs.python.org/3/library/os.html#os.replace)、[Python fsync](https://docs.python.org/3/library/os.html#os.fsync)。锁定 fs2 Unix 实现使用相同 flock/LOCK_EX 与 LOCK_UN，Local 仍以原 core helper 为权威。

源码初期面向 Unix OpenSSH multiplexing；其它平台、实际编译、依赖风险资格以及真实 GUI 密码/远程项目仍需独立证据。测试定义和旧终端二进制不代表该流程通过。现有 examples 没有项目 SSH 消费者；产品说明和官网由合格集成源码同步，不执行部署。SDK 公开/accepted 合同未变化。

依据：[OpenSSH multiplexing 配置](https://man.openbsd.org/ssh_config.5#ControlMaster)、[标准 SFTP 客户端](https://github.com/openssh-rust/openssh-sftp-client/tree/ad1b77f1d7b35fc39183bae9d68034e3d9f9e8e6)、[已发布 0.15.10](https://crates.io/crates/openssh-sftp-client/0.15.10)。官方原始响应与源码字节保存在本次工程证据中。

## 远端结算知识与原有界观察

SandboxExecutor.execute 继续表示原物理执行的结算 future。command deadline/cancel 是请求，不构成该 trait 裸调用有界返回的新合同，也不证明远端终止。实际 TaskRun/command-cell 沿原 owner 提供有界非终态观察；GUI 文件/Git/CAS 直调由集成应用原 ProductDataIoService 的有界观察承接：截止只限制调用方等待，原已准入 I/O 操作、blocking closure、host lifetime 与 permit 继续保留。后端不伪造 TaskRun 身份、普通 Cancelled 终态或第二监督对象。

真实 command/subsystem child 启动后、下一次 await 之前，同一 ProjectSshConnection 的 retained_children 容器同步保留原 operation、invocation guards 和不可变 effective target。丢失物理观察者不丢此资源投影。真实完成后移除同一项并释放，不让已完成命令持续堆积。原本地 child 的重观测与远端停止知识分开；local EOF/wait、master check、重连、等待时间或 alias 改变不能清除远端未知义务。现有 cleanup 只重观同一原资源。

命令可能已启动后收到 native SSH255 或没有 exit status，表示远端停止证明不可得。原调用在同一连接的 progress 上等待，保留 guards 与 master 义务，不返回普通 Possible 错误让 Cell 冒充已完成。当前 transport 没有可恢复的远端命令身份或丢失退出回执重放能力，所以真正断线后的义务没有自动核清事件；须诚实保留，不引入 daemon、进程 store、猜测 PID/kill、用户手工 receipt 控制台或确认绕过。真实非255命令退出且必要本机资源已 join 则证明命令完成，即使退出125..127、CAS结构回复报告替换后错误，或文件副作用仍待核验。这些 Possible 内容不造成永久物理保活。

标准 SFTP 薄适配在映射 ToolFailure 前保留 native mutating request 是收到服务器响应还是丢失响应。标准 Sftp.close 会真实等待 buffered/sent requests 的响应；写响应丢失且没有该 close 证明时，同连接保留原义务与非终态 future。已完成请求的文件内容不确定可以正常释放。不能只凭本机 child 终态冒写入响应，也不能重放失败的 mutation。

v4 回归定义在同一生产 launch/wait/resource 路径注入自有本机 fixture 进程，覆盖255、缺 status、观察者丢失、真实已知完成与 deadline/cancel 观察。本切片仅定义源码，未执行；它们和旧本机 CAS fixture 都不代表 Rust 编译、集成 ProductDataIO/TaskRun/Cell 路由、真实 SFTP/服务器或自动重连恢复合格。examples 与官网仍由合格项目集成交付同步。

SFTP 错误薄适配把当前请求是否丢失写响应，与整次操作此前已完成的副作用分开。递归删除、逐级建目录保留本次局部的 completed-mutation 标志，原子写在父目录创建后的 metadata 查询也带回此事实。后续只读失败在已有 mutation 时仍为 PartialSideEffect/Possible/VerifyThenRetry，同时不制造未完成写请求的停止义务；没有 mutation 的只读失败保留普通无副作用分类。两份受控本机 fixture 对照仍只是未编译测试定义，不代表真实 SFTP/服务器验收。
