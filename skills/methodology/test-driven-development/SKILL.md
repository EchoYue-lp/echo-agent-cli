---
name: test-driven-development
description: 在编写实现代码之前先写测试——Red/Green/Refactor 循环
metadata:
  category: methodology
  source: superpowers
  upstream-version: 6.0.3
  author: obra
  tags: tdd, testing, development
---

# Test-Driven Development

Use Red-Green-Refactor when behavior can be expressed in an automated test and the test provides useful regression protection. Scale the method to risk and repository conventions.

## Process

1. **Red** — Write a failing test that defines the expected behavior
2. **Green** — Write the minimal code to make the test pass
3. **Refactor** — Clean up the code while tests stay green
4. **Repeat** — For each new behavior, start with Red

## Key Principles

- Prefer a failing regression test first for bugs and contract changes; when infrastructure or exploratory work makes that impractical, identify the validation before editing and add coverage as soon as the behavior is stable
- Tests are executable evidence, not the whole specification; also respect user requirements, interfaces, and repository rules
- Minimal implementation — only what's needed to pass
- Refactor only within scope and keep the full relevant suite green
