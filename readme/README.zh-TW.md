# WPTSALL Client — 本地優先的翻譯用戶端(WebUI + Desktop)

**WPMMCC ATS WordPress 外掛的配套翻譯用戶端——跑在你自己的機器上,不在雲端。**

[English](../README.md) | [简体中文](README.zh-CN.md) | **繁體中文** | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Português (Brasil)](README.pt-BR.md) | [Italiano](README.it.md) | [Русский](README.ru.md) | [العربية](README.ar.md) | [हिन्दी](README.hi.md) | [Türkçe](README.tr.md) | [Tiếng Việt](README.vi.md) | [Bahasa Indonesia](README.id.md)

WPTSALL Client 是 WPMMCC ATS WordPress 外掛的配套用戶端。它運行在你的機器上——可以是本地 WebUI,也可以是原生桌面應用——所有設定保存在本地資料庫,使用裝置級權杖直連你自己的 WordPress,並驅動你設定的任意 HTTP(S) 翻譯廠商。無需帳號、無需授權、不依賴雲端。

## 相關連結
- 專案官網 — https://www.wpmm.cc/
- 文件與使用說明(English / 簡體中文)— https://www.wpmm.cc/docs/
- WPMMCC ATS 外掛(WordPress.org)— https://wordpress.org/plugins/wpmmcc-ats/
- 外掛原始碼儲存庫 — https://github.com/wpmmcc/wpmmcc-ats
- 已簽署 kit 與安裝器 — https://github.com/wpmmcc/wptsall-client-releases

## 功能
- 雙產品同核心——本地 WebUI(127.0.0.1:8977)與原生 Desktop 應用(Tauri)共享同一 Rust 核心
- 本地優先——站台、廠商、元件、規則都保存在本地資料庫,絕不上雲
- 直連——透過外掛的 Protocol v2 API 與你的 WordPress 對接
- 任意廠商——自帶 HTTP(S) 翻譯端點與憑證即可
- Worker——單次執行與持續模式,帶界定的重試與冪等回呼
- 打包——跨平台 kit(Linux / Windows / macOS)+ 簽署 OTA 升級

## 環境需求
- 運行 WPMMCC ATS 外掛 2.x 的 WordPress 站台——https://github.com/wpmmcc/wpmmcc-ats
- 外掛簽發的裝置級權杖與 route secret
- 一個翻譯廠商端點(任意 HTTP(S) API)

## 安裝
1. 從 https://github.com/wpmmcc/wptsall-client-releases 取得已簽署 kit 或安裝器
2. 或從原始碼建置:在 client-wpplugin/source 內 cargo build --release(WebUI 二進位),或在 client-desktop 內用 Tauri 工具鏈(Desktop 應用)
3. 啟動 WebUI 二進位並開啟 http://127.0.0.1:8977

## 快速上手
1. 新增站台——貼上外掛的 client URL、route secret 與裝置級權杖
2. 設定翻譯廠商——端點加憑證,全部本地保存
3. 執行一次 Worker——領取批次、翻譯、寫回

## 本地資料與隱私
全部設定與任務狀態都留在你的機器上(SQLite + 本地檔案)。用戶端只會與你設定的 WordPress 站台和翻譯廠商端點通訊。

## Languages
用戶端 UI 自帶本地化。本 README 提供 16 種語言——見頂部語言表。歡迎貢獻更多語言。

## License
GPL-2.0-or-later,見 [LICENSE](../LICENSE)。

