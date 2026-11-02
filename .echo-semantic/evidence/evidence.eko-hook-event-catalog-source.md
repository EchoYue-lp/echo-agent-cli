---
schema_version: 1
id: evidence.eko-hook-event-catalog-source
kind: evidence
observed_at: source:81b5f0bddffa7213e888bf2f38838ff90b8dcf1592f4bea0aa2be5d4e6b5e6ec
source_refs:
- web-frontend/src/components/hooks/HooksPanel.tsx
- web-frontend/src/components/hooks/HooksPanel.scope.test.tsx
- docs/zh/guides/extension-panel-scope-settlement.md
- web-frontend/src/api/endpoints.ts
- src/tauri/commands/hooks.rs
- docs/zh/adr/0012-extension-control-authority.md
supports:
- rule.eko-framework-consumer-authorities
limitations:
- 仅当前Hook面板读取consumer的源码与具名前端focused，不证明Hook执行或native
- 元数据专属ABA/卸载/重叠请求仅source guard审查，不伪新增动态票
- 不关闭历史Finding、宽Rule或全局资格；不改工具/旧refs/排除
---

# Hook事件目录失败恢复的当前源码依据

## 来源与范围

事件目录只读使用原hooksApi.events与list_hook_events，后端枚举原HookEvent::ALL，不执行配置reload、matcher或Hook脚本。EKO现有HooksPanel保留HookView/current/scope及请求sequence，事件错误与规则列表错误分别呈现；显式重读只重新获取事件元数据，不引入第二存储、权限或执行owner。

## 支持的结论

原列表迟到成功不能抹去事件错误；事件重读失败保原规则列表和错误，成功按真实available选择事件。相同scope的旧请求仍受原view/current与eventsSequence限制。候选原组件RED8pass/3fail自然1、候选GREEN11pass自然0及独审pass各保原输入；正式接收后在真实frontend同11项实际pass11/fail0、自然0，两个roots noEmit诊断0、ESLint warning0/自然0和两文件Prettier自然0。完整caller/raw仅本工作区 `.echo-semantic/local/hook-event-catalog-formal-20261102-r01/` 可用，不以哈希替代原件。此Evidence只给具名consumer的当前依据，不创建新Rule/Map或新Hook语义。

## 已知缺口

本次没有真实App/后端Hook执行、配置reload、用户数据或完整工程验证。首次正常Source自然1“高风险变化没有有效语义依据”原件保留；这份普通Evidence的正式采用和后继Source/final结果另由CLI唯一writer及Root处理，不能把候选正文当门禁成功。原已保存Gov、侧栏和指南资格不续签为此Hook/native通过。无API或示例调用变化，README/examples/官网不含该内部读取状态，不适用。
