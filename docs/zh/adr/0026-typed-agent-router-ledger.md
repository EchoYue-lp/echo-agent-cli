# ADR 0026：Typed AgentRouter Ledger

## 状态

采纳

## 背景

AgentRouter 过去把 `AgentMessage` 编码成 framework 的 `String` route、JSON `Value` payload
和 metadata，再在读取时重建 EKO projection。即使生命周期 authority 已经下沉 framework，这种
形状仍然让应用看起来拥有第二套 message 和 record 模型。

## 决策

直接使用已文档化的 framework facade：

```text
DeliveryLedger<Journal, AgentAddress, AgentMessage>
```

`AgentAddress` 实现 framework 的 `DeliveryRoute` 合同。framework 的
`DeliveryRecord<AgentAddress, AgentMessage>` 是唯一 durable record，AgentRouter status query
直接返回它。EKO 只保留 endpoint、workspace、wake、retirement、group 和 surface policy，不定义
AgentRouter projection reducer，也不再有按来源命名的 framework record 转换方法。
生命周期命令直接使用 framework `DeliveryTransition`；EKO 不再镜像一份应用侧 settlement enum。

旧的 `AgentInboxEvent` wire 与 `delivery-ledger.checkpoint.json` bridge 已从活动代码删除。本项目
仍在开发期，不承诺兼容这些本地文件；升级后需要重新创建 data root。

## 影响

- EKO status response 现在直接包含 framework typed record 字段：`route`、`payload`、`phase`、
  lifecycle timestamps 和 retention metadata。
- framework 的 `effect_started` 与 `deferred` phase 对客户端可见，不再被压平为 `claimed` 与
  `persisted`。
- framework 与应用只有一个 delivery reducer 和 projection；GUI、TUI、CLI、channel 共用同一结果。
