# WPTSALL Client — ローカルファーストの翻訳クライアント(WebUI + Desktop)

**WPMMCC ATS WordPress プラグインの companion クライアント — クラウドではなく、あなたのマシンで。**

[English](../README.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | **日本語** | [한국어](README.ko.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Português (Brasil)](README.pt-BR.md) | [Italiano](README.it.md) | [Русский](README.ru.md) | [العربية](README.ar.md) | [हिन्दी](README.hi.md) | [Türkçe](README.tr.md) | [Tiếng Việt](README.vi.md) | [Bahasa Indonesia](README.id.md)

WPTSALL Client は WPMMCC ATS WordPress プラグインの companion クライアントです。ローカルの Web UI またはネイティブのデスクトップアプリとしてあなたのマシンで動作し、すべての設定をローカルデータベースに保存し、デバイストークンであなたの WordPress に直接接続し、設定した任意の HTTP(S) 翻訳プロバイダーを駆動します。アカウントもライセンスもクラウド依存もありません。

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

## Languages
クライアント UI は独自のローカライズを同梱。この README は16言語で利用できます — 上部の表を参照。さらなる言語の貢献を歓迎します。

## License
GPL-2.0-or-later。[LICENSE](../LICENSE) を参照。

