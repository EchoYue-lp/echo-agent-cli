# Eclip 文档入口与完整性检查

正式产品文档以[中文主源](./zh/README.md)为当前维护入口。按仓库AGENTS，新增和修改的正式说明使用中文，不为形式补英语副本。既有`docs/en`保留其已登记的翻译历史；`reviewed`只说明对应翻译的审阅资格，不自动把历史英文更新成当前产品事实。

## 文档登记与检查

`doc-parity-manifest.json`的schemaVersion=2登记所有正式中文页面。`targetPath`相对`docs/zh`，必须唯一且使用已知kind；ADR文件名与标题编号必须一致；已提供的翻译状态/替代关系仍须与中文一致。每个实际中文Markdown必须登记，每个登记页面必须存在；选择`--scope`仍执行全清单覆盖校验。

`translationStatus: reviewed`保留该页面已有英语发布检查：英语文件必须存在、语言和审阅标记合法，ADR编号/状态/替代关系一致。`not_required`表示当前只维护中文，不制造翻译审阅结果；若后来添加英语页，必须真实审阅并改登记，不能把文件存在当作reviewed。

原迁移的`legacyPath`/`legacyRemoved`继续保留历史，不为新增中文页面虚构旧路径。`engineeringEntries`独立登记工程计划/交接/审计/测试记录；例如当前`MASTER-PLAN.md`是本轮集成计划，必须实际存在并具有中文标题/正文，不属于长期产品文档。它可与此前同名旧页面迁移到`zh/project-status.md`的记录并存，不能据工程文件存在恢复旧产品状态页面。根目录新增Markdown也必须明确属于工程记录或既有保留入口，不能静默漏检。

从本仓根执行：

```sh
node scripts/check-docs-parity.mjs
node scripts/check-docs-parity.mjs --scope adr
node --test scripts/check-docs-parity.test.mjs
```

`--scope`只接受登记的文档kind，缺值/未知/重复参数失败，不能得到空范围绿色。`--write-manifest`在已有合法登记上只补新中文页，不删除缺页记录、不续期英语审阅、不虚构legacy来源，也不自动把任意工程材料变产品页；写后仍执行完整检查。它不能与`--scope`混用。

框架正式API文档归同级echo-agent，SDK合同归echo-agent-sdk。本脚本只检查EKO文档归属、覆盖和已有翻译合同，不证明Rust、原生界面、官网来源同步或部署验收。
