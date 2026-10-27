# Eclip 品牌与图标规范

**Eclip /ˈɛk.lɪp/**，读作 **EK-lip**，两个音节，重音在前。

**标语：听见你的声音，让想法飞跃。**

品牌延续 Echo（回声、共鸣）的初衷：让 AI 听见用户的声音，理解诉求，在协作中解决问题。Eclip 是产品名；不把拼写包装成未经确认的英文词源。

## 图标

使用用户选定的卡通矢量简笔猫头：天蓝色圆角底、白色猫头、短竖眼睛、少量鼻口与胡须。不要改回几何轨道、箭头或复杂动物插画。

- `app-icon.svg`：唯一矢量母版。
- `app-icon.png`：1024 × 1024 导出。
- `../../web-frontend/public/app-icon.svg` 与 `favicon.svg`：桌面前端图标。
- `../../src-tauri/icons/`：PNG、Windows ICO 与 macOS ICNS 导出。
- 官网 `echo-website/public/app-icon.svg`、`app-icon.png`：同一母版导出。

旧 `eko-icon.svg`、`eko-icon-light.svg` 和 `eko-mark.svg` 只保留为历史草稿，不是当前品牌素材。

## 名称与读音

正文、窗口标题和安装包产品名使用 **Eclip**。官网产品页、文档首页、桌面欢迎页和品牌介绍同时展示 **/ˈɛk.lɪp/**；紧凑导航只显示产品名，避免挤占空间。

`echo-agent` 继续是独立框架名，`echo-agent-sdk` 继续是独立 SDK 名。仓库、crate、二进制、`EKO_*` 环境变量、`eko.yaml`、`~/.eko`、bundle identifier `com.eko.desktop` 以及官网 `/eko` 路由保留技术身份，避免仅为更名改变已有数据与链接。

本次只改产品展示与图标，不改变运行时、公共 API 或示例合同，因此不新增架构 ADR，也不改代码示例。已运行桌面进程需要后续重新构建才能采用原生包名和 Dock 图标；源码更新不代表原生验收或官网部署已经完成。
