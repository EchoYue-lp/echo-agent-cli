# Eclip SSH 远程工作区优化实施计划

日期：2026-11-06。状态：待实施（本文件只定义执行方案，不代表任何源码已修改或已通过验证）。

## 0. 实施状态

| WP | 状态 | 已落地内容 | 验证 |
| --- | --- | --- | --- |
| WP0 | 已完成（未提交） | `wait_for_unknown_settlement` 有界观察；`channel::finish` 与 `with_sftp` JoinError 分支改用它 | `cargo test -p echo-agent-app-core --lib terminal::project_ssh`：37 通过 |
| WP1 | 已完成（`grep`/`glob`/`code_search`/`repo_map`） | framework `filesystem::with_read_session`；四个多文件工具一次调用一个会话；app `project/context.rs` 改为委托，消除重复实现 | `cargo test -p echo_tools --lib --features files`：130 通过 |
| WP2 | 已完成 | 目录树分层并发预取 + 单线程深度优先渲染，输出与截断语义不变 | `cargo test -p echo-agent-app-core --lib project::`：21 通过（含新增树等价用例） |
| WP3 | 完成（A2 调整见下） | `SftpLanes`（容量 2）落地；opener 接缝 + 三条夹具回归；操作数/新开数/复用数计数与 `debug`/`info` 结构化日志 | `cargo test -p echo-agent-app-core --lib terminal::project_ssh`：42 通过 |
| WP4 | 完成（B1+B2+B3） | framework 接缝 + CLI `RemoteContentSearch`（远端 `find` + `grep`，只回传结果行）+ 三处 builder 注入 + **ERE 兼容性守卫**（不可忠实表达的正则回退本机遍历） | app：`remote_search` 17 通过、`terminal::project_ssh` 59 通过、`infra::` 32 通过、`project::` 21 通过；framework：`echo_tools` 137 通过 |
| | WP5 | 前置完成（C0），缓存未开始 | framework `FileMetadata.modified: Option<SystemTime>` 已落地，本地与 SFTP 提供者都填真值；CLI 侧两个构造点已同步 | framework：`echo_tools` 新用例通过、`cargo check -p echo_agent` 通过；CLI：`terminal::project_ssh` 59、`project::` 21、`project_environment` 9 全通过 |
| WP6 | 第一阶段完成，第二阶段**决定不做**（见 12 节） | 远端看门狗 + 结构化取消结论（截止时被杀 / 截止前自然结束）+ 取消时记录剩余上界 | `terminal::project_ssh` 63 通过 |

WP1 的 `diff` 不包裹：它只读 1–2 个文件，会话最多省一次通道建立，收益不足以增加一层嵌套结果处理，已从本 WP 范围内移除。

本轮未创建分支、未提交、未推送。四个仓库当前工作树均带其它会话的未提交改动，写入范围仅限本计划列出的文件。

WP3 剩余：

- A3（结构化日志）已落地：`SftpLanes` 记录 `operations / opened / reused`，每次操作一条 `debug`（含 `lane_reused`、`elapsed_ms`、`ok`），关闭时一条 `info` 汇总；夹具用例直接断言计数 `(3,1,2)` 与 `(2,2,0)`。
- **A2（`open_read` 接入车道）经评估后调整**：`ProjectSftpReader` 是长生命周期流，一条车道会被整个文件读取过程占住；在容量 2 的池里等于把并发砍半。正确做法是给流式读取单独的 1 通道池 + 租约 API（`try_acquire`，占用不到就退回独立通道），这需要新增 `LaneLease` 生命周期与 Drop 释放路径。当前不改，理由是收益（省一次 `ssh` 启动）小于并发风险与实现复杂度；先用 A3 的计数观测 reader 实际触发频率，再决定是否引入。


WP4 framework 契约（已落地，B2/B3 依此实现）：

- `ContentSearchProvider::search(&ContentSearchRequest) -> Result<Option<ContentSearchOutcome>>`；`None` = 不支持、必须回退；`Err` = 支持但失败、必须上抛（不得降级成空结果）。
- `records` 是**最终展示文本**（含上下文行的多行记录），调用方只做分页与拼接。
- `max_results` 固定 1000，是传输上界，与 `PageRequest.limit` 解耦；提供者可截断但必须置 `truncated`。
- 已知限制：远端结果被截断时无法给出真实 `total`，也不支持按 offset 续传。这是有意的**最小切缝**，B3 需把它写进测试与文档，不要假装远端分页可续。

构建环境注意事项：共享的 `echo-agent-cli/target` 在本次验证期间被并发会话清理，导致一次增量构建读到不存在的产物。后续批次必须按 AGENTS 使用本任务自有的 `CARGO_TARGET_DIR`（`.echo-semantic/local/<task-id>/cargo-target`），不再共用 primary target；在其它会话正在编译且可用空间低于约 15 GiB 时不要另起全量构建。

## 1. 目标与范围

### 1.1 目标

在不引入远端常驻服务的前提下，把 Eclip 的 SSH 远程项目从“能用但每条消息与每次工具调用都付进程启动和逐项往返”优化到“连接复用、批量准备、远端搜索、可真实停止”。判据是可度量的往返次数、进程启动次数与取消行为，不是抽象架构偏好。

### 1.2 范围内

- 工具阶段的多文件读取复用同一条 SFTP 通道（framework `echo-tools`）。
- 连接级 SFTP 车道池，消除“每次文件操作启动一个 `ssh -s sftp`”（app-core）。
- 目录树预取的分层并发（app-core）。
- 远端内容搜索接缝与实现（framework 接缝 + app 实现）。
- 项目规则正文的指纹校验缓存（app-core）。
- 远端运行器脚本：截止时间与真实终止（app-core）。
- 支撑上述改动的计数器、日志字段与回归夹具。

### 1.3 范围外

- 不实现 ls-device 式常驻设备服务、设备协议、安装身份与操作记录存储。
- 不实现远端 MCP 生命周期、设备级隔离或跨客户端重启的任务查询。
- 不改动认证方式、`ControlMaster` 模型、目标解析与产品权限边界。
- 不新增工具，不扩大工具 schema（`16_000` bytes / `4_000` tokens 门槛不变）。
- 不引入 SQLite 或任何持久化规则缓存；缓存只在进程内存中。
- 不改 `FileSystem` 的既有写语义、未知副作用分类和禁止自动重放的合同。

## 2. 现状证据

| 现象 | 机制 | 位置 |
| --- | --- | --- |
| 每个文件操作启动一个 `ssh` 子进程并重做 SFTP 初始化 | `run()` → `block_on(with_sftp(...))` → `SftpChannel::open` → `Command::spawn` | `project_ssh/sftp.rs:71`、`sftp.rs:583`、`channel.rs:32` |
| 流式读取每文件再开一条通道 | `ProjectSftpReader::open` 调 `SftpChannel::open` | `reader.rs:32` |
| 准备阶段只读会话，仅覆盖上下文准备 | `with_read_session` 只在 factory/context 调用 | `project/context.rs:18`、`factory.rs:1343` |
| grep 每文件 `metadata` + `read`、每目录 `read_dir` | `search_file` / `walk_and_search` | `echo-tools/src/files/grep.rs:366`、`grep.rs:391` |
| 目录树预取顺序 DFS | `collect_dir_entries_with_file_system` 递归 | `project/context.rs:327` |
| git status 与 diff 各自启动一次执行 | 两次 `executor.execute` | `factory.rs:150`、`factory.rs:163` |
| 取消/超时只观察，不发终止 | `execute_with_launcher` 的 select 循环 | `sandbox.rs:269` |
| 后台 cell 的 stop 只取消本地 token | `BackgroundCommandManager::stop` | `echo-orchestration/src/tasks/command_cell.rs:1033` |

ADR 0063 明确采纳“阶段内复用”，并把“永久共享 SFTP 通道”留待后续。本计划的工作包 WP1–WP3 即兑现该后续；WP4–WP6 是新增能力。

## 3. 分层与仓库归属

| 层 | 内容 | 仓库 |
| --- | --- | --- |
| 通用机制 | 只读会话作用域助手、`SearchProvider` 接缝、`CommandCellRegistry` 复用 | `echo-agent` |
| 产品策略 | 车道池、规则指纹缓存、远端搜索实现、运行器脚本、准备并发 | `echo-agent-cli` |
| 适配边界 | `ProjectSshFileSystem` / `ProjectSshSandbox` 对上述机制的适配 | `echo-agent-cli` |

约束：framework 改动先合入 `echo-agent` main，再合 `echo-agent-cli`；不得在 app 侧平行实现 framework 已有语义（`FileSystemReadSession`、`CommandCellRegistry` 均复用，不新建）。

## 4. 目标指标

以下为验收门槛，不是承诺的线上数字。RTT 以 50 ms 假设举例，实测记录真实值。

| 指标 | 现状（推断） | 目标 |
| --- | --- | --- |
| 单文件工具操作 | 1 次进程启动 + SFTP 初始化 + 1–2 往返 | 0 次进程启动 + 1 往返 |
| 1000 文件 grep 的进程启动数 | ≈ 2000+ | 0（复用车道） |
| 1000 文件 grep 的往返数 | ≈ 2000+ | 1 条远程搜索命令 |
| 40 目录树预取往返 | ≈ 80 | ≤ 12 |
| 每条消息规则读取往返 | ≈ 20 | ≤ 2（指纹未变时） |
| 每条消息 git 执行次数 | 2 | 1 |
| 取消到远端停止 | 不停止，等到自然结束 | 有终止证据或明确“未确认”三态 |

观测字段（每条准备与每次工具调用）：`ssh_spawns`、`sftp_channels_opened`、`requests`、`bytes_in`、`elapsed_ms`、`lane_reused`。

## 5. 工作包

### WP0：未知结算的无界等待（前置）

**问题**：`SftpChannel::finish` 在“写请求响应丢失且 close 失败”时会 `wait_for_settlement()`，而 `retry_retained_children` 明确跳过 `RemoteOperation`，该 `Notify` 没有其它 settle 来源，调用线程可能永久阻塞。`with_sftp` 的 JoinError 分支同样无超时。车道池会把这条路径从“偶发”变成“每次丢弃车道都会走”。

**改动**：

- `project_ssh/channel.rs:144`：`wait_for_settlement()` 改为有界等待（`super::CLOSE_OBSERVATION_TIMEOUT`），超时后返回既有的 `unsettled_remote_operation()` 诊断，资源保持 retained，不清债。
- `project_ssh/sftp.rs:599`：同一处 JoinError 分支同样加界。
- 不改 `unknown` 判定条件；只把“永久挂起”变成“明确失败并保留负债”。

**测试**：

- 新增 RED：构造“写请求已发、响应丢失、close 失败”的夹具（可用本地 stdio sftp-server 加一个人为截断的通道），断言调用在超时内返回 `PartialSideEffect` 且连接 `close_project_ssh` 仍失败。
- 反控：无未知负债时关闭仍成功，`active_operations` 归零。

**完成定义**：两条测试先 RED 后 GREEN，现有 36 项 SSH 生命周期回归全绿。

预估 0.5 人日。

### WP1：工具级只读会话（framework）

**目标**：让 grep/glob/code_search/repo_map 一次调用只开一条通道。

**改动**：

1. `echo-agent/echo-core/src/tools/filesystem.rs`：新增 `pub fn with_read_session<T>(files: &dyn FileSystem, f: impl FnOnce(&dyn FileSystem) -> Result<T>) -> Result<T>`，语义与 `echo-agent-cli/.../project/context.rs:18` 相同（begin → scoped → finish，双向错误都要传播）。随后删除 app-core 的同名实现，改为调用 framework 版本，避免两套语义。
2. `echo-tools/src/files/grep.rs:111`：`execute_with_context` 内把 `walk_and_search` 包进 `with_read_session`。
3. 同法包裹 `glob.rs`、`code_search.rs`、`repo_map.rs`、`diff.rs` 的多文件遍历入口；`read_file`/`list_dir` 单操作不包（无收益）。

**不变式**：

- 会话内禁止写入的既有断言保持；会话语义“只读、禁止逃逸流式 reader”不变。
- 提前返回（取消、错误、上限）必须 `finish`，由作用域助手保证。
- `LocalFileSystem` 的 `begin_read_session` 返回 `None`，本地行为逐字节不变。

**测试**：

- 新增计数型 `FileSystem` 包装（测试专用）：`begin_read_session` 计次，断言一次 grep 调用 `== 1`。
- 在 `startup_protocol_tests` 夹具（`sftp.rs:686`）上加断言：对 N 文件树执行 grep，通道打开次数为 1。
- 现有 grep/glob/code_search 全部用例结果不变（含隐藏目录、`SKIP_DIRS`、上下文行格式）。

**完成定义**：framework 合并门禁全绿；计数断言进入默认测试。

预估 0.5–1 人日。

### WP2：目录树预取分层并发（app-core）

**目标**：把顺序 DFS 的 `1 + D1 + D2` 次往返压到按层并发。

**改动**：`project/context.rs` 的 `collect_dir_entries_with_file_system`。

**方案**：两阶段，保持输出逐字节一致。

1. 采集阶段：读根目录；对其中的目录按排序取前 `N` 个（`N` 受现有 80 条上限约束），用 `std::thread::scope` 开 8 个线程并发 `read_dir`；再对第二层同样并发。所有结果放进 `HashMap<PathBuf, Vec<FileEntry>>`。
2. 渲染阶段：沿用现有递归函数，改为从 map 取；缺失则同步 `read_dir` 兜底。截断标记、排序、`SKIP_DIRS`、`.env.example` 特例全部保持原逻辑。

**约束**：

- 并发上限固定 8，不做配置项。
- 线程数受 `dyn FileSystem: Send + Sync` 约束；`ProjectSshFileSystem` 已满足。
- 采集可能读取到最终被截断未渲染的目录，属可接受浪费（深度 ≤ 3 且有条数上限）。

**测试**：

- 保留旧实现为测试内 oracle（`#[cfg(test)]`），构造 4 层、>100 条目的夹具树，断言新旧输出完全相等。
- 计数型 FileSystem 断言 40 目录场景下的 `read_dir` 调用次数不变、并发窗口内同时在途 ≤ 8。

**完成定义**：等价性测试与现有项目上下文回归全绿。

预估 0.5 人日。

### WP3：SFTP 车道池（app-core）

**目标**：连接级复用 2–4 条 SFTP 车道，单操作从“进程 + 握手 + 往返”降到“1 往返”。

**接口草案**（`project_ssh/channel.rs` 或新 `lanes.rs`）：

```rust
pub(super) struct SftpLanes {
    connection: Arc<ProjectSshConnection>,
    permits: Arc<tokio::sync::Semaphore>,        // 车道上限
    idle: tokio::sync::Mutex<Vec<SftpChannel>>,  // 空闲车道
}

impl SftpLanes {
    async fn run<T>(&self, mutating: bool, op: impl FnOnce(Arc<SftpClient>) -> Fut) -> Result<T>;
    async fn shutdown(&self) -> Result<()>;      // 等所有 permit 归还后逐条 finish
}
```

**规则**：

1. 车道独占：一条车道同一时刻只服务一个操作，`finish()` 的 `Arc::try_unwrap` 结算证据保持不变。
2. 车道失效即弃：`response_was_lost` 命中的车道当场 `finish` 并从池中移除，绝不复用；下一次请求新开车道。
3. 关闭顺序：`ProjectSshConnection::begin_shutdown` 先置 closing → `lanes.shutdown()` → 现有 `observe_idle()`。车道生命周期内持有的 `ProjectSshOperation` 在 shutdown 时释放，否则 `active_operations` 不会归零。
4. 车道数固定 2（可按 WP3 验收数据调整到 4），不做用户配置。
5. 只读会话（WP1）优先于车道：`read_client.is_some()` 时仍走会话，不借车道绕过只读限制。

**测试**（本 WP 的主要风险面，必须逐条覆盖）：

- 复用：连续 10 次 `metadata` 只产生 1 次进程启动。
- 写语义：车道上的模糊写失败后，下一次操作使用新车道，失败分类仍为 `PartialSideEffect`，且 `allows_automatic_retry() == false`。
- 关闭：空闲车道存在时 `close_project_ssh` 成功；有在途请求时先失败（保留 owner），请求结算后重试成功。
- 退休：删除仍在使用的目标时，等待车道 drain 后完成，不借关闭观察者清理。
- 断线：`exit 255` / 通道中断后，车道被弃、连接保持 unknown 语义，重连后新车道可用。
- 与只读会话共存：会话期间车道不被创建。
- 夹具：沿用 `sftp.rs:686` 的 stdio 夹具，统计通道打开次数。

**回滚**：单一提交，`git revert` 即回到每操作开通道；不设运行时开关（避免双路径）。

预估 2–3 人日。

### WP4：远端内容搜索（framework 接缝 + app 实现）

**目标**：大仓库搜索只回传匹配结果，不回传候选正文。

**改动**：

1. `echo-core`：新增 `pub trait SearchProvider: Send + Sync { fn search(&self, req: &SearchRequest) -> Result<Option<SearchOutput>>; }`，语义为“返回 `None` 表示不支持，落回本机遍历”。
2. `echo-tools/src/files/grep.rs`：`GrepTool` 增加 `with_search_provider(Option<Arc<dyn SearchProvider>>)`；`execute_with_context` 先尝试 provider，`None` 走原路径。
3. app-core：构造 SSH workspace 的 tool 装配处注入 `RemoteGrepProvider`，内部用 `ProjectSshSandbox` 执行一条命令：

```sh
cd <root> && { git grep -n -I -E -e <pattern> -- . 2>/dev/null || grep -rn -I -E --binary-files=without-match --exclude-dir=... <pattern> .; }
```

**语义对齐（必须先写测试再实现）**：

- 路径范围与 `path` 参数一致；绝对路径仍受既有 root 集约束。
- 隐藏目录与 `SKIP_DIRS` 排除保持现状（**注意：现有 grep 不读 `.gitignore`，不得因改用 git grep 而改变**）。
- 输出格式 `path:line:  content`、上下文行前缀 `>`/空格、结果上限、`PageRequest` 分页、`check_file_cancel` 取消点全部保持。
- 二进制与 >10MB 文件跳过规则保持不变。
- 远端不可用（命令缺失、退出码异常、输出截断）时落回本机遍历，并在结果中不暴露内部降级细节。

**测试**：

- 同一夹具树分别用本机遍历与远端 provider，断言结果逐行相等（含上下文行、glob 过滤、路径前缀）。
- 反控：provider 返回 `None`、命令退出 127、输出超过上限，均落回且结果正确。
- 权限/不存在路径的错误分类与现状一致。

预估 2–3 人日。

### WP5：规则正文指纹缓存（app-core）

**目标**：在“每条消息使用当前规则”的前提下，把 N 次文件读取降为 1 次批量校验。

**改动**：`instruction_provider.rs` 与 `unified_memory.rs` 的规则装配入口。

**方案**：

1. 每轮准备时，对已知候选路径集合（AGENTS/AGENTS.override 链、`.eko/rules`、Skill/Subagent 清单目录）做一次批量 `symlink_metadata`（沿用 `first_existing_path` 的分批流水化，32 在途）。
2. 用 `(绝对路径, 存在, len, mtime, 文件类型)` 组成签名，与缓存比对；只重读变化项与新增项。
3. 缓存键含：连接物理身份、目标、解析后根目录、候选集合版本。重连、切目标、`/reload` 显式失效时全量清空。
4. SFTP 时间戳只有秒级：`mtime` 落在当前时间 2 秒内的文件强制重读。
5. 缓存只在内存，容量上限（例如 256 条）按最近使用淘汰；不落盘。

**必须保持**：`AGENTS.override` 优先于 `AGENTS`、root→cwd 顺序、非普通文件/符号链接的拒绝规则、错误传播顺序。

**测试**：

- 同一消息两次准备：第二次 `read` 调用数为 0，`symlink_metadata` 调用数 ≤ 1 轮。
- 外部修改文件（改内容不改 mtime 的负控：应被 2 秒规则覆盖；改内容改 mtime：必须重读）。
- 重连后缓存失效。
- 删除文件后规则消失。
- 与本地 FileSystem 输出等价。

预估 1 人日。

### WP6：远端运行器脚本与真实停止（app-core）

**目标**：取消与超时能真实终止远端进程，并给出三态证据。

**方案**：

1. 连接建立后按需上传 `runner.sh`（带版本号与 SHA-256 校验，路径 `~/.eko/runner/<version>.sh`），只上传一次；上传失败则完全退回现状（只观察），不阻断连接。
2. `runner.sh` 职责：记录 `pid`/`cookie` 到临时文件；`exec` 传递真实退出码；实现 POSIX 截止时间（`TERM` → 宽限 → `KILL`）；输出原样透传。
3. `ProjectSshSandbox::execute_with_launcher`：命令包裹为 `sh -c '<runner> run --deadline <s> --cookie <uuid> -- <原脚本>'`。
4. 取消/超时：另开一条短 `ssh` 执行 `runner kill --cookie <uuid>`；校验 cookie 后才发信号。
5. 三态结果：`stop_confirmed`（runner 回报已终止）/ `stop_requested_unconfirmed`（已发信号但无回执）/ `stop_unavailable`（无 runner 或远端缺命令）。后两态继续走现有 `unknown` + `PartialSideEffect` 语义，绝不报“已取消”。
6. 后台 cell：`CommandCellRegistry` 的 `stop` 走同一终止通道，使 `bash background=true` 获得真实停止；不新增工具、不复用 `kill` 白名单。

**约束**：

- 脚本缺失/不可执行时行为必须与今天完全一致（这是能力降级，不是并行实现）。
- 终止必须幂等；重复取消不产生第二次错误分类。
- 不把“已发 kill”写成“已停止”；UI 文案与 `ToolFailure` 一致。

**测试**：

- 本地 `/bin/sh` 夹具模拟：长命令被取消后在超时内退出，返回 `stop_confirmed`。
- runner 缺失夹具：取消仍等到自然结束，返回 `stop_requested_unconfirmed`，分类不变。
- 后台 cell：`background=true` 启动长命令，`stop` 后远端进程消失（用本地夹具进程验证），cell 进入 `Cancelled` 而非等完成。
- 幂等：连续两次取消，第二次不改变结果。
- cookie 不匹配时不发信号（PID 复用防护）。

预估 3–4 人日。

## 6. 依赖顺序与提交切分

```
WP0 ──► WP3 ──► WP6
 │        ▲
 └► WP1 ──┘
WP2（独立）
WP4（依赖 WP3 才有完整收益，可并行开发）
WP5（独立，依赖 WP2 的批量探测更佳）
```

建议提交序列（每个都是可独立编译与回滚的单元）：

1. `fix: 有界化 SFTP 未知结算等待`（WP0）
2. `refactor: 提取只读会话作用域助手`（WP1 第 1 步，framework）
3. `perf: 多文件工具复用只读会话`（WP1 第 2–3 步，framework）
4. `perf: 目录树预取分层并发`（WP2）
5. `perf: 连接级 SFTP 车道池`（WP3）
6. `feat: 远端内容搜索接缝`（WP4 framework）+ `feat: SSH 项目远端搜索`（WP4 app）
7. `perf: 规则正文指纹缓存`（WP5）
8. `feat: 远端运行器与真实终止`（WP6）

分支：`feature/<user>/ssh-session-reuse`（framework 侧 `feature/<user>/read-session-scope` 与 `feature/<user>/search-provider`）。合并前按各仓 AGENTS 执行完整门禁；framework 先合。

## 7. 验证矩阵

| 层级 | 命令/用例 | 适用 WP |
| --- | --- | --- |
| 迭代快检 | `cargo test -p echo-tools <name>` / `cargo test -p echo-agent-app-core project_ssh` | 全部 |
| 合并门禁（framework） | `cargo fmt/clippy/test --workspace --all-features --locked` | WP1、WP4 |
| 合并门禁（app） | `cargo fmt/clippy/test --workspace --all-features --locked` + `cargo check -p echo-agent-app-core --no-default-features --locked` | WP0、WP2、WP3、WP5、WP6 |
| 条件矩阵 | GUI feature 与前端仅在本计划不触及对应文件时免于执行；若触碰 `src-tauri/` 或 `web-frontend/` 则按 AGENTS 执行 | — |
| 夹具 | `startup_protocol_tests`（stdio sftp-server）计通道数；本地 `/bin/sh` 夹具测取消；计数型 `FileSystem` 测请求数 | WP1、WP3、WP6 |
| 真实环境 | 单台真实远端、固定目录与 RTT，按第 4 节指标记录；不在 CI 中执行 | 交付验收 |

真实 SSH 网络计时、GUI 手验、构建包与 main CI 分别记录，不从夹具计数推出线上耗时。真实远端的配对测量协议、日志键、判定阈值与记录模板见 [acceptance-real-ssh.md](acceptance-real-ssh.md)。

## 8. 风险登记

| 风险 | 影响 | 处置 |
| --- | --- | --- |
| 车道池扩大关闭/退休影响面（ADR 0063 当时的主要顾虑） | 关闭失败率上升、目标删不掉 | WP3 的 5 类生命周期测试必须先写；关闭顺序固定为 closing → drain → finish → observe_idle |
| 模糊写失败后车道复用 | 静默错误 | 车道失效即弃 + 现有 `response_was_lost` 分类 + 反控测试 |
| 远端搜索语义漂移 | 结果与现状不一致 | 先写等价性测试；不可用时落回本机；不引入 `.gitignore` 过滤 |
| 指纹缓存读到旧规则 | 违反“每条消息用当前规则” | 秒级 mtime 的 2 秒强读窗口；重连/切目标全清；显式 reload |
| 运行器脚本引入新的部署面 | 失败面扩大 | 脚本可选、失败降级到今天的行为；版本 + 摘要校验；不常驻 |
| 无界等待（WP0）被池化放大 | 工具永久挂起 | WP0 前置完成后再做 WP3 |

回滚：每个 WP 一个提交，`git revert` 即可；WP6 的脚本缺失路径天然等价于旧行为。

## 9. 已裁决事项（2026-11-06）

用户已就四项作出决定，以下替代原“待裁决”列表，并作为实施依据：

1. **接受** framework 新增 `SearchProvider` 公开接缝（WP4）。该 trait 必须保持通用、无 SSH 依赖，默认实现为“不支持，落回本机遍历”。
2. **接受** WP6 的“按需上传带版本脚本”轻量部署。脚本失败必须降级到今天的行为；不接受常驻服务。
3. **车道数固定为 2**（WP3）。不得为此新增用户配置项；如需调整，按实测数据另行裁决。
4. **长期保留结构化日志**（第 4 节指标）。计数器与字段是产品观测的一部分，不是一次性验收脚本；实现时须避免高频调试日志淹没日志文件，按每次准备/每次工具调用各一条聚合记录输出。

由此，WP0–WP6 全部在本次范围内，交付顺序不变。

## 10. 完成定义

- 第 4 节全部指标在真实远端上完成一次配对测量并留原始日志。
- 现有 36 项 SSH 生命周期回归、81 项池回归、141 项 GUI Rust 测试与本次新增用例全绿。
- 合并门禁与条件矩阵按实际触及范围全部通过，零 fmt diff、零 clippy 警告。
- 文档同步：本文更新为最终状态；如 WP3 与 WP4 改变了公开行为，补 ADR 0063 的后续决策记录，并在 `docs/MASTER-PLAN.md` 登记权威路径。

## 11. WP4 落地记录与已知 residual

### 远端命令

`RemoteContentSearch` 用一条 `sh -c` 脚本在目标机器完成筛选与匹配：

- `find -H` 复现既有忽略项（`.` 开头、`target`/`node_modules`/`__pycache__`/`.git`/`vendor`/`build`/`dist`）、10MB 上限、文件名 glob（含 `{a,b}` 展开）；
- `grep -Hn --null -I -E` 只做正则匹配，`--null` 让路径含 `:`/`-` 也能无歧义解析；
- 退出码 0=有匹配、1=无匹配、其它=失败（含远端根不存在），不做静默降级。

**不用 `git grep`**：`grep.rs::walk_and_search` 完全不读 `.gitignore`，`git grep --untracked` 会漏掉被忽略目录里的命中，且没有单文件 10MB 上限。

### 决策

1. **保留三处注入**（主 agent、writer subagent、readonly subagent）。subagent 也会调用 grep，只接主 agent 会让它们在 SSH 项目里退回逐文件拉取。签名变更带来的 `infra/tests.rs` 4 处调用同步已包含。
2. **正则不可忠实表达时回退本机遍历**，不翻译、不报错。Rust 的 `\w`/`\d`/`\b` 是 Unicode 语义，翻译成 ASCII 字符类会静默改变命中集合；远端 ERE 不支持 `(?i)`/非贪婪/`\p{}`，硬发过去会 exit 2 变成整次失败。`ere_compatible_request` 保守判定后返回 `Ok(None)`，工具自动走本机路径——慢但正确。
3. **传输截断时丢弃最后一个不完整文件**，避免半组上下文行。

### 已接受的 residual（写进代码注释与本计划，不做对齐）

| 差异 | 影响 | 处置 |
| --- | --- | --- |
| 二进制判定：本机 `read_to_string` 成功 vs 远端 `grep -I`（NUL 启发式） | 非法 UTF-8 且无 NUL 的文件远端会搜、本机跳过 | 接受；改成逐文件编码校验会让收益消失 |
| 文件名 glob 引擎：本机 `*`/`?` vs `find -name` 的 fnmatch | `[` `]` `\` 语义不同 | 接受；文档标注只用 `*`/`?`/`{a,b}` |
| 符号链接：本机跟随树内 symlink，远端只跟随命令行根 | 树内符号链接目标远端会漏 | 接受；如需一致，后续用 `find -L` 并单独验证环路 |
| 不可读条目：本机静默跳过，远端整次失败 | 一个不可读文件会让搜索报错 | 刻意保守，不把故障伪装成完整结果；后续可改为“跳过并标注” |
| 路径含换行/根含 `*?[` | 病态输入下解析或 `-path` 匹配异常 | 接受；属非常规路径 |

### WP4 仍未覆盖

- 真实 SSH 远端执行（当前等价性用例是本机 `sh -c` 跑同一条命令）。
- 正式文档/ADR：`docs/adr/0106-*` 已被其它会话的未跟踪文件占用，ADR 编号需由任务分支 owner 统一分配。

## 12. WP6 第二阶段（取消即真终止）的取舍

### 为什么原计划要做

取消当时只观察：用户点取消后，本机继续等原命令自然结束，再返回"取消已请求"。

### 阶段一之后，剩下的收益已经很窄

`SandboxCommand` 默认 `timeout` 30 秒、shell 工具默认 60 秒、上限 300 秒；后台 cell 把
cell 剩余期限写进 `command.timeout`。阶段一给每条命令都装了目标机器上的看门狗，因此
取消的最坏等待从"命令跑多久"变成 `min(剩余时间, timeout) + 5 秒`：

| 场景 | 取消后最坏等待 |
| --- | --- |
| 普通前台命令（默认 60 秒） | ≤ 65 秒 |
| 显式 300 秒命令 / 后台 cell | ≤ 305 秒 |

第二阶段能再改善的只是把这几分钟压到约 1 秒，属于体感，不涉及正确性。

### 不做的代价

取消后仍会等满上界；没有正确性损失——不报假成功、不漏报副作用、不自动重放。

### 做的代价（这才是决定性的）

1. **PID 复用**：从外部 `kill <pid>`，若原进程已退出且 pid 被复用，会杀掉无关进程。
2. **进程树**：杀直接子进程，`cargo`/`make` 的孙进程继续跑。
3. **平台缺 `setsid`**：macOS 远端默认没有该命令，拿不到独立进程组。
4. **语义成本**：三态要穿透 `ToolFailure`、恢复建议与 UI，且"已发 kill 未确认"绝不能
   写成"已取消"。
5. **测试不可及**：脚本可本地测，真实信号传播只能真机验证。

为了省几十秒引入"可能误杀用户机器上的进程"的机制，风险收益比不成立。

### 若将来要做，应换形态

不要从外部猜 pid。阶段一的看门狗**自己持有 `$eko_child`**：取消改为另开一条短 SSH
写 `<cookie>.cancel` 文件，看门狗轮询发现后由父进程自己终止子进程并写 `.stopped`。
这样没有 PID 复用、不依赖 `setsid`、不需要 `/proc` 或 `ps`，三态天然可判，成本约 1 人日
（原形态 3–4 人日）。

### 触发条件

真实 SSH 验收里 S5（取消到返回）的 p95 长期超过 30 秒，或实际使用中频繁取消长任务时，
再按上面的形态实施。
