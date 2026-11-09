# ADR 0061：工具按功能分组，系统必需策略由EKO后端维护

状态：人类已批准六组展示；本文件与实现是待EKO唯一owner正式合流的候选。JS定向验证与Rust/原生资格分别报告。

## 背景

93项平铺目录难以查找。人类在Root的call_10819250b7b04a8094e546317f60ff36明确采用任务与协作、文件与代码、命令与运行、搜索与浏览、数据与文档、记忆与技能六组，并要求快捷开关及系统必需工具不可关闭。旧ToolInfo把所有来源标Builtin，没有必需原因；原开关是本进程revisioned内存策略，没有接入配置落盘。

## 候选与决策

来源与功能混排的五组会把绝大部分工具放进“系统内置”，并误导为全部必需；八组方案更细但当前没有采用。最终用六个功能主类和来源badge，未知分类进入“未分类”，未知来源明确未提供。分类只负责展示，不授权执行。当前MCP真实owner/server/tool信息沿原McpServerId与客户端目录投影，不从工具名拆来源；ToolManager尚未记录非MCP producer身份，不能因工具名看起来内置就发Builtin证明。

注册仍归ToolManager，显式选择仍归ToolControlService，发布仍用pool generation；不新增store或执行器，不改Framework通用合同。EKO后端单一required_tool_reason既生成列表的锁定原因，也拒绝直接关闭。用户页面的disabled不是唯一防线。

| 必需工具 | 当前真实产品依赖 |
| --- | --- |
| tool_search | tool_exposure.rs延迟schema；ToolSearchTool是发现/激活未首轮曝光能力的入口 |
| task_create、task_update、task_list、task_execute | infra/factory.rs真实任务指导与tasks/task_runtime/register.rs接入的唯一revisioned TaskRun graph |
| read_artifact | TaskRuntime compact_context中长Goal/原文artifact的真实读取与准入合同 |

本地场景仍需拒绝，是为防止设置操作破坏上述产品自身唯一能力路径；这不是权限升级或Web安全闸。shell、run_code和其它合理内置能力不因来源而必需。当前profile依赖仍沿DomainPluginBinding.check_capabilities及正上界交集判定；系统必需不越过只读/allowed_tools或模型能力授予额外能力。

## 行为与取舍

类别头给可选已启用x/y和必需z，mixed只按可选项算；总开关只影响可选身份，锁定原因仍可查看。搜索自动展开结果但清除后恢复折叠，筛选不偷偷缩小分类批量的范围。详情保留说明和参数。

批量先取最新目录，再逐项调用原命令，实际回执逐项更新；部分失败明确保留，不承诺原子批次。 原命令可能已提交策略但发布回执报错，结束后沿原目录读取权威状态；若这次回读失败，旧列表和已收到回执只供查看，不再允许单项或分类提交，直到用户显式重新加载成功。重载失败继续保留未确认状态；不自动重试命令、回滚策略或猜测后端开关。页面的未确认标记只属于当前scope的显示/操作就绪度，不是第二套工具策略；系统必需理由和后端拒绝不变。新动态工具不因旧类别操作自动继承禁用规则。GUI请求携原workspace id与product_data_generation；后端持原transition读锁验证当前scope并完成操作，前端订阅每次scope切换使A→B→A的旧异步批次失效。

选择只影响后续run，已开始run仍持原snapshot；不重写active上下文、不取消已发起工具。原内存策略保持，不新增跨重启保存；不把MCP连接配置当工具开关配置。用户主动GUI终端和MCP连接不受Agent开关或full-auto误卡。

## 影响与验收

ToolInfo、ToolSource及生成TS同步新typed当前合同，不保旧DTO双轨。公共endpoints仅改toolsApi区域，AppState只加工具投影/拒绝及scope接缝；不回灌其它Gov/Provider改动。examples没有构造本产品ToolInfo或工具设置命令的消费者；官网未承诺这类设置/核心名单，无需改公共框架示例或为本产品设置发明SDK例子。

JS定向覆盖锁定、混合态、未知元数据、筛选、失败、注销、A→B→A和卸载；Rust候选测试覆盖实际AppState拒绝与scope guard。低盘未运行Cargo；静态格式与JS通过不等Rust/native验收。正式合流后由owner在已授权资源窗运行Rust与适用门禁，原失败不删除。

参考：[VS Code官方工具配置](https://code.visualstudio.com/docs/agents/run/tools)、[MCP官方Tools协议](https://modelcontextprotocol.io/specification/2025-11-25/server/tools)。前者支持搜索/选择与每次实际工具上下文分开，后者支持producer显示信息且元信息不等于应用策略；六类是EKO的人类产品裁决。


## 2026-11-02 后继：回读未知时的操作就绪度

当前正式页面已具有操作后权威回读和原scope/epoch校验；后继候选仅补上回读失败后仍可再次操作旧开关的缺口。两条定向用例定义覆盖成功回执后目录读取失败，以及策略已提交但发布回执报错且目录也不可读；后一条同时检查显式重载失败仍停、重载成功才恢复选择，并保持系统必需项锁定。

本增量只改前端现有组件和测试，不改ToolControl authority、revision/policy、required名单、数据库、后端权限或运行时；没有跨重启新状态。2026-11-02 K2独占前按Root指令只形成源码候选；窗口解除后，单fork定向验证使用同一13个断言，原组件11过/2失败，候选组件13过/0失败，TSX语法与patch适用性检查通过。这仅证明本候选的前端定向行为，不授正式CLI合流、完整工程或整应用native资格。由CLI唯一writer接收精patch并完成其适用门禁；examples/官网没有该内部就绪标记或ToolInfo构造入口，无需扩张公共SDK示例或对外工具清单。
