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

## 原生聊天时间线的JS测量接缝

`web-frontend/src/testing/nativeChatTimeline.tsx`仅供private性能构建入口导入，shipping index不引用。它沿原`handleChatEventEnvelope`消费AssistantMessage、ToolCall/ToolResult、ExecutionStatus和最终通知；工具摘要复用原`ingest/recordToolStart`，publication DTO只薄接原`applyFinalAnswerSource`。真实`MessageBubble/NativeMessageTimeline/ToolExecutionGroup`在同一个生产模块图内呈现，没有复制组件、建立产品控制台或增加第二性能runner。

场景使用`gui/NativeMessageTimeline/`前缀，保原probe四操作ABI：mount观察消息块，single-row观察工具摘要与raw result ingress，batch观察执行状态，run-state观察最终通知及publication身份。原trace顺序和已验证DTO必须保留，不为阶段分组制造terminal。只测消息层DOM，不把工具执行/详情读取、模型/网络、TaskRun/TurnStatus结算或整页导航计入该结果。真实输入中的普通最终通知先于Settled时也保原顺序，不能用Settled自行发布正文。

JS分别记录dispatch、真实React layout-effect提交及DOM-ready；原native Mach计时仍止于下一次WebKit transaction回调入口。typed边界放在原样本`correctness`内部；不同clock不混算，transaction/rAF或截图都不等于屏幕paint。prepare/execute/cleanup失败和在途取消均拒绝成功见证并恢复专用空现场；现有聊天/会话/工具/TaskRun不能被夹具接管。文字/工具摘要离线负载在挂载前拒绝自动媒体及分段delta拼接出的媒体语法，不改变产品对图片/附件的正常支持。

`nativeChatTimeline.test.tsx`是已有Vitest链上的合成JS回归，不是实际native样本。完整数据仍需准确最终source、真实capture步骤/原stdout marker、retained binary、原envelope/get DTO和tool-summary trace。旧unknown-only capture缺少完整工具轨迹，不能按缺失填零或换来源标签取得资格。正式App、capability、quiet、原Node统计与report均复用原测量链，未取得时对应项继续未完成。新增接缝不增加产品API/普通examples或官网性能数字。

完整工具捕获继续复用原`native_unknown_actual_backend_replay_capture`单个显式ignored入口，在原unknown/Guard案例之外增加`tool-progress-signed`。同一资源链producer保持Untouched原rows；可选测试renderer接到真实`bind_surface_chat_sink(Gui)`，ToolExecutionProjector先提交原仓库摘要，再经原callback交付并记录started/finished与envelope的实际顺序。捕获写出完整source rows、canonical、冷journal envelopes、tool-events、delivery-trace、实际Settled及已确认get publication，不手造summary/result或把Candidate改成Settled。

新模块仍属原测试链，无产品入口/模型服务/App/poller。所有输出沿既有FILE/INDEX stdout marker和create-new文件语义，必须由准确最终源的真实warm/同源批运行，再逐bytes/SHA、retained binary及compiled source核对；source-only候选、rustfmt或更早来源的捕获都不获得最终native计时资格。阶段分组只能筛选原回调，不能把实际summary-before-envelope改排成想象顺序；private reader同时核原索引、stdout与全delivery trace。缺finished或错owner负控只变异内存副本，原件保持。

## TaskRun 健康同 cut 读取控制

Run 集合原 `all+validation` 入口保留 1000 个对象、完整结果/状态/排序校验、128 个 authority 的有界缓存及原 N/watchdog。热的含义是同一进程，不保证全部 Run authority 常驻。种子、snapshot restore 和根构造位于原计时边界之外，authority 冷恢复与产品读取副作用则按真实调用计入。

checkpoint 的已恢复 cut 不能单独证明 run-state/plan 已落到该 cut。读取复用原 journal/checkpoint 恢复、原 typed projection builder 和现存派生文件；仅 Loaded checkpoint 的有效 cut 等于实际 applied cut，且所需 readmodel 的完整成功值与原 builder 输出相等时，复用当前投影标记。缺失、损坏、陈旧或不可证的投影继续原修复；尾部事件仍真实重放并发布，foreign/pruned 等原失败规则不放宽。no-plan 不增加派生文件清理。

同 cut 冷重开/逐出不重写、checkpoint 后尾事件与“checkpoint 已前进但 readmodel 仍旧”、原冷投影修复矩阵均属 correctness 控制，不是性能样本。新增控制必须先在原 source+test-only 下实跑，再用同测试加生产增量核 GREEN；静态格式、可应用性和原 300 秒 baseline 失败都不代替这些结果。writer/目录持久化屏障不降低，before 原始数据及 partial 保持冻结。

## 工程来源：真实 Git root 与显式对象快照

`scripts/performance-baseline-runner.py` 的 `source_identity` 继续是唯一源码身份入口。普通 Git 输入必须是请求目录自身的真实 Git root；无 `.git` 的 owned 源不能向上借用 superproject HEAD，也不能在 Git 检查失败时自动选择另一来源。只有原 `--profile-metadata` 中明确提供 `source_inputs.eko/framework`、且 `kind=git_object_snapshot` 时才进入快照分支。

旧39c快照 descriptor 继续绑定 repository、origin_commit，以及原 manifest、Git objects、compile_start 和 sourceguard 的路径／SHA；Framework 另绑定原 readonly permission phase 及其前置记录。新增具名 `materialization-v1` 直接识别080实际源manifest和原postguard，不伪造旧39c状态；编译start／caller／FP按每个实际程序分别绑定。入口核对实际 owned root、原编译 cwd／command、完整唯一文件与目录集合、kind／size／SHA、Git exec 和实际 fullmode；绝对／越根／重复路径、额外文件或空目录、缺失成员、symlink及错误模式均拒绝。EKO mode须与原Git对象一致；Framework只允许已绑定阶段规定的444／555文件与包括根在内的555目录，不泛化成忽略writebit。

该来源记录保留 `root=实际owned目录`、`source_kind=git_object_snapshot`、`origin_commit=原提交`、`head=null`、`status=null`。`origin_commit` 不冒充当前 Git HEAD；descriptor和完整成员共同进入原source摘要及capsule公式。capture后的seed、run和GUI沿同一record／profiles重新核回，不另建来源authority。原report复用同目录且以精路径／SHA绑定的runner实现，并分别绑定实际执行的 `evidence_reader` 与 `source_identity_reader`；`step.command/cwd == final.command/cwd` 始终保持；没有具名独立同源证明时，`final.cwd == capsule.repository.root` 也保持。原F340 P1仅在独立完整双根证明与固定目录锚实际核回后建立“原编译root→qualified依赖root”的关系，两条路径和原cwd都不改；原FP／profile／features／binary及成功回执仍须匹配。

Rust被测源码、实际编译cwd、profile与binary是一层来源，执行capture／reader的Python工程版本是另一层来源。后继正式采用工程脚本不回写已测39c快照或旧receipt，也不能用当前formal root替换原hostsourceRoot。该区分沿用[SLSA provenance材料摘要与执行上下文](https://slsa.dev/spec/v1.0/provenance)及[Cargo实际环境目录语义](https://doc.rust-lang.org/cargo/reference/environment-variables.html)，不新增性能协议、运行owner或Git失败fallback。

## 具名逐程序来源与纯捕获合同

工程工具按实际program／role分别保存原Cargo caller、start、argv／env／cwd、target、Executable、FP、dep-info与feature来源。requested selection来自原Cargo argv；CLI四role的resolved features还须与真实观察rustc argv中全部feature cfg精集合相同，并关联crate／test、extra-filename、out-dir、编译unit及原Executable／FP。大u64 profile只作无损十进制比较。原P1只有已接受的Cargo／FP／dep来源范围，observed cfg保留null，不给CLI缺失观测作fallback。

新增证明路径按本轮具名080／F340／原P1组合交付，仍含精确状态、固定Root目录锚5397…及后置3874…、原捕获controller／接受材料哈希；它不是自动接受其它epoch的通用authority。原P1源码／cwd／FP不改，formal644／755与快照444／555分别实核。378目录的独立锚仅约束其当前采集点之后的生成／消费，不补称P1构建时目录metadata；缺失、错误root／SHA、目录mode／inode漂移均拒绝，prepare与reader不能自行重立expected。Root签票不能代替每次完整Git／成员／物理身份扫描。

显式080快照核原Git objects及完整Bsource；postguard的formal_clean只表示原NoRun时点，不要求当前formalCLI回退、清洁或仍停080。formalF340及双根／目录锚则继续动态严格核验。历史源保origin_commit及head=null，不冒新D前端cut。

`scripts/performance_baseline_native.py` 是原report加载的唯一纯capture／fixture合同，复用同一reader的SHA／JSON／argv原语，不启动进程或统计分位。它分别验证原707 child与外层Python事实，保原41文件／40成员双射、五份compiled Rust SHA和原callback／publication顺序；不是App／WebView构建或资源采样validator。一次性prepare／qualify／compose及其特定Root路径只保存在ignored工程证据，不进入正式scripts。

本轮三份080／F340来源胶囊及派生fixture已通过其具名输入资格；原step9数组误走对象reader的exit1保原，独立compose-r04后继自然0。此历史受控资料不等于4c2a/D新frontend、assets或App已验。child监督、数据规模、5warmup／30hot／cold10、oracles与quiet边界不变；正式采用新Python字节不回写旧原件，也不把它们称为新cut样本。本次正式布局在独立新fixture补齐真实目录inode替换负控后，一次完整相关Python回归实际73pass／0fail（原58＋便携15），自然退出0；没有运行benchmark。新代码与模块布局验证由独立当前工具Evidence记录，Source／final及复审另列；旧原件不换绑新formal模块，实际GUI／RTL／nativepaint及发布继续独立验收。

## 2026-10-31：39c 的 Run1000 单组 after 实测

本组被测EKO源码为 `39c28e7c44c54c9367884f3c08094788ff2f5f72` 的1078文件owned对象快照，实际依赖为F340的2850文件只读快照。原default app-core lib和integration NoRun均自然0，实际binary、FP／dep-info已保全且准确target已清理；本组调用integration程序 `af0349b71e51d130bb0a26f3a101059ef41734a223026d095c1641e611f06bce`。实际工程runner为 `707d628181304652f61cd623f772d8aeefa81937df2afb16d54d85b90e41f504`，reader为 `3091464948fb64c4b1eae9009f2cdc4a492be15609b7659dab3585de7d3ec76a`；它们的后继正式采用不改变该Rust source epoch。

原capture／seed／validate及本组runner、Rust child均自然0。原八个场景各5次warmup、30次measurement，共280行，其中240条计时样本；全部原correctness校验通过，1000对象、128 authority容量、300秒watchdog与5秒清理期限均未改。下表只将原Node summary的ns转换为ms，没有重算统计或引用setup／validate耗时；每行measurement N=30、warmup N=5，不报p99。

| 原场景（省略 `task-runtime/` 前缀） | median ms | p95 ms |
| --- | ---: | ---: |
| all+validation | 310.6825415 | 483.064917 |
| pending+validation | 313.912188 | 512.838542 |
| conversation-hit+validation | 304.131229 | 424.850958 |
| conversation-miss+validation | 306.0085625 | 471.462916 |
| formal-page/all/first+validation | 317.6835415 | 489.570416 |
| formal-page/all/next+validation | 301.1655835 | 474.847791 |
| formal-page/pending/first+validation | 307.4215625 | 532.194292 |
| formal-page/pending/next+validation | 301.028646 | 504.511916 |

原Node统计自然0；工程reader自然2、`incomplete`、`errors=[]`，编译识别为 `recognized_success`，仍保留 `complete_required_matrix_not_independently_established`、`real_RTL_interaction_not_established`、`native_paint_not_established` 三项global gap。因此本组仅证明这些输入在原期限内完成原N和全部校验，不代表完整性能矩阵、App／native paint或产品SLA通过。

quiet租约已于北京时间2026-10-31 21:19:54.581至21:21:34.462实际取得并释放（原字段为UTC `13:19:54.581Z` 至 `13:21:34.462Z`）。103次周期观察未命中原named-heavy分类，但仍观察到Codex Renderer 108.9%、cc-switch 94.9%、fseventsd 77.3%、aTrustXtunnel 59.2%等背景峰值；周期top8不证明整机solo、持续低CPU或优化因果。整进程wait4资源只适用于child及其等待的后代，0／0 block操作不等于无I/O，不能摊成每次操作成本。

旧before1000在原300秒超时，仅留14行partial、`accepted_samples=false`，不是有效分位数或速度比baseline；本次source／dataset／root亦有变化，未做A/B comparison或因果加速宣称。原错误、准入拒绝、partial和不同来源均保持。完整原件在顶层ignored目录 `.echo-semantic/local/parallel-executors-20261027/performance/source-snapshot-reader-candidate-20261031-r01/actual-after-runs1000-hot-r02/`；`result.json` SHA为 `cf39292a9367121cdd1030fe67d74492c1e11d6e3f66d5373a4320ffcd1d081d`，原Node `run/summary.json` SHA为 `5ec66bc906cab8e25cc615319c32218f4dd6aa927adc63da4ddabb1a7325c460`。这些本地原件和来源范围不因后继正式脚本提交而晋升；examples／官网不发布这组局部数值或增加产品API。


## 2026-11-08：无 retention 工作时的派生调度后继

App仅在需要裁剪或收敛cleanup时由safe-point推进派生投影；普通native读取仍沿原API自动
追平。此为ADR0057的应用调度后继，未改变journal/atomic checkpoint确认、pins、默认retention、
批次、预算或性能资格。没有实测就不报告收益。

seed在原producer完成后、追加unprojected tail或超限Flush-only负载之前，显式调用原usage
reader物化合法基点；该准备在计时外，随后missing仍真实删除派生目录，tail仍保原未追平尾部。
validate及测量后的correctness检查不能补物化来伪造已完成的测量。特别是safe-point输入在
计时前仍须超过默认max_segments；被测append必须完成原durability、coverage和真实prune，
其结果在后置pub oracle前核对，不能提前读取而裁剪输入，也不跳过degraded失败。
