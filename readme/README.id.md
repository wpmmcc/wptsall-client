# WPTSALL Client — Klien terjemahan lokal-first (WebUI + Desktop)

**Klien untuk plugin WordPress WPMMCC ATS — di mesin Anda, bukan di cloud.**

[English](../README.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Português (Brasil)](README.pt-BR.md) | [Italiano](README.it.md) | [Русский](README.ru.md) | [العربية](README.ar.md) | [हिन्दी](README.hi.md) | [Türkçe](README.tr.md) | [Tiếng Việt](README.vi.md) | **Bahasa Indonesia**

WPTSALL Client adalah klien pendamping plugin WordPress WPMMCC ATS. Ia berjalan di mesin Anda — sebagai antarmuka web lokal atau aplikasi desktop native — menyimpan setiap pengaturan di basis data lokal, terhubung langsung ke WordPress Anda dengan token perangkat, dan menggerakkan penyedia terjemahan HTTP(S) apa pun yang Anda konfigurasikan. Tanpa akun, tanpa lisensi, tanpa ketergantungan cloud.

## Links
- Project website — https://www.wpmm.cc/
- Documentation and usage help (English / 简体中文) — https://www.wpmm.cc/docs/
- WPMMCC ATS plugin on WordPress.org — https://wordpress.org/plugins/wpmmcc-ats/
- Plugin source repository — https://github.com/wpmmcc/wpmmcc-ats
- Signed kits and installers — https://github.com/wpmmcc/wptsall-client-releases

## Fitur
- Dua produk, satu inti — WebUI lokal di 127.0.0.1:8977 dan aplikasi desktop native (Tauri) berbagi inti Rust yang sama
- Lokal-first — situs, penyedia, komponen, dan aturan hidup di basis data lokal Anda, tidak pernah di cloud
- Koneksi langsung — berbicara dengan WordPress Anda lewat API Protocol v2 milik plugin
- Penyedia apa pun — bawa endpoint terjemahan HTTP(S) dan kredensial Anda sendiri
- Worker — mode sekali jalan dan berkelanjutan, dengan percobaan ulang terbatas dan callback idempoten
- Pengemasan — kit lintas platform (Linux / Windows / macOS) dengan pembaruan OTA bertanda tangan

## Persyaratan
- Situs WordPress dengan plugin WPMMCC ATS 2.x — https://github.com/wpmmcc/wpmmcc-ats
- Token perangkat dan route secret yang diterbitkan plugin
- Endpoint penyedia terjemahan (API HTTP(S) apa pun)

## Instalasi
1. Ambil kit bertanda tangan atau installer di https://github.com/wpmmcc/wptsall-client-releases
2. Atau bangun dari sumber: cargo build --release di dalam client-wpplugin/source (biner WebUI), atau toolchain Tauri di dalam client-desktop (aplikasi desktop)
3. Jalankan biner WebUI dan buka http://127.0.0.1:8977

## Mulai cepat
1. Tambahkan situs Anda — tempel URL klien plugin, route secret, dan token perangkat
2. Konfigurasikan penyedia terjemahan — endpoint plus kredensial, semuanya tersimpan lokal
3. Jalankan Worker sekali — ia mengambil satu batch, menerjemahkan, dan menulis hasilnya kembali

## Data lokal & privasi
Semua konfigurasi dan status tugas tetap di mesin Anda (SQLite plus berkas lokal). Klien hanya berkomunikasi dengan situs WordPress yang Anda konfigurasi dan endpoint penyedia yang Anda pilih.

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
Antarmuka klien membawa pelokalannya sendiri. README ini tersedia dalam 16 bahasa — lihat tabel di atas. Kontribusi bahasa lain diterima dengan senang hati.

## License
GPL-2.0-or-later. Lihat [LICENSE](../LICENSE).


---

> This translation is an initial draft; corrections and improvements via pull requests are welcome.
