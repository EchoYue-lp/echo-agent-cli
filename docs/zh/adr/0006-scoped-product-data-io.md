# ADR 0006：Workspace-scoped product-data I/O

## 状态

已采纳。

## 背景

EKO 的文件、研究、分析和附件操作必须绑定明确的 workspace generation。直接在 Tokio
executor 上执行同步文件 I/O 会阻塞其它 Agent surface，调用方取消也不能提前结束已接纳的
操作。

## 决策

所有产品数据 I/O 通过 `ProductDataIoService` 和有界 blocking adapter 执行。操作捕获
精确的 workspace authority，使用 shared flow receipt 保持 caller drop 后的生命周期。进程级
semaphore 只负责容量，不拥有操作生命周期；workspace 删除必须等待 accepted operation settlement。

## 影响

GUI、TUI、CLI/JSONL、channel、分析、研究和 CommandCell 共享相同的 I/O authority。失败或
清理未完成时保留 typed repair debt，不让 surface 误报成功，也不引入第二个文件 store。
