# ADR 0067：原 CSP 下的本地图表渲染

日期：2026-11-07。状态：候选，原生 App 验收单独记录。

## 背景和候选

原 ChartCard 把 CDN 上的 Vega 5、Vega-Lite 5、Embed 6 与内联脚本放入 srcdoc，桌面既有 script-src self 策略拒绝这些脚本。CLI ad0a22b 的 chart 事件已进入 chatStore.chartSpecs，但 ChartCard 没有消息调用点，单独更新组件不能修复可见图表。

## 决策

保留原 spec 和工具协议，采用本地官方 Vega 6.4.0／Vega-Lite 6.4.1／Embed 7.3.0／Interpreter 2.3.2，按需加载、输出 SVG。官方解释器支持 parse 的 ast 和 View 的 expr，依据为[官方 npm 包随附说明](https://www.npmjs.com/package/vega-interpreter)与 Embed 类型/实现。不扩大 CSP，不新增图表语言。

MessageBubble 消费已存在的 chartSpecs，保留没有正文的图表专用行，在消息正文之后独立显示图表。原最终发布、Fork 与操作资格保持；轮次投影、累计统计和冷历史补充属于独立界面方案。

每次渲染使用独立 surface，释放成功和迟到的 View，不能污染后继图表；同步卸载不再启动模块导入，展开只改变容器高度。失败保留原 spec 并显示明确原因。无需内联脚本、CDN 或 unsafe-eval。

## 依赖与取舍

Vega 5 候选引入新的已知漏洞，因此弃用。Lite 6.4.1 仍覆盖项目 Node 20.19 下限，未采用要求 Node 22 的后继版本；Embed 的 peer 范围覆盖所选版本。

既有锁的 21 项风险使用最小安全更新，保留原 audit 失败。非 force audit fix 后余下同一 minimatch 传递链，仅在 typescript-estree 下绑定最早修复版本 11.0.1：唯一消费者仍使用命名 minimatch(filePath, pattern, {dot:true})，CJS/ESM 导出与 Node 18/20/>=22 范围保持。10 项正负控及 audit0 原件保留；不接受新风险、不使用 force 或审计过滤。

Vite 按 runtime、compiler、embed 分块，维持原 700KB 警告门槛。已在组合候选的原 CSP 浏览器中实际绘制原工具七类 v5 spec、Vega v5 和窄屏 bar，SVG、数据、展开检查通过；该结果不自动变成 head+subset 或原生 App 资格。

## 验证与同步

head+subset 须独立执行组件生命周期、原消息消费、全源格式、lint/全部前端测试、TypeScript/Vite 和当前锁 audit，并在实际 CSP 下重跑真实 SDK 绘图、两宽、展开、销毁与包体检查。原生 App、真实 provider、main CI 和发布分别记录。

没有新增模型工具或框架公共字段，examples 调用不变；官网不将候选当作已发布能力。原工具导出的独立 HTML 页面由原路径维护。
