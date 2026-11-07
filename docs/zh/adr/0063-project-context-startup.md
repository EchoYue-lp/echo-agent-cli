# ADR 0063：项目上下文准备的生命周期与远程访问成本

日期：2026-11-06。状态：用户已授权实现，验证结果另记。

## 背景

SSH 首次输入出现 32.499 秒才产生首个 Agent 状态事件。工厂和 driver 各自完整准备上下文，内部又重复发现项目根；每次文件操作重新开关 SFTP 子通道。本地 stat 的低成本假设不能用于远程文件系统。

## 候选方案与决策

1. 永久缓存规则正文或根据输入是否是问候跳过规则：不采用，无法保证每条消息使用当前规则。
2. 无界共享所有 SFTP 通道：不采用。普通文件操作沿已存在的两条独占车道复用；准备会话与流式读取各自持有原通道，不能把车道容量当成整条连接的通道总数。
3. 当前准入一次准备、阶段内共享只读通道、阶段后结算：采用。

由实际 chat driver 驱动的创建使用 `acquire_for_turn/agent_for_turn`，工厂仅发现构建必需的角色定义；项目规则、结构和 Git 状态由现有可失败的 invocation 准备入口读取一次。普通 `acquire/agent_for`、primary 和独立工厂默认继续完整初始化，避免其它直接消费者拿到缺规则的 Agent。显式延后入口只允许与原 driver 配套使用。

指令加载器接收已经解析的项目根，cwd 保持独立；每条消息重新读取，不沿用上一条消息的正文。延后实例进入缓存时记录 raw 初始化尚未完成；普通消费者取得它前，在同一个实例上可失败地补全投影，不丢消息、不重建。driver 成功准备并发布后同步确认原实例已初始化，保持活跃追加输入与只读面板可用；准备尚未完成时，独立模型调用明确拒绝未初始化的 raw 获取，避免持构造锁等待活动 Agent。初始化标记只接受与原 lease 相同的 Agent 实例，ambient foreground scope 中的其它 Agent 不能改写它。模型循环的 projector 仍只发布当前 turn 已准备的不可变事实，Execute/reset 和 checkpoint 恢复不能抹掉本轮规则。不同 Subagent 的实际 cwd 继续由原适配入口解析。显式项目的初始化与取消后 raw 补全共用同一个位置准备入口；cwd 中的嵌套 Git 仓库不能重定向显式外层根。

## 分层与已有实现复用

关键指令每次 invocation 准备都通过所选文件提供者读取并校验 UTF-8；不使用 mtime/size 或本机短期窗口复用上一轮正文。外部编辑器可能保留旧 mtime 和长度，依赖这些属性会静默注入过期指令。性能收益来自阶段内共用传输和同轮复用 `PreparedProjectContext`，不以跨轮正文陈旧换取。

- 通用机制：framework `FileSystem` 增加可选只读会话接缝，默认没有资源或缓存；不依赖 SSH、Workspace 或 Eclip。属于产品无关的传输批次机制。
- Eclip 策略：何时准备、哪些指令、目录和 Subagent 来源，以及 pool admission 继续留应用。
- 适配边界：现有 `ProjectSshFileSystem` 在准备作用域内复用原 SFTP 客户端，关闭时沿原 channel 的 close/child/reader/resource 结算。不自建协议或第二个连接 owner。

沿用 `PreparedProjectContext`、`InstructionProjectionSnapshot`、turn registration、原目录/规则加载器和 `openssh-sftp-client`。没有新任务状态、规则存储或调度器。

## 远程探测与并发约束

根搜索仅提交已知 marker 路径，不枚举仓库或 HOME 的无关文件。远程会话最多流水化 32 个 metadata 请求，按原顺序解释结果和错误；先搜索所有祖先 VCS 根，再按原 fallback 顺序选择，不能让嵌套 Cargo.toml 隐藏仓库 AGENTS.md。目录规模不会放大 marker 搜索的请求数量。

Agent 构造使用独立互斥，远程 I/O 期间释放实例表写锁。已有会话的获取不等待构造互斥；同一 key 不重复创建。模型、插件及工作区发布与构造串行，普通配置模板更新使用相同锁序。关闭准入后只短暂等待在途构造，再释放构造互斥并等待现有执行结算；原 shutdown/retirement 在途构造结束后才完成；创建结束重新检查关闭与退休，防止已删除会话复活。

## 失败与资源生命周期

只读会话的显式 `finish` 传播关闭错误。外层 await 被取消时，原 blocking 准备仍持有 invocation guards，直到会话自然关闭；Drop 也结算原通道。作用域内禁止写入、CAS、checkpoint 与逃逸的流式 reader。普通写入工具的逐操作原结算、未知副作用及禁止自动重放保持原合同。

关闭连接先封闭新操作准入；普通车道每次复用也检查原连接和 TerminalService 的准入。排队、原通道关闭及剩余操作观察共用一次有界期限。观察超时后原物理任务继续持有通道、child、reader 和 permit，不取消或重放已发请求；原 owner 可恢复准入并重试观察，不能清除未知副作用或假报关闭成功。

每个已实际启动的子进程按连接 generation 记录普通 SFTP、准备会话、流式读取和业务 SSH 命令累计数；SFTP 初始化失败仍计一次启动。原车道统计继续表示 checkout/open/reuse，不代表连接总通道数；认证检查、主连接与交互 PTY 不属于业务命令计数。观测不成为连接或结算权威，不记录口令或文件正文。

## 外部依据与取舍

[VS Code Remote SSH](https://code.visualstudio.com/docs/remote/ssh)通过远端 Server 执行工作，减少跨网络碎片访问；Eclip 保留既定本地主循环和标准 SFTP，用阶段内复用降低往返。[TRAMP 连接缓存说明](https://raw.githubusercontent.com/emacs-mirror/emacs/master/doc/misc/tramp.texi)强调复用及失效；本次只复用当前阶段传输，不长期缓存规则。[标准 SFTP 客户端](https://docs.rs/openssh-sftp-client/0.15.10/openssh_sftp_client/)本身支持请求缓冲，取消 future 不撤回已发操作，因此必须保留物理结算。

## 验收与同步范围

覆盖根发现请求数、VCS 优先级、悬空链接、读取及关闭失败、真实 factory→driver 的首次/后续规则读取次数和外部编辑、慢创建期间已有会话获取、退休期间在途创建，以及 macOS 本地真实标准 SFTP 子系统的多次读取与关闭。真实 SSH 网络计时、GUI 手验、构建包和 main CI 分别记录，不从合成计数推出线上耗时。

应用没有新增用户配置或 SDK 入口。framework 新接缝由编译文档及消费者测试覆盖；应用示例使用原 driver 即保持相同行为。官网无新的产品功能，相关正式说明按所属仓库维护，不将未发布源码写成 main 发布。
