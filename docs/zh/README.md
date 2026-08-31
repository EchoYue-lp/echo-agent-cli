# EKO 文档（中文主源）

本目录是 EKO 长期文档的中文编辑主源。页面按教程、操作、参考、架构、项目状态和 ADR
分类，并与 `docs/en/` 使用完全相同的相对路径。

所有长期文档已迁移完成。旧根目录路径由
[`doc-parity-manifest.json`](../doc-parity-manifest.json) 保留删除记录，发布前必须运行：

```text
node ../../scripts/check-docs-parity.mjs
```

框架公共能力请阅读同级 `echo-agent` 仓库的正式文档；本目录只记录 EKO 应用策略和组合方式。
