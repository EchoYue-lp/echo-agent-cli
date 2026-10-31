---
schema_version: 1
id: evidence.eko-plugin-committed-degraded-gui
kind: evidence
observed_at: source:5ba3e3307602d3ab6a16a56e25984a476d43d397a684e8768429baf67c39bae0
source_refs:
- echo-agent-app-core/src/extension_commands.rs
- echo-agent-app-core/src/extension_control/service.rs
- echo-agent-app-core/src/plugin_runtime/runtime.rs
- web-frontend/src/api/endpoints.ts
- web-frontend/src/api/endpoints.extensions.test.ts
- web-frontend/src/components/plugins/PluginPanel.tsx
- web-frontend/src/components/plugins/PluginPanel.test.tsx
- web-frontend/src/components/plugins/PluginPanel.scope.test.tsx
- docs/zh/architecture/runtime.md
supports:
- rule.eko-framework-consumer-authorities
limitations:
- 仅当前冻结497输入的前端工程与fixture验证，不是新App、真实插件操作或native验收
- 原080完整Rust门禁与性能证据保留原切点，不作为新前端或新App测量
- 不关闭原Map其它unknown、needs_review或全仓验收缺口
---

# 插件已提交后降级的GUI呈现证据

## 来源与范围

本次从正式 CLI `08047344d1bc1b29aed3a5d309618193bb92c91a` 的干净切点开始，仅五个前端文件和必要的中文runtime、原Map及本Evidence发生变化。当前源码观察由未修改的官方 `verify_semantic.source_digest` 计算，见上方 observed_at；497个Git跟踪前端输入在本次最终验证前后逐项SHA、字节数与mode相同。

原PluginRuntime、extension service与dispatch继续拥有提交、发布、结算和回滚。已有 `plugins/mutation` 成功分支提供可确认的提交事实；`degraded` 仍保留 `success=false`、`wiring_ok=false`、诊断、省略数与目标收据。GUI只读该typed事实，刷新真实目录/配置，并明确提示已提交但部分组件未启用或存在诊断。真正failed/null、validation-only、裸degraded或不一致状态不冒提交。原view/generation/listSequence及迟到应答守卫不变，没有新增wire字段、存储或状态机。新增纯判断函数为TS模块export，预检如实标记publicApi/high；它不改变后端或SDK公开shape。

Map原观察和其它场景不变；新增场景沿原已存在的生产路径导航。本Evidence以上方当前Source摘要单独绑定全部五份前端源和测试，包括历史Map观察时尚不存在的Panel测试，不把新文件反填为旧revision事实。

## 支持的结论

原件统一保存在本工作区 `.echo-semantic/local/final-plugin-committed-degraded-r01/formal-execution/`，仅当前工作区可用。普通Evidence摘要不代替原raw，不把local路径当正式源码引用。

| 验证对象 | 当前Source实际结果 | 原件入口 |
| --- | --- | --- |
| API、Panel和scope三文件 | 31pass／0fail，自然0；保原degraded success=false，覆盖typed提交、rollback不刷、配置服务端重载、当前view刷新与换generation迟到不污染 | `formal-D5-focused-r02-caller.json`及原log |
| 全src格式 | 原Prettier检查自然0，零diff | `formal-D5-full-src-prettier-r02-caller.json` |
| 全前端lint | 原npm lint自然0，零warning | `formal-D5-full-lint-r02-caller.json` |
| 全前端unit | 901pass／4原skip，93文件pass／1原skip，自然0；Vitest固定参数maxWorkers=2仅为测试进程并发 | `formal-D5-full-unit-r02-caller.json`及原log |
| 默认build及类型 | 原 `npm run build` 的tsc-b与默认Vite均自然0，零构建warning | `formal-D5-default-build-r02-caller.json`及原log |
| 差异格式 | 原git diff检查自然0 | `formal-D5-diff-check-r02-caller.json` |
| 源码与风险身份 | 497输入前后无漂，原consumer_digest采用前后各两读均为74411914…54bf74；lock399509…与policyc1993e…精同 | `frontend-after-and-consumer-two-reads.json`、`before-formal-apply.json` |

首轮Source058b的31项focused与901项unit也曾自然0，但随后的tsc-b因三处新测试使用ByRoleOptions不支持的exact:true自然退出2，未进入Vite。原件保留；后继仅将三查询改为等价锚定名称RegExp，不改生产或产品/测试断言。上表全部来自这一修正后的当前Source，未将首轮901移签。

第一次Source检查的六项新材料错误保原：Evidence缺合同标题、两处新测试被错误反填到Map旧0d1历史、TS模块export漏报publicApi。后继补齐标题，将新测试仅绑定当前Evidence，Map原历史观察不刷新，并按原风险单调升级信号，Source正常重验通过。前置可选旧ADR设计绑定与跨根search参数的两次工程预检失败也保原；本次不改架构，旧ADR作为普通不可变依据，跨仓调查不登记为本仓允许路径，风险high和验证范围未降低。

## 已知缺口

这里只验证当前GUI投影和fixture，未执行新App构建/启动、真实插件安装或native/provider/微信/cron/SSH操作。原080完整Rust门禁与性能NoRun/B-source证据继续归原切点，不作为当前前端或新App测量；本次未重复无改Rust矩阵或风险scanner。原consumer744、锁和policy均不变，不新增风险接受，也不向Framework/SDK继承。

中文runtime与既有Map已增加对应事实。没有新示例调用、官网公共入口或配置承诺，examples无需改动；官网sole只按Root新CLI提交后的runtime必要差异正常同步，不以dirty源宣称source-aware通过。本任务不关闭原Map其它unknown/needs_review或全仓缺口，Root统一stage/commit；真实用户授权、配置和历史未被操作。
