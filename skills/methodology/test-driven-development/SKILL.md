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
