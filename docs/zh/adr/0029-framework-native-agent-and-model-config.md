# ADR 0029：Framework-Native Agent 与模型配置

## 状态

已采纳

## 背景

`EkoConfig` 曾保存一份与 framework `AgentSettings` 字段完全相同的
`AgentYamlConfig`，然后逐字段复制成新的 framework 值。顶层 `model` 还在 canonical
`configured_models` 与 `model_providers` 之外重复保存 provider、model、credential、endpoint、
protocol、sampling 和 context window。设置默认模型会回写这些 mirror 字段；没有 configured
model 时，runtime resolver 还能从 mirror 合成一个模型。

重复值让配置优先级变得隐式，也可能在切换模型后继续把旧 credential 或模型参数当权威。
这与 framework ADR 0021 和 EKO ADR 0027 的 framework-native value 决策冲突。

## 决策

1. `EkoConfig.agent` 直接保存 framework `AgentSettings`。EKO 通过 `EkoConfig::default`
   提供产品默认值；部分序列化设置先以结构化方式合并到这些默认值，再反序列化为同一个
   framework 类型。EKO 不再定义按文件格式命名或 framework-shaped 的 Agent settings 类型。
2. 顶层 `model` 使用只包含 `default_model_id` 的 `ModelSelectionConfig`；反序列化时拒绝未知
   mirror 字段。
3. `ConfiguredModel` 唯一拥有 model id、provider 引用、protocol、modality、sampling 参数和
   context window；`ModelProviderConfig` 唯一拥有 endpoint 与 credential。
4. `resolve_runtime_model(config, selector)` 是唯一 runtime resolver，返回
   `Result<ModelRuntimeConfig, ModelSelectionError>`，不会从缺失或陈旧字段合成模型。
5. `FrameworkConfig` 由选中的 `ConfiguredModel` 和已有 framework `AgentSettings` 构建。
   secret 只进入 `LlmConfig` 构造，不复制到 `FrameworkConfig`。
6. full-config 模型更新直接修改选中的 `ConfiguredModel`；设置或删除默认模型只改变
   `default_model_id`。
7. tracked schema 与维护中的 `config/eko.example.yaml` 删除 runtime mirror 字段；被忽略的
   本地配置继续属于用户，不由仓库清理修改。

## 备选方案

1. 只重命名 `AgentYamlConfig`，继续逐字段复制。拒绝：仍保留第二份 framework-shaped 值。
2. 把 model mirror 当 cache 保留。拒绝：普通配置字段没有 generation/invalidation 合同，
   而且已经被当成 fallback authority 读取。
3. 未配置时返回空或 synthetic runtime model。拒绝：调用方必须在构造 provider client 前
   区分 not-configured、disabled、unknown 与 ambiguous selection。

## 影响

- Agent 配置跨 application/framework 边界时不再需要 conversion adapter。
- 模型选择、provider credential 与 runtime validation 各自只有一个明确来源。
- GUI、TUI、CLI/JSONL、channel、Cron 与未来 pooled Agent 共用同一个 typed resolver/error。
- YAML schema 在开发期直接变化，不保留 compatibility mirror。
