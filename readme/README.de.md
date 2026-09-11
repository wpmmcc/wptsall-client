# WPTSALL Client — Local-First-Übersetzungsclient (WebUI + Desktop)

**Der Client zum WordPress-Plugin WPMMCC ATS — auf deinem Rechner, nicht in der Cloud.**

[English](../README.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Français](README.fr.md) | **Deutsch** | [Português (Brasil)](README.pt-BR.md) | [Italiano](README.it.md) | [Русский](README.ru.md) | [العربية](README.ar.md) | [हिन्दी](README.hi.md) | [Türkçe](README.tr.md) | [Tiếng Việt](README.vi.md) | [Bahasa Indonesia](README.id.md)

WPTSALL Client ist der Begleit-Client zum WordPress-Plugin WPMMCC ATS. Er läuft auf deinem Rechner — als lokale Web-UI oder als native Desktop-App —, speichert jede Einstellung in einer lokalen Datenbank, verbindet sich mit einem Gerätetoken direkt mit deinem eigenen WordPress und steuert jeden HTTP(S)-Übersetzungsanbieter, den du konfigurierst. Ohne Konto, ohne Lizenz, ohne Cloud-Abhängigkeit.

## Links
- Project website — https://www.wpmm.cc/
- Documentation and usage help (English / 简体中文) — https://www.wpmm.cc/docs/
- WPMMCC ATS plugin on WordPress.org — https://wordpress.org/plugins/wpmmcc-ats/
- Plugin source repository — https://github.com/wpmmcc/wpmmcc-ats
- Signed kits and installers — https://github.com/wpmmcc/wptsall-client-releases

## Funktionen
- Zwei Produkte, ein Kern — lokale WebUI auf 127.0.0.1:8977 und native Desktop-App (Tauri) mit demselben Rust-Kern
- Local-first — Sites, Anbieter, Komponenten und Regeln liegen in deiner lokalen Datenbank, nie in der Cloud
- Direkte Verbindung — spricht über die Protocol-v2-API des Plugins mit deinem WordPress
- Beliebiger Anbieter — bringe deinen eigenen HTTP(S)-Übersetzungsendpunkt und deine Zugangsdaten mit
- Worker — Einmal- und Dauermodus mit begrenzten Wiederholungen und idempotenten Callbacks
- Pakete — plattformübergreifende Kits (Linux / Windows / macOS) mit signierten OTA-Updates

## Voraussetzungen
- Eine WordPress-Website mit dem Plugin WPMMCC ATS 2.x — https://github.com/wpmmcc/wpmmcc-ats
- Ein vom Plugin ausgestellter Gerätetoken und Route Secret
- Ein Übersetzungsanbieter-Endpoint (beliebige HTTP(S)-API)

## Installation
1. Hol dir ein signiertes Kit oder einen Installer auf https://github.com/wpmmcc/wptsall-client-releases
2. Oder baue aus dem Quellcode: cargo build --release in client-wpplugin/source (WebUI-Binary) oder die Tauri-Toolchain in client-desktop (Desktop-App)
3. Starte das WebUI-Binary und öffne http://127.0.0.1:8977

## Schnellstart
1. Site hinzufügen — Client-URL des Plugins, Route Secret und Gerätetoken einfügen
2. Übersetzungsanbieter konfigurieren — Endpoint plus Zugangsdaten, alles lokal gespeichert
3. Worker einmal ausführen — holt einen Stapel ab, übersetzt und schreibt zurück

## Lokale Daten & Privatsphäre
Alle Konfigurationen und Aufgabenstände bleiben auf deinem Rechner (SQLite plus lokale Dateien). Der Client spricht nur mit der von dir konfigurierten WordPress-Website und dem gewählten Anbieter-Endpoint.

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
Die Client-Oberfläche bringt ihre eigene Lokalisierung mit. Dieses README gibt es in 16 Sprachen — siehe Tabelle oben. Beiträge für weitere Sprachen sind willkommen.

## License
GPL-2.0-or-later. Siehe [LICENSE](../LICENSE).

