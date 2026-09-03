# ADR 0035：会话归档投影

## 背景

EKO 需要在会话列表提供归档和永久删除。可复用的 `echo-agent`
`ConversationStore` 负责 transcript 持久化，不包含产品专属的可见性生命周期。

## 决策

继续使用现有应用层聚合删除路径删除会话；归档状态作为 EKO 应用层、按工作区划分的
JSON 投影保存在 EKO 数据根目录。该投影通过原子写入持久化，并由 Tauri 命令与 TUI
命令共享。GUI 列表/搜索返回 `archived`；删除会尽力清除归档标记，因为 transcript
删除才是权威操作。

## 备选方案

- 把 `archived` 加入框架 `ConversationStore`：拒绝，会把 EKO UI 策略强加给所有框架消费者。
- 只放在浏览器 `localStorage`：拒绝，会导致多个 GUI 窗口和 TUI/channel 状态漂移。

## 影响

归档和恢复是可逆的可见性变化；永久删除清理会话聚合及应用层投影。归档投影损坏或
暂时不可用不会阻止 EKO 启动；归档变更失败会返回可观察的错误，删除后的标记清理
失败只记录告警。
