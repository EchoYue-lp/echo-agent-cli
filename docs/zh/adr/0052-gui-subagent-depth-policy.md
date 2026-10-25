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
