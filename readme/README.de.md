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

## Languages
Die Client-Oberfläche bringt ihre eigene Lokalisierung mit. Dieses README gibt es in 16 Sprachen — siehe Tabelle oben. Beiträge für weitere Sprachen sind willkommen.

## License
GPL-2.0-or-later. Siehe [LICENSE](../LICENSE).

