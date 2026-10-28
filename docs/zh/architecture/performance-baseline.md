# 文件存储与普通原生历史的性能观测

性能入口是显式调用的开发测试，不是EKO产品入口。只在协调者冻结源码、锁、实际feature/profile、编译回执和二进制，并安排非计时数据准备与独占quiet窗口后执行。文档、harness存在、静态检查或准备队列均不代表已测得时延、瓶颈、优化收益或完整原生界面性能。

## 保留的原始范围

会话与Run集合各10/100/1000，会话每对象10条中文/emoji消息；TaskRuntime journal另按10k/100k事件、valid/tail/missing/corrupt及boot分列。记忆entry10/100/1000与history0/1k/10k分别改变；合法任务图1/10/50/100、depth不超过10；实际工具保small/near/over。热5次warmup及30次measurement，独立进程cold10，完整记忆成功写20且每manager不超过50次。拒绝、失败、partial和setup不计成功分布，N小于100不报p99。

普通GUI历史的native publication持久投影是新增负载族，不能用FileConversationStore列表、TaskRuntime恢复、工具wire或TaskRuntimePanel/jsdom替代。所有原负载继续保留。

## 普通历史、Fork与safe-point

EKO根GUI lib中的私有ignored入口复用既有真实producer、Guard/approved blocks、FileConversationStore和同一个ChatEventLog；通过原性能runner的seed/validate/run及原report统计。它不公开benchmark API、不创建Store或runner、不启动用户App或WebView。

| case | 计时的实际调用 | 边界 |
| --- | --- | --- |
| `native-history-gui` | `project_conversation_messages_with_native_history`完整await；cold另含store/日志构造、读取与首次stream恢复 | 不包含Tauri IPC、前端、layout/paint或command外层workspace fence |
| `native-history-fork` | 原`reply_fork_prefix`及`attach_reply_fork_native_history_imports` | App/Workspace装配与关闭在计时外；不包含目标创建、文件复制或worktree/SSH操作 |
| `native-history-safe-point` | 原合法TurnConfiguration append、首次lazy stream验证、durability、checkpoint和默认retention | facade构造在计时外，实际首次authority恢复在append内；不把后置oracle的重试算进成功 |

初始批量Message轴为真实source的final-only1条与progress+final2条，不是重复同一行或改ID造规模。追加代表输入包括已追平head8193、首次缺投影4097/8193、head8193但尾4096未追平、默认retention已裁过原pub，以及下一合法safe-point触发默认retention的输入。`--events 0`仅为默认retention构造选择器，实际事件数由真实journal回读；不能标为0条事件。

seed在一次专用绝对工作根中生成，构造/模型夹具/复制均在计时外。采用原离线ResourceChainModel和MockTool，实际driver、文件持久化与publication不替换；测量caller不请求模型或外部服务。validate只核canonical、冻结oracle及派生文件字节，不能提前调用会追平checkpoint的历史查询。所有冷样本从同一冻结字节快照恢复，进程cold不代表OS page cache或磁盘cold。

每个成功样本核完整canonical摘要、原pub envelope/身份及完整SavedMessage DTO；期望来自真实source原完整publication和原selector。计时结束后、任何可重试的pub oracle前，先核generation/head/coverage、retained floor及实际段清理。private观测中的prune commit或cleanup degraded均不能当成功样本；不在校验中补完后再宣称测量函数已经结算。新输入、换generation、Gap、foreign/corrupt及取消继续受既有合同约束，不能返回空或skip蒙混。

## 私有I/O观测

既有`test-utils` feature启用私有tracing事件，观察native projection显式sync、checkpoint load/save及retention prune边界的调用尝试与Result成功/失败。它没有产品公共API，默认产品构建不启用；App-core单元诊断另要求cfg(test)。GUI lib测试通过原进程tracing dispatcher收集blocking线程事件，按本fixture的准确日志根隔离。

这些计数不是OS syscall、物理字节或各子步骤耗时。原append内部其它barrier不在这些边界计数中；观测器的开销包含在启用test-utils的测试二进制测量里，不能冒未经观测的产品时延。原runner的wait4 CPU/峰值RSS/block操作仍属于整个子进程及其等待的子进程；APFS block计数0不表示没有I/O，也不分摊为每条Message资源值。

## 构建与资格

普通历史私有入口位于根crate，app-core-only构建不会编译它。复用协调者分配的独立target、主根内TMPDIR/TS_RS_EXPORT_DIR、jobs不超过5、debug0/inc0，在同一冻结相邻批中增加：

```sh
cargo test -p echo-agent-cli --release --no-default-features --features gui --lib --locked --no-run
```

实际保全binary的`--list`必须包含：

```text
tauri::commands::conversations::native_history_tests::resource_chain_acceptance::performance_baseline::performance_baseline_native_history
```

App-core私有I/O诊断需要单独精确的test-utils lib构建；它与GUI lib及default app-core的实际features、binary和回执分开绑定。原report只精确认可具名GUI lib构建，不放宽source/lock/feature/binary守卫。每个Cargo批次先保日志、自然出口及必要制品，核真实reader为空，再清准确自有target。无窗口不编译、不生成合法dataset或计时。

这些入口本身属于可执行示例/测试链；没有新增产品API示例或官网性能宣传。底层持久化、barrier、retention策略、4096有界批和预算均沿原实现，不因测试便利调整。单run报告继续保守输出incomplete；整份性能目标、真实WKWebView/paint及发行验收分别完成，不能靠改报告状态核销。
