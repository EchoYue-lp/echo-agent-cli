---
schema_version: 4
slug: ssh-operations-without-server/plan
outcome:
  summary: 用户通过现有SSH工作空间完成服务与进程查询、启动重启停止、服务日志和Docker运维，操作结果可核验，断线与未知结果清楚呈现，全程免部署Eclip远端服务。
  acceptance:
    - 在同一已核对的SSH目标上，通过真实GUI/Agent路径完成查询→明确授权的控制→状态复查→日志取证；回答准确区分服务状态、进程数量、Docker容器状态和健康检查，不以命令exit0代替业务健康。
    - 原服务查询场景能对活跃进程、指定后端数量、指定服务健康状态逐项给出证据、采样时间及未知项；证据足够后结束调查，发现异常不自动扩展到数据库、cron或服务修复。
    - 断线、权限不足、目标消失、PID复用、同名进程、容器重建、输出截断、超时和取消均有明确结果；不自动重放结果未知的控制操作，不把取消观察或SSH进程结束当成目标已停止。
    - 新建及复用SSH会话的准备/命令/模型阶段分别记录耗时与连接计数，证明原重复准备和逐文件开关通道未回归；同条件冷/热样本保留真实结果，不以固定首token时延或本地夹具替代真实网络验收。
    - 最终源码、必要回归、GUI条件矩阵、文档/示例检查和构建包逐项绑定；交付说明明确区分源码通过、本地夹具、真实SSH操作、App手验及main
      CI资格。
out_of_scope:
  - 部署Eclip专属远端backend/server、监听端口、设备协议、远端Agent循环、SQLite或远端操作数据库。
  - 独立设备管理控制台、设备到多个Workspace共享owner的大重构、跨客户端重启接管任意进程；继续使用现有SSH工作空间和原执行位置权威。
  - 远端IDE/LSP、远端MCP生命周期、批量集群编排、自动巡检与告警、Windows远端支持扩张、SSH密码持久化与新的提权机制。
  - 新建服务/Docker专用工具、第二套TaskRun/command
    cell/store或扩大工具schema；优先使用现有shell、文件与命令cell路径，只有原接口无法表达必要行为时回传明确缺口。
  - 把独立的本机checkpoint超时/保存恢复改造混入本计划，或重写Goal轮次总结、其文档与真实模型样本。
design_ref: null
design_sections: []
delivery_ref: null
todos:
  - id: stabilize-current-ssh-path
    summary: 收敛当前SSH优化来源并修正影响运维的连接、目标和上下文一致性缺口，先建立可复测基线。
    files:
      - echo-agent-app-core/src/terminal/project_ssh.rs
      - echo-agent-app-core/src/terminal/project_ssh/lanes.rs
      - echo-agent-app-core/src/terminal/project_ssh/sftp.rs
      - echo-agent-app-core/src/instruction_provider.rs
      - echo-agent-app-core/src/infra/factory.rs
      - echo-agent-app-core/src/workspace/runtime.rs
      - echo-agent-app-core/src/state/app_state.rs
      - echo-agent-app-core/src/terminal.rs
      - echo-agent-app-core/src/terminal/project_ssh/generations.rs
      - echo-agent-app-core/src/terminal/project_ssh/sandbox.rs
    acceptance:
      - 复用已经实现的通道池、远端搜索、目录预取、期限看门狗与factory/driver准备交接，不重复实现；已有优化计划与当前源码不一致的部分先按实际字节核对。
      - 测试连接、重新连接、命令和文件读取绑定相同的实际host/user/port及Workspace
        generation；未连接不能回退本机，切换GUI焦点不能改变在途目标；运维可使用已验证的远端主目录，不要求Git工程。
      - 同一turn使用固定的已准备规则；下一turn外部修改规则后不能因同长度并还原mtime而命中过期正文。mtime/size不得替代关键指令内容校验，重连与位置变化必须使旧缓存失效。
      - 计数区分普通lane、准备会话、流式reader及SSH命令，不能把lane容量2误写成整连接总通道上限；冷热基线记录源码和运行包身份。
      - 按已确认的连接代际设计保留原Host、TaskRuntime和本机store；旧255/unknown与guard不阻止新认证同目标代际供后续请求使用，旧请求/准备会话/reader/复合方法始终持原快照，不清unknown、不重放控制。
      - 新draft验证及发布重查实际目标、目录、产品generation、预期物理generation和master原身份；并发重连、取消、关闭、service
        shutdown及错误目标不能替换当前连接或丢owner。
  - id: query-with-evidence
    summary: 让现有Agent按运维问题准确发现能力和目标，形成有依据且范围收敛的服务、进程及Docker查询答复。
    files:
      - echo-agent-app-core/src/project/prompt.rs
      - echo-agent-app-core/src/infra/factory.rs
      - echo-agent-app-core/src/infra/tests.rs
      - echo-agent-app-core/src/turn_context.rs
      - echo-agent-app-core/src/chat_driver.rs
      - echo-agent-app-core/src/terminal/project_ssh/sandbox.rs
    acceptance:
      - 沿现有应用提示/投影向SSH任务提供运维行为要求，模型使用实际已有shell/文件工具；不新建工具或重复任务规划接口，也不为普通聊天全局注入大量命令手册。
      - 按需发现目标OS、服务管理器、Docker可用性和当前账号权限；不存在systemd、Docker不可用或权限不足时返回具体限制，不安装依赖、不猜服务不存在。
      - 查询先明确服务名、进程归属和容器标识；统计明确容器/服务副本与操作系统进程的不同口径，实际输出正确处理空结果、多个候选、中文和shell特殊字符。
      - 检查结果携带目标、时间和原命令/结果入口；健康检查失败与接口仍能响应可以同时陈述，证据不足标未知，完成原问题后正常汇总，不继续无关调查。
  - id: control-and-verify-targets
    summary: 通过原SSH执行路径完成用户要求的启动、重启和停止，并验证精确目标的实际状态。
    files:
      - echo-agent-app-core/src/terminal/project_ssh/sandbox.rs
      - echo-agent-app-core/src/permission.rs
      - echo-agent-app-core/src/tasks/task_runtime/command_cells.rs
      - echo-agent-app-core/src/chat_driver.rs
      - echo-agent-app-core/src/tool_execution.rs
    acceptance:
      - 服务和容器优先使用目标已有管理器；仅操作用户指定目标，读取时记录稳定容器ID及服务归属，控制前重新核对，避免同名候选或容器重建造成错误操作。
      - 普通进程停止核对原进程身份，PID复用负控不得伤及替代进程；缺少可靠身份依据时明确无法安全自动执行，不能用killall/模糊pkill或扩大目标范围替代。
      - 普通进程的启动或重启必须有明确的原启动方式、工作目录和必要配置，或交由所属服务管理器完成；不能从ps输出猜测完整启动环境，也不能把Eclip命令cell当成服务器服务托管。
      - 控制命令返回后进行有界状态复查，分别呈现命令受理/退出、服务或容器实际状态及健康结果；停止后被服务管理器重新拉起时如实指出，不宣称已持久停止。
      - Agent自动动作沿原权限/HITL路径执行；用户交互终端不增加full-auto门槛。需要提权时给出原权限错误和可用交互路径，口令不进入命令参数、环境变量、日志或模型。
  - id: bounded-logs-and-interruptions
    summary: 让服务、文件和Docker日志可读可追溯，并在超时、取消或断线后保留已获得结果和正确恢复方式。
    files:
      - echo-agent-app-core/src/terminal/project_ssh/sandbox.rs
      - echo-agent-app-core/src/terminal/project_ssh/reader.rs
      - echo-agent-app-core/src/tasks/task_runtime/command_cells.rs
      - echo-agent-app-core/src/tool_execution.rs
      - echo-agent-app-core/src/tool_execution_projection.rs
      - echo-agent-app-core/src/chat_driver.rs
      - web-frontend/src/hooks/chatEventHandler.ts
      - web-frontend/src/components/chat/MessageBubble.tsx
    acceptance:
      - 默认日志查询读取最近200行，同时遵守原输出字节上限并显示截断；支持按用户指定的时间范围或行数查看，保留来源、时间、stdout/stderr及原结果入口，不整份灌入模型。
      - 用户明确要求跟踪时使用有期限的读取/采样和原command cell观察；停止跟踪只结束Eclip自己的读取，不停止被观察服务。不把SSH
        executor的完成后缓冲输出称为实时流。
      - 断线或控制响应丢失后先恢复连接并重新查目标状态，不自动重发start/restart/stop；单凭重新查询到在线或离线状态，不能倒推出原命令是否成功执行。
      - 区分取消已请求、命令自然结束、看门狗期限到达及停止尚未确认；工具观察有界返回，同时保留原物理资源和未知副作用，不自动清债。
      - 已批准发布的阶段发现和完成工具入口在失败后仍可见，运行失败与保存状态分别展示；沿现有journal/投影去重，不从工具日志合成新的业务结论，不新建恢复状态机。
      - 当前与退休连接沿同一TerminalService关闭及退出路径观察；快照lifetime未结束不封闭原请求的子步骤，已结算退休代际能回收、未知继续可达；全部代际共用同一个绝对关闭观察deadline，删除/退出如实报告原未结算资源。
  - id: accept-and-package-ssh-operations
    summary: 以隔离目标完成完整运维场景验收，交付与最终源码一致的App和中文使用说明。
    files:
      - echo-agent-app-core/src/terminal/project_ssh/
      - echo-agent-app-core/src/infra/tests.rs
      - echo-agent-app-core/src/chat_driver.rs
      - web-frontend/src/components/workspace/ProjectSourceFields.test.tsx
      - web-frontend/src/hooks/chatEventHandler.contract.test.ts
      - examples/lh6_product_soak.rs
      - docs/zh/adr/0055-project-workspaces.md
      - docs/zh/adr/0063-project-context-startup.md
      - docs/supreme/specs/ssh-remote-device-optimization/acceptance-real-ssh.md
    acceptance:
      - 受控回归至少覆盖无systemd/无Docker/权限拒绝、同名目标、PID复用、容器重建、日志截断、连接丢失、取消和状态复查超时；不通过放宽断言、吞错误或重复操作使结果变绿。
      - 真实SSH验收在用户指定或自有隔离服务/进程/容器上分别完成状态查询、启动、重启、停止、服务日志和Docker日志；记录每步实际前后状态，禁止以TCP连通、本地脚本或Mock通过代替。
      - 真实GUI用同一SSH工作空间完成一轮查询和一轮控制；退出重开后保留原发现与错误，新连接仍需核对实际目标。真实模型样本通过原TaskRun/工具/事件路径，测试成功以原运行时结算为准。
      - 最终冻结切点执行适用仓库门禁和示例编译，独立审查通过后才构建App；实际源清单、资产与主程序绑定、签名及启动器核验有原件，当前仍在运行的包或target不得借清理替换。
      - 正式中文说明随所属子仓维护；检查examples及官网相关内容，未涉及的新功能不虚写，官网同步受其真实source
        checkpoint规则约束。docs/doc-parity-manifest.json保持既有单一writer，新增登记由该owner协调。
      - 受控整条Workspace恢复路径证明原Host/TaskRuntime/store实例不变，旧unknown及guard保留，新请求使用新generation；在该修复后的最终冻结源码重新构包，再进行真实SSH/GUI恢复验收。
artifact_id: plan:c4243941-6f33-4330-a9cd-d38307042c5a
lifecycle: ready
---

## Notes

本计划以 2026-11-06 当前 CLI `dbfaa95a6a8dc4f2eda477331bbad655c712bc0c`、framework `1f22f2a57ca7fbfb214ab4c75afa17194fac6eef` 加已有 WIP 为调查起点；HEAD 不是完整源码身份。已有 `ssh-remote-device-optimization/plan.md` 是优化工作与历史验证来源，本计划补足面向用户运维操作的闭环，不重置其历史失败或把局部通过追认为当前App通过。

按 Todo 顺序推进。前一项暴露的目标绑定、错误传播或资源结算缺陷在本行为单元闭合后再继续；无关的独立项目维持原owner和证据边界。预计工作量约 4–6 个有效开发日，取决于当前集成验证及真实SSH场景暴露的问题，不是交付日期承诺。

现有 `FileSystem`、`SandboxExecutor`、`ContentSearchProvider`、`CommandCellRegistry` 和 TaskRuntime 继续拥有通用机制。应用只装配SSH运维策略、目标/结果呈现与现有能力适配；如必须改通用接口，先核对整个framework及合理消费者，明确分层和验证后修订本计划，不把产品策略下沉成新框架状态。

规划阶段未执行业务修复、Cargo、真实SSH、GUI或服务启停。后续实施使用各自仓库的非main分支和唯一主根，按文件划分单一写权；验证期间冻结本次实现及依赖源码。


2026-11-07 用户已确认补齐连接代际设计并继续完整修复。本Plan同一Outcome、artifact_id和位置保持；原design_ref=null按artifact修订合同不可改，恢复行为以明确指定的 `docs/supreme/specs/ssh-remote-device-optimization/design.md` 为设计输入，由实施和审查读取该唯一路径，不另建可执行Plan或第二交付链。原已通过单元保持各自源范围，最终包须重新绑定修复后源码。
