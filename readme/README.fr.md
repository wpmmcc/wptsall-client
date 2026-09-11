# WPTSALL Client — Client de traduction local-first (WebUI + Desktop)

**Le client du plugin WordPress WPMMCC ATS — sur votre machine, pas dans le cloud.**

[English](../README.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | **Français** | [Deutsch](README.de.md) | [Português (Brasil)](README.pt-BR.md) | [Italiano](README.it.md) | [Русский](README.ru.md) | [العربية](README.ar.md) | [हिन्दी](README.hi.md) | [Türkçe](README.tr.md) | [Tiếng Việt](README.vi.md) | [Bahasa Indonesia](README.id.md)

WPTSALL Client est le client compagnon du plugin WordPress WPMMCC ATS. Il tourne sur votre machine — en interface web locale ou en application bureautique native — conserve chaque réglage dans une base de données locale, se connecte directement à votre propre WordPress avec un jeton d'appareil et pilote n'importe quel fournisseur de traduction HTTP(S) que vous configurez. Sans compte, sans licence, sans dépendance au cloud.

## Links
- Project website — https://www.wpmm.cc/
- Documentation and usage help (English / 简体中文) — https://www.wpmm.cc/docs/
- WPMMCC ATS plugin on WordPress.org — https://wordpress.org/plugins/wpmmcc-ats/
- Plugin source repository — https://github.com/wpmmcc/wpmmcc-ats
- Signed kits and installers — https://github.com/wpmmcc/wptsall-client-releases

## Fonctionnalités
- Deux produits, un cœur — une WebUI locale sur 127.0.0.1:8977 et une application bureautique native (Tauri) partageant le même cœur Rust
- Local-first — sites, fournisseurs, composants et règles restent dans votre base locale, jamais dans le cloud
- Connexion directe — dialogue avec votre WordPress via l'API Protocol v2 du plugin
- N'importe quel fournisseur — apportez votre endpoint de traduction HTTP(S) et vos identifiants
- Worker — modes ponctuel et continu, avec tentatives bornées et rappels idempotents
- Empaquetage — kits multiplateformes (Linux / Windows / macOS) avec mises à jour OTA signées

## Prérequis
- Un site WordPress faisant tourner le plugin WPMMCC ATS 2.x — https://github.com/wpmmcc/wpmmcc-ats
- Un jeton d'appareil et un route secret émis par le plugin
- Un endpoint de fournisseur de traduction (toute API HTTP(S))

## Installation
1. Récupérez un kit signé ou un installateur sur https://github.com/wpmmcc/wptsall-client-releases
2. Ou compilez depuis les sources : cargo build --release dans client-wpplugin/source (binaire WebUI), ou la chaîne Tauri dans client-desktop (application bureautique)
3. Démarrez le binaire WebUI et ouvrez http://127.0.0.1:8977

## Démarrage rapide
1. Ajoutez votre site — collez l'URL client du plugin, le route secret et le jeton d'appareil
2. Configurez un fournisseur de traduction — endpoint et identifiants, stockés localement
3. Lancez le worker une fois — il réclame un lot, le traduit et écrit les résultats

## Données locales et confidentialité
Toute la configuration et l'état des tâches restent sur votre machine (SQLite et fichiers locaux). Le client ne communique qu'avec le site WordPress configuré et l'endpoint du fournisseur choisi.

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
L'interface du client embarque sa propre localisation. Ce README est disponible en 16 langues — voir le tableau en haut. Les contributions pour d'autres langues sont bienvenues.

## License
GPL-2.0-or-later. Voir [LICENSE](../LICENSE).

