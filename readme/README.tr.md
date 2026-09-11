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
Tüm yapılandırma ve görev durumu makinenizde kalır (SQLite artı yerel dosyalar). İstemci yalnızca yapılandırdığınız WordPress sitesi ve seçtiğiniz sağlayıcı uç noktasıyla iletişim kurar. Hata ayıklama günlükleri varsayılan olarak kapalıdır; Ayarlar sayfası çalışma zamanında yerel günlük kaydını açıp kapatabilir.

## Güncelleme
- WebUI'de Ayarlar'ı açın ve güncelleme kontrolünü kullanın — yeni sürümler imzalı kit'ler olarak indirilir ve yerinde uygulanır
- Her kit herhangi bir dosya değiştirilmeden önce doğrulanır: minisign imzası ve SHA-256 sağlama toplamı ile geri alma koruması. Hizmet sonrasında otomatik olarak yeniden başlar; Windows'ta çalışan ikili dosya güvenli bir şekilde değiştirilir ve güncelleme başarısız olursa geri alınır
- Manuel alternatif: https://github.com/wpmmcc/wptsall-client-releases adresinden en son yükleyiciyi indirin ve mevcut kurulumun üzerine çalıştırın

## Kaldırma
- Her ürün ve platform için kaldırıcılar releases deposunda mevcuttur: uninstall-webui.sh / uninstall-desktop.sh (Linux, macOS) ve eşleşen .ps1 komut dosyaları (Windows)
- Kaldırıcı, arka plan hizmetini (systemd kullanıcı birimi, LaunchAgent veya Windows hizmeti), komut satırı kısayollarını ve kurulum dizinini durdurur ve kaldırır
- Verileriniz — yerel SQLite veritabanı, yapılandırma ve günlükler — varsayılan olarak saklanır. Bunları da kaldırmak için --purge-data (Linux/macOS) veya -PurgeData (Windows) ekleyin
- Hem WebUI hem de Desktop uygulamasına sahipseniz, --purge-shared seçeneğini de geçmediğiniz sürece paylaşılan kurulum dizini korunur

## Açık kaynaklı bileşenler
- Rust çekirdeği — tokio, reqwest (rustls TLS), serde/serde_json, dahili SQLite ile rusqlite, kripto için aes-gcm/hkdf/sha2/hmac/rsa/jsonwebtoken, S3 uyumlu sağlayıcılar için aws-sdk-s3, WASM bileşen çalışma zamanı extism
- Desktop uygulaması — Tauri 2 (sistem webview'ını kullanan yerel kabuk)
- Web UI — Svelte 5, Vite, Tailwind CSS, svelte-i18n ve lucide simgeleri
- Güncelleme güvenliği — minisign imzaları ve imzalı SHA256SUMS
- Sürümleri sabitlenmiş tam listeler Cargo.toml ve frontend/package.json içinde yer alır; her bileşen bu projenin GPL-2.0-or-later lisansıyla uyumlu MIT / Apache-2.0 / ISC tarzı lisanslarla dağıtılır

## Languages
İstemci arayüzü kendi yerelleştirmesiyle gelir. Bu README 16 dilde mevcuttur — üstteki tabloya bakın. Daha fazla dil için katkılar memnuniyetle karşılanır.

## License
GPL-2.0-or-later. Bakınız [LICENSE](../LICENSE).


---

> This translation is an initial draft; corrections and improvements via pull requests are welcome.
