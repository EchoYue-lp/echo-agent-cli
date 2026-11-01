---
schema_version: 1
id: evidence.eko-scheduler-run-history-navigation
kind: evidence
observed_at: source:e2d2c706446c691eb75bc450890b7ff51ba5c2bbf2c953307b2b9f67298f9f01
source_refs:
- web-frontend/src/components/scheduler/SchedulerHistory.tsx
- web-frontend/src/components/scheduler/SchedulerHistory.test.tsx
- web-frontend/src/components/layout/LeftSidebar.tsx
- web-frontend/src/components/layout/LeftSidebar.test.tsx
- web-frontend/src/components/layout/RightRail.tsx
- web-frontend/src/stores/conversationStore.ts
- web-frontend/src/stores/conversationStore.test.ts
- web-frontend/src/stores/taskRuntimeStore.ts
- web-frontend/src/stores/taskRuntimeStore.test.ts
- docs/zh/architecture/runtime.md
supports:
- rule.eko-framework-consumer-authorities
limitations:
- 真实React组件与Zustand状态、API边界mock，只证明前端合同，不是真实Rust IPC或原生真人验收
- 不读取或修改UserRoot，不重放Failed Run，Framework物理文件名修复另线负责
- 不关闭原cron恢复、全IM、外部模型、全仓unknown或整体Goal
---

# Scheduler 原 Run 历史入口证据

## 来源与范围

Root报告自动Cron在创建消息日志时失败，存在原TaskRuntime Run而无Conversation文件。
本线只处理EKO GUI入口，不复制Framework物理key修复，不缩逻辑ID、不造会话、不重新执行。
当前61d3的SchedulerRunLink强制loadConversation后调用聊天态refresh、检查activeRun并关闭设置，
没有显式历史选择或打开任务面板。真实conversationStore的loadConversation内部catch并toast后resolve，
因此“await抛错导致refresh早停”并不成立。侧栏已有metadata存在性判断及loadByRun(scope,id)，
RightRail也直接选择原Run；这两条是已有生产可达机制，不新增导航store、调度器或Rust/DTO。

最小复现使用真实SchedulerRunLink和真实Zustand store，只mock API IO；未接原生或UserRoot。
同类planless Failed Run包含原run/workspace/conversation身份及errno63错误事件。
原61d3组件实际RED自然1：getRun已经调用，selectedRunId仍null；同时尝试不存在的Conversation。
单点修复后同一selector实际GREEN自然0，保留原Failed状态、无plan与完整错误事件，聊天activeRun不被替换。

## 支持的结论

SchedulerRunLink复用原productDataScope、导航/新稿代次及loadByRun。
切换工作区后仅由成功读取的原元数据判断是否加载transcript；无transcript直接选择原Run。
网络/权限与Run读取错误保持可见，不用catch把错误转换为不存在。已有transcript沿原加载与错误呈现，
目标尚未载入时不打开面板。任务快照的scope/generation/run/conversation全部匹配后才openChat/openTasks并closeSettings。
状态只来自原Run，不按链接的状态字符串合成成功或写回；后继导航、新稿与generation使迟到响应失去打开资格。

r01局部85项、完整925pass／4原skip及Source/final0只属于原快照。Root与独立前端审查发现I1：
原目标conversation已经active时，读取失败保留旧activeId，RunLink仍会getRun并打开页面。
本次different/same activeId双负控实际RED自然1，different通过而same失败，原same实际getRun调用1次。
不能用r01绿票覆盖这个缺陷。

本后继仅扩展原conversationStore.loadConversation内部返回值：本次成功投影transcript返回true，
读取错误、旧请求或canProject拒绝返回false；上下文restore失败后实际投影的只读历史仍为true，
原toast、旧view保留与readonly行为不变。RunLink先消费本次boolean，false立即停止后续Run读取和
openTasks/closeSettings，不用旧activeId替代成功票。其它既有忽略返回值的调用者保持行为，全部纳入tsc。
没有Rust、DTO或外部API变化。当前相关History/Conversation/侧栏/TaskRuntime四文件158pass；
当前完整前端928pass／4原有skip，94文件pass／1原有skip，全src Prettier、lint、tsc-b加默认Vite
build均自然0、零构建warning。r01首轮fixture类型错误及历史Map引用错误原件保持；本r02官方fresh
preflight重新建立合同，正常Source/final只绑定当前快照，不复用已consumed票。

当前498项前端来源在最终验证前后精确核SHA/bytes/mode；Rust consumer由原collector前后各两读，
锁和policy一并核对，不用前端验证代替Rust或审计。中文runtime与既有Map仅补入口场景，历史observed_at及unknown保持。
没有新Rust/DTO/外部API或示例调用，examples无需改动；官网镜像由Root交所属sole writer按正式新commit同步必要runtime段落。
这是既有历史行为的局部修复，不改变架构，不新增ADR。

## 已知缺口

原始RED/GREEN、首轮类型错误、最终完整前端验证及正常Source/final原件仅本工作区
`.echo-semantic/local/scheduler-history-planless-failed-navigation-20261101-r01/` 可用；I1双负控及本次新完整验证、
fresh预检与正常Source/final在相邻`-r02/`目录，哈希不替代原件。
没有Cargo、App重建、App停止/打开、用户数据读写或任务重放。Root独审与正常stage/commit仍须另做；
新App在Framework修复后统一构建，再由Root确认真人历史入口，本证据不能证明该原生验收已经通过。
