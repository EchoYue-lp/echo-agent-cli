---
name: mcp-builder
description: 创建 MCP (Model Context Protocol) 服务器——连接外部 API 和服务
metadata:
  category: automation
  source: anthropic
  upstream-version: "1.0"
  author: anthropic
  tags: [mcp, server, integration, api]
triggers:
  - MCP
  - mcp server
  - 模型上下文协议
  - 工具服务器
allowed-tools: [bash, read, write]
---
# MCP Builder

Create high-quality MCP (Model Context Protocol) servers that enable LLMs to interact with external services through well-designed tools.

## Features

- Build MCP servers in Python (FastMCP) or Node/TypeScript
- Design clean tool interfaces with proper validation
- Handle authentication and rate limiting
- Test and debug MCP servers
