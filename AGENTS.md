# echo-agent-cli(EKO)AGENTS.md

本文件是 AI agent 在本仓库工作时的**最高优先级约束**(优先级高于 agent 默认行为和任何技能),请严格遵守。本仓库可独立检出使用,本文件即完整约束,不依赖外部 superproject。

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

**多模式功能对等:TUI 与 GUI 是功能完全一样的完全体**,只是交互方式不同(对标 Claude Code 纯 TUI)。任何一方有的能力(复杂任务/plan/subagent/任务运行时/HITL/记忆/附件…),其它方也应有。代码里"X 模式 doesn't use Y"的注释/None 传参是**待补缺口,不是产品定位**;禁止以"某模式不需要"为由拒绝接入能力。

## 统一术语:只有 Subagent,没有 Worker(强制)

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

- 通用机制(依赖 DAG、重试、取消、revision safe point)归框架;EKO 产品策略(DomainProfile、reviewer 策略、worktree、文件权威、UI/TUI/CLI 投影)留应用。
- **动手前先搜完整仓库再新增**:按类型名、trait、字段、调用路径同时搜索本仓库与 `../echo-agent`,区分"已定义""已注册""主路径真实可达";禁止平行实现框架已有的语义。
- 应用 adapter 保持薄且转换无损:只做类型转换、metadata 注入、产品 policy/hook;语义不同的字段不得压平。adapter 若开始出现 ready frontier、DAG 主循环、通用重试取消,说明分层失败,停下来重新设计。
- 任务关系只有一个权威 API:框架默认 `task_create/task_update/task_list`,EKO 在此之上加 `task_execute`;`TaskPlan` 是版本化 artifact,`TodoItem` 是 UI 投影,不得各自拥有 store/状态机/执行器;禁止重新引入 `todo_write`、`plan_create/plan_patch/plan_execute` 等平行任务 CRUD。
- 关键架构决策(状态机、API 形状、编排模式)动手前先调研业界:Claude Code、Codex、Cursor/Devin 的做法;方案里写明参考依据。

## 分支规范:任务在非 main 分支开发,按任务合并(强制)

- **新任务必须在非 main 分支开发**,分支命名 `<type>/<user>/<任务名>`。`<type>` 是任务事件类型,与 conventional commits 前缀对齐:`feature`(新功能)、`fix`(bug 修复)、`doc`(文档)、`debug`(排查/诊断类调查)、`refactor`(重构)、`test`(补测试)、`chore`(构建/工具/依赖)。如 `feature/Echoyue/unified-turn`。worktree 场景见后文。main 保持随时可发布。
- 开发阶段可以细粒度拆分提交,每个 commit 只需通过 focused 快检(见"验证节奏"),**不要求每个 commit 跑全量门禁**。
- **合并粒度 = 任务**:任务完成后,先 `git merge main` 进任务分支(注意先确认依赖的框架改动已合入 echo-agent main),在任务分支上跑下节全量门禁,开 PR 等远端 CI 全绿(或确认本地门禁已全绿),然后 **squash merge 成一个 commit 进 main**,commit message 用与分支一致的类型前缀(如 `fix: ...` / `feat: ...` / `docs: ...`)。门禁或 CI 不绿不得合并;合并后 `git branch -D` 删除任务分支。

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

## 文档同步(强制)

- 任何代码修改同步检查对应文档与示例,确保实现与对外说明一致;不适用时在提交说明写明原因。
- 架构变更必须记录 ADR(背景、候选方案、决策、取舍、影响),遵循 `docs/adr/` 既有约定。
- 注释写设计意图与关键约束,不写复述代码表面的无效注释。

## 跨仓库依赖与提交顺序

- 本仓库 path 依赖 `../echo-agent`:框架新增 API 被 CLI 消费时,**必须先把框架改动合并进 echo-agent 的 main,再把本仓库的任务分支合并进 main**,最后更新上层 superproject 的 submodule 指针。
- CI 编译的 echo-agent 跟踪其 `main` 分支。**不要改回钉死 hash**:pin 会在框架每次演进后立刻过期,导致 CI 编译旧框架而本地门禁编译新框架,两边验证的不是同一份代码(2026-08-25 至 09-03 曾因此连续红盘)。
- worktree 开发期间临时改过 `Cargo.toml` 绝对路径的,合并前必须改回相对路径:`echo-agent-cli/Cargo.toml` 用 `../echo-agent`,`echo-agent-app-core/Cargo.toml` 用 `../../echo-agent`。

## Worktree 并行开发

- worktree 放 `.worktrees/`(`.gitignore` 必须含 `.worktrees/`)。
- 合并前先 `git merge main`(merge commit 用 `--no-gpg-sign --no-edit`),验证 main 新改动未丢失,再 squash merge;分支用 `-D` 删除,worktree `remove --force` + `prune` 清理。

## 提交方式

- `git -c commit.gpgsign=false commit -m "..."`,推送正常 `git push`。
- 注释和 commit message 可用中文;代码风格与周围一致。

## CI 形态与环境差异

完整 Rust、GUI 与前端门禁由任务分支在合并前执行一次。CI 只补本地 macOS 无法等价
覆盖的信号:Linux all-target/all-feature lint、app-core 默认 feature 测试和 Node LTS 前端
门禁(包含已提交 TypeScript 契约的编译);不重复完整 workspace、GUI test 或 JSONL 产品验收。
app-core 测试必须把 `ts-rs` 导出隔离到 runner 临时目录,避免覆盖需要人工补齐跨 crate
import 的正式契约。同一 ref 的旧 run 由 concurrency gate 自动取消。

已知 Linux-only 环境差异:依赖 `bwrap` 沙箱 netns 的测试在 GitHub runner 容器内会因
`RTM_NEWADDR: Operation not permitted` 失败——这是运行环境限制,不是回归;测试侧应探测
能力降级或跳过。不得仅因共享 runner 负载而反复放宽产品测试等待时间或加入 `gdb` 等
常驻诊断依赖;需要调查时使用临时诊断分支。
