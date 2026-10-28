# ADR 0052：GUI 可配置的 EKO Subagent 层数

## 状态

Accepted product policy; source implementation awaits Rust/native qualification.
- Date: 2026-10-24
- Owners: EKO configuration, TaskRuntime, shared chat execution

## 背景

ADR0034 开启嵌套委派并描述默认三层，ADR0039 要求固定一层。用户已明确裁决：
GUI 可选 1、2、3 层，默认 1，绝不准入第 4 层。主智能体为第 0 层，首次真实
子智能体为第 1 层；它不同于并发数、模型轮次和图依赖长度。框架仍提供独立可配置
的通用嵌套能力，SDK accepted 合同不因此变化。

## 决策

EKO 在 Framework AgentSettings 之外拥有已校验的 `max_subagent_depth`。GUI、IPC
和 YAML 只接受整数 1..3。原 owned 配置 mutation 原子保存，写入失败仍是错误；
设置页说明“对新任务生效”。

已提交配置的共享内存投影只供新运行/turn 准入取值，不是第二持久权威。新 TaskRun
把不可变的 `TaskRunExecutionProfile.subagent_depth_limit` 写入原 RunCreated journal，
由原 fold、checkpoint、snapshot 和 resume identity 保留。同 Run 新增 PlanTask、fix、
retry 和恢复继续使用此绑定。GUI 的 1→3 或 3→1 不改旧 profile，也不取消已准入
物理执行；create-or-read 的显式冲突身份明确失败。

轻量 turn 在原 invocation context 捕获上限，无需创建 TaskRun。primary 为第 0 层，
RuntimeTaskService 传实际目标 child 第 1 层；exact-attempt 和 Fork 只传递，不重复
加深。角色可以收窄资格，静态 flag 不能覆盖每次运行上限。历史缺绑定仍为缺绑定：
可观察、取消和结算原工作，但新委派/恢复沿原错误或恢复阻塞拒绝，不能借用当前
GUI 值或 Framework fallback 伪造旧许可。

原 PlanTask 非空 allowed_tools 同样约束 primary Verification 和所有后代。Verification
保留原 primary 执行路线，不创建假 primary Subagent。原 invocation 把正向硬上界
带到执行锁后的框架 snapshot；旧目录的禁用补集不足以表达它。搜索激活、新角色、
fresh child 和排队期间动态注册均不能放宽。通用 inheritance 修复归框架 owner，
不新增 executor、validator、store、终态或用户手动协作控制台。

## 程序化 Fork 桥的继承合同

原 readonly 与 writer 消费者已把本次 TaskRun 的完整目标 policy 放进 runtime
context。Framework 的 text/message helper（含 exact-attempt）必须保留三字段，
不能仅取当前 depth 再重建 legacy 最大值 3。显式 depth 与该目标 policy 不一致时
在派发前拒绝；不会重新读取 GUI 默认值或多推进一次层数。writer 附件沿原持久
引用重建 message，策略与普通文本路径相同。primary Verification 仍为第 0 层，
继续原路径，不增加假 primary Subagent。

该修复不改变用户政策：默认最大层数 1 允许主智能体派发首层，首层不能再派发；
配置 2/3 允许同一绑定内对应层数，任何模式均不准进入第 4 层。Framework 通用
复用方仍可选择独立上限，EKO 不向 Framework 注入产品常量。真实工作树隔离、
GUI 配置操作、原生进程和 main CI 仍各自需要对应资格，不能由 Fork 测试替代。

## 候选方案

永久禁用子角色 flag 会破坏 2/3 设置和旧绑定 3 的任务；改全框架默认会影响独立
复用方；逐工具重读 GUI 会悄悄改变运行中权限，三者都不采用。AI 自动消息、并行
分工与同一版本化任务图保持；达到上限后可向原 parent 提出有范围的后续分工建议。

## 影响

本 ADR 仅替代 ADR0034 的无条件嵌套/默认三层政策及对应测试期待，和 ADR0039 的
无条件固定单层政策。原通信、生命周期和 GUI 归属仍有效。统一任务主题 ADR0038
仍为 Proposed，本记录不顺带接受整份方案。

## 验证与发布边界

源码测试定义覆盖配置范围/持久化、1/2/3 边界、create-or-read 身份、journal 和
checkpoint+tail 恢复。前端 focused 不能证明 Rust 类型、原生 GUI、真实副作用或
main CI。Rust 类型导出须等明确租约后从精确源码生成，禁止手改生成合同。官网
镜像归唯一 root writer，以真正接受的源为准。框架/SDK 示例无需新增 EKO 设置
API；本仓 YAML 配置示例原位同步。


## 2026-10-27：真实只读工厂的调用上界继承

默认首层与用户选择1／2／3总层数的产品政策不变。内置reviewer仍由原loader、
`register_default_subagents`和`build_readonly_subagent_agent`装配，保留`readonly: true`与
`can_delegate: true`；不会为了允许下一层而移除只读开关、添加产品权限配置或扩大工具名单。
框架在原dispatch装配中选择受限只读实例，普通writer实例仍保原来的变更能力。

调用级`tool_access: Option<ToolAccess>`由框架沿原invocation／ToolContext／SubagentContext
转交，在锁后的原工具快照合并配置只读与祖先只读上界。子调用实际可用工具是原catalog、
父正向工具上界与有效只读能力的交集；模型schema、`tool_search`和实际执行使用同一结果。
角色标签或模型参数不能提升上界，空交集也不能回退为无限制。此项是Agent自动执行约束，
不影响用户主动操作终端或MCP。

EKO的聊天、前台任务与无人值守完整invocation字面量补同义`None`，保留原产品入口行为。
`ProjectAgentAdapter`已有三个invocation入口完整转交原值，不拆字段或重算权限；字段级
回归检查只读、工具上界、运行身份、嵌套policy与history均保留。真正只读执行仍由框架
snapshot／pipeline落实，适配器测试不能替代执行测试。

真实消费者回归应使用同一内置工厂、原TaskRuntime与Fork；模型transport可用夹具替换，
但实际请求schema、`ToolContext`、事件lineage与工厂／工具调用计数必须一致。绑定层数为
1／2／3的运行分别恰达上限，下一层不启动；修改默认值不能改变既有Run。计数器只包原
工厂并安装测试工具，不手造未开启readonly的child，也不清去重记录或改任务ID避开问题。
写探针与父上界外的只读探针均不得执行；普通writer及附件、None和框架合法max4对照继续
由各自真实消费者验证。新回归尚未执行时只记录为待验证，不据本ADR宣布深度已通过。

`lh6_product_soak`不需要新增invocation签名，继续纳入原示例编译入口而不启动长soak。
框架示例与公共API条件矩阵、新Tauri／同conversation双Run原生验收、精确main／CI分别
由各自批次取得资格。SDK沿独立pin与accepted合同处理，不因调用字段追加自动升级。

### 项目准备与工具执行使用同一工作目录

`ProjectAgentAdapter` 在转交三个 typed invocation 入口之前解析实际工作目录：显式调用
目录（含隔离 checkout）优先，其次为原 Agent 配置，最后使用 EKO 工厂的项目根兜底。
项目投影准备和被委派 Agent 收到同一目录；适配器不修改共享 Agent 配置，不重建权限、
嵌套深度或执行循环。回归须同时检查三入口的字段转交与真实工厂执行的 `ToolContext`，
仅在 prepare 中读到项目规则不能证明工具已在该项目执行。通过资格以实际消费者回执为准。
