---
schema_version: 1
id: evidence.eko-temperature-workspace-controlled-validation
kind: evidence
observed_at: source:42a1dd0dd843ee8629ff12dc20d238dc9356c7b35d29e4bd3b353c0b72d53f42
source_refs:
- echo-agent-app-core/src/config.rs
- echo-agent-app-core/src/model_config.rs
- echo-agent-app-core/src/profiles/types.rs
- echo-agent-app-core/src/infra/factory.rs
- echo-agent-app-core/src/infra/llm.rs
- echo-agent-app-core/src/infra/diagnostics.rs
- echo-agent-app-core/src/infra/tests.rs
- echo-agent-app-core/src/reflection.rs
- echo-agent-app-core/src/state/tests.rs
- echo-agent-app-core/src/types/request.rs
- echo-agent-app-core/src/types/response.rs
- echo-agent-app-core/src/workspace/registry.rs
- src/tauri/commands/config.rs
- src/tauri/commands/providers.rs
- web-frontend/src/components/providers/ProviderPanel.tsx
- web-frontend/src/components/providers/ProviderPanel.test.tsx
- web-frontend/src/components/workspace/ProjectSourceFields.tsx
- web-frontend/src/components/workspace/NewTaskDialog.tsx
- web-frontend/src/components/layout/LeftSidebar.tsx
- web-frontend/src/stores/uiStore.ts
- docs/zh/adr/0029-framework-native-agent-and-model-config.md
- docs/zh/adr/0055-project-workspaces.md
- config/eko.example.yaml
- docs/en/configuration.md
- docs/zh/configuration.md
- echo-agent-app-core/src/agent_pool/tests.rs
- web-frontend/src/App.navigation.test.tsx
- web-frontend/src/api/endpoints.ts
- web-frontend/src/components/chat/ChatInput.submission.test.tsx
- web-frontend/src/components/chat/ChatInput.thinking.test.tsx
- web-frontend/src/generated/ConfiguredModel.ts
- web-frontend/src/generated/ModelConfigResponse.ts
- web-frontend/src/generated/ModelUpdate.ts
- web-frontend/src/types/api.ts
- web-frontend/src/components/workspace/ProjectSourceFields.test.tsx
- web-frontend/src/components/workspace/WorkspaceDialogs.a11y.test.tsx
- web-frontend/src/components/layout/LeftSidebar.test.tsx
- web-frontend/src/stores/uiStore.test.ts
supports:
- rule.eko-framework-consumer-authorities
- rule.eko-project-location-consistency
limitations:
- 仅当前受控app-core回环和Registry真实临时文件系统、具名前端测试，不是GUI/App或外部provider验收
- 42a1/2028/d542当前fresh RustSec fetch失败，正式双风险gate0仍缺；语义Source/Final不能替代该资格
- 原旧App完整beforeHTTP重放未执行，旧夹具dialect失败不是temperature产品RED或403根因
- 本Evidence不关闭历史Finding、不创建Rule/Map、不扩大原五风险或未来来源接受
---

# EKO 取消temperature与Workspace修复的受控验证

> **历史归档（2026-11-03 至 11-05，2026-11-09 整理）**：以下“当前”仅指原 `42a1…` consumer／`d542…` 锁及具名来源。原 `observed_at`、fetch 失败、受控通过和未知项保留，不续签后继 `5541…`、新锁、完整工程、GUI、手机或 main CI；当前整合资格另取实际验证。

## 来源与范围

用户要求删除EKO的temperature产品参数。应用配置、Profile、DTO、TS合同、设置表单及主/只读/写入Subagent装配已删除对应字段或传递链；框架API边界显式使用None，原thinking、输出/context限制、认证、协议和模型代际owner保持。原共享GUI连接使用同一个已准备client，doctor不再请求固定0；本Evidence不把通用框架的显式temperature API退役。Framework当前来源为91797fed1462ad9668956ddcf038998bc8353a84，同原788b包2857文件真实字节，不能只读旧c623 HEAD冒充当前实现。

Workspace沿原Registry.base_dir处理显式目录名的空parent；未自定义的默认构造器保持None路径，显式原字符串、碰撞/回滚和位置权威继续由原Registry负责。SSH入口从原Sidebar/uiStore intent携带workspace id及generation进入原重连对话框，继续原prepare/verify/backend expected_generation及后继导航围栏，不新增reconnector或第二地址权威。Workspace r02前端5suite90P、温度前端4file40P及各自RED/类型检查原件分别保留，不能把mock界面测试当作真实SSH或HTTP通过。

observed_at记录本次实际原collector42a1的274个Rust/build输入范围；其他UI/文档精SHA由各独审切点及最终联合清单另行绑定，不因该Rust摘要自动续签任何界面或文档字节。

## 支持的结论

Workspace原Registry实现加同两测试取得真实临时文件系统行为RED101/control0，fixed实现取得两项GREEN0。r04在同目标上编译当前6e测试切点，实际NoRun0/W0/E0、features=[]、test opt0/debug0，Registry两项各actual list1/run1P0。新T外不可变程序SHA256为49b3fe556d48773e9b890b183e206f2269f771052078409a9e92558d6acb76cd，499177616字节、0755；原CargoJSON、FP完整u64/.d共4413件已保全核验。1758项Rust/Toml当前输入与正式组合精同，复用没有再冷编或修改源。

同一49b3程序上，CLI新执行号逐9项each exact-list1后，各exact1P/0F/0I、自然0。第1项通过本机TcpListener和原真实client/Responses编码器，调用共享test_runtime_llm_connection与doctor各一次POST；实际完成两body均无temperature、精deepseek-flash模型及input存在、doctor max_output_tokens=1的强断言。15/15/5秒预算、shutdown及完整server await保持，未导出原HTTP body时不补造请求日志。其余8项实际通过配置/Profile/view序列化移除字段且max/context保留、主及继承reviewer温度None、writer/readonly None，以及原hot switch/fixed/loaded/future/provider刷新代际控制。

Workspace同一newcut targeted Clippy自然0/W0/E0；既有delete保留代码回归另以list1/run1P0记录，既不并入Registry2也不并入EKO9。最终Workspace交件6f7cefccf510700328888550133bee6bfa85294911b63d449a62514cc63914fb保留这些原caller/raw及3945owned源守卫。其ADR0055最后事实段改为产品数据/项目位置分别记录但路径可物理重合，精SHA为d9143177e2660ba786cd3dfc4035874196eeccdb4ca4db7d114e632075ea2910；这份MD后继不触发重编，不把metadata root推作自定义目录parent。

原82ae程序首项实际list1后101是fixture误用未准入deepseek-v4-flash，编码前拒绝，不当temperature行为RED。另一个独立诊断号用同82ae逐原2..9取得8P，不改原1701余8未跑历史，也不当新49b3结果。Root2482具名审查后仅将fixture三处模型字面量改为原准入deepseek-flash，完整fixture await先保存结果，原result/doctor/fixture处理顺序保首因；协议、provider准入、max、断言数量和预算不变。

唯一9P原件入口为本仓ignored `.echo-semantic/local/eko-temperature-tests-continuation-20261103-r01/r04-new-program-nine-selectors-r01/新49b3程序九项真实验证9P交付入口.json`，SHA256 20cdd0ed862c3a0f4641a08d0e919aa773f7c932a1675a899858d042e7f206bf；绑定18组原list/run caller、stdout、stderr和raw。Workspace真实Registry/NoRun/原FP环境交件为actual-r04的61fd4230a40a9982a60be766a6e2eddf9cb19d25983941ca56b41fef7d6b1599入口。两执行主体和Node/源码资格分别记录，不伪造统一原owner事件。

## 已知缺口

本切点原controls8/8自然0、正式collector实际两读42a1/274，policy仅consumer字段变为2028fdc16ebc07c0d29ae6aa9c5a6902eed61b03360760d4a14a980aa0a751be，锁d542/768包和原五项完整风险身份不变。Root新42 actual scanner/caller约21.35秒后原URL TCP fetch1、空audit，canonical尚无本切点gate0。旧66失败、候选与历史gate、原五风险都保持；不重扫网络、不加ignore/no-fetch/代理、不把受控运行或Source/Final称为风险0。

本证据不签真实外部API、403原因、真实SSH、PTY、GUI/App、UserRoot、凭据、启动或发布，不证明全部provider参数支持。新包候选构建、完整工程矩阵、主线CI及全局Goal仍独立。Workspace新cut Clippy及最终ADR0055事实尾段采用其作者原件；最终联合Source/Final按全部实际当前bytes另取，globalClosure不冒称已查。无公共Framework/SDK API或示例调用新增，配置代码示例已同步，官网通用helper说明由Framework作者维护。
