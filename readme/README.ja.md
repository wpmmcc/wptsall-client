# WPTSALL Client — ローカルファーストの翻訳クライアント(WebUI + Desktop)

**WPMMCC ATS WordPress プラグインの companion クライアント — クラウドではなく、あなたのマシンで。**

[English](../README.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | **日本語** | [한국어](README.ko.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Português (Brasil)](README.pt-BR.md) | [Italiano](README.it.md) | [Русский](README.ru.md) | [العربية](README.ar.md) | [हिन्दी](README.hi.md) | [Türkçe](README.tr.md) | [Tiếng Việt](README.vi.md) | [Bahasa Indonesia](README.id.md)

WPTSALL Client は WPMMCC ATS WordPress プラグインの companion クライアントです。ローカルの Web UI またはネイティブのデスクトップアプリとしてあなたのマシンで動作し、すべての設定をローカルデータベースに保存し、デバイストークンであなたの WordPress に直接接続し、設定した任意の HTTP(S) 翻訳プロバイダーを駆動します。アカウントもライセンスもクラウド依存もありません。

## Links
- Project website — https://www.wpmm.cc/
- Documentation and usage help (English / 简体中文) — https://www.wpmm.cc/docs/
- WPMMCC ATS plugin on WordPress.org — https://wordpress.org/plugins/wpmmcc-ats/
- Plugin source repository — https://github.com/wpmmcc/wpmmcc-ats
- Signed kits and installers — https://github.com/wpmmcc/wptsall-client-releases

## 機能
- 2つの製品、1つのコア — ローカル WebUI(127.0.0.1:8977)とネイティブ Desktop アプリ(Tauri)が同じ Rust コアを共有
- ローカルファースト — サイト・プロバイダー・コンポーネント・ルールはローカル DB に保存、クラウドには送りません
- 直接接続 — プラグインの Protocol v2 API であなたの WordPress と対話
- 任意のプロバイダー — HTTP(S) の翻訳エンドポイントと認証情報を自分で用意
- Worker — 単発実行と連続モード、上限付きリトライと冪等コールバック
- パッケージ — クロスプラットフォーム kit(Linux / Windows / macOS)+ 署名付き OTA 更新

## 要件
- WPMMCC ATS プラグイン 2.x が動く WordPress サイト — https://github.com/wpmmcc/wpmmcc-ats
- プラグインが発行するデバイストークンと route secret
- 翻訳プロバイダーのエンドポイント(任意の HTTP(S) API)

## インストール
1. https://github.com/wpmmcc/wptsall-client-releases から署名済み kit かインストーラーを入手
2. またはソースからビルド: client-wpplugin/source で cargo build --release(WebUI バイナリ)、client-desktop で Tauri ツールチェーン(Desktop アプリ)
3. WebUI バイナリを起動して http://127.0.0.1:8977 を開く

## クイックスタート
1. サイトを追加 — プラグインの client URL・route secret・デバイストークンを貼り付け
2. 翻訳プロバイダーを設定 — エンドポイントと認証情報、すべてローカルに保存
3. Worker を1回実行 — バッチを取得、翻訳、書き戻し

## ローカルデータとプライバシー
設定もタスク状態もすべてあなたのマシンに置かれます(SQLite + ローカルファイル)。クライアントが通信するのは、設定した WordPress サイトと選んだ翻訳プロバイダーのエンドポイントだけです。

## Updating
- Open Settings in the WebUI and use the update check — new versions download as signed kits and apply in place
- Every kit is verified before anything is replaced: minisign signature plus SHA-256 checksum, with anti-rollback protection. The service restarts automatically afterwards; on Windows the running binary is replaced safely, with rollback if the update fails
- Manual alternative: download the latest installer from https://github.com/wpmmcc/wptsall-client-releases and run it over the existing installation

## Uninstalling
- Uninstallers for every product and platform live in the releases repository: uninstall-webui.sh / uninstall-desktop.sh (Linux, macOS) and the matching .ps1 scripts (Windows)
- An uninstaller stops and removes the background service (systemd user unit, LaunchAgent or Windows service), the command-line shortcuts and the install directory
- Your data — local SQLite database, configuration and logs — is kept by default. Add --purge-data (Linux/macOS) or -PurgeData (Windows) to remove it as well
- If you have both the WebUI and the Desktop app, the shared install directory is kept unless you also pass --purge-shared

## Open-source components
- Rust core — tokio, reqwest (rustls TLS), serde/serde_json, rusqlite with bundled SQLite, aes-gcm/hkdf/sha2/hmac/rsa/jsonwebtoken for crypto, aws-sdk-s3 for S3-compatible providers, extism as the WASM component runtime
- Desktop app — Tauri 2 (native shell with the system webview)
- Web UI — Svelte 5, Vite, Tailwind CSS, svelte-i18n and lucide icons
- Update security — minisign signatures and signed SHA256SUMS
- Complete version-pinned lists are in Cargo.toml and frontend/package.json; every component ships under an MIT / Apache-2.0 / ISC-style license compatible with this project's GPL-2.0-or-later
## Languages
クライアント UI は独自のローカライズを同梱。この README は16言語で利用できます — 上部の表を参照。さらなる言語の貢献を歓迎します。

## License
GPL-2.0-or-later。[LICENSE](../LICENSE) を参照。

