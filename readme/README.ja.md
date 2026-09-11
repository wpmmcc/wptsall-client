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
設定もタスク状態もすべてあなたのマシンに置かれます（SQLite＋ローカルファイル）。クライアントが通信するのは、設定した WordPress サイトと選んだ翻訳プロバイダーのエンドポイントだけです。デバッグログはデフォルトでオフです。設定ページから実行時にローカルログのオン/オフを切り替えられます。

## アップデート
- WebUI の設定画面を開き、アップデート確認を使用してください — 新バージョンは署名済み kit としてダウンロードされ、その場で適用されます
- すべての kit は置き換え前に minisign 署名と SHA-256 チェックサムで検証され、ロールバック防止機能も備えています。更新完了後はサービスが自動再起動します。Windows では実行中のバイナリを安全に置き換え、失敗した場合はロールバックします
- 手動での代替手段：https://github.com/wpmmcc/wptsall-client-releases から最新のインストーラーをダウンロードし、既存のインストールに上書き実行してください

## アンインストール
- 各製品・プラットフォーム向けのアンインストーラーは releases リポジトリで提供されています：uninstall-webui.sh / uninstall-desktop.sh（Linux、macOS）および対応する .ps1 スクリプト（Windows）
- アンインストーラーはバックグラウンドサービス（systemd ユーザーユニット、LaunchAgent、Windows サービス）、コマンドラインショートカット、インストールディレクトリを停止・削除します
- ローカルの SQLite データベース、設定、ログなどのデータはデフォルトで保持されます。これらも削除する場合は --purge-data（Linux/macOS）または -PurgeData（Windows）を追加してください
- WebUI と Desktop アプリの両方をインストールしている場合、--purge-shared も渡さない限り共有インストールディレクトリは保持されます

## オープンソースコンポーネント
- Rust コア — tokio、reqwest（rustls TLS）、serde/serde_json、rusqlite（組み込み SQLite）、暗号用 aes-gcm/hkdf/sha2/hmac/rsa/jsonwebtoken、S3 互換プロバイダー用 aws-sdk-s3、WASM コンポーネントランタイム extism
- Desktop アプリ — Tauri 2（システム webview を使用するネイティブシェル）
- Web UI — Svelte 5、Vite、Tailwind CSS、svelte-i18n、lucide アイコン
- アップデートセキュリティ — minisign 署名と署名済み SHA256SUMS
- バージョン固定の完全なリストは Cargo.toml および frontend/package.json に記載されています。すべてのコンポーネントは本プロジェクトの GPL-2.0-or-later と互換性のある MIT / Apache-2.0 / ISC ライセンス下で提供されています

## Languages
クライアント UI は独自のローカライズを同梱。この README は16言語で利用できます — 上部の表を参照。さらなる言語の貢献を歓迎します。

## License
GPL-2.0-or-later。[LICENSE](../LICENSE) を参照。

