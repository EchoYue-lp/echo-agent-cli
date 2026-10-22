---
schema_version: 1
id: baseline.repository
kind: baseline
source_snapshot:
  base_revision: f57b90a2b0962fa5561f8839a3df70be1f261ffa
  content_digest: a316119d3d822166fd260b6806ad05b2bf2762f349d47185e64a2bea4f964241
inventory_closure: open
behavior_model_closure: open
map_refs: [map.eko-conversation-collaboration, map.eko-framework-consumer-settlement]
regions:
  - { path: .agents, status: supporting }
  - { path: .cargo, status: supporting }
  - { path: .env.example, status: supporting }
  - { path: .github, status: supporting }
  - { path: .gitignore, status: supporting }
  - { path: AGENTS.md, status: supporting }
  - { path: CHANGELOG.md, status: supporting }
  - { path: Cargo.lock, status: supporting }
  - { path: Cargo.toml, status: supporting }
  - { path: README.md, status: supporting }
  - { path: build.rs, status: supporting }
  - { path: capabilities, status: supporting }
  - { path: config, status: supporting }
  - { path: design, status: supporting }
  - { path: docs, status: supporting }
  - { path: echo-agent-app-core, status: in_scope }
  - { path: examples, status: supporting }
  - { path: init.sh, status: supporting }
  - { path: rust-toolchain.toml, status: supporting }
  - { path: scripts, status: supporting }
  - { path: src, status: in_scope }
  - { path: src-tauri, status: in_scope }
  - { path: tauri.conf.json, status: supporting }
  - { path: tests, status: supporting }
  - { path: web-frontend, status: in_scope }
boundaries:
  - id: boundary.eko-conversation-collaboration
    map_ref: map.eko-conversation-collaboration
    risk: high
  - id: boundary.eko-framework-consumer-settlement
    map_ref: map.eko-framework-consumer-settlement
    risk: high
coverage: []
---

# EKO application semantic baseline

2026-10-17 primary checkpoint仅刷新当前非语义源码摘要，base保已公开main祖先f57；原c874源码482a摘要已由官方source_digest_at_revision核同。GUI/channel两独立red→after/4控制共6pass、具名cron1pass与17file独审/Clippy零warning均属有限证据；完整workspace/GUI/精main CI未完，原needs_review/stale/库存与行为open不关闭。三依赖为人的精确risk_accepted，不冒修复或原audit0。

## 源码快照

32c6719完整83路径审查新增的shared preparation Important经真实journal重开/shared入口反例与same AFTER2pass关闭源码遗漏。原普通control/四surface参数边界见共用Evidence；原Clippy仍属此前切点，当前完整矩阵继续待验。消费者输入先拒绝后经明确准入及独审无消费扩大重评精确重绑，未改锁或三项风险；本次仅刷新实际源码身份，不关闭宽状态或历史引用收据缺口。

dfd5750之后的窄交付增量：精确三依赖risk gate经6测试方法及独审通过，真实锁/新origin在线扫描在单进程HTTP/1.1设置下各scanner1/gate0；原默认fetch失败保留。只刷新当前源码摘要及受影响Rule/Map/Evidence引用，宽B场景、stale audits、库存/行为open与完整workspace/GUI/main CI义务不变。

基线绑定 `echo-agent-cli` 可恢复的开发提交 revision 与当前非语义源码摘要。设计和 Plan 已在当前工作树中，后续业务差异必须通过 semantic-diff 刷新摘要和受影响对象。

## 仓库区域

所有当前顶层 Git 路径均唯一分类。应用核心、Rust surface、Tauri 壳和前端属于生产范围，其余文档、配置、测试与构建材料作为 supporting consumer。

## 能力图与边界

已建立 EKO Conversation collaboration 与最新 framework consumer settlement 两个边界。Side Conversation 与 framework consumer 的既有对象绑定可恢复的 main revision，本次 GUI Fork 及受影响 Map/Rule 绑定当前候选源码。后者覆盖 Background Review caller-owned 生命周期及最新框架下 Skill/Plugin、Managed conversation、workspace 与 Scheduler 的应用适配。

## 覆盖网格

库存与行为模型保持 open，因此不宣称全应用八视角覆盖已经完成；本次高风险任务由新增 capability map、Behavior、Rule、Evidence 与后续独立复审闭合。

## 未知与缺口

既有 Side Conversation 与 framework consumer 的验证范围保留在历史 Evidence 中，不以本次源码摘要冒充重新验收。本次 GUI Fork 由新的 Behavior/Evidence 记录按钮、完整回合快照、独立列表条目与迟到选择保护；本地候选门禁不等于远端合并或原生 Tauri 窗口验收。

## 闭合结论

当前基线结构与路径分类可作为后续增量依据；GUI Fork 的当前候选单独绑定测试证据，全仓其它能力的库存闭合与行为模型闭合仍为 open。

## Cancellation observation 开发 checkpoint

当前任务以可恢复a88d247为前置，仅刷新实际非语义源码摘要。原owner typed observation、durable fence与聚合保留的三个场景仍为needs_review、三个lens仍stale；有限源码独审与两条focused测试不等于宽行为Finding/最终消费者闭合。当前构建借public f0a中间framework切点；TUI/GUI条件、其余强例、最终F main和全量门禁仍待。库存与行为模型仍open，不以本checkpoint收缩或删除既有义务。


## Scheduler 增量开发 checkpoint

前置提交为5cd9eff0，当前相对依赖是公开开发3ded，不冒最终F main。原Scheduler同slot/Root生命周期两逻辑case各真实before行为fail→after1pass/零warning；独审只该窄scope。原215699时代的三个Behavior/三个stale Audit仅恢复为等价Git5cd9历史锚，状态和结论不变。原scheduler场景新增B影响标needs_review；其它所有宽义务/剩余强例/GUI/全量门禁/最终消费者继续未闭。此处只刷新实际非语义源码摘要与真实前置，不宣全项目行为闭合。


## Historical pre-approval Framework356 managed import candidate (2026-10-20)

历史批准前记录（截至 2026-10-20，新增三项当时尚未获批；当前接受状态见后节 2026-10-21 exact-six checkpoint）：当时的候选仍以公开main祖先f57为base，刷新实际非语义源码摘要。CLI现有ordinary import补足两个无derived绑定游标，删除应用合成base System，改按checkpoint非System消息计数；框架resume仍唯一拥有canonical rendered root。双语ADR0045、独立窄审C0与原6case真实3fail→same AFTER6pass/零warning/无漂移只支持此局部修复。原3稳定依赖继续精确risk_accepted，有限消费复评重绑e2d5；新actual/origin online扫描都scanner1/riskgate1，新增zbus_names4.3.4、zvariant5.13.1、zvariant_derive5.14.0三个未接受停止维护告警。不能以旧scanner1/gate0、0vuln或本次6pass冒完整交付。宽needs_review/stale/库存与行为open、framework356完整Rust/GUI、CLI精mainCI及受保护插件/nativeStop均不关闭；当前未stage/commit/push。


## Focused exact-six dependency and consumer milestone

2026-10-21 仅刷新本候选实际非语义源码摘要，base仍公开main祖先f57。新增人的精三项使当前policy精六项，old3冻结及事件期限保持；WebPKI0382精两值lockpatch、现有fixed-six gate与6unit、两officialscanner1/separate riskgate0均在原Evidence/Decision/Finding有界记录。e46要求managed import与factory两Rust既有修复同提交，否则parent0d1四政策路径投影精consumer不匹配。原旧锁full8/25selected仅属历史；新锁d2e7/F7ded原managed6、modelmodule24、exactpool1三组实际31具名0fail/0ignore/零warning并有限独审C0，原17是25子集不另加。该focused资格不冒patched-lock full8与最终main/nativeHook，现有needs_review/stale/库存与行为open不变。


## Final accepted-framework consumer qualification

2026-10-22，本消费者在接受的framework main f7bac与patched CLI lock d2e7上完成原八项工程命令、included factory格式及原风险三项矩阵：workspace1978pass/15既有ignore、GUI226pass/3既有ignore，分配置不相加；零编译warning/源码漂移，原17失败具名PASS。fresh官方DB4b587两scanner自然1、零漏洞/精六告警、独立riskgate0，原三版本继续冻结；详见共用Evidence末节。此前开发/focused段为历史切点，当前摘要只绑定实际文稿收束后的源；相同编译/测试/工具输入经投影承接本次成绩，不宣称再次执行。精确合并mainCI/cold属于交付条件；inventory与behavior_model仍open，Rule/Audit原状态不扩大，取消的plugin消费者专项不再列前置。
