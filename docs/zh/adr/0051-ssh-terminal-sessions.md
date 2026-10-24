# ADR 0051：显式 SSH 目标与原终端会话结算

## 状态

已采纳

## 背景

已批准 Plan06（plan:08c22af5-243e-4460-a70c-2c5e2ec186b6，SHA 7f0a96b913a019d2623283ea711f89a93daa2d60a1e86b4635f72797081624c0）从 EKO 2b52251 与框架 e4e5039 接续。现有 TerminalService 已拥有 PTY，但 GUI 先 create 后监听、切 tab/隐藏卸载、逐块解码 UTF-8，close 仅调用 clone_killer 后立刻移除。portable-pty 0.9.0 的 Unix clone_killer 只发 SIGHUP；原 Child::kill 才有 HUP 宽限后强终止，且仍需原 wait 与 reader join。

## 候选方案

1. 原 create 内使用系统 OpenSSH；前端先就绪再创建，原 Session 保留所有资源与结算任务。采用。
2. 新 SSH 协议库、daemon、独立执行器/TaskRun 或后端 reserve/start、输出重放 store。均超出已批准边界。
3. 用 Connected 动画或 shell prompt 判断登录，或 kill 返回即发布 Closed。不构成认证或物理结算证据，拒绝。

## 决策

通用 SSH 协议、认证、config/agent/key/host-key 仍归系统 OpenSSH；PTY/Child 原语归 portable-pty。EKO 的 TerminalService/TerminalSession 仍是唯一会话与目标/准入/资源权威，Tauri 只转发。框架、SDK pin、Agent ShellTool 权限与 TaskRuntime 不变。GUI 手工连接不授权模型自动执行；当前模型不注册 TerminalService 工具。

创建仅有既有 create 一步：target 为 local 或结构化 ssh（host/alias、可选 user/port），参数直接给系统 ssh，不经 shell 拼接、不覆盖用户未填写的配置、不添加弱化 host-key 的参数。程序缺失明确失败，不回退本地 shell。产品不保存凭据，也不硬编码验收服务器；启动仅证明本地进程已启动。

每个 tab 先挂载 xterm、输出/退出/缺口观察及有界串行写队列，ready 后才 create。所有 tab 跨切换和隐藏持续挂载。PTY raw Uint8Array 直接交 xterm，其写回调释放在途预算；输入同样串行且有界。真实广播 lag/队列溢出可见，不伪称完整历史或 VT 重放。每个异步 setup 步骤检查取消并清理自己的实例；已发出的 create 保留原 Promise，取消后精确补偿 close，失败保留可重试 tab。

原 Session 保存原 Child、输出 reader 的 JoinHandle 和一个结算任务句柄。关闭设置原会话请求，结算任务使用原 Child::kill/try_wait/wait 并实际 join 原 reader，全部证明后才按实例身份移除并发布一次退出。调用者只借用等待句柄；取消或有界观察超时不 abort 原结算、不取走活句柄。kill/wait/reader 失败保留原资源和目标供重试，不能发正常 Closed。不可恢复的 reader 错误不得靠重试抹掉。直接 child 回收不表示进程组或远端副作用全部停止。

ApplicationLifecycleOwner 的原 shutdown 阶段同步关闭终端准入；close_all 与 create 共用既有创建边界。终端未结算使同一应用 owner 继续 retained 重试，事件 bridge 的 join 不替代 PTY reader join。创建/list 的实际 id/pid/target 与 close 实值同步到手写 TS 合同，删除虚构 cwd/created_at。

## 影响

源码与测试阶段不等于已交付运行能力。当前无 Cargo/native/远端租约；Rust、原生窗口、真实认证与远端交互、完整矩阵和 main/CI 都待实际验证。不复用旧 GUI binary 作为新协议验收。现有真实目标仅在协调验收阶段由用户提供，产品配置/测试使用通用目标。

existing examples/lh6_product_soak.rs 不调用终端/SSH，无源码迁移；若后续变更示例须纳入原编译链。官网由协调者按已验收精确来源镜像，本任务不部署。新 Rust 字节不自动继承 ace3 风险指纹，后续按原六项精确风险独立复评，不扩大接受范围。

## 实现依据与验证

本机 OpenSSH 10.3p1 的官方 ssh(1) 说明 destination、-l、-p 与交互 TTY；不读取用户 SSH config。portable-pty 锁定 0.9.0 的 lib.rs/unix.rs 是 Child kill/wait 与 EIO→EOF 的精确源码依据。xterm 官方 API 明确接受原始 Uint8Array，并通过 write callback 表示异步处理完成。

参考：[OpenSSH ssh(1)](https://man.openbsd.org/ssh.1)、[portable-pty 0.9.0 Child](https://docs.rs/portable-pty/0.9.0/portable_pty/trait.Child.html)、[xterm Terminal API](https://xtermjs.org/docs/api/terminal/classes/terminal/)。与 VS Code Remote SSH 的系统 SSH/明确目标模式一致，只实现已批准的交互终端，不实现远程 IDE。

验收沿 Plan06：Node 反例覆盖首输出、setup/cleanup、raw UTF-8、多 tab/隐藏、队列和 close 失败；Rust 后续覆盖忽略 HUP 的原 child、wait/reader、取消/重试和 shutdown 准入；真实 Tauri/远端交互单独取证，未执行项保持未完成。
