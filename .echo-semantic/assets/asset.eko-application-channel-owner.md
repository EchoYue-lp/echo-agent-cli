---
schema_version: 1
id: asset.eko-application-channel-owner
kind: asset
title: 原EKO应用侧渠道owner
asset_type: state_authority
status: active
risk: high
observed_at: source:513d810144fb2c3348549b4734c9ae9275edd7cfbe1c152419b76db135f871f3
boundary_refs:
- boundary.eko-framework-consumer-settlement
behavior_refs: []
rule_refs:
- rule.eko-framework-consumer-authorities
evidence_refs:
- evidence.eko-channel-helper-relocation
- evidence.eko-console-source-exit-admission
finding_refs:
- finding.eko-channel-helper-relocation
- finding.eko-console-source-exit-admission
candidate_refs: []
code_refs:
- echo-agent-app-core/src/channel_runtime.rs
- echo-agent-app-core/src/channel_runtime/input_pump.rs
- echo-agent-app-core/src/channel_runtime/outbound.rs
- echo-agent-app-core/src/channel_runtime/tool_render.rs
- echo-agent-app-core/src/channel_runtime/management.rs
consumer_refs:
- echo-agent-app-core/src/runtime.rs
- src/tauri/state.rs
- src/tauri/desktop.rs
---

# 原EKO应用侧渠道owner

## 资产身份

本资产是既有应用侧渠道与生命周期权威；本次删除准入没有创建新的执行owner。

## 来源与消费者

原ChannelManager、session/coordinator/pump与text adapters继续由app-core持有。ApplicationServices与Tauri共享同一原Arc；独立产品入口退出没有替换owner、store、queue或executor。当前GUI bootstrap/compose消费同一生命周期，独立bin薄壳已按具名源码切片退出。旧三helper迁移的原Evidence与Finding保留其原cut，没有被改绑为本次54删除。

## 生命周期

原准入、incarnation、运行handle与settlement继续原权威。此处仅更新本次源码接线和实际消费者；删后锁/风险、Cargo、native与手机/SSH继续独立待验，不把source-only准入结算为整个Plan10完成。

## 候选关系

已具名退役的独立终端入口记录在 `asset.eko-console-product-entrypoints`；其保留能力继续指向本资产，没有第二套渠道owner。

## 未知与限制

这里只陈述源码接线与消费者身份；当前源码组合尚无删后Rust、依赖风险或原生运行验收。旧LSP及helper证据保留各自原切点，不能替代后继验证。
