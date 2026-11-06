# 真实 SSH 验收准备（配对测量协议）

日期：2026-11-06。状态：待执行。本文件定义"在有真实远端时怎么测、记什么、判定什么"，不是验收结果。

## 1. 目的与边界

验证 WP0–WP4 在真实网络下的行为，重点是**进程启动次数、往返次数、回退语义、关闭结算**，不是模型效果或 GUI 观感。

不在本文件范围：WP5（规则缓存）与 WP6（真实停止）尚未实现，相关项只记录当前基线，不作为通过条件。

## 2. 前置条件

| 项 | 要求 | 说明 |
| --- | --- | --- |
| 远端主机 | 一台可 SSH 登录的 Linux 或 macOS | 与本机不同机器，禁止用 localhost 冒充 |
| 账号 | 支持密码或 key 登录（GUI 走原 PTY 认证） | 不把凭据写进配置或本文件 |
| 项目目录 | 一个**真实代码仓库**，至少 200 个文件、≥3 层目录、含 `node_modules` 或 `target` 之一 | 用于观察忽略项与规模效应 |
| 网络 | 记录 RTT 与丢包 | 见 3.1 |
| 日志 | `~/.eko/logs/app.log`，`logging.level: debug` | `debug` 才能看到车道逐条记录 |

### 2.1 先量网络，否则数字无法解释

```bash
ping -c 20 <host> | tail -2          # 平均 RTT / 丢包
ssh <host> 'uname -a; nproc; df -h .'
```

RTT 决定同一份代码的期望值：50ms 与 5ms 下"每操作 1 跳"的绝对差异是 10 倍。**没有 RTT 记录的计时数字不进入结论。**

## 3. 记录口径

四个时间点必须分开记，不要混算：

1. **T0** 输入保存（GUI 里回车那一刻）
2. **T1** 首个 Agent 状态事件（用户可见的第一次响应）
3. **T2** 首个模型请求发出
4. **T3** 该轮结束

日志键：

| 事件 | 键 | 关键字段 |
| --- | --- | --- |
| 准备结算 | `project file preparation settled` | `elapsed_ms`、`reused`、`success` |
| 定义准备 | `agent project definitions prepared` | `elapsed_ms`、`deferred_to_invocation` |
| 车道逐条（debug） | `project SSH SFTP lane operation settled` | `lane_reused`、`elapsed_ms`、`ok` |
| 车道汇总（info） | `project SSH SFTP lanes closed` | `operations`、`opened`、`reused`、`closed_lanes` |
| 远端搜索回退 | `remote content search declined a pattern without an ERE equivalent` | `pattern` |

抓取示例：

```bash
grep -E "project file preparation settled|SFTP lane|lanes closed|declined a pattern" ~/.eko/logs/app.log | tail -80
```

## 4. 步骤

每一步都要：先 `tail -1` 记下日志当前行号，操作后再抓新增行，不要事后凭印象。

### S1 连接与首次进入

1. 新建 SSH 项目，指向准备好的目录，测试连接并进入。
2. 记录：连接耗时、`verify_directory` 是否成功、有无重试。

**判定**：进入后 `lanes closed` 不应出现；`opened` 在此时可以为 0（还没用过 SFTP）。

### S2 首条消息（冷启动）

1. 发一条不触发工具的短消息（例如"你好"）。
2. 记录 T0→T1、T0→T2，以及 `project file preparation settled` 的 `elapsed_ms` 与 `reused`。

**判定**：

- `elapsed_ms` 应与 RTT 大致成"常数 + 少量往返"关系，而不是随目录大小线性增长；
- 整个准备过程 `opened` 增量应 ≤ 1（一次准备复用一条通道）。

### S3 连续工具调用（车道复用）

在同一会话里连续做 5 次只读操作：`list_dir` 根目录 → `read_file` 一个源码文件 → 再 `list_dir` 子目录 → 再两次 `read_file`。

**判定**：

- `opened` 相对 S2 结束时的增量应为 **0**（全部命中复用）；
- 5 次操作应各有一条 `lane operation settled`，`lane_reused=true` 至少 4 条；
- 若出现 `opened` 每次 +1，说明复用失效，**必须停下来查**，不要继续后面的步骤。

### S4 远端搜索

1. 在 ≥200 文件的仓库里 `grep` 一个确定存在的普通字符串（例如 `TODO`），记录 T0→T3。
2. 再 `grep` 一个含 `\b` 或 `\d` 的模式（例如 `\bpub\b`）。
3. 再 `grep` 一个只存在于 `.gitignore` 忽略目录里的字符串。

**判定**：

- 步骤 1：结果正确，且日志里**没有**大量 SFTP 读；对比同一查询在本地项目的耗时，远端应只差一跳命令。
- 步骤 2：应出现 `declined a pattern without an ERE equivalent`，并回退本机遍历——**结果必须与本地一致，不能报错**。这是 WP4 的关键回归点。
- 步骤 3：应与本机路径一致（即"能搜到"，因为现有工具不读 `.gitignore`）；若远端漏掉，说明 `find` 剪枝与 `walk_and_search` 不一致。

### S5 取消与超时（当前基线，不是通过条件）

1. 让 agent 跑一条长命令（例如远端 `sleep 300`），在 GUI 取消。
2. 记录"点击取消"到"工具返回"的墙钟时间，以及最终错误文案。

**判定**：按当前实现，取消只观察、不终止，返回时间 ≈ 远端命令自然结束时间。**这是预期基线**，WP6 落地后本项才转为"应在数秒内返回并给出停止三态"。

### S6 关闭与退休

1. 关闭项目、再删除该项目。
2. 记录是否出现关闭超时、`lanes closed` 的 `operations/opened/reused`，以及 `closed_lanes`。

**判定**：

- 关闭应在秒级完成，不应出现"operations remain active or uncertain"；
- `closed_lanes` 应等于当前空闲车道数（1 或 2）；
- 删除项目后远端不应残留 `ssh -s sftp` 进程：`ssh <host> 'pgrep -af sftp-server'` 应为空。

## 5. 指标表（每项都要真实数字）

| 指标 | 采集方式 | 参考阈值（相对 RTT N ms） |
| --- | --- | --- |
| 首条消息准备耗时 | `project file preparation settled.elapsed_ms` | ≤ 30×RTT |
| 单次工具操作耗时 | `lane operation settled.elapsed_ms` | ≤ 3×RTT |
| 每消息新开通道数 | `opened` 增量 | ≤ 1 |
| 车道复用率 | `reused / operations` | ≥ 0.8（5 次操作后） |
| grep 远端命中耗时 | T0→T3 | 不随候选文件数线性增长 |
| 关闭耗时 | S6 墙钟 | ≤ 5 s |
| 取消到返回 | S5 墙钟 | 记录基线，暂不设阈值 |

## 6. 失败处理

- 任一步的日志与预期不符：**保留该次 `~/.eko/logs/app.log` 片段与时间戳**，不要重启后继续测，重启会丢失当次 `opened/reused` 计数。
- 出现写操作（`write`/`atomic_write`）失败时不要重试：先按既有 `unknown` / `Possible` 语义核对远端文件，再决定。
- 远端残留进程不要用 `pkill sftp-server` 粗暴清理：先记录 pid 与命令行，确认属于本次连接后再处理。

## 7. 记录模板

```
日期 / 主机 / 远端 OS / RTT / 丢包：
项目目录 / 文件数 / 是否含忽略目录：

S1 连接：耗时 __ s，结果 __
S2 首消息：T0→T1 __ s，T0→T2 __ s，preparation elapsed_ms __，opened Δ __
S3 五次工具：opened Δ __，reused __/5，各次 elapsed_ms [...]
S4 搜索：普通字符串 __ s / 结果 __；`\b` 模式是否回退 __ / 结果是否一致 __；忽略目录命中 __
S5 取消：点击到返回 __ s，错误文案 __
S6 关闭：耗时 __ s，closed_lanes __，operations/opened/reused __/__/__，远端残留进程 __

异常与原始日志片段：
```

## 8. 与本计划其它部分的关系

- 本协议覆盖 WP0–WP4；WP6 落地后需要补"取消 → 远端实际停止"的三态记录，并替换 S5 的判定。
- WP5（规则缓存）落地后需要在 S2 增加一项：连续两条相同内容消息的 `preparation elapsed_ms` 对比，且外部修改 `AGENTS.md` 后必须重读——但这依赖 C0 的 `FileMetadata.modified`，当前未实现。
