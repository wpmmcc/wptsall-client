# WPTSALL Client — локальный клиент перевода (WebUI + Desktop)

**Клиент для плагина WPMMCC ATS — на вашей машине, а не в облаке.**

[English](../README.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Português (Brasil)](README.pt-BR.md) | [Italiano](README.it.md) | **Русский** | [العربية](README.ar.md) | [हिन्दी](README.hi.md) | [Türkçe](README.tr.md) | [Tiếng Việt](README.vi.md) | [Bahasa Indonesia](README.id.md)

WPTSALL Client — клиент-компаньон плагина WPMMCC ATS для WordPress. Он работает на вашей машине — как локальный веб-интерфейс или нативное настольное приложение, — хранит все настройки в локальной базе данных, подключается напрямую к вашему WordPress с токеном устройства и работает с любым HTTP(S)-провайдером перевода, который вы настроите. Без аккаунта, без лицензии, без облачной зависимости.

## Links
- Project website — https://www.wpmm.cc/
- Documentation and usage help (English / 简体中文) — https://www.wpmm.cc/docs/
- WPMMCC ATS plugin on WordPress.org — https://wordpress.org/plugins/wpmmcc-ats/
- Plugin source repository — https://github.com/wpmmcc/wpmmcc-ats
- Signed kits and installers — https://github.com/wpmmcc/wptsall-client-releases

## Возможности
- Два продукта, одно ядро — локальный WebUI на 127.0.0.1:8977 и нативное Desktop-приложение (Tauri) на одном Rust-ядре
- Локальность — сайты, провайдеры, компоненты и правила живут в вашей локальной БД, никогда в облаке
- Прямое соединение — работает с вашим WordPress через клиентское API Protocol v2
- Любой провайдер — используйте свой HTTP(S)-эндпоинт перевода и учётные данные
- Worker — режимы разового запуска и непрерывный, с ограниченными повторами и идемпотентными колбэками
- Упаковка — кроссплатформенные kit-ы (Linux / Windows / macOS) с подписанными OTA-обновлениями

## Требования
- Сайт WordPress с плагином WPMMCC ATS 2.x — https://github.com/wpmmcc/wpmmcc-ats
- Токен устройства и route secret, выданные плагином
- Эндпоинт провайдера перевода (любой HTTP(S) API)

## Установка
1. Возьмите подписанный kit или установщик на https://github.com/wpmmcc/wptsall-client-releases
2. Или соберите из исходников: cargo build --release в client-wpplugin/source (бинарник WebUI) или инструментальная цепочка Tauri в client-desktop (Desktop-приложение)
3. Запустите бинарник WebUI и откройте http://127.0.0.1:8977

## Быстрый старт
1. Добавьте сайт — вставьте client URL плагина, route secret и токен устройства
2. Настройте провайдера перевода — эндпоинт и учётные данные хранятся локально
3. Запустите Worker один раз — он заберёт партию, переведёт и запишет результаты

## Локальные данные и приватность
Все настройки и состояние задач остаются на вашей машине (SQLite плюс локальные файлы). Клиент обменивается данными только с настроенным сайтом WordPress и выбранным эндпоинтом провайдера.

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
Интерфейс клиента поставляется со своей локализацией. Этот README доступен на 16 языках — см. таблицу наверху. Взносы с другими языками приветствуются.

## License
GPL-2.0-or-later. См. [LICENSE](../LICENSE).

