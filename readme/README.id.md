# WPTSALL Client — Klien terjemahan lokal-first (WebUI + Desktop)

**Klien untuk plugin WordPress WPMMCC ATS — di mesin Anda, bukan di cloud.**

[English](../README.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Português (Brasil)](README.pt-BR.md) | [Italiano](README.it.md) | [Русский](README.ru.md) | [العربية](README.ar.md) | [हिन्दी](README.hi.md) | [Türkçe](README.tr.md) | [Tiếng Việt](README.vi.md) | **Bahasa Indonesia**

WPTSALL Client adalah klien pendamping plugin WordPress WPMMCC ATS. Ia berjalan di mesin Anda — sebagai antarmuka web lokal atau aplikasi desktop native — menyimpan setiap pengaturan di basis data lokal, terhubung langsung ke WordPress Anda dengan token perangkat, dan menggerakkan penyedia terjemahan HTTP(S) apa pun yang Anda konfigurasikan. Tanpa akun, tanpa lisensi, tanpa ketergantungan cloud.

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

## Languages
Antarmuka klien membawa pelokalannya sendiri. README ini tersedia dalam 16 bahasa — lihat tabel di atas. Kontribusi bahasa lain diterima dengan senang hati.

## License
GPL-2.0-or-later. Lihat [LICENSE](../LICENSE).


---

> This translation is an initial draft; corrections and improvements via pull requests are welcome.
