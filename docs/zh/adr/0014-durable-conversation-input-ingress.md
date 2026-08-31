# ADR 0014：Durable Conversation Input Ingress

## 状态

已采纳，2026-08-27。

## 背景

GUI、TUI、CLI/JSONL 和 channel 曾有不同的 follow-up 排队和 drain 语义，caller drop 可能
导致输入已接受却没有 durable terminal。

## 决策

所有普通 conversation 输入先进入 durable frontier，再通过 framework tracked receipt 进入
turn。接受、drain、消费和 terminal 是不同事实；`ConversationInputService` 负责顺序、attempt
和 replay-safe receipt，surface 只适配请求和渲染结果。

## 影响

重启、workspace 切换和 caller 取消不会丢失已接受输入。没有明确 owner terminal 时禁止自动
重放，不把 transport EOF 或文本答案当成完成证明。
