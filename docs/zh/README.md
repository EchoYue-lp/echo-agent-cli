# Eclip 文档（中文主源）

**Eclip /ˈɛk.lɪp/**，读作 **EK-lip**，两个音节。**听见你的声音，让想法飞跃。**

品牌延续 Echo（回声、共鸣）的初衷，图标与用法见 [品牌说明](../../design/branding/README.md)。

本目录是 Eclip 长期文档的中文主维护来源。按教程、操作、参考、架构、项目状态与ADR组织；新增和修改页面不要求创建英语副本。既有英语页只按其明确登记的翻译审阅范围维护，不自动代表中文后继已同步。

所有正式中文页面必须登记在[`doc-parity-manifest.json`](../doc-parity-manifest.json)中。工程计划另行登记，不与正式产品状态页面混用。发布或官网来源检查前，从EKO仓库根执行：

```sh
node scripts/check-docs-parity.mjs
node --test scripts/check-docs-parity.test.mjs
```

框架公共能力请阅读同级 `echo-agent` 仓库的正式文档；跨语言 SDK、协议合同和 Host 适配器请阅读独立的
[echo-agent-sdk](https://github.com/EchoYue-lp/echo-agent-sdk) 仓库。本目录只记录 Eclip 应用策略和组合方式。

## 当前入口退役同步

独立TUI／REPL／CLI/JSONL产品退出，GUI主入口与IM／cron、GUI内置终端、Agent shell及共享
Task/Subagent／HITL／附件／记忆／恢复继续保留。包名、identifier、数据路径和固定恢复refs不改。
本轮代码与中文文稿由各唯一writer合流；文稿同步不代表删后构建/锁/SDK正式Git/native/发布通过。

## 当前新增文档导航

- [原生消息与进度](adr/0057-native-messages-and-progress-policy.md)
- [IM原生输出投影](adr/0058-im-native-output-projection.md)
- [Subagent原生观察](adr/0060-subagent-native-observations.md)
- [工具分类与必需策略](adr/0061-tool-categories-and-required-tools.md)
- [文件存储性能观测](architecture/performance-baseline.md)
- [扩展面板回执归属](guides/extension-panel-scope-settlement.md)
- [HITL回放身份](guides/hitl-replay-identity.md)
- [插件面板回执归属](guides/plugin-panel-scope-settlement.md)
- [有限任务历史](guides/task-run-history.md)
