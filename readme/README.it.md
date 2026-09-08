# WPTSALL Client — Client di traduzione local-first (WebUI + Desktop)

**Il client del plugin WordPress WPMMCC ATS — sulla tua macchina, non nel cloud.**

[English](../README.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Português (Brasil)](README.pt-BR.md) | **Italiano** | [Русский](README.ru.md) | [العربية](README.ar.md) | [हिन्दी](README.hi.md) | [Türkçe](README.tr.md) | [Tiếng Việt](README.vi.md) | [Bahasa Indonesia](README.id.md)

WPTSALL Client è il client companion del plugin WordPress WPMMCC ATS. Girà sulla tua macchina — come interfaccia web locale o app desktop nativa — salva ogni impostazione in un database locale, si collega direttamente al tuo WordPress con un token di dispositivo e pilota qualunque fornitore di traduzione HTTP(S) tu configuri. Senza account, senza licenza, senza dipendenza dal cloud.

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

## Languages
L'interfaccia del client include la propria localizzazione. Questo README è disponibile in 16 lingue — vedi la tabella in alto. Contributi per altre lingue sono benvenuti.

## License
GPL-2.0-or-later. Vedi [LICENSE](../LICENSE).

