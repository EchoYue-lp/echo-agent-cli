---
name: verification-before-completion
description: 声称完成前先验证——证据先于断言
metadata:
  category: methodology
  source: superpowers
  upstream-version: "6.0.3"
  author: obra
  tags: [verification, quality, testing]
triggers:
  - 验证
  - verify
  - 检查
  - 确认完成
  - 测试通过
  - 修好了
allowed-tools: []
hooks:
  Stop:
    - matcher: "*"
      hooks:
        - type: prompt
          prompt: |
            声称完成前，确认你已经验证了结果：
            - 编译通过？跑过测试？
            - 改动是否破坏了其他地方？
            - 声明"完成"必须有证据，不是凭感觉。
            如果只是"应该可以了"，先验证再说。
---

# Verification Before Completion

When about to claim work is complete, fixed, or passing, ALWAYS run verification commands and confirm output before making any success claims.

## The Rule

**Evidence before assertions.** Never say "done" or "fixed" or "passing" without having run the actual verification. Your memory of the last run is not evidence.

## Process

1. **Identify the verification command** — what exact command proves the work is done? (`cargo test`, `npm test`, etc.)
2. **Run it** — in the actual environment, with actual data
3. **Read the output** — don't assume. Look at the test results, exit codes, error messages
4. **Report honestly** — if tests fail, say so with the output. If something was skipped, say that.
5. **Only then claim completion** — with the evidence in hand

## Anti-Patterns

- "I fixed it, should be working now" — without running tests
- "The tests passed last time" — stale verification
- "I'm pretty sure this works" — belief is not verification
