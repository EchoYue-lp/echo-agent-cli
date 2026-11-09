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
3. `ConfiguredModel` 唯一拥有 model id、provider 引用、protocol、modality、输出 tokens 限制和
   context window；`ModelProviderConfig` 唯一拥有 endpoint 与 credential。
4. `resolve_runtime_model(config, selector)` 是唯一 runtime resolver，返回
   `Result<ModelRuntimeConfig, ModelSelectionError>`，不会从缺失或陈旧字段合成模型。
5. `FrameworkConfig` 由选中的 `ConfiguredModel` 和已有 framework `AgentSettings` 构建。
   secret 只进入 `LlmConfig` 构造，不复制到 `FrameworkConfig`。
6. full-config 模型更新直接修改选中的 `ConfiguredModel`；设置或删除默认模型只改变
   `default_model_id`。
7. tracked schema 与维护中的 `config/eko.example.yaml` 删除 runtime mirror 字段；被忽略的
   本地配置继续属于用户，不由仓库清理修改。
8. EKO 是完整 Agent surface，因此工具、workspace memory 与 human-loop 是固定产品能力，
   不是可由用户关闭的 Agent 配置。应用内存中仍直接使用 framework `AgentSettings`，但 EKO
   YAML 边界拒绝且不序列化 `enable_tools`、`enable_memory`、`enable_human_in_loop`。

## 备选方案

1. 只重命名 `AgentYamlConfig`，继续逐字段复制。拒绝：仍保留第二份 framework-shaped 值。
2. 把 model mirror 当 cache 保留。拒绝：普通配置字段没有 generation/invalidation 合同，
   而且已经被当成 fallback authority 读取。
3. 未配置时返回空或 synthetic runtime model。拒绝：调用方必须在构造 provider client 前
   区分 not-configured、disabled、unknown 与 ambiguous selection。

## 影响

- 已配置的主 Agent、writer 与只读 Subagent 构造通过框架完整 builder 的 `configured_llm_client(client, original_config)` 传递同一个已准备 client 和它的原始 `LlmConfig`，不重建 transport，也不在 build 后再用 opaque client setter 覆盖已知配对。只有 config 或无 config 的 opaque client 保持原行为。EKO 继续复用原 resolver、critic 共享、继承代际发布、settings 与 registry 路径；profile/install 仍只有框架一个权威。
- Agent 配置跨 application/framework 边界时不再需要 conversion adapter。
- 模型选择、provider credential 与 runtime validation 各自只有一个明确来源。
- GUI、TUI、CLI/JSONL、channel、Cron 与未来 pooled Agent 共用同一个 typed resolver/error。
- YAML schema 在开发期直接变化，不保留 compatibility mirror。
- framework 的其它复用方仍保留三个通用能力开关；只有 EKO 产品 schema 固定启用它们。

## 2026-11-03：EKO 取消 temperature 产品参数

用户在手动验证 Responses 连接时遇到“不支持请求的 temperature”错误，明确要求取消该参数。
实际 GUI 连接沿 `test_runtime_llm_connection` 使用原准备 client，旧便捷调用又带入框架隐藏默认值；
doctor 另有固定 0.0，EKO ConfiguredModel、runtime/view、IPC/TS 合同、模型编辑器和父子代际还保留传递链。

采用应用层删除产品字段及多余参数的局部修复：配置、Profile、DTO、生成的 TS 合同和表单均不再提供
temperature；主/只读/写入 AgentBuilder 与热换 `prepare_model_generation` 边界显式传 `None`，
连接/doctor/reflection 也明确不请求采样温度。继承与固定模型仍复用原 resolver、client/config 配对、
代际发布、Critic owner 和任务生命周期。thinking、max_tokens、context_window、认证和协议选择保持原语义。

未采用“只在 Responses 丢弃该参数”或保留隐藏产品默认值的方案：它们会让配置、连接测试与运行请求继续
拥有不同含义。通用框架仍保留显式 temperature API，框架作者仅修复便捷调用中未经调用者请求的隐藏默认值；
EKO 不把取消产品旋钮扩张成全局删除未知 provider 参数。既有模型配置无迁移/双语义，未知旧字段不形成新配置来源，
也不自动改写 UserRoot 中的真实配置文件。

验收覆盖设置页不显示/保存 temperature、GUI 共享连接与 doctor 的真实回环 HTTP 请求不含该字段，
以及主/只读/写入和父子模型热换均为 None。配置序列化同时保留输出/context限制。前端 mock 只用于界面，
不能代替真实 HTTP；Rust 编译和回环结果按独立授权窗口的实际原件记录，不以静态检查替代。

本次没有新增框架/SDK公开能力或示例调用；`config/eko.example.yaml` 与中英文文档里的配置代码示例同步移除字段。
官网引用的框架显式 temperature 能力继续有效，便捷 helper 默认值的官网内容归 Framework 作者同步；
EKO 不跨写该作者文件。GUI/App、外部真实 API 与发布资格另行验收。

## 2026-11-03：当前受控共享请求与代际验证

当前组合以Framework91797同已验2857源码包、EKO fixture精6e及原Workspace fixed Registry构建真实app-core
lib测试程序。r04实际NoRun自然0、零compiler warning/error，features=[]、test opt0/debug0；原CargoJSON、
完整FP/u64/.d与T外程序按源清单保全。新程序49b3fe556d48773e9b890b183e206f2269f771052078409a9e92558d6acb76cd
在受控本地环境取得Registry2各1P及EKO9各exact-list1→1P/0F/0I，随后同源targeted Clippy0。

共享GUI连接/doctor测试通过同真实client向localhost Responses各发一次请求，实际执行两body均无temperature、
精deepseek-flash模型/input及doctor max_output_tokens=1断言；原15/15/5秒和shutdown完整等待保持。
其余配置/Profile/view不再序列化temperature而保max/context，主/继承/readonly/writer以及模型热换、固定与继承、
已载入/后继workspace和provider刷新控制在同新程序通过。原82ae首项的未准入fixture名错误不是temperature产品RED，
原独立8P也不冒新程序结果；Root具名审查后只修三处fixture模型名及原错误优先级，provider准入规则、预算和断言未放宽。

本次原件分别记录在Workspace r04交件与CLI同程序9P入口，普通Evidence
`.echo-semantic/evidence/evidence.eko-temperature-workspace-controlled-validation.md`绑定具体范围。
正式collector42a1/274、policy2028、锁d542与原五风险维持；Root本次42 fresh审计仍TCP fetch1/空audit，
该外网风险门禁缺口独立于语义Source/Final和受控Rust测试。没有真实外部API/403原因、GUI/App或旧客户端beforeHTTP
回放资格，也不宣称所有推理参数通用支持。此处只追加事实，不修改当前Rust/Toml/TS或运行客户端。


## 2026-11-06：Eclip 产品身份

当前 GUI 品牌为 Eclip。应用在既有 `prepare_runtime_llm` 主路径创建带有 `Eclip/<版本>` User-Agent 的 reqwest::Client，经 framework 的 `build_client_with_http_client` 注入；不在应用重做协议分派，也不在通用框架硬编码品牌。模型配置结构与现有用户数据位置保持原合同。普通/流式、三协议及连接测试的实际请求头由 loopback 回归覆盖；真实 Ls-ds 的无头 403/有头 200 为修复前的故障证据。
