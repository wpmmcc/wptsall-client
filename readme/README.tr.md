# WPTSALL Client — Yerel-öncelikli çeviri istemcisi (WebUI + Desktop)

**WPMMCC ATS WordPress eklentisinin istemcisi — makinenizde, bulutta değil.**

[English](../README.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Português (Brasil)](README.pt-BR.md) | [Italiano](README.it.md) | [Русский](README.ru.md) | [العربية](README.ar.md) | [हिन्दी](README.hi.md) | **Türkçe** | [Tiếng Việt](README.vi.md) | [Bahasa Indonesia](README.id.md)

WPTSALL Client, WPMMCC ATS WordPress eklentisinin companion istemcisidir. Makinenizde çalışır — yerel bir web arayüzü ya da yerel bir masaüstü uygulaması olarak — her ayarı yerel bir veritabanında tutar, bir cihaz token'ı ile kendi WordPress'inize doğrudan bağlanır ve yapılandırdığınız herhangi bir HTTP(S) çeviri sağlayıcısını yönetir. Hesap yok, lisans yok, bulut bağımlılığı yok.

## Links
- Project website — https://www.wpmm.cc/
- Documentation and usage help (English / 简体中文) — https://www.wpmm.cc/docs/
- WPMMCC ATS plugin on WordPress.org — https://wordpress.org/plugins/wpmmcc-ats/
- Plugin source repository — https://github.com/wpmmcc/wpmmcc-ats
- Signed kits and installers — https://github.com/wpmmcc/wptsall-client-releases

## Özellikler
- İki ürün, tek çekirdek — 127.0.0.1:8977 üzerindeki yerel WebUI ve yerel masaüstü uygulaması (Tauri) aynı Rust çekirdeğini paylaşır
- Yerel-öncelik — site, sağlayıcı, bileşen ve kurallar yerel veritabanınızda yaşar, asla bulutta değil
- Doğrudan bağlantı — eklentinin Protocol v2 API'si ile WordPress'inizle konuşur
- Herhangi bir sağlayıcı — kendi HTTP(S) çeviri uç noktanızı ve kimlik bilgilerinizi getirin
- Worker — tek seferlik ve sürekli modlar, sınırlı yeniden denemeler ve idempotent callback'lerle
- Paketleme — imzalı OTA güncellemeleriyle çapraz platform kit'leri (Linux / Windows / macOS)

## Gereksinimler
- WPMMCC ATS eklentisi 2.x çalıştıran bir WordPress sitesi — https://github.com/wpmmcc/wpmmcc-ats
- Eklentinin verdiği bir cihaz token'ı ve route secret
- Bir çeviri sağlayıcı uç noktası (herhangi bir HTTP(S) API)

## Kurulum
1. https://github.com/wpmmcc/wptsall-client-releases adresinden imzalı kit ya da yükleyici alın
2. Ya da kaynaktan derleyin: client-wpplugin/source içinde cargo build --release (WebUI ikili dosyası) veya client-desktop içinde Tauri araç zinciri (masaüstü uygulaması)
3. WebUI ikili dosyasını başlatın ve http://127.0.0.1:8977 adresini açın

## Hızlı başlangıç
1. Sitenizi ekleyin — eklentinin client URL'sini, route secret ve cihaz token'ını yapıştırın
2. Bir çeviri sağlayıcı yapılandırın — uç nokta artı kimlik bilgileri, hepsi yerelde saklanır
3. Worker'ı bir kez çalıştırın — bir toplu işi alır, çevirir ve sonuçları geri yazar

## Yerel veriler ve gizlilik
Tüm yapılandırma ve görev durumu makinenizde kalır (SQLite artı yerel dosyalar). İstemci yalnızca yapılandırdığınız WordPress sitesi ve seçtiğiniz sağlayıcı uç noktasıyla iletişim kurar.

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
İstemci arayüzü kendi yerelleştirmesiyle gelir. Bu README 16 dilde mevcuttur — üstteki tabloya bakın. Daha fazla dil için katkılar memnuniyetle karşılanır.

## License
GPL-2.0-or-later. Bakınız [LICENSE](../LICENSE).


---

> This translation is an initial draft; corrections and improvements via pull requests are welcome.
