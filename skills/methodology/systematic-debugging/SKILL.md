---
name: systematic-debugging
description: 系统化调试方法论——找根因而非治症状
metadata:
  category: methodology
  source: superpowers
  upstream-version: "6.0.3"
  author: obra
  tags: [debugging, troubleshooting, root-cause]
triggers:
  - 调试
  - debug
  - bug
  - flaky
  - 测试失败
  - 报错
  - 异常
  - 为什么失败
allowed-tools: []
hooks:
  PostToolUseFailure:
    - matcher: "*"
      hooks:
        - type: prompt
          prompt: |
            工具执行失败了。不要急着猜原因改代码——先系统化调试：
            1. 重现：确认能稳定复现吗？
            2. 隔离：最小化复现条件（哪个输入/路径/状态触发的？）
            3. 根因：读错误信息、栈、日志，找到真正的失败点
            4. 验证假设：改之前确认你理解了为什么失败
            治症状的修复会回来咬你。
---

# Systematic Debugging

When encountering any bug, test failure, or unexpected behavior, follow this systematic approach BEFORE proposing fixes.

## The Core Rule

**Find root cause, not symptoms.** Never apply a fix until you understand WHY the bug exists. Quick patches that suppress symptoms create more bugs later.

## Process

1. **Reproduce reliably** — can you trigger the bug consistently? If not, make it reproducible first.
2. **Isolate** — narrow down to the minimal reproduction case. What's the smallest input/state that triggers it?
3. **Trace** — follow the data flow from input to failure. Add logging/assertions to pinpoint where reality diverges from expectation.
4. **Hypothesize** — form a theory about the root cause. "If X is the cause, then Y should be true."
5. **Verify the hypothesis** — test your theory. If disproven, form a new hypothesis (don't just try random fixes).
6. **Fix the root cause** — NOT the symptom. And add a regression test.

## Anti-Patterns

- "Let me try changing this and see if it helps" — guessing, not debugging
- "This error message is annoying, let me catch it" — suppressing symptoms
- "Works on my machine" — ignoring environmental factors
- "Probably a race condition" — without evidence
