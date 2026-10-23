# ADR 0049：进程丢失后的 TaskRuntime owner 恢复

## 状态

已采纳。

## 背景与参考

`PhysicalOwnerIntent` 在原 driver 或工具尚未物理结算时阻止重复执行。
现有 `RootTransactionAuthority` 由进程内 canonical 注册表共享，并持续持有
root 的 OS 排他文件租约。进程崩溃后内核释放租约，但原 intent 缺少进程身份，
新进程即使已独占 root，旧 run 仍可永久 Running、Paused command cell 永久 active。
终态已经落盘、intent 确认尚未落盘的崩溃窗口也会永久保留 fence。

既有框架使用的 [fs2 Unix 实现](https://docs.rs/fs2/latest/src/fs2/unix.rs.html)
调用 `flock`；[flock(2)](https://man7.org/linux/man-pages/man2/flock.2.html)
以所有相关文件描述符关闭作为锁释放边界。请求取消、超时或 PID 猜测都不能证明原
owner 已退出。[LangGraph persistence](https://docs.langchain.com/oss/python/langgraph/persistence)
也将检查点恢复与副作用重放区分。这里仅参考该边界，具体证明来自 EKO 已有 OS
租约，不宣称 LangGraph 采用本方案的 epoch 实现。

## 候选方案

1. boot 清空所有 intent：拒绝。同进程其它 store 仍可能拥有活 driver。
2. PID、TTL 或第二套 owner store：拒绝。它们增加猜测或平行权威。
3. 新 intent 绑定进程 nonce，在现有 root 权威下只解析旧进程准入 fence：采用。

## 决策

- 这是 EKO 应用层的文件权威和 boot 投影策略；框架沿用现有通用租约、journal
  与任务恢复原语，不新增框架产品字段或调度器。
- root authority 暴露进程 `OnceLock` UUID。同进程不同 store、不同 root 及
  drop 后重开均使用同一个 UUID。UUID 表示进程 incarnation，OS 排他租约证明
  root 当前归属；二者均不代表工具物理完成。
- 唯一的新 owner 写边界 `begin_physical_owner_intent` 强制覆盖调用者 epoch，
  写入当前 root owner 的 epoch。driver 与独立工具生产者保留该精确身份。
  真正完成路径继续要求与原 intent 完整匹配。
- 只有 boot 专用路径解析有效的 foreign epoch。缺 epoch 代表旧版本 journal：
  当前生产者不可能写出该事实，且当前 OS root 租约排除了旧版本进程仍持有 root。
  空值或无效 UUID 属于畸形证据，保持拒绝。
- 解析追加原 intent 不变的 `RecoveryResolved`，携带
  `owner_process_interrupted` 原因和 recovery process epoch；绝不将
  `physical_terminal_observed` 改成 true。当前进程 epoch 保持 fence，包括
  重复 boot 与同进程 store 重开。
- Running、Paused 继续走已有 interruption recovery；不安全工具/Subagent
  副作用仍阻断等待检查，command cell 记录 interrupted，不由恢复重放模型或工具。
  其它状态仅解除过时准入债务，不改已提交业务状态、不增加恢复工作计数。
- owner fence 解析与 Running/Paused interruption recovery 使用同一个 journal
  batch；恢复失败不得先暴露尚未建立副作用 blocker 的无 fence run。Paused
  保留原暂停原因与已有 cell 终态事件，不新增 auto-resume 资格。
- 已接受的精确 claim 取消或暂停通过框架既有 interruption-first resolution
  结算，boot 保存其完整 execution，包含结算后的 claim/request，不丢弃已接受
  裁决、不增加 retry_count。即使任务已取消或完成，不安全副作用仍独立保留
  inspection blocker。
- 普通准入、取消、重试没有新增 reset 入口。journal 仍是唯一权威；任何写入
  失败均可从既有 journal 重试，包括原子恢复追加或其重放期间再次崩溃。

## 影响与验证

本决策细化 ADR 0015 的原 owner fence，将当前进程执行与进程丢失区分；不新增
TaskRun 状态、public API、SQLite、wire contract。示例无相关改动。官网没有这项
私有恢复策略的独立说明，其 source inventory 在发布阶段按已审提交重新生成。

回归覆盖真实注册 driver 的子进程强制退出、活进程租约拒绝、缺 epoch 旧 journal、
同进程 fence 保留、生产者强制 stamp、畸形 epoch、终态确认债务、Paused command
cell、不安全副作用阻断、重复恢复及不伪造完成事实。子进程证明本机内核租约恢复，
不声称已验证远端工具真正结算或跨平台原生验收。
