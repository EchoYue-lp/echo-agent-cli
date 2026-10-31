# echo-agent-cli(EKO)AGENTS.md

本文件是 AI agent 在本仓库工作时的**最高优先级约束**(优先级高于 agent 默认行为和任何技能),请严格遵守。本仓库可独立检出使用,本文件即完整约束,不依赖外部 superproject。

## 本次 EKO 迭代的开发目录

用户指定唯一开发根 `/Users/ls/MyWork/code/ylp_agent_learn/lp-agent`。本仓在该根的 `echo-agent-cli/` 内开发；源码、工程文档、构建产物、回执与恢复材料均写在主根内，不创建外部 worktree。已有外部工作树只作冻结迁移来源，备份、哈希回读和依赖核对完成后按授权回收。同一主根多会话按文件划分单一写权；验证期间冻结对应源码及依赖。后文 worktree 规则仅用于维护既有工作树，不取消本轮目录限制。

## 文档语言

后续新增或修改的设计、计划、说明、验收清单和交付报告统一使用中文，不再为形式新增英文副本。代码、命令、API/协议标识及原始日志保留原文；历史证据不因翻译而改写。

## 仓库定位

EKO 应用层:Rust workspace(`echo-agent-app-core` 应用核心 + Tauri 壳 `src-tauri`)+ React/TS 前端 `web-frontend/`(Tailwind v4 + Zustand)。通过 path 依赖使用兄弟目录的 `../echo-agent` 框架。

## 产品定位与安全边界(做安全决策前必读)

**EKO 是本地个人超级智能助理,运行在用户自己的机器上,不部署到线上,不存在多用户/公网攻击场景。** 这是所有安全设计的出发点:

- 威胁模型是本地的:用户能打开这个应用,就说明信任这台机器。不要套用线上 Web 服务的威胁模型("防 XSS→RCE""防 SSRF""多用户权限隔离")。
- 终端、文件选择器等**用户主动操作**的开发者工具,不该被 agent 自动执行权限(`full-auto` 等 permission_mode)卡住——那类闸只管 agent 自动决策路径。
- 用户自扩展(MCP server、技能、hook)由用户自己负责;框架只保留对明显错误输入(拼错命令名、明文 http URL)的轻量校验,不做权限级拦截。
- 仅在(1)防数据丢失(覆盖未提交改动)、(2)防框架自身 bug、(3)本地也成立的通用安全(密钥不进日志)时才加防护;默认不加门控。

> 教训:曾给 `create_terminal`/`connect_mcp_server` 加 `require_full_auto` 门控,导致默认权限下终端打不开、MCP 连不上。

**数据持久化:EKO 不需要 SQLite。** 对话历史/记忆用文件或内存实现;禁止引入或保留 SQLite 依赖(`SqliteStore`/`SqliteConversationStore`/echo-state 的 `sqlite` feature 在本仓库不启用)。禁止把"schema 迁移/前端契约"当作反对改动的理由——开发阶段无迁移负担,过时代码直接删。

**产品目标：EKO 只保留 GUI 主入口，继续保留 IM 收发和 cron 后台触发。** 独立 TUI、REPL、CLI/JSONL 产品入口进入退役范围，不再要求扩张这些入口或维持它们与 GUI 的长期能力对等。GUI 内置终端、Agent shell、桌面启动参数、构建命令与独立 SDK Host stdio 不属于这次 CLI 产品入口退役。MCP、Task/Subagent、HITL、共享会话/任务恢复和文件持久化继续保留。

**当前事实与目标必须区分。** `pre-retirement-2026-10-24` 基线仍包含 TUI/REPL/CLI/JSONL 与 LSP；本次规则与 ADR 只固化已批准方向，不表示实现已删除。先让 GUI/app-core 接管 IM 连接、cron、共享启动与退出 owner，再实际删除独立终端入口；不得按 `src/cli` 目录名整包删除共享能力。隐藏窗口保留进程内后台服务；真正退出停止准入并等待既有生命周期结算，不承诺退出后继续运行。

`Side Conversation` 继续是 GUI/Tauri 的侧栏与并行面板能力，提供创建、树形导航和支线管理。保留 IM 的普通 conversation、Subagent 和已有 fork 行为，不新增 Side Conversation 专用 channel wire contract。当前终端入口的普通 conversation、Subagent 和 `/fork` 实现随入口退役核对，不再作为长期保留终端入口的理由；底层共享能力不得连带删除。

**A2A/LSP 是明确批准的跨仓能力退役，不能泛化为“EKO 没调用就删框架 API”。** `echo-agent` 仍是独立通用框架，合理公开 API 和其它存储实现由其自身复用语义决定。仅退役两类能力的生产实现、注册、feature 请求、配置、插件组件、工具调用和消费者合同，保留其它合法 Plugin/Skill/Hook/MCP/Subagent 能力。处置清单与验收见 [ADR 0050](docs/zh/adr/0050-gui-product-convergence.md) 和 [运行时清单](docs/zh/architecture/runtime.md#产品收敛处置清单)。

## 统一术语:只有 Subagent,没有 Worker(强制)

Agent 协作由 AI 使用既有工具和运行时自动组织,包括跨会话通信、主/子 Subagent 消息和组编排。GUI 不提供手动选地址、发送协作消息、维护 Agent 组或管理投递收据的页面/控制台。用户参与并行探索使用 Sidechat 或 Forkchat;既有对话、任务/Subagent 结果呈现及 HITL 保持其核心能力。

产品/领域/运行时模型和代码术语中只有 `Subagent`,标准关系 `TaskRun → PlanTask → SubagentRun`。禁止新增 worker 命名(类型、字段、函数、事件、注释、文档、UI 文案);触及遗留 worker 命名必须随手迁移为 subagent。仅第三方固定 wire name 可在最小适配边界保留。

## Rust 编码硬性约束(最高优先级)

### 1. 字符串处理:UTF-8 安全,禁止字节级截断

`str::len()` 是字节数;字节索引切片在中文/emoji 上会 panic。处理任意文本必须用字符迭代器:

```rust
// 正确(本项目既有 pattern)
let preview: String = s.chars().take(N).collect::<String>();
if s.chars().count() > N { ... }
// 禁止:&s[..100] / &s[100..] / s.len() > 100
```

### 2. 禁止任何会导致系统 panic 的 API

| 禁止 | 安全替代 |
|---|---|
| `.unwrap()` | `.ok_or(...)?` / `unwrap_or(default)` / `unwrap_or_else(...) \| ... \|` |
| `.expect("msg")` | 同上,带明确错误处理 |
| `arr[i]`(可能越界) | `arr.get(i)` + Option 处理 |
| `&s[..n]` 字节切片 | `.chars().take(n).collect()` |
| `"123".parse()` 不处理错误 | `.parse().map_err(...)?` |
| 整数运算可能溢出 | `checked_add` / `saturating_*` / `wrapping_*` |
| `panic!` / `unreachable!` / `todo!` | 返回 `Result` 或处理该分支 |

CI 对 lib/bins 强制 `-D clippy::unwrap_used -D clippy::expect_used -D clippy::panic -D clippy::unreachable`。

## 框架 vs 应用分层(强制)

本仓库是应用层,依赖 `echo-agent` 框架。动手写任何"能放框架、也能放应用"的功能前必须先回答分层:

- 通用机制(依赖 DAG、重试、取消、revision safe point)归框架;EKO 产品策略(DomainProfile、reviewer 策略、worktree、文件权威、GUI/IM/cron 投影及待退役入口适配)留应用。
- **动手前先搜完整仓库再新增**:按类型名、trait、字段、调用路径同时搜索本仓库与 `../echo-agent`,区分"已定义""已注册""主路径真实可达";禁止平行实现框架已有的语义。
- 应用 adapter 保持薄且转换无损:只做类型转换、metadata 注入、产品 policy/hook;语义不同的字段不得压平。adapter 若开始出现 ready frontier、DAG 主循环、通用重试取消,说明分层失败,停下来重新设计。
- 任务关系只有一个权威 API:框架默认 `task_create/task_update/task_list`,EKO 在此之上加 `task_execute`;`TaskPlan` 是版本化 artifact,`TodoItem` 是 UI 投影,不得各自拥有 store/状态机/执行器;禁止重新引入 `todo_write`、`plan_create/plan_patch/plan_execute` 等平行任务 CRUD。
- 关键架构决策(状态机、API 形状、编排模式)动手前先调研业界:Claude Code、Codex、Cursor/Devin 的做法;方案里写明参考依据。

## 分支规范:任务在非 main 分支开发,按任务合并(强制)

- **新任务必须在非 main 分支开发**,分支命名 `<type>/<user>/<任务名>`。`<type>` 是任务事件类型,与 conventional commits 前缀对齐:`feature`(新功能)、`fix`(bug 修复)、`doc`(文档)、`debug`(排查/诊断类调查)、`refactor`(重构)、`test`(补测试)、`chore`(构建/工具/依赖)。如 `feature/Echoyue/unified-turn`。worktree 场景见后文。main 保持随时可发布。
- 开发阶段可以细粒度拆分提交,每个 commit 只需通过 focused 快检(见"验证节奏"),**不要求每个 commit 跑全量门禁**。
- **合并粒度 = 任务**:任务完成后,先 `git merge main` 进任务分支(注意先确认依赖的框架改动已合入 echo-agent main),在任务分支上跑下节适用的全量本地门禁;本地门禁全部通过后,开 PR 审查并 **squash merge 成一个 commit 进 main**,commit message 用与分支一致的类型前缀(如 `fix: ...` / `feat: ...` / `docs: ...`)。CI 仅由推送 `main` 触发,任务分支和 PR 不触发 CI。合并后必须核对精确 main 提交的远端 CI 全绿才能完成交付;任何失败仍须修复,不得以已合并为由跳过或绕过。交付完成后 `git branch -D` 删除任务分支。

## 全量提交门禁(合并到 main 前强制)

在任务分支(已 merge 最新 main)根目录依次执行,**全部通过(零失败、零警告、零 fmt diff)才能合并**;任何失败都必须修复,不允许跳过或绕过:

```bash
cargo fmt --all
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo clippy --workspace --lib --bins --all-features --locked -- \
  -D clippy::unwrap_used \
  -D clippy::expect_used \
  -D clippy::panic \
  -D clippy::unreachable
cargo test --workspace --all-features --locked
cargo check -p echo-agent-app-core --no-default-features --locked
```

历史 01137 / cd05 交付的 CLI 依赖审计按当时用户明确接受的精确合同执行：Cargo.lock SHA256 `cd05fc1c3ffa425b972d48a687486446ea70f24dea30282d096b4f2856fac335` 与既有 consumer 指纹绑定的 glib 0.18.5、proc-macro-error 1.0.4、bincode 1.3.3、zbus_names 4.3.4、zvariant 5.13.1、zvariant_derive 5.14.0 六项风险，以 `.cargo/accepted-dependency-risks.json` 和 `docs/supreme/specs/cancellation-observation-consumer/dependency-risk-decision.md` 为唯一机器身份与人类裁决依据；执行既有六项工具测试及 actual-lock/canonical-origin 两次 `scripts/check_cli_dependency_risks.py` 检查，保留完整官方输出与 scanner 自然出口 1，由独立精确风险 gate 0 表示已接受，不能称零 warning 审计。原三稳定版本继续冻结，不升级、不增 ignore、不将风险改称 resolved；仅本次精确 lock/consumer/source/version/checksum/advisory/kind 合同适用，变化须复评，不自动续期，不接受其它 warning、漏洞、yanked 或扫描错误。SDK/framework 的独立例外不继承到 CLI，编译、Clippy、fmt、测试与精确 main 门禁仍须全部通过。

当前中央 Project855 的有限同六项复评单独绑定 Cargo.lock SHA256 `2f1f128b2ba6e1cac7e254d9a499d96926e860bbdce353ee71c94a0ba2289a57`（855 包）、原 `consumer_digest` 两次稳定读取的 consumer `e50f45c974d30461edebdd4715defa2083f244d3c7692c9ff2c9fef6e0d5b875`（316 个 Rust／构建输入）及 policy SHA256 `0caf363da624552728f49febd3408464cc31d0a96bf0f46b9e11befacece13ff`。它只维护上述同六项完整身份，不继承 Domain eb2b 或 SDK/framework 的风险例外；原 cd05/3b76、cca/d2 和其它交付记录仅属于各自历史输入。按原六个 Python 方法及 actual-lock／canonical-origin 原门禁进行真实复验；官方 scanner 自然出口 1 与独立精确 gate 0 分开记录，scanner 原始六项 warning 不代表已修复。任何新 warning、漏洞、yanked、身份、锁或 consumer 漂移仍阻断，不增 ignore、不自动续期。此风险维护不代表本轮两项 GUI 文件回归、完整 Rust／native／main／CI 或发布已完成。

### 条件矩阵(仅触及对应风险面时)

修改 `src-tauri/`、`src/tauri/`、GUI feature 或相关依赖:

```bash
cargo check --no-default-features --features gui --bin echo-agent-tauri
cargo test --no-default-features --features gui
```

修改 `web-frontend/`:

```bash
cd web-frontend
npx prettier --check "src/**/*.{ts,tsx}"
npm test
npm run build
```

### 验证节奏

开发阶段(非 main 分支)每个 commit 跑 focused test / `cargo check -p <crate>` 快检即可;全量门禁只在任务合并到 main 前跑。延后全量 ≠ 延后修复已知问题。

### 构建并发与批次后清理(强制)

- 同机最多五条 focused Rust 构建/测试并发，完整 all-feature/GUI/语言矩阵同一时间只运行一套；每条 Rust 构建 `jobs <= 5`，dev/test debuginfo 关闭。
- 每个 worktree 使用自有独立 target，禁止与其它聊天或 primary 共用 `CARGO_TARGET_DIR`。重构建前后记录 `df`/`du`；可用空间低于 5 GiB 时停止新的重构建。
- **每个编译/测试批次结束后及时 `cargo clean` 自有准确 target**：先把必要日志、真实退出状态以及需保留的 Host/制品保存到 target 外（原始日志放本仓 ignored `.echo-semantic/local/<task-id>/`）；再确认该 target 无活动 cargo/rustc/Host 或其它制品读取者，最后只清本任务准确 target。不能先删再声称证据或制品可恢复，不能误清其它聊天或 primary 缓存。
- 纯文档/只读任务没有产生 Cargo 产物时明确报告不适用，不为形式清理无关目录。工作树、分支、未提交/未跟踪源码、stash、保留 tag 和原件的维护清理按独立授权及协调归属执行，不借 `cargo clean` 扩大范围。

## 文档同步(强制)

- 任何代码修改同步检查对应文档与示例,确保实现与对外说明一致;不适用时在提交说明写明原因。
- 架构变更必须记录 ADR(背景、候选方案、决策、取舍、影响),遵循 `docs/adr/` 既有约定。
- 注释写设计意图与关键约束,不写复述代码表面的无效注释。

## 跨仓库依赖与提交顺序

- 本仓库 path 依赖 `../echo-agent`:框架新增 API 被 EKO 消费时,**必须先把框架改动合并进 echo-agent 的 main,再把本仓库的任务分支合并进 main**,最后更新上层 superproject 的 submodule 指针。
- **撤销 API 顺序相反**：先在旧框架仍保留 API 时让 EKO 与 SDK 主线解除 A2A/LSP 依赖，再删除 framework 实现。EKO CI 跟踪 framework main，每个中间主线必须可构建；SDK Host 使用自身精确 framework pin，不由顶层 gitlink 替代，也不强迫为退役升级 pin。
- CI 编译的 echo-agent 跟踪其 `main` 分支。**不要改回钉死 hash**:pin 会在框架每次演进后立刻过期,导致 CI 编译旧框架而本地门禁编译新框架,两边验证的不是同一份代码(2026-08-25 至 09-03 曾因此连续红盘)。
- worktree 开发期间临时改过 `Cargo.toml` 绝对路径的,合并前必须改回相对路径:`echo-agent-cli/Cargo.toml` 用 `../echo-agent`,`echo-agent-app-core/Cargo.toml` 用 `../../echo-agent`。

## Worktree 并行开发

- worktree 放 `.worktrees/`(`.gitignore` 必须含 `.worktrees/`)。
- 合并前先 `git merge main`(merge commit 用 `--no-gpg-sign --no-edit`),验证 main 新改动未丢失,再 squash merge;分支用 `-D` 删除,worktree `remove --force` + `prune` 清理。

## 提交方式

- `git -c commit.gpgsign=false commit -m "..."`,推送正常 `git push`。
- 注释和 commit message 可用中文;代码风格与周围一致。

## CI 形态与环境差异

完整 Rust、GUI 与前端适用门禁由任务分支在合并前执行一次。CI 仅在推送 `main` 后运行,
不响应其它分支推送或 PR;必须验证精确合并提交,不能借用相邻提交的结果。CI 只补本地 macOS 无法等价
覆盖的信号:Linux all-target/all-feature lint、app-core 默认 feature 测试和 Node LTS 前端
门禁(包含已提交 TypeScript 契约的编译);不重复完整 workspace、GUI test 或 JSONL 产品验收。
app-core 测试必须把 `ts-rs` 导出隔离到 runner 临时目录,避免覆盖需要人工补齐跨 crate
import 的正式契约。同一 ref 的旧 run 由 concurrency gate 自动取消。

已知 Linux-only 环境差异:依赖 `bwrap` 沙箱 netns 的测试在 GitHub runner 容器内会因
`RTM_NEWADDR: Operation not permitted` 失败——这是运行环境限制,不是回归;测试侧应探测
能力降级或跳过。不得仅因共享 runner 负载而反复放宽产品测试等待时间或加入 `gdb` 等
常驻诊断依赖;需要调查时使用临时诊断分支。

## 2026-10-27 本轮六项依赖风险的用户复评

用户在当前新协调会话明确批准“继续保留原六个依赖，完成本次修复和验证”。本轮实际锁为`3fb8955b56e86eb4bef2454d3328b6ef8eb7f36d7663806387f3b990244e7ed6`（856包），本次运行隔离/Goal边界GREEN冻结consumer为`d38f9a06ca1df7f042295d579bf8144ab3a741a92b6b5bd99fb104941c58ecd9`（321输入），以`.cargo/accepted-dependency-risks.json`及原风险决策文档的本日中文后继为机器身份/真实用户来源。原b0完整源码未复原已明确告知，不据旧小增量假设自动续期；本轮直接对当前完整来源复评，六项name/version/source/checksum/advisory/kind保持。

这覆盖上文各历史锁/consumer的CLI当前时态，不改变Framework/SDK独立合同，不扩大风险集合或以后迭代许可；scanner自然1与精确gate0必须分开保留，原六测试和actual/canonical两gate实际通过后才有本切片风险资格。后继本轮源增量须重新核暴露面、冻结并执行原精确gate；锁/身份/范围变化必须重新裁决。编译/测试/fmt、mainCI与原生验收门禁不变。

## 2026-10-29 F15当前CLI五项严格子集的候选绑定

本节只覆盖CLI旧3fb/d38f和较早7659候选的当前绑定时态，原人类“继续保留原六个依赖，完成本次修复和验证”的原话及失败保留，不称用户新批准五项。bincode1.3.3因具名终端入口/syntect闭包退役离开当前锁；剩余五项完整name/version/source/checksum/advisory/kind是原接受六项的严格子集，不接受新风险或以后自动续期。Framework/SDK独立合同不继承此项。

本候选只绑定CLI锁SHA256`399509eeb951c87d407ffd949c60ac4c5b7dcc460a189e078a25f8287f4e2a4f`（768包）、Root指定GUI3后consumer`d027ea4999a8bcbdc8d6262d4cfdd64f4669565135547e2b0e540e0baa01af9e`（273输入），原freeze SHA为`20dc7b2019f78f2bfd5a3b8fbfcfbbedabab8d34b29e6afb13a8e5a3194fcb96`（292构建来源＋14元数据），机器身份仍看CLI `.cargo/accepted-dependency-risks.json`。原human来源、完整321→273复查、此前两次test-only增量以及本次GUI3生产差异均见原风险决策本日后继和ADR0062。6a17→d027实际删除两处no-native成功早退，统一原publication校验与错误传播；源码复查没有新增依赖/native桥或原五风险完整身份，但它是production语义收紧，不能沿旧6a票直接续期。当前实际重算两读并精确绑定d027；后继非Rust文档索引不改变collector的判断也须用两读事实，不给再后来源码签票。正式采用和fresh actual/canonical门禁仍待Root＋唯一writer正常处理，GUI行为/source票不是风险gate。

checker要求当前固定五项完整warning集合，4/6/少项/重复/字段/settings/漏洞/yanked/扫描错误都阻断；原六方法保留并有缺warning和settings反控，scanner命令/URL/fetch/yanked/ignore不改。候选控制和source复评不代表正式actual/canonical门禁通过；原官方scanner自然1与最终精确gate结果继续分列。正式采用仅由唯一owner在reader安全点正常预检，必要Rust验证结束且source稳定后Root另排两原gate。任何source/lock/consumer漂移须具体复核，任何新风险/身份/范围扩大交Root，不自行续期。其它编译、fmt、文档、native、mainCI和发布门禁不变。


### 2026-10-29 当前绑定的真实门禁结果

上述d027/399509五项严格子集已由唯一CLI writer正式采用。原actual-lock和canonical-origin两次调用均保持默认fresh fetch、完整官方输出与自然scanner退出1；独立精确风险gate实际各退出0。两报告均为0漏洞、4项unmaintained及1项unsound，五完整身份与原接受集严格相等，数据库报告commit为`40f5be6fb12ed93fc591bf4ab23b7d793b3f2239`、1381条；Root已对原raw、原caller出口和当前输入逐项复核。原DB六份未跟踪RUSTSEC-0000草稿的存在已如实保留记录，未修改或忽略它们，未将scanner自然1改称零warning审计。

本段只将上节“候选/待正式采用和fresh gate”的本切点时态更新为已采用、已验证；不新增人类风险接受，不把风险标resolved，不续期未来source/lock/consumer变化。原human原话、全身份与暴露复评、真实失败、原six来源及各仓独立合同继续有效；Rust/文档/原生/全矩阵/SDK来源/mainCI和整体Goal仍分别验收。

### 2026-10-31 CLI 当前 f173 精确绑定的真实门禁结果

本节仅更新 CLI 当前绑定时态，保留原人类“继续保留原六个依赖，完成本次修复和验证（推荐）”、退役后五项严格子集及原失败；不新增风险接受，也不授未来来源自动续期。当前锁 SHA256 `399509eeb951c87d407ffd949c60ac4c5b7dcc460a189e078a25f8287f4e2a4f`（768 包）、原 collector `f17364a7e07685e35b90a40af4b70b5893ec60a300c3796aa058efb418e54aa1`（274 输入）及正式 policy `3144106beb2acb946ba313e0c12df4e378fcee6763c18ac59596a56047817294` 精确绑定，五项完整风险身份不变。原八项控制全部通过；默认 fresh actual-lock/canonical-origin 两次官方 scanner 均自然 1、零漏洞和恰五项已接受 warning，独立精确 gate／caller 各自然 0，DB 报告 `2295466c4e9df26647fd67d8fa1c705ac2fc7f81`／1395 条，原六份未跟踪草稿保持。具体原件与中文决定后继见 CLI 原 decision 及其本日 f173 执行回执。新 source/lock/身份/advisory/漏洞/warning/settings 漂移仍阻断，不增加 ignore、不关闭 fetch，不向 Framework／SDK 继承。本段不代替新决定字节的正常 Source／final、原生、性能、main CI 或完整 Goal 验收。

### 2026-10-31 CLI HookTest 单 cfg(test) 的 525f 精确绑定

本节仅记录 Root 已具体审定的唯一测试源码增量：相对已保存 f173／bc08，原274输入仅既有 extension_control/tests.rs 一项修改、新增删除零，生产三文件及其它输入不变。当前 consumer `525f19abeb161c7b118cee03c2e3bbdcd5344cf9f1bf0911509fb969b215f55a`、锁 `399509eeb951c87d407ffd949c60ac4c5b7dcc460a189e078a25f8287f4e2a4f`／768包、policy `e99f7641ec5fb8fe0eb7b792b73470cfa6eb3a827b207daf79bc79b6590c2ff2` 精确绑定。原八工具控制全部通过，默认 fresh actual-lock／canonical-origin 两次官方 scanner 自然1、零漏洞、恰五项原已接受 warning，独立精确 gate／caller 各自然0；DB `2295466c4e9df26647fd67d8fa1c705ac2fc7f81`／1395条与原六draft保护均有真实回执。原人类六项来源、五身份严格子集及全部旧失败保留，不新增风险、ignore、过滤或未来输入自动续期，不向 Framework／SDK 继承。新 Source／final、原生、性能、main CI 和整体 Goal 仍独立；完整原件及中文决定后继在CLI本次 hook-test-fixture-formal-integration-20261031-r01 风险执行目录。
