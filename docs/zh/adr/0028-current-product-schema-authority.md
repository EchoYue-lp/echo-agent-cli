# ADR 0028：当前产品 Schema 权威

## 状态

已采纳

## 背景

EKO 仍处于开发期，不承诺过时本地 schema 或 command marker 的兼容性。若干生产路径在新
权威已经成为唯一 writer 后，仍继续解释旧格式：

- `learned-rules.md` 已是 RulePromoter 权威，但 `.eko/AGENTS.md` 仍会被重命名或读取；
- `.eko/workspace.json` 之外，root `.workspace.json` 仍被当成可读取 workspace manifest；
- 所有 cron prompt 已进入同一个 TaskRuntime driver，但 `[plan]` 前缀仍被静默删除；
- `TaskRuntimeStore::open()` 和生产 `with_run_id` wrapper 只为保留旧调用形状。

这些路径模糊当前产品合同，也会让无关用户文本或文件获得隐藏的 EKO 语义。

## 决策

1. 自动提升规则只读取 `.eko/learned-rules.md`。`.eko/AGENTS.md` 不重命名、不解释；
   `.eko` 之外的标准 `AGENTS.md` / `AGENTS.override.md` 仍是正常 repository 指令源。
2. `.eko/workspace.json` 是唯一可读取 workspace manifest。open、list、detect、logging、
   delete 和 config discovery 都不再读取 root `.workspace.json`。
3. 创建 workspace 时仍拒绝覆盖含 `.workspace.json` 的目录。这只是防止用户数据被覆盖；
   retired marker 不会被解析、迁移、删除或接受为 workspace 权威。
4. Cron prompt 按存储内容原样传给 TaskRuntime，只拒绝全空白输入；`[plan]` 不再具有 marker
   语义。
5. `TaskRuntimeStore::new()` 是唯一默认构造器。只设置 run id 的 task-local wrapper 仅在
   测试编译；生产调用方必须提供完整 run context。
6. 发现不兼容 checkpoint 后从权威 journal 重建的恢复逻辑继续保留。旧 worktree 的保守
   清理也保留，因为它保护或显式暴露用户 Git 改动，而不是继续解释旧执行 schema。

## 备选方案

1. 一直保留 fallback 到 release。拒绝：开发期复杂度会反向定义公开合同，并让歧义文件
   影响执行。
2. 删除所有历史检查。拒绝：retired marker 的防覆盖检查避免误删用户数据；journal/Git
   recovery 保护当前权威数据。
3. 自动迁移旧文件。拒绝：隐式 rename/delete 会修改用户文件，也会让从未发布的 schema
   长期背负迁移代码。

## 影响

- 产品输入格式只有一种当前解释和一个 owner。
- 旧文件仍原样留在磁盘，但不再影响 runtime。
- journal 与 Git recovery 安全措施保留，但不成为 compatibility authority。
- 本次不需要修改 framework；被删除的语义全部属于 EKO 产品策略。
