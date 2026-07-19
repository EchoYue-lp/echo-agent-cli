# M8 Ownership/Dependency 并行调度与 Worktree 集成

## 目标

M8 让 TaskRuntime 只依据显式 `depends_on` 和文件 ownership 决定并行度。写任务在独立 worktree 执行；subagent 完成、结构化结果验收和 review 通过后，还必须经过可观察的 Git 集成阶段，集成成功后 task 才能进入 `completed`。描述文本、任务排列顺序和模型自报“已合并”都不构成调度或完成依据。

## 业界依据

- [Claude Code worktrees](https://code.claude.com/docs/en/worktrees)：并行 session/subagent 使用独立 Git worktree，避免文件编辑互相触碰；无改动的临时 subagent worktree 可自动清理，有改动的 worktree 保留给后续处理；worktree 基线是显式的 default branch 或 local `HEAD`。
- [OpenAI Codex worktrees](https://learn.chatgpt.com/docs/environments/git-worktrees)：并行 task 使用独立 checkout；Local 是前台权威工作区，Worktree 是后台隔离环境；Handoff 用明确 Git 操作把代码安全移回 Local，而不是把 subagent 结束等同于代码已进入主工作区。
- [Git `merge-tree`](https://git-scm.com/docs/git-merge-tree)：`git merge-tree --write-tree` 执行与真实 merge 相同的三方合并和 rename/directory-file 冲突处理，但不读写工作树或 index；退出码明确区分 clean merge、conflict 和执行错误。
- [Git worktree](https://git-scm.com/docs/git-worktree)：每个 linked worktree 有独立 working tree/index/HEAD，同时共享对象库和 refs；同一 branch 不能在两个 worktree 同时 checkout，分支更新必须有唯一权威工作副本。

跨系统共性是：并发编辑先物理隔离，主工作区只在独立集成边界变化；集成失败是 workflow failure，不是可忽略日志；Git 自身的三方合并结果优先于基于文本或文件名的冲突猜测。EKO 的正式 DAG 需要让下游 verification 看到上游代码，因此在 review 通过后自动执行一个显式 integration stage；这相当于 Codex Handoff 的运行时版本，但不新增主 task/run 状态。

## 现状审计

- `PlanTask` 已有 `depends_on`、`files` 和 `parallel_group`；`depends_on` 已是 DAG 权威边，`files` 已进入 subagent prompt、planner overlap 分析和 per-file lock。M8 不新增平行 ownership 类型。
- `run_dag` 已只按依赖计算 ready frontier，`max_concurrent_writes` 默认是 4，per-file mutex 可让完全相同的文件名串行；但 planner overlap 仍被标成“advisory”，空 `files` 的未知 writer 会与其它 writer 并行。
- overlap 当前只做规范化后的精确字符串交集，且 update task 只有修改 `depends_on` 时才重新分析 ownership；glob、绝对路径和未知 scope 没有保守语义。
- writer 已在 `eko-fork-*` worktree 中运行，但 worktree branch 直接拼接含 `:` 的 execution id，真实 Git ref 可能非法；base 只保存字符串 `HEAD`，subagent commit 后 diff 基线会漂移。
- framework finalize 只把 diff 追加到 Subagent output，并保留 worktree；TaskRuntime review 通过后直接把 task 标成 Completed。主工作区没有收到代码，下游 verification 可能测试旧代码，merge failure 也没有进入 task 终态。
- 已有 unattended worktree merge helper 会直接 checkout/merge main，未先做无副作用冲突预检，也没有覆盖 fork writer 的 `eko-fork-*` 生命周期。

## 框架与应用边界

### `echo-agent`

不修改。框架已经提供通用的 `WorktreeFactory -> WorktreeHandle(path, finalize)` 隔离合同，并把实际 working directory 绑定到单次 invocation。Git branch 命名、文件 owner、主工作区 dirty 状态、review 后何时合并，全部依赖 EKO 产品工作流，不应进入通用框架。

### `echo-agent-cli`

- 把 writer 的 `PlanTask.files` 定义为精确、workspace-relative 的 exclusive write ownership。空列表、glob、绝对路径、父目录跳转或其它不能可靠归一化的声明视为 unknown ownership。
- 每一轮 ready frontier 选择 maximal ownership-safe wave：read-only task 不占 ownership；已知且不相交 writer 可并行；相交 writer 串行；unknown writer 与所有 writer 串行。依赖仍由 `depends_on` 唯一决定，不从 description 推断。
- 保留 per-file mutex 和独立 worktree 作为物理安全网；调度器 ownership wave 是第一层，Git merge preflight 是最终层。
- worktree branch 使用合法、稳定且带 hash 的 ref component；创建时把 base 固化为 commit SHA。
- review 通过后，TaskDispatcher 进入 integration stage：stage worktree 改动、用实际 Git changed paths 核验声明 ownership、为未提交改动生成 EKO-owned commit、检查主工作区同路径 dirty changes、用 `merge-tree` 无副作用预检、再执行真实 merge。
- integration 以 execution id 作为幂等身份。merge commit 写入稳定 trailer；进程若在 merge 后、TaskCompleted 前退出，resume 可从 Git history 识别 already integrated，不重复合并。
- merge conflict、ownership 越界、主工作区同路径 dirty、已有 Git operation 或实际 merge failure 都使 task 进入 Failed/Paused 既有路径；worktree 保留并解锁，错误包含 branch/path。成功或无改动时清理 EKO fork worktree/branch；清理失败只记录 note，不反转已经完成的 merge。

## Ownership 合同

`files` 对 mutating task 的语义：

1. `src/a.rs`、`tests/a_test.rs` 等可规范化的相对路径是已知 exclusive owner；两个 task 只有精确 scope 交集时冲突。
2. 空列表、`*.rs`、`src/**`、绝对路径、`../outside`、目录式尾随分隔符是 unknown；允许执行，但本轮不与其它 writer 并行。
3. read-only task 的 `files` 只是阅读目标，不取得 write ownership。
4. subagent 实际改动以 Git index/base diff 为准，不信任模型 `touched_files.written`。已知 ownership 下出现未声明文件时拒绝 integration。
5. ownership 只决定能否并行，不制造隐式依赖。需要“先 A 后 B”的语义必须写入 `depends_on`。

## Integration 顺序

1. subagent 在 isolated worktree 返回 versioned structured result；runtime 持久化 `SubagentReleased`。
2. M7 completion contract 通过；implementation/debugging review 通过或明确使用既有 no-review fallback。
3. 获取 repo 级 integration mutex，防同一进程多个 run 同时写主 Git index。
4. 如果 execution trailer 已存在于当前 `HEAD`，返回 `already_integrated`。
5. `git add -A`，从固定 merge-base 读取实际 changed files 并执行 ownership 核验。
6. 必要时在 EKO fork branch 创建关闭 GPG 签名的内部 commit。
7. 检查主 checkout：index 必须全局干净，未暂存/未跟踪文件只在与 writer 路径相交时阻塞；同时拒绝未结束 Git operation。这样既不把用户 staged 内容带进 EKO merge commit，也不覆盖同路径本地工作。
8. `git merge-tree --write-tree --name-only --messages HEAD <branch>`；exit 1 是真实 conflict，主工作树/index 保持不变。
9. clean 时执行 `git merge --no-ff`。若仍失败，仅在确认本次产生 `MERGE_HEAD` 后执行 `git merge --abort`，保留原 dirty work。
10. 记录 integration note/trace；成功后 task 才写 `TaskCompleted`，失败写 `TaskFailed` 并阻塞显式 dependents。

## 失败与恢复

- 同一 wave 多 subagent 部分成功：每个结果独立验收和 integration；成功 task 可 Completed，冲突 task Failed；下一轮按已有 failure propagation 阻塞 dependents。
- review/contract 未通过：不合并该 worktree；retry 使用新的 execution id 和 worktree，旧 worktree保留用于诊断。
- merge 后进程退出：resume 复用 durable subagent result，integration 用 execution trailer 返回 already integrated，再补 TaskCompleted。
- merge conflict：主 checkout 不进入 conflicted index；worktree branch/path 留存，task error 是权威终态。
- 用户 staged 或同路径 dirty：在 merge 前失败，避免把 staged 内容提交进 EKO merge commit或覆盖未提交工作。这是本地场景仍成立的数据丢失防护，不是 agent permission gate。

## 验收

- 两个不相交 writer 在同一 DAG wave 并行执行，并能顺序、无冲突地集成到主 checkout。
- 相交 writer 和 unknown writer 不在同一 write wave；无 description/排序推断。
- 未完成 dependency 的下游不启动；上游 integration 失败时下游进入 Blocked。
- subagent 改动未声明文件时 integration 失败，主 checkout 不变。
- 两个从同一 base 修改同一文件的 worktree：首个可合并，第二个由 `merge-tree` 报 conflict；主 index/工作树不残留 conflict。
- merge 成功但 TaskCompleted 尚未持久化的恢复路径不重复 merge。
- branch label 含 `:`/空格等字符时仍生成合法、稳定、不碰撞的 Git ref；diff 使用固定 base SHA。
- GUI/TUI/CLI 继续消费同一 TaskCompleted/TaskFailed 与 summary；merge 失败不被 subagent 的 completed 文本掩盖。
