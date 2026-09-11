# WPTSALL Client — Local-first Translation Client (WebUI + Desktop)

**The translation client for the WPMMCC ATS WordPress plugin — on your machine, not in a cloud.**

**English** | [简体中文](readme/README.zh-CN.md) | [繁體中文](readme/README.zh-TW.md) | [日本語](readme/README.ja.md) | [한국어](readme/README.ko.md) | [Español](readme/README.es.md) | [Français](readme/README.fr.md) | [Deutsch](readme/README.de.md) | [Português (Brasil)](readme/README.pt-BR.md) | [Italiano](readme/README.it.md) | [Русский](readme/README.ru.md) | [العربية](readme/README.ar.md) | [हिन्दी](readme/README.hi.md) | [Türkçe](readme/README.tr.md) | [Tiếng Việt](readme/README.vi.md) | [Bahasa Indonesia](readme/README.id.md)

WPTSALL Client is the companion client for the WPMMCC ATS WordPress plugin. It runs on your machine — as a local web UI or as a native desktop app — keeps every setting in a local database, connects directly to your own WordPress with a device-scoped token, and drives any HTTP(S) translation provider you configure. No account, no license, no cloud dependency.

## Links
- Project website — https://www.wpmm.cc/
- Documentation and usage help (English / 简体中文) — https://www.wpmm.cc/docs/
- WPMMCC ATS plugin on WordPress.org — https://wordpress.org/plugins/wpmmcc-ats/
- Plugin source repository — https://github.com/wpmmcc/wpmmcc-ats
- Signed kits and installers — https://github.com/wpmmcc/wptsall-client-releases

## Features
- Two products, one core — a local WebUI on 127.0.0.1:8977 and a native Desktop app (Tauri) sharing the same Rust core
- Local-first — sites, providers, components and rules live in your local database, never in a cloud
- Direct connection — talks to your WordPress through the plugin's Protocol v2 API
- Any provider — bring your own HTTP(S) translation endpoint and credentials
- Worker — run-once and continuous modes with bounded retries and idempotent callbacks
- Packaging — cross-platform kits (Linux / Windows / macOS) with signed OTA updates

## Requirements
- A WordPress site running the WPMMCC ATS plugin 2.x — https://github.com/wpmmcc/wpmmcc-ats
- A device-scoped token and route secret issued by the plugin
- A translation provider endpoint (any HTTP(S) API)

## Installation
1. Get a signed kit or installer from https://github.com/wpmmcc/wptsall-client-releases
2. Or build from source: cargo build --release inside client-wpplugin/source (WebUI binary), or the Tauri toolchain inside client-desktop (Desktop app)
3. Start the WebUI binary and open http://127.0.0.1:8977

## Quick start
1. Add your site — paste the plugin's client URL, route secret and device-scoped token
2. Configure a translation provider — endpoint plus credentials, stored locally
3. Run the worker once — it claims a batch, translates it and writes the results back

## Local data & privacy
All configuration and task state stay on your machine (SQLite plus local files). The client only ever talks to the WordPress site you configured and the translation provider endpoint you chose. Debug logging is off by default; the Settings page can turn local logging on or off at runtime.

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
The client UI ships its own localization. This README is available in 16 languages — see the table at the top. Contributions for further languages are welcome.

## License
GPL-2.0-or-later. See [LICENSE](LICENSE).

