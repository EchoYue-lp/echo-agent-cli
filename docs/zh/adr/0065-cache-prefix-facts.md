# ADR 0065：Eclip 固定规则与动态事实分层

状态：实现与本地验证中，真实模型收益单独验收。

## 背景与候选

Goal/recovery 前置投影混入每步预算和 journal 观察前沿，使已见历史失去缓存前缀。只移动替换位置不能保留旧消息；每步永久保护完整 recovery 又会造成增长。本应用采用框架 ADR 0108 的版本投影，不增加产品自有 Context store 或去重主循环。

## 决策与边界

| 来源与 owner | 语义版本 | 角色与生命周期 |
| --- | --- | --- |
| 已准备项目规则、人工 MEMORY.md、Domain 方法 | 原固定内容 revision | 保留固定指令权限；文件更新允许新 generation。MEMORY.md 无法可靠自动分类，不自动降权。 |
| TaskRuntime Goal 原文、引用与延续规则 | 原 Goal 身份及内容 | 固定 boundary，不混入 run.status、plan revision 或每步用量。 |
| TaskRuntime recovery | run 身份及有效状态内容 | User 历史内快照，承载 run status、plan revision、最新任务和恢复信息。仅最新版本保护。 |
| 有界预算 | run 身份、限制及剩余百分比 5% 台阶 | 不附精确每步剩余值；无界预算无内容，曾有预算时显式清除。执行额度仍由原 TaskRuntime 精确值决定。 |
| 原文来源引用 | 除观察前沿外的完整序列化来源身份，包括 run、journal 代际、来源记录及 InputResourceRef 的 locator/内容字段 | 仅 `observed_through_sequence` 增长不刷新全文；定位变化须刷新，原文资格继续由原 reader/guard 校验。 |
| turn contract、进度提醒、后台 watch | turn 身份及对应事实/提醒安全点 | 历史内追加，事实消失时显式清除；prepare 重入不重复。 |
| 当前召回记忆 | 内容或稳定的 cleared 版本 | 历史内追加；无召回使旧快照失效。 |

动态投影只是模型可见状态，不拥有 Goal、权限或完成资格。框架负责版本、最新保护、压缩回收和恢复去重。规则变化及时生效，不为命中率保留旧规则；用户原文不重写。未改变计费、统计公式或缓存率显示。

## 验证与取舍

参考 DeepSeek Harness 的历史内更新与版本刷新、ZCode 的稳定/动态分层，协议转换后验证 Chat/Responses/Anthropic 的实际 prompt。保留原 32.5% 运行记录和失败，不将本地合同称为外部收益。真实 provider、App、SDK pin、main CI、发布均独立验收，官网不新增量化宣传。
