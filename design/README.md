# EKO Design

`design/` 保存仍在使用的设计资产和未完成规格，不是项目使用文档。

- `branding/`：品牌标志和视觉资产。
- `specs/`：尚未完成、仍驱动代码变更的规格。

当前活跃规格：

- [runtime reliability](./specs/runtime-reliability.md)
- [surface parity cleanup](./specs/surface-parity.md)

规格完成后，先把稳定事实合并到 `docs/` 或代码注释，再删除规格。阶段 review diff、
task brief、soak 日志和临时 agent progress 不进入仓库项目文档。
