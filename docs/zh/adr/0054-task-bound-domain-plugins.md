# ADR 0054：任务绑定的领域插件

## 状态

已接受方向，正在实现。当前只源修改尚未编译或执行验收。

## 背景

编译期专业模板独立选择规划方法、默认 Subagent 与审查项，TaskRun 只记录领域枚举，不能解释原插件版本和规则。更新插件不能静默改变已有任务义务。

原 PluginRuntimeService/AgentPool publication 与 TaskRun journal 已拥有所需生命周期和状态；框架提供同 prepare 的插件内容身份和依赖选择。这些是 EKO 策略的输入，不构成新增 executor、store 或插件 manager 的理由。

## 候选方案

- 在原 TaskRun admission 固定有效规则，并在实际执行边界核对原来源（采用）。
- 跨 reload 保存完整旧运行资源。PreparedPluginSet Arc 不拥有旧 MCP、Hook 或完整包文件，需要额外多版本生命周期设施。
- 按当前插件重新计算旧任务。它改变任务含义，不采用。

## 决策

插件根可选 `domain.yaml` 声明 schema_version 1、method、默认角色、allowed_tools、产物、执行检查、验收、审查清单及显式相对 inputs。manifest name 和真实可选 version 是唯一包身份；EKO 随原组件 generation 准备和发布声明。

RunCreated 与原 journal 唯一拥有完整 DomainBinding；TaskRun/图 metadata 保存摘要引用，checkpoint/IPC 只是投影。存在但非法的绑定不能吞成无领域 General。显式领域使用已有最小 Orchestrated TaskRun：无正式义务才用 AllowDirect，否则 RequirePlan 与原最小真实图；General 原无 plan 对话结算不变，不放宽固定 direct Summary 合同。

有效身份覆盖根／依赖闭包、EKO monitors/output-style 定义、显式输入和实际 active style 的 provider/instructions。闭包外实际 style 是明确额外贡献来源；纯主题颜色仍属呈现。插件角色必须以真实 Plugin source 证明来自领域声明闭包，不能只匹配角色名。

身份核准与实际 invocation 保持在原 execution lease/publication 同代边界内；原 cleanup receipt 保留 MCP/Hook/Agent 物理义务。领域工具范围只约束 Agent 自动行为，不约束用户主动终端或 MCP 配置。empty allowed_tools 沿原合同表示不追加限制；受限交集为空必须诊断，不能编码成 unrestricted。原深度1..3/default1、图 revision/claim 规则保持。

## 影响

专业消费者切到固定规则投影并退出编译期专业模板；General 宿主方法和合理通用能力保留。更新、禁用或原内容不可用可见阻断新工作，不取消清理 owner，不重新解释旧证据。journal 只保存规则／身份，不保存凭据、活动资源或完整旧包。

回归须覆盖 typed 往返、两条 direct、动态图最低义务、真实角色/style来源、无关插件控制、reload/排队执行与恢复。源码、focused/full、native/平台和精确main CI 分别验收；当前只源工作不宣称这些已通过。

## 参考

- 阶段设计 `eko-domain-plugin-runtime/design.md` 的已确认 A 路线及 `plan_01_task-bound-domain-execution.md`。
- [Temporal版本固定](https://github.com/temporalio/documentation/blob/013b6f8b13f0613056250b17a5034960b3a440ae/docs/production-deployment/worker-deployments/worker-versioning/roll-out-and-pin.mdx)：固定执行路由不等于 checkpoint 保存代码。
- [LangGraph兼容性](https://github.com/langchain-ai/docs/blob/4506df5fb4105a39b84c2b7390d21bed89e28668/src/oss/langgraph/backward-compatibility.mdx)：状态持久化本身不固定恢复后的业务含义。

## 本次实现截点

首份源码贡献切换真实 create_complex_task、GUI 持久输入准入、两种 RunCreated、typed
任务图/revision、计划 dispatch、Verification、review 与固定 direct Summary。四套编译内
专业模板和枚举专属可见性/路由退出；General 与原执行/清理 owner 保留。

源码续接已让 adhoc agent_tool 的 background/fork/teammate 沿原 pool 物理收据保活，
并消费框架同一 registration 的准入与执行。原 TaskRuntime projector 在 agent_tool 无
结构化 prompt payload 时仍读取持久规则。GUI prepare 只投影已持有 lease，不另发 counted
令牌；弱控制 carrier 不在结算时持有强投影，只有真实 invocation 资源延长准入。
独立框架源码仍待统一采用；当前 EKO 源码未编译/运行测试，也不代表 Outcome 完成。

包示例被 app-core 回归引用，后续原 Rust 测试链会执行其解析/inputs 合同。官网由协调的
独立贡献维护，本应用源码不改营销说明或宣称可发布。TypeScript 生成物待 Rust 租约内从
既有导出链真实生成，不手写生成文件。

## registration 与物理寿命的机制澄清

已确认的同身份执行边界具体化为：框架原 registry 一次解析并保留同一份 definition、
factory/instance 记录；通用可失败准入核这份记录和原 DispatchRequest，原 dispatcher
原样消费。EKO 解释持久领域绑定、provider 闭包与定义摘要。提前查名字、观察 hook 或
当前 catalog wrapper 都不是这一权威。

EKO 原 pool lease 只保有一份实际 keyed/process 准入；opaque invocation 投影跨框架
spawn 保留同一物理准入，不另占槽，也不只保一个可被取空的 Option 容器。原 supervised
release 在原监督 future 中等待物理投影释放，再完成既有 cache retirement。取消观察和
未知执行继续保原 receipt/debt 权威。原 TaskRuntime 投影 registration 仅随调用保活，
使子执行仍读取同一 RunCreated 规则，不新增 store 或 dispatch 循环。

本续接仍属原 Outcome，来源为阶段 Design c0506ab8、Plan f4d68e63、Root checkpoint
5b6b7df。框架 API 当前在独立源码树真实存在，其合流、EKO 编译/测试及 native/main 资格待做。

真实 invocation 资源交出前，原应用可失败 registration 承接已核 RunCreated 的不可变
binding/method 投影；模型边界消费同一 token 寿命的数据，即使之后 journal/operation
读取失败也不丢必需方法。框架观察式 projector 的报错不充当执行闸；General 仍按 None
清除旧 marker，不增加新的 store 或执行 owner。
