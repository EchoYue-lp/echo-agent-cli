---
schema_version: 1
id: evidence.eko-ssh-source-checkpoint
kind: evidence
observed_at: source:688b4e517f8a99abb3d08d6b7547d03bc9ef84216f708054829d385f788b27e5
source_refs:
- echo-agent-app-core/src/terminal.rs
- echo-agent-app-core/src/runtime.rs
- echo-agent-app-core/src/developer_commands.rs
- src/tauri/terminal.rs
- src/cli/channels.rs
- src/cli/channels/outbound.rs
- src/cli/cmd_impls/developer.rs
- src/tui/events.rs
- web-frontend/src/api/endpoints.ts
- web-frontend/src/components/terminal/Terminal.tsx
- web-frontend/src/components/terminal/TerminalDrawer.tsx
- web-frontend/src/components/terminal/terminal-io.ts
- web-frontend/src/api/endpoints.terminal.test.ts
- web-frontend/src/components/terminal/TerminalDrawer.test.tsx
- web-frontend/src/components/terminal/terminal-io.test.ts
- docs/en/adr/0051-ssh-terminal-sessions.md
- docs/zh/adr/0051-ssh-terminal-sessions.md
- .cargo/accepted-dependency-risks.json
- docs/supreme/specs/cancellation-observation-consumer/dependency-risk-decision.md
- scripts/check_cli_dependency_risks.py
- scripts/test_cli_dependency_risks.py
supports: [asset.eko-interactive-terminal, rule.eko-framework-consumer-authorities]
limitations:
- 原25个Rust cases只属于ea4/2b/e4e，不冒充本轮710的Rust或Clippy执行。
- Node仅在425输入字节不变的范围复用，浏览器构建不证明Tauri原生运行。
- 风险扫描资格不等于漏洞已修复或已接受路径不可达。
- SSH完整Rust/GUI、原生/真实远端、其它平台、main/CI/release及Plan06整体尚未资格。
---

# SSH terminal source checkpoint evidence

## 支持的结论

这份 Evidence 只绑定原交互终端 owner 的来源、消费者和本地源码阶段资格。源码依据与两份 ADR0051 支持 asset.eko-interactive-terminal 中记录的 target/admission/physical-settlement 与观察边界；它不定义新状态权威，不把前端 TerminalViewState 变为后端事实，也不改变既有宽 Rule 或其 needs_review 状态。

SSH 原22路径 manifest ea4c118ac05e284e4cb0e2314097840c549f792450751762a64fd2f5b7562e54 在受保护 fast-forward 后逐字节不变；相对4ab0de9fcc071bc9b390732737f9cfc09176c4f9 的 manifest 为3f6f6edc721e580d9c81e3a729fa8c5ec3c3f0073fac6cfe6612205627f57203。294 Rust/build inputs 的实际 consumer 为710a34bcb0367b7d6af239df9afa75925a2bd222f0a992253d290fd02f0b4f1f；与4ab的8个Rust输入差分包含7个生产增量与1个既有fixture字段增量，不是纯测试豁免。paired framework 保持 e4e50396876c5e7e85b52cdc3dbb510bab9c4f4d。

## 来源与范围

source_refs 都指向当前可读取的正式源码、测试、ADR或精六项决策，不引用 ignored raw 充当源码。下面本机原件仅在当前任务工作树可用；摘要不替代它们。

原 focused Rust 原件在 .echo-semantic/local/eko-ssh-remote-terminal/：4条具名原入口真实选择14/4/6/1，共25 distinct cases，0fail/0ignored/0warning，filtered分别1733/1743/209/127。它们覆盖argv/目标、默认本地控制、原child wait/reader join、取消/错误重试/准入、已有流适配和首输出，只属于 ea4/2b/e4e。最初有deprecated warning和0selected的尝试各自保留、不计入行为资格。

原 Node 61files/314tests、含lint的npm test、Prettier和browser Vite build的最新出口均0；425个 tracked+新增 frontend inputs 由2b基线421及SSH4个新增文件重建并逐字节匹配，Rust字面量基线adoption和本轮治理/风险 metadata 未改前端。61/314不能替换为Plan04的58/295，也不称新鲜运行或native制品。旧npm失败出口1在run内保留，后续共享log只剩成功原文，不声称失败stdout仍完整。

当前710a精六项资格原件在 .echo-semantic/local/eko-ssh-source-adoption/：暴露独审原件 SHA413f64d7ea92a0abf3cfb03d77421cde4ac2dc941d6f9a08c24ce8f1f3ce7dd6 为pass0；正式 metadata 独审为pass0。原六 policy methods 出口0。2026-10-24T15:18:41Z–15:18:53Z 两个新 actual/canonical-origin 原checker各自保留 scanner1、原六warnings、0vuln、gate/caller0、noFetch=false，官方 DB21d909e4c8bd4e14c7aea58bf980c881a0053ab1/tree e48a5ffdb569c69337911adf13262a0058ebccc5、1352advisories/839packages。独立副本1371tracked=physical且逐Gitblob/mode匹配、extra0、前后Git无差异，FETCH_HEAD同期更新；相同输出hash由同锁/DB产生，不借旧352结果。默认5unknown原件、HOME/全局Git、accepted6、lockcca90和helpers/scanner未改。docs parity本轮62pairs/52ADRs，出口0；diff whitespace检查0。

第一次正常task semantic出口1的5项原错误保留在 semantic-final-710a.log：机器因TerminalViewState interface/type import要求newStateAuthority风险标记，四个终端路径缺正式语义关联。工具合同的signals与boundaryDecision分开；保守声明该风险触及，仍复用原 TerminalService/Session，createsNew=false。只补既有owner的Asset/Evidence路径和真实证据，不更改检测器或扩大运行时功能。

## 已知缺口

正常artifact/task semantic的最终结果、后续local checkpoint身份另存任务回执；该结构证据本身不冒充门禁通过。原4ab/352的八项完整矩阵和Clippy101历史不会重标为SSH710全绿。本轮没有Rust/Clippy、native或真实远端执行；没有新制品因而不清理其它target。原SSH批次准确cargo clean已0，target不存在。

真实认证不可用或任一窗口/主题/resize/Ctrl-C/多会话/断线/退出场景未实测时保持未验；进程启动和本地wait不证明登录或远端副作用停止。Linux/BSD GTK/glib及notification/zbus等六项接受的残余风险继续存在。Plan06保持ready，main/CI、发布和网站镜像只由协调者按精确已资格来源分别推进。
