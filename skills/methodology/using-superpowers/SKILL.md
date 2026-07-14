---
name: using-superpowers
description: 使用超能力技能系统的元技能——确保在任何任务前检查适用技能
metadata:
  category: methodology
  source: superpowers
  upstream-version: "6.0.3"
  author: obra
  tags: [meta, skills, workflow]
triggers:
  - 超能力
  - superpower
  - 技能
  - 有什么技能可用
allowed-tools: []
hooks:
  SessionStart:
    - matcher: "startup|clear|resume"
      hooks:
        - type: prompt
          prompt: |
            收到任务后，快速检查是否有直接适用或用户点名的技能:
            - 创造性工作(新功能/设计) → brainstorming
            - 修 bug/测试失败 → systematic-debugging
            - 写代码 → test-driven-development
            - 多步任务 → writing-plans
            - 声称完成前 → verification-before-completion
            只激活能实质改变执行方式的最小技能集合，不要为了流程阻塞简单任务。
  UserPromptSubmit:
    - matcher: "*"
      hooks:
        - type: prompt
          prompt: "检查是否有用户点名或明显适用的技能；没有则直接处理，不要强行套流程。"
---

# Using Skills

## The Rule

Use a skill when the user names it or when its workflow materially improves correctness, safety, or artifact quality. Read the current skill before acting. Do not activate skills based on a remote possibility or let a generic process skill override explicit user/repository instructions.

For a simple request, the correct skill decision may be “none.” For a mixed task, choose the smallest set that covers the work and apply process skills before artifact-specific skills.

## Skill Priority

1. **Explicitly requested skills** — user intent takes priority
2. **Process skills** — only when they change the method (debugging, planning, verification)
3. **Artifact/domain skills** — for the actual file type or professional workflow
