# Eclip 文档（中文主源）

**Eclip /ˈɛk.lɪp/**，读作 **EK-lip**，两个音节。**听见你的声音，让想法飞跃。**

品牌延续 Echo（回声、共鸣）的初衷，图标与用法见 [品牌说明](../../design/branding/README.md)。

本目录是 Eclip 长期文档的中文编辑主源。页面按教程、操作、参考、架构、项目状态和 ADR
分类，并与 `docs/en/` 使用完全相同的相对路径。

所有长期文档已迁移完成。旧根目录路径由
[`doc-parity-manifest.json`](../doc-parity-manifest.json) 保留删除记录，发布前必须运行：

```text
node ../../scripts/check-docs-parity.mjs
```

框架公共能力请阅读同级 `echo-agent` 仓库的正式文档；跨语言 SDK、协议合同和 Host 适配器请阅读独立的
[echo-agent-sdk](https://github.com/EchoYue-lp/echo-agent-sdk) 仓库。本目录只记录 Eclip 应用策略和组合方式。

## 当前入口退役同步

独立TUI／REPL／CLI/JSONL产品退出，GUI主入口与IM／cron、GUI内置终端、Agent shell及共享
Task/Subagent／HITL／附件／记忆／恢复继续保留。包名、identifier、数据路径和固定恢复refs不改。
本轮代码与中文文稿由各唯一writer合流；文稿同步不代表删后构建/锁/SDK正式Git/native/发布通过。
