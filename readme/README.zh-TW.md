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
- 外掛簽發的站台連接包（wp-admin → 任務管理 → 用戶端授權，或 `wp wptsall security issue-pairing-pack`）
- 一個翻譯廠商端點(任意 HTTP(S) API)

## 預設連接埠
- WebUI 預設監聽 127.0.0.1:8977 —— 僅本機回環，絕不暴露到網路
- 8977 未被 IANA 註冊，並避開常見服務與開發連接埠（3306、5432、6379、8080、9000、9200 等），衝突機率很低
- 若連接埠已被占用，啟動會報 bind web ui failed —— 設定 WPTSALL_WEB_UI_PORT 或 WPTSALL_WEB_UI_BIND 改用其他連接埠後重新啟動服務

## 安裝
1. 從 https://github.com/wpmmcc/wptsall-client-releases 取得已簽署 kit 或安裝器
2. 或從原始碼建置:在 client-wpplugin/source 內 cargo build --release(WebUI 二進位),或在 client-desktop 內用 Tauri 工具鏈(Desktop 應用)
3. 啟動 WebUI 二進位並開啟 http://127.0.0.1:8977

## 快速上手
1. 新增站台——在 wp-admin（任務管理 → 用戶端授權）產生連接包，連同配對碼一起貼到用戶端的站台頁面
2. 設定翻譯廠商——端點加憑證,全部本地保存
3. 執行一次 Worker——領取批次、翻譯、寫回

## 本地資料與隱私
全部設定與任務狀態都留在你的機器上(SQLite + 本地檔案)。用戶端只會與你設定的 WordPress 站台和翻譯廠商端點通訊。除錯日誌預設關閉;可在設定頁運行時開啟或關閉本地日誌。

## 更新
- 開啟 WebUI 的設定頁面使用更新檢查——新版本將下載為已簽署 kit 並原地套用更新
- 在替換任何檔案前均經過嚴格驗證:minisign 簽名與 SHA-256 總和檢查碼,並具備防回滾機制。更新完成後服務自動重啟;在 Windows 上執行中二進位檔案會被安全替換,更新失敗自動復原
- 手動替代方案:從 https://github.com/wpmmcc/wptsall-client-releases 下載最新安裝程式並覆蓋安裝

## 解除安裝
- 各產品與平台的解除安裝指令碼均可在 releases 儲存庫取得:uninstall-webui.sh / uninstall-desktop.sh(Linux、macOS)以及對應的 .ps1 指令碼(Windows)
- 解除安裝指令碼會自動停止並移除背景服務(systemd 使用者單元、LaunchAgent 或 Windows 服務)、命令列捷徑以及安裝目錄
- 你的資料(本地 SQLite 資料庫、設定與記錄)預設保留。如需連同資料一併清理,可追加 --purge-data(Linux/macOS)或 -PurgeData(Windows)參數
- 若同時安裝了 WebUI 與 Desktop 桌面版,共用安裝目錄預設保留,除非同時傳入 --purge-shared 參數

## 開源元件
- Rust 核心——tokio、reqwest(rustls TLS)、serde/serde_json、rusqlite(內建 SQLite)、aes-gcm/hkdf/sha2/hmac/rsa/jsonwebtoken 加密庫、適用於 S3 相容廠商的 aws-sdk-s3、WASM 元件執行環境 extism
- Desktop 應用——Tauri 2(呼叫系統原生 webview 的輕量外殼)
- Web UI——Svelte 5、Vite、Tailwind CSS、svelte-i18n 與 lucide 圖示庫
- 更新安全——minisign 簽名與簽署的 SHA256SUMS
- 完整鎖定版本清單見 Cargo.toml 與 frontend/package.json;所有元件均以相容於 GPL-2.0-or-later 的 MIT / Apache-2.0 / ISC 等授權釋出

## Languages
用戶端 UI 自帶本地化。本 README 提供 16 種語言——見頂部語言表。歡迎貢獻更多語言。

## License
GPL-2.0-or-later,見 [LICENSE](../LICENSE)。

