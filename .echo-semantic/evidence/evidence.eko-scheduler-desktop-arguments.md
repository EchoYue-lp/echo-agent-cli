---
schema_version: 1
id: evidence.eko-scheduler-desktop-arguments
kind: evidence
observed_at: source:8ea5f1d7be21b6199587c40c2c6b0ebf5e68780234ce87d88518532212b152a1
source_refs:
- web-frontend/src/api/endpoints.ts
- web-frontend/src/api/scheduler-invoke-contract.test.ts
- web-frontend/src/lib/tauri-bridge.ts
- web-frontend/src/components/scheduler/SchedulerPanel.tsx
- web-frontend/src/components/scheduler/SchedulerHistory.tsx
- src/tauri/commands/scheduler.rs
- echo-agent-app-core/src/scheduler/management.rs
- docs/zh/architecture/runtime.md
supports:
- rule.eko-framework-consumer-authorities
limitations:
- 当前JavaScript真实bridge加实际Rust签名和锁定Tauri源合同；mock仅native invoke边界，不是真Rust IPC或新App验收
- 没有操作实时用户配置、任务或PID75416；新bundle实际创建/手动运行/历史仍需Root后继验证
- 不关闭原cron恢复、HITL、全IM、外部模型或全仓unknown与整体Goal
---

# Scheduler桌面创建参数的边界证据

## 来源与范围

真人新App创建报错为 `invalid args cronExpr for command add_scheduler_task: missing required key cronExpr`。当前23f3前端表单传cron_expr，经schedulerApi/create、apiInvoke、tauri bridge原样送到native；Rust#[tauri::command]的cron_expr:String按默认camelCase解码，故handler执行前已失败。当前Cargo.lock为tauri2.12.4/macros2.7.3；原registry wrapper默认ArgumentCase::Camel，再以to_lower_camel_case生成CommandItem.key；tauri ipc/command.rs对JSON取该key，独立证明cronExpr。没有借新mock返回值定义正确参数。

同仓weixin-invoke-contract现有模式提供source-derived signature与真实bridge控制。新测试读取当前scheduler.rs七个实际签名，mock仅@tauri/core原生边界；endpoints/apiInvoke/bridge和HTTP client serializer均为生产实现。唯一生产修复是桌面create顶层键cron_expr→cronExpr，REST与嵌套CronTask/CronTaskEdit不变。history已是opaque string/null，管理投影原serde_json::to_string保u64，未发现对象cursor问题，未扩修。

## 支持的结论

原23f3 endpoint配新合同测试实际RED：9项中1fail/8pass，唯一create经真实bridge报missing required key cronExpr。同测试在单行修复后实际通过；新scheduler合同与原Weixin/Panel/History四文件共27pass／0fail，原caller自然0。当前498输入全src格式0、全lint0、完整unit910pass／4原skip（94files pass／1原skip）、tsc-b加默认Vite build0／零构建warning、git diff检查0；全部为本次当前源码实跑，未把旧901移签。前后498文件SHA/bytes/mode无漂；Rust consumer原collector采用前后各两读仍74411914…54bf74，lock399509…与policyc199…精同，未重复Rust/scanner。原PNG两份、Cargo锁定macro与bridge/Rust源码、RED/GREEN原caller/raw仅本工作区 `.echo-semantic/local/scheduler-create-IPC-argument-fix-20261101-r01/` 可用，哈希不代替原件。

## 已知缺口

源合同通过不等新App真实IPC成功。当前75416运行旧bundle，不热改/退出/重开；F0独立重型窗口进行，本线不Cargo。Root正常提交后需新source嵌入的新App构建与真人安全新任务创建、手动运行/禁用/历史验收。没有新增业务API、依赖、schema、框架调度器或持久化；Rust、锁和风险collector保持。中文runtime/原Map仅增加必要边界说明，新测试用当前Evidence绑定，Map原观察与历史资格不改。examples无新调用或参数语义，官网仅按新正式commit同步runtime必要文稿，不新增性能或原生完成宣传。
