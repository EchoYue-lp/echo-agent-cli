---
name: test-driven-development
description: 在编写实现代码之前先写测试——Red/Green/Refactor 循环
metadata:
  category: methodology
  source: superpowers
  upstream-version: "6.0.3"
  author: obra
  tags: [tdd, testing, development]
triggers:
  - TDD
  - 测试驱动
  - 先写测试
  - red green refactor
allowed-tools: []
hooks:
  PreToolUse:
    - matcher: "write_file|edit_file|create_file"
      hooks:
        - type: prompt
          prompt: |
            你即将修改代码文件。先确认：
            - 这个改动的测试写了吗？测试能跑吗？
            - 如果是 bugfix：先写一个能复现 bug 的失败测试。
            - 如果是新功能：先写定义期望行为的测试。
            不是每次都要完美 TDD，但"先写测试"的习惯能省你大量调试时间。
---

# Test-Driven Development

When implementing any feature or bugfix, follow the Red-Green-Refactor cycle BEFORE writing implementation code.

## Process

1. **Red** — Write a failing test that defines the expected behavior
2. **Green** — Write the minimal code to make the test pass
3. **Refactor** — Clean up the code while tests stay green
4. **Repeat** — For each new behavior, start with Red

## Key Principles

- Never write implementation code without a failing test first
- The test IS the specification
- Minimal implementation — only what's needed to pass
- Refactor fearlessly — tests have your back
