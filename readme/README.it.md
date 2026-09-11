# WPTSALL Client — Client di traduzione local-first (WebUI + Desktop)

**Il client del plugin WordPress WPMMCC ATS — sulla tua macchina, non nel cloud.**

[English](../README.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Português (Brasil)](README.pt-BR.md) | **Italiano** | [Русский](README.ru.md) | [العربية](README.ar.md) | [हिन्दी](README.hi.md) | [Türkçe](README.tr.md) | [Tiếng Việt](README.vi.md) | [Bahasa Indonesia](README.id.md)

WPTSALL Client è il client companion del plugin WordPress WPMMCC ATS. Girà sulla tua macchina — come interfaccia web locale o app desktop nativa — salva ogni impostazione in un database locale, si collega direttamente al tuo WordPress con un token di dispositivo e pilota qualunque fornitore di traduzione HTTP(S) tu configuri. Senza account, senza licenza, senza dipendenza dal cloud.

## Links
- Project website — https://www.wpmm.cc/
- Documentation and usage help (English / 简体中文) — https://www.wpmm.cc/docs/
- WPMMCC ATS plugin on WordPress.org — https://wordpress.org/plugins/wpmmcc-ats/
- Plugin source repository — https://github.com/wpmmcc/wpmmcc-ats
- Signed kits and installers — https://github.com/wpmmcc/wptsall-client-releases

## Caratteristiche
- Due prodotti, un nucleo — WebUI locale su 127.0.0.1:8977 e app desktop nativa (Tauri) che condividono lo stesso nucleo Rust
- Local-first — siti, fornitori, componenti e regole vivono nel tuo database locale, mai nel cloud
- Connessione diretta — parla con il tuo WordPress tramite l'API Protocol v2 del plugin
- Qualsiasi fornitore — porta il tuo endpoint di traduzione HTTP(S) e le tue credenziali
- Worker — modalità singola e continua, con tentativi limitati e callback idempotenti
- Pacchetti — kit multipiattaforma (Linux / Windows / macOS) con aggiornamenti OTA firmati

## Requisiti
- Un sito WordPress con il plugin WPMMCC ATS 2.x — https://github.com/wpmmcc/wpmmcc-ats
- Un token di dispositivo e route secret rilasciati dal plugin
- Un endpoint di fornitore di traduzione (qualsiasi API HTTP(S))

## Installazione
1. Preleva un kit firmato o un installer da https://github.com/wpmmcc/wptsall-client-releases
2. Oppure compila dai sorgenti: cargo build --release dentro client-wpplugin/source (binario WebUI), oppure la toolchain Tauri dentro client-desktop (app desktop)
3. Avvia il binario WebUI e apri http://127.0.0.1:8977

## Avvio rapido
1. Aggiungi il tuo sito — incolla l'URL client del plugin, il route secret e il token di dispositivo
2. Configura un fornitore di traduzione — endpoint più credenziali, salvati in locale
3. Esegui il worker una volta — preleva un batch, lo traduce e riscrive i risultati

## Dati locali e privacy
Tutte le configurazioni e lo stato delle attività restano sulla tua macchina (SQLite più file locali). Il client comunica solo con il sito WordPress configurato e l'endpoint del fornitore scelto.

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
L'interfaccia del client include la propria localizzazione. Questo README è disponibile in 16 lingue — vedi la tabella in alto. Contributi per altre lingue sono benvenuti.

## License
GPL-2.0-or-later. Vedi [LICENSE](../LICENSE).

