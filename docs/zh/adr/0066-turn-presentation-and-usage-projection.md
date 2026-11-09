# ADR 0066：可信轮次呈现与原日志累计

日期：2026-11-07。状态：实现候选，原生验收待安排。

## 背景和候选

逐消息渲染使同一轮在冷历史中拆开，单个 MessageBubble 的折叠不能覆盖整轮。原前端缓存累计在加载历史时清零，而原 journal 回放存在 4096 条上限。只缩小 margin、仅包单消息或扫描尾窗均不能满足完整轮次及实际费用累计。

参考 ZCode 先建立轮次/过程单元再渲染的结构，以及本机 DeepSeek 官方 Harness 的 whole-log 用量投影；采用原事实上的只读呈现，保留本应用的发布、保存和执行 owner。

## 决策

从已验证 publication 投影可选 `NativeTurnSource`。实时和冷历史共用 `ConversationTimeline`，按目标容器及源 workspace/conversation/root_turn/turn/execution 分组。缺失、冲突及跨硬边界的重复轮次保持原行；不根据正文、时间或 provider ID 猜测归属。Fork 的源坐标不变成目标 owner。

两段正文之间的连续过程只有一个摘要入口，完整原序可展开；分页补前缀时迁移用户最近显式选择、焦点及原事件锚点。只有原发布事实确认且来源唯一的非空最终正文，才把中间正文和过程整体默认收起。最终答案、错误、保存警告、产物和待处理输入保留可见。组件不改变 canonical 历史及动作资格。

块的外层 cursor 来自原已核 publication 的首次记录，与响应内 block sequence、执行 status sequence 分开。Subagent 只按原 parent execution／工具事件关系及同一原 journal cursor 插入过程；没有 cursor 或 parent 就明确显示位置/归属未恢复，不按相邻时间、最新助手消息推断。Fork 的源运行只读展示，不把源详情控制路由成目标会话的 owner。

统计是原 ChatEventLog 的派生投影：在原 `NativePublicationCoverage` 内保留累计及独立 usage through-sequence，复用同一 journal identity、事实锚、原子检查点和后缀回放。首次调用/尝试锚先落盘，再提交累计覆盖；中断后只有已提交 through 之前的事实才算已经累计，不能把锚文件存在误当已记账。发布与统计均追平且 barrier 确认后才裁原日志。旧发布 coverage 不证明旧 usage 已累计，缺失前缀保持不完整；不新建统计权威 store、SQLite 或消息副本。

同一个原固定 batch 同时推进 publication、usage 和只读呈现补充的各自 cursor，避免每种投影另扫 4096 条。呈现补充仍在原 owner/checkpoint/barrier 内，只保公开图表、工具身份/cursor、Subagent 生命周期；不额外长期复制工具参数/日志、token 噪声或私有推理。原 digest 只是派生引用的来源标识，不能把引用还原成原 envelope 或作为正文发布证明。旧前缀缺失明确标记；当前 generation 缺原 parent scope 见证时，不把新建的空日志当作旧图表从未存在。Fork 使用 `AttachmentsPayload` 的独立只读 sibling，原 `NativeHistoryImport` 的正文/最终许可验证不变。

按原调用/尝试去重。失败已报告的费用保留，漏报、待返回、身份不全和冲突分别记录；cache write 不算 hit，inclusive input 不重复加缓存。主行只计目标会话实际主链，Subagent 单列，辅助调用未覆盖就明示未知。编辑、回退和前台清空不冲销原费用；Fork 继承前缀不计入目标日志账目。明细按模型列出已报告值。

Owned Run 的原 LlmUsage 也沿既有 run_turn publication owner 记录，不能依赖可选的前台 trace。保留的 Exec Usage 只薄转交真实 native source，与原 Agent 表示按同一尝试去重，不从 Run ID 猜 execution。用量不解锁 native 发布头；完成标记必须晚于原 Store accounting 成功，同 envelope 在失败后仍可重入原幂等路径。图表/执行引用与这些费用事实各自只读，不互相授予资格。

速度只消费原模型可比较观测：主行最近合格值，当前生成时标“上次”；明细使用合格输出总数除以合格时长总和。没有证据显示 `--`。框内工具行保留现有发送、附件、权限和菜单 owner；框外一行放运行统计及当前上下文，草稿估算不入累计。

只读 `get_conversation_usage` 复用原 workspace 代次和会话创建身份栅栏；前端固定节奏查询同一原日志投影，结束再读，不因可见消息清空重记费用。迟到响应不能污染另一会话；读取失败保留最后确认的值并明确暂不可读。

图表沿既有 Vega／Vega-Lite spec 绘制。原 `srcdoc` 中的内联脚本与 CDN 依赖被桌面既有 CSP 拒绝，不能把有数据卡片等同于已绘图。改用本地打包的官方 `vega-embed`，按需加载官方 `vega-interpreter`：parse 开启 `ast`，View 使用 `expressionInterpreter`，输出 SVG。依据为官方 npm 包随附 README 及 embed 的 `ast`／`expr` 接口；不放宽 CSP、不新增自制图表解释器。每次 spec 使用独立 DOM surface，卸载和迟到结果均释放各自的 View，不能清空后继图表；失败保留原 spec 和明确错误。

Vega 5／Vega-Lite 5／Embed 6 候选引入新的已知漏洞，因此弃用。采用已修复的 Vega 6.4.0／Vega-Lite 6.4.1／Embed 7.3.0／Interpreter 2.3.2；Lite 6.4.1 的 Node 要求仍覆盖项目 Node 20.19 下限，未采用要求 Node 22 的后继版本。主版本升级须用原工具七种 v5 spec 及既有 Vega v5 spec 在准确 CSP 下实际绘图验证，不能只检查 SDK mock。既有前端依赖的安全修复单独保留原 audit 失败与修复结果，不把新增或旧漏洞列为默认接受。

标准 `generate_chart` 原来只返回普通文本，不能据工具名称声称 builtin 或从任意 JSON 猜图表。通用工具现在以既有 `data` 和官方 Vega-Lite v5 MIME 声明有界公开产物，保留 Text kind、原 pretty output 与 HTML 导出；具体合同归框架工具说明。应用只在原 native presentation fold 提取这些完整数据：成功、未截断、未 spill、声明/schema 正确，且原 checkpoint 中只有一个不同的调用见证，call_id、有效名称、execution、完整 scope 与原 cursor 均一致。重复调用别名不以外层 hash 区分；必要时复用原 cursor 在固定 batch 上限内读取原内层 envelope，只有摘要一致才归一，原件已裁或冲突则保留未恢复，不另存私有 args/log。本轮应用策略仅处理 `generate_chart` 工具位；有意替换同一工具位并声明相同格式的扩展可作为公开图表消费，但不会因此获得 builtin 身份、权限或最终发布资格。其它工具名不在本轮识别范围，不由名称或任意文本猜图。失败、Guard 替换、纯文本同名工具、缺见证和 foreign/child 关联均不能补成主链图表。

实时视图复用原 `TranscriptProjectionSettlement` 的实际成功确认（op、generation、attempt 和 conversation 均匹配）作为一次读取触发，通过原 `conversation.get` 取得同一 Rust Supplement。前端不解析 ToolResult、不另造 Chart 事件、poll、store 或 IPC。请求去重沿原 transcriptSaves 的事件身份；只读补件允许原轮次自然结束，但原 hook identity generation（包括卸载）、view epoch、workspace generation 和 source/调用见证必须仍匹配。迟到的其它视图结果不能串写。最终确认已有的读取也合并同一补充。这里的保存确认只开放只读补件，不代表运行或最终回答完成。

## 验证及限制

必须分别验证原 journal 裁剪/冷开、锚与 coverage 中断切点、重复/重试/漏报、主子费用隔离，以及组件实时/冷历史/补前缀的相同行为。浏览器合成状态验证只能证明实际 React 组件和布局，不能替代原生 Tauri 点击、真实 provider 或精确 main CI。

公共计时合同见框架 ADR 0109；消息发布权威继续遵守 ADR 0057。没有新增模型工具，examples 不增加应用 UI 调用；既有公开事件消费者须编译。官网只在真实交付后由其 owner 同步，不发布候选截图或假命中率。
