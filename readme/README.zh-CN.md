# WPTSALL Client — 本地优先翻译客户端（WebUI + Desktop）

**WPMMCC ATS WordPress 插件的配套翻译客户端 — 运行在你的本地机器，绝不上云。**

[English](../README.md) | **简体中文** | [繁體中文](README.zh-TW.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Português (Brasil)](README.pt-BR.md) | [Italiano](README.it.md) | [Русский](README.ru.md) | [العربية](README.ar.md) | [हिन्दी](README.hi.md) | [Türkçe](README.tr.md) | [Tiếng Việt](README.vi.md) | [Bahasa Indonesia](README.id.md)

WPTSALL Client 是 WPMMCC ATS WordPress 插件的配套客户端。它运行在你的本地机器上 — 可以是本地 WebUI，也可以是原生 Desktop 桌面应用 — 所有配置均保存在本地数据库中，使用设备级令牌直连你自己的 WordPress，并驱动你配置的任意 HTTP(S) 翻译厂商。无需账号、无需许可证密钥、不依赖云端。

## 相关链接
- 项目官网 — https://www.wpmm.cc/
- 文档与使用帮助（English / 简体中文）— https://www.wpmm.cc/docs/
- WPMMCC ATS 插件（WordPress.org）— https://wordpress.org/plugins/wpmmcc-ats/
- 插件源码仓库 — https://github.com/wpmmcc/wpmmcc-ats
- 已签名 kit 与安装程序 — https://github.com/wpmmcc/wptsall-client-releases

## 功能
- 双形态产品，同一核心 — 本地 WebUI（127.0.0.1:8977）与原生 Desktop 桌面应用（Tauri）共享同一套 Rust 核心
- 本地优先 — 站点、厂商、组件与规则均保存在本地数据库中，绝不上云
- 直连通信 — 通过插件的 Protocol v2 API 与你的 WordPress 站点直接对接
- 任意厂商 — 自带任意 HTTP(S) 翻译端点与凭据即可接入
- Worker — 支持“执行一次”与持续模式，具备界定重试与幂等回调能力
- 分发打包 — 跨平台发布 kit（Linux / Windows / macOS），具备签名 OTA 升级能力

## 环境要求
- 运行 WPMMCC ATS 插件 2.x 的 WordPress 站点 — https://github.com/wpmmcc/wpmmcc-ats
- 插件签发的站点连接包（wp-admin → 任务管理 → 客户端授权，或 `wp wptsall security issue-pairing-pack`）
- 一个翻译厂商端点（任意 HTTP(S) API）

## 默认端口
- WebUI 默认监听 127.0.0.1:8977 —— 仅本机回环，绝不暴露到网络
- 8977 未被 IANA 注册，并避开常见服务与开发端口（3306、5432、6379、8080、9000、9200 等），冲突概率很低
- 若端口已被占用，启动会报 bind web ui failed —— 设置 WPTSALL_WEB_UI_PORT 或 WPTSALL_WEB_UI_BIND 换用其他端口后重启服务

## 安装
1. 从 https://github.com/wpmmcc/wptsall-client-releases 获取已签名 kit 或安装程序
2. 或从源码构建：在 client-wpplugin/source 内执行 cargo build --release（WebUI 二进制），或在 client-desktop 内使用 Tauri 工具链（Desktop 应用）
3. 启动 WebUI 二进制并访问 http://127.0.0.1:8977

## 快速上手
1. 添加站点 — 在 wp-admin（任务管理 → 客户端授权）生成连接包，连同配对码一起粘贴到客户端的站点页面
2. 配置翻译厂商 — 填入端点与凭据，全部保存在本地
3. 执行一次 Worker — 领取批次任务，翻译并写回结果

## 本地数据与隐私
全部配置与任务状态均保存在本地机器（SQLite 与本地文件）。客户端仅与已配置的 WordPress 站点和所选的翻译厂商端点通信。调试日志默认关闭；可在设置页运行时开启或关闭本地日志。

## 更新
- 打开 WebUI 的设置页面使用更新检查 — 新版本将下载为已签名 kit 并就地应用更新
- 在替换任何文件前均经过严格校验：minisign 签名与 SHA-256 校验和，并具备防回滚机制。更新完成后服务自动重启；在 Windows 上运行中二进制会被安全替换，更新失败自动回滚
- 手动备选方案：从 https://github.com/wpmmcc/wptsall-client-releases 下载最新安装包并直接覆盖安装

## 卸载
- 各产品与平台的卸载脚本均可在 releases 仓库获取：uninstall-webui.sh / uninstall-desktop.sh（Linux、macOS）以及对应的 .ps1 脚本（Windows）
- 卸载脚本会自动停止并移除后台服务（systemd 用户单元、LaunchAgent 或 Windows 服务）、命令行软链接以及安装目录
- 你的数据（本地 SQLite 数据库、配置与日志）默认予以保留。如需连同数据一并清理，可追加 --purge-data（Linux/macOS）或 -PurgeData（Windows）参数
- 若同时安装了 WebUI 与 Desktop 桌面端，共享安装目录默认保留，除非同时传入 --purge-shared 参数

## 开源组件
- Rust 核心 — tokio、reqwest（rustls TLS）、serde/serde_json、rusqlite（内置 SQLite）、aes-gcm/hkdf/sha2/hmac/rsa/jsonwebtoken 加密库、用于 S3 兼容厂商的 aws-sdk-s3、WASM 组件运行时 extism
- Desktop 桌面端 — Tauri 2（调用系统原生 webview 的轻量壳体）
- Web UI — Svelte 5、Vite、Tailwind CSS、svelte-i18n 与 lucide 图标库
- 升级安全 — minisign 签名与签名校验的 SHA256SUMS
- 完整锁定版本清单见 Cargo.toml 与 frontend/package.json；所有组件均基于与 GPL-2.0-or-later 兼容的 MIT / Apache-2.0 / ISC 等宽松协议分发

## 多语言
客户端 UI 内置多语言支持。本 README 提供 16 种语言版本 — 详见顶部语言表。欢迎为更多语言贡献翻译。

## 许可证
GPL-2.0-or-later，详见 [LICENSE](../LICENSE)。

