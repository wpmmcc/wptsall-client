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
Semua konfigurasi dan status tugas tetap di mesin Anda (SQLite plus berkas lokal). Klien hanya berkomunikasi dengan situs WordPress yang Anda konfigurasi dan endpoint penyedia yang Anda pilih. Pencatatan debug dimatikan secara default; halaman Pengaturan dapat mengaktifkan atau menonaktifkan pencatatan lokal saat runtime.

## Pembaruan
- Buka Pengaturan di WebUI dan gunakan pemeriksaan pembaruan — versi baru diunduh sebagai kit bertanda tangan dan diterapkan langsung di tempat
- Setiap kit diverifikasi sebelum file diganti: tanda tangan minisign ditambah checksum SHA-256, dengan perlindungan anti-rollback. Layanan dimulai ulang secara otomatis setelahnya; di Windows biner yang sedang berjalan diganti dengan aman, dengan rollback jika pembaruan gagal
- Alternatif manual: unduh penginstal terbaru dari https://github.com/wpmmcc/wptsall-client-releases dan jalankan di atas instalasi yang ada

## Copot Pemasangan
- Pencopot pemasangan untuk setiap produk dan platform tersedia di repositori rilis: uninstall-webui.sh / uninstall-desktop.sh (Linux, macOS) dan skrip .ps1 yang cocok (Windows)
- Pencopot pemasangan menghentikan dan menghapus layanan latar belakang (unit pengguna systemd, LaunchAgent, atau layanan Windows), pintasan baris perintah, dan direktori instalasi
- Data Anda — basis data SQLite lokal, konfigurasi, dan log — disimpan secara default. Tambahkan --purge-data (Linux/macOS) atau -PurgeData (Windows) untuk menghapusnya juga
- Jika Anda memiliki WebUI dan aplikasi Desktop, direktori instalasi bersama tetap dipertahankan kecuali Anda juga menyertakan opsi --purge-shared

## Komponen sumber terbuka
- Inti Rust — tokio, reqwest (rustls TLS), serde/serde_json, rusqlite dengan SQLite bawaan, aes-gcm/hkdf/sha2/hmac/rsa/jsonwebtoken untuk kriptografi, aws-sdk-s3 untuk penyedia yang kompatibel dengan S3, extism sebagai runtime komponen WASM
- Aplikasi Desktop — Tauri 2 (shell native menggunakan webview sistem)
- Web UI — Svelte 5, Vite, Tailwind CSS, svelte-i18n dan ikon lucide
- Keamanan pembaruan — tanda tangan minisign dan SHA256SUMS bertanda tangan
- Daftar lengkap versi terkunci ada di Cargo.toml dan frontend/package.json; setiap komponen dilisensikan di bawah lisensi gaya MIT / Apache-2.0 / ISC yang kompatibel dengan GPL-2.0-or-later proyek ini

## Languages
Antarmuka klien membawa pelokalannya sendiri. README ini tersedia dalam 16 bahasa — lihat tabel di atas. Kontribusi bahasa lain diterima dengan senang hati.

## License
GPL-2.0-or-later. Lihat [LICENSE](../LICENSE).


---

> This translation is an initial draft; corrections and improvements via pull requests are welcome.
