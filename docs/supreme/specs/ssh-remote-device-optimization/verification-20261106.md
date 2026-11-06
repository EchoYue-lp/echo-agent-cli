# WP0 / WP1 / WP2 / WP3 / WP4 验证回执

日期：2026-11-06。范围：WP0 全部、WP1（framework 助手 + 四个多文件工具）、WP2 目录树并发预取、app 去重。
本回执只记录本机实际执行的命令与真实结果，不代表合并门禁或真实远端验收。

## 执行命令与结果

| 命令 | 结果 |
| --- | --- |
| `cargo test -p echo-agent-app-core --lib unknown_settlement_wait` | 1 passed |
| `cargo test -p echo-agent-app-core --lib terminal::project_ssh` | 37 passed, 0 failed |
| `cargo check -p echo_tools --lib --locked` | 通过（12.35s） |
| `cargo test -p echo_tools --lib --features files files::grep` | 8 passed, 0 failed |
| `cargo test -p echo_tools --lib --features files` | 130 passed, 0 failed |
| `cargo test -p echo-agent-app-core --lib project::` | 21 passed, 0 failed（含新增 `tree_prefetch_keeps_depth_first_order_and_filters`） |
| `cargo test -p echo-agent-app-core --lib terminal::project_ssh`（车道池后） | 39 passed, 0 failed |
| `cargo test -p echo-agent-app-core --lib --no-run` | 零 warning、零 error |
| `cargo test -p echo-agent-app-core --lib terminal::project_ssh::lanes` | 5 passed（2 策略 + 3 夹具：复用 / 弃用 / 关闭结算） |
| `cargo test -p echo-agent-app-core --lib terminal::project_ssh`（夹具后） | 42 passed, 0 failed |
| `cargo test -p echo-agent-app-core --lib terminal::project_ssh::lanes`（计数后） | 5 passed（新增 `(3,1,2)` / `(2,2,0)` 计数断言） |
| `cargo test -p echo-agent-app-core --lib remote_search` | 17 passed（含主 Agent 新增的 ERE 兼容性用例） |
| `cargo test -p echo-agent-app-core --lib terminal::project_ssh` | 59 passed |
| `cargo test -p echo-agent-app-core --lib infra::` | 32 passed（builder 签名变更后） |
| `cargo test -p echo-agent-app-core --lib project::` | 21 passed |

注：`echo_tools` 默认 feature 不编译 `files` 模块，文件/搜索类用例必须带 `--features files`。

## 变更文件

- `echo-agent/echo-core/src/tools/filesystem.rs`：新增 `with_read_session` 通用作用域助手。
- `echo-agent/echo-tools/src/files/grep.rs`：`execute_with_context` 用该助手包裹整次遍历，新增一次调用一个会话的计数回归。
- `echo-agent/echo-tools/src/files/glob.rs`、`code_search.rs`、`repo_map.rs`：同法包裹，存在性检查也进入会话作用域。
- `echo-agent-cli/.../terminal/project_ssh.rs`：新增 `wait_for_unknown_settlement` 有界观察。
- `echo-agent-cli/.../terminal/project_ssh/channel.rs`：`finish` 改用有界观察，新增负债保留回归。
- `echo-agent-cli/.../terminal/project_ssh/sftp.rs`：JoinError 分支改用有界观察。
- `echo-agent-cli/.../project/context.rs`：删除重复会话实现，改为委托 framework 助手；新增分层并发预取（8 线程、每层最多 32 个目录）与单线程渲染，新增树等价用例。
- `echo-agent-cli/.../terminal/project_ssh/lanes.rs`（新增）：连接级 SFTP 车道池，容量 2；车道独占、可疑即弃；复用判定抽成 `lane_is_reusable` 纯函数并配两条策略用例。
- `echo-agent-cli/.../terminal/project_ssh/channel.rs`：新增 `can_reuse`（客户端克隆数为 1）与 `has_unknown_debt` 访问器。
- `echo-agent-cli/.../terminal/project_ssh/sftp.rs`：`with_sftp` 改为调用车道池，删除按操作新开通道的实现。
- `echo-agent-cli/.../terminal/project_ssh.rs`：新增 `sftp_lanes` 字段与惰性访问器；`observe_idle` 先关闭车道再等待 `active_operations` 归零。

## 环境事件（影响可复现性）

1. `echo-agent-cli/target` 在验证期间被并发会话清理：一次 `cargo test --no-run` 读到不存在的增量产物而失败，随后 `target/` 整个消失。该失败是环境竞态，不是代码错误；同一批代码在此之前已通过 21 项 `project::` 用例。
2. 清理后唯一改动是删除一个多余的 `mut`（编译器警告），尚未重新编译确认；属于语法级无行为变化，但仍须在自有 target 下补一次真实编译。
3. 当时并发会话正在使用 `.echo-semantic/local/goal-turn-summary-20261106/cargo-target` 编译同一 crate，可用空间 12 GiB，因此未另起约 10 GiB 的全量构建。

## WP3 已知缺口（已收窄）

原缺口“车道运行时行为无夹具回归”已补齐：`SftpLanes` 新增 opener 接缝，测试用本地 `sftp-server` 夹具注入，三条回归真实覆盖了通道复用（3 次操作只开 1 条）、可疑车道弃用（错误后重新开一条）、关闭结算（关闭后 `active_operations` 归零且拒绝新操作）。

仍缺：`reader.rs` 流式读取未接入车道——**这是评估后的有意决定**，不是遗漏：长生命周期流会独占 2 条车道之一，正确做法是独立的 1 通道池 + `try_acquire` 租约，收益小于并发风险，等计数观测到实际频率后再做。

结构化日志已落地：`SftpLanes` 每次操作一条 `debug`（`lane_reused`/`elapsed_ms`/`ok`），关闭时一条 `info`（`operations`/`opened`/`reused`/`closed_lanes`）。

## WP4-B1（framework，子智能体执行）

- 新增 `echo-core/src/tools/search.rs`：`ContentSearchRequest` / `ContentSearchOutcome` / `ContentSearchProvider`，中文契约注释。
- `echo-tools/src/files/grep.rs`：provider 优先于本机遍历，`Some` 时不建立本机只读会话；`None` 走原路径；`Err` 上抛不降级。
- `echo-tools/src/registry.rs`、`src/agent/config.rs`、`src/agent/react/builder.rs`、`src/agent/react/mod.rs`：`content_search_provider` 透传，默认 `None`。
- 子智能体报告的实际结果：`cargo check -p echo_core -p echo_tools` 与 `cargo check -p echo_agent` exit 0；`cargo test -p echo_tools --lib --features files` 137 passed；grep 用例 13 passed（含新增 5 条）；三条 clippy（含 unwrap/expect/panic/unreachable 拒绝）exit 0；`rustfmt --check` 仅对本次文件为 0。
- 主 Agent 复核：阅读了 `search.rs` 与 `grep.rs` 接入段，确认 WP1 的只读会话包裹仍在 else 分支内、未被覆盖；CLI 侧尚未消费该 API。

## 未执行

- 未运行 clippy、fmt 检查、workspace 全量门禁、GUI 条件矩阵。
- 未连接真实 SSH 服务器，未做真实网络计时。
- 未创建分支、未提交、未推送。
- 未使用自有 `CARGO_TARGET_DIR`（本轮失误，后续批次必须改正）。

## C0（framework，子智能体执行）与 CLI 同步

- `FileMetadata` 新增 `modified: Option<SystemTime>`；本地提供者用 `std::fs::Metadata::modified()`，SFTP 提供者用 `metadata.modified().map(UnixTimeStamp::as_system_time)`（SFTP v3 只有秒级精度），内存/形状 fixture 保持 `None`，**没有用本机时间顶替**。
- framework 侧：`cargo check -p echo_core -p echo_tools`、`cargo check -p echo_agent` 均 exit 0；新增用例 `local_metadata_exposes_comparable_modification_time` 通过；`--lib --bins` 形状的 clippy 零警告。
- 子智能体如实报告的既有问题：`cargo clippy -p echo_tools --lib --tests` 在 `files.rs`/`security.rs` 的**既有测试代码**里有 55 处 `unwrap_used`，与本次改动无关，且 CI 的 deny 门禁只跑 `--lib --bins`。保留记录，不顺手改他人文件。
- CLI 侧：`sftp.rs` 与 `hook_config_loader.rs` 两处构造已同步；其余测试 fixture 都是委托 `LocalFileSystem`，不构造字面量。
- 验证：`terminal::project_ssh` 59 passed、`project::` 21 passed、`project_environment` 9 passed，编译零警告。

## 真实 SSH 验收准备

新增 [acceptance-real-ssh.md](acceptance-real-ssh.md)：六个步骤（连接 / 首消息 / 五次工具调用 / 远端搜索与回退 / 取消基线 / 关闭与退休）、日志键对照表、判定阈值、失败处理与记录模板。其中 S4 专门验证"含 `\b`/`\d` 的模式必须回退本机且结果一致"这条新增防护。

## WP6 第一阶段：远端截止时间

- `deadline_script` 用 POSIX `sh` 包住原负载：负载在后台运行，看门狗每秒轮询 `kill -0`，到点写标记文件并 `TERM`→（宽限 5s）`KILL`，命中标记则以 **124** 退出（与 `timeout(1)` 约定一致）。
- 本地侧只在 `timeout_observed && !cancellation_requested && exit_code == 124` 时返回 `Ok(ExecutionResult{timed_out:true})`——这是"目标机器确实终止了负载"的有证据超时；取消路径不变（真实终止仍属第二阶段）。
- 看门狗自己轮询并在负载提前结束时立即退出，避免每条快命令在远端留一个睡到超时的孤儿 `sleep`。
- 新增 3 条 shell 级用例：到点终止并返回 124、快命令退出码与 stdout 原样透传、负载提前结束时包装立即返回。
- 验证：`terminal::project_ssh` **62 passed**，其中 `sandbox::deadline_tests` 3 条全部通过。
