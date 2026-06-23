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
            收到任务后，你必须先检查是否有技能适用:
            - 创造性工作(新功能/设计) → brainstorming
            - 修 bug/测试失败 → systematic-debugging
            - 写代码 → test-driven-development
            - 多步任务 → writing-plans
            - 声称完成前 → verification-before-completion
            有 1% 可能适用就要检查。
  UserPromptSubmit:
    - matcher: "*"
      hooks:
        - type: prompt
          prompt: "本轮用户消息，检查上面列出的技能是否有适用的。"
---

# Using Skills

## The Rule

**Invoke relevant or requested skills BEFORE any response or action.** Even a 1% chance a skill might apply means you should invoke it to check. If an invoked skill turns out to be wrong for the situation, you don't need to use it.

## Red Flags

These thoughts mean STOP — you're rationalizing:

| Thought | Reality |
|---------|---------|
| "This is just a simple question" | Questions are tasks. Check for skills. |
| "I need more context first" | Skill check comes BEFORE clarifying questions. |
| "Let me explore the codebase first" | Skills tell you HOW to explore. Check first. |
| "I remember this skill" | Skills evolve. Read current version. |
| "The skill is overkill" | Simple things become complex. Use it. |

## Skill Priority

1. **Process skills first** (brainstorming, systematic-debugging) — these determine HOW to approach the task
2. **Implementation skills second** (test-driven-development, writing-plans) — these guide execution
