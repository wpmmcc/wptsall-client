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
Tutte le configurazioni e lo stato delle attività restano sulla tua macchina (SQLite più file locali). Il client comunica solo con il sito WordPress configurato e l'endpoint del fornitore scelto. Il debug logging è disattivato per impostazione predefinita; la pagina Impostazioni consente di attivare o disattivare i log locali in fase di esecuzione.

## Aggiornamento
- Apri Impostazioni nella WebUI e usa il controllo aggiornamenti — le nuove versioni vengono scaricate come kit firmati e applicate sul posto
- Ogni kit viene verificato prima di sostituire qualsiasi file: firma minisign più checksum SHA-256, con protezione anti-rollback. Il servizio si riavvia automaticamente dopo; su Windows il binario in esecuzione viene sostituito in modo sicuro, con ripristino se l'aggiornamento fallisce
- Alternativa manuale: scarica l'installer più recente da https://github.com/wpmmcc/wptsall-client-releases ed eseguilo sopra l'installazione esistente

## Disinstallazione
- I programmi di disinstallazione per ogni prodotto e piattaforma sono disponibili nel repository dei rilasci: uninstall-webui.sh / uninstall-desktop.sh (Linux, macOS) e i rispettivi script .ps1 (Windows)
- Un disinstallatore arresta e rimuove il servizio in background (unità utente systemd, LaunchAgent o servizio Windows), i collegamenti da riga di comando e la directory di installazione
- I tuoi dati — database SQLite locale, configurazione e log — vengono conservati per impostazione predefinita. Aggiungi --purge-data (Linux/macOS) o -PurgeData (Windows) per rimuovere anche quelli
- Se hai installato sia la WebUI che l'app Desktop, la directory di installazione condivisa viene mantenuta a meno che non si passi anche --purge-shared

## Componenti open source
- Core Rust — tokio, reqwest (rustls TLS), serde/serde_json, rusqlite con SQLite integrato, aes-gcm/hkdf/sha2/hmac/rsa/jsonwebtoken per la crittografia, aws-sdk-s3 per i fornitori compatibili con S3, extism come runtime di componenti WASM
- App Desktop — Tauri 2 (shell nativa con webview di sistema)
- Web UI — Svelte 5, Vite, Tailwind CSS, svelte-i18n e icone lucide
- Sicurezza degli aggiornamenti — firme minisign e SHA256SUMS firmati
- Gli elenchi completi con versioni bloccate sono in Cargo.toml e frontend/package.json; ogni componente è distribuito con licenza di tipo MIT / Apache-2.0 / ISC compatibile con la GPL-2.0-or-later del progetto

## Languages
L'interfaccia del client include la propria localizzazione. Questo README è disponibile in 16 lingue — vedi la tabella in alto. Contributi per altre lingue sono benvenuti.

## License
GPL-2.0-or-later. Vedi [LICENSE](../LICENSE).

