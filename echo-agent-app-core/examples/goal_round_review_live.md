# 轮次总结真实模型验证

`goal_round_review_live.rs` 使用应用正常配置加载、模型解析和客户端构造路径，精确选择
`ls-ds` 下启用的 DeepSeek v4.1 Flash。它不打印、复制或散列配置正文和认证值，不启动 GUI。
所有任务与样本数据写入指定的新目录，已有目录拒绝覆盖。

```bash
cargo build --locked -p echo-agent-app-core --example goal_round_review_live
cargo run --locked -p echo-agent-app-core --example goal_round_review_live -- \
  /absolute/path/to/config.yaml /absolute/path/to/new-sample-output
```

执行需真实模型调用授权。正常成功路径共四次主模型请求：部分完成样本跨两轮、只整理材料
样本一轮、缺少输入且要求两个报告标题的样本一轮。输入事实由隔离验收宿主提供；这不是
让模型实际操作业务数据，也不证明 GUI、IM 或自动续接 launcher 已通过。

回执记录真实请求正文、模型回答、脱敏失败、运行终态、Goal 原文/revision/hash 与完成门禁。
只有流已结束、收到 Settled、存在最终回答且没有错误才记该模型轮次成功。样本任务的总 Goal
仍有未完成义务，回答不能让它变成 Completed。回执中的行为判断初始为待人工对照；应逐项
核对推进、证据、剩余义务、障碍和下一步，不用关键词计分或固定模板代替评阅。

示例自动发现为 Cargo example，已纳入上述编译入口；模型选择不匹配、协议或网络错误时停止，
不自动切换模型、协议或权限。预算与取消的确定性证据沿原 app-core 测试保留。
