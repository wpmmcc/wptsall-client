# WPTSALL Client — Client dịch thuật local-first (WebUI + Desktop)

**Client của plugin WordPress WPMMCC ATS — trên máy bạn, không phải trên cloud.**

[English](../README.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Português (Brasil)](README.pt-BR.md) | [Italiano](README.it.md) | [Русский](README.ru.md) | [العربية](README.ar.md) | [हिन्दी](README.hi.md) | [Türkçe](README.tr.md) | **Tiếng Việt** | [Bahasa Indonesia](README.id.md)

WPTSALL Client là client đi kèm của plugin WordPress WPMMCC ATS. Nó chạy trên máy của bạn — dưới dạng giao diện web cục bộ hoặc ứng dụng desktop gốc — lưu mọi cấu hình trong cơ sở dữ liệu cục bộ, kết nối trực tiếp tới WordPress của bạn bằng token thiết bị, và điều khiển bất kỳ nhà cung cấp dịch HTTP(S) nào bạn cấu hình. Không tài khoản, không giấy phép, không phụ thuộc cloud.

## Links
- Project website — https://www.wpmm.cc/
- Documentation and usage help (English / 简体中文) — https://www.wpmm.cc/docs/
- WPMMCC ATS plugin on WordPress.org — https://wordpress.org/plugins/wpmmcc-ats/
- Plugin source repository — https://github.com/wpmmcc/wpmmcc-ats
- Signed kits and installers — https://github.com/wpmmcc/wptsall-client-releases

## Tính năng
- Hai sản phẩm, một lõi — WebUI cục bộ tại 127.0.0.1:8977 và ứng dụng desktop gốc (Tauri) dùng chung lõi Rust
- Local-first — site, nhà cung cấp, thành phần và quy tắc nằm trong CSDL cục bộ, không bao giờ trên cloud
- Kết nối trực tiếp — trò chuyện với WordPress của bạn qua API Protocol v2 của plugin
- Mọi nhà cung cấp — tự mang endpoint dịch HTTP(S) và thông tin xác thực
- Worker — chế độ chạy một lần và liên tục, retry có giới hạn và callback idempotent
- Đóng gói — kit đa nền tảng (Linux / Windows / macOS) kèm cập nhật OTA có chữ ký

## Yêu cầu
- Một site WordPress chạy plugin WPMMCC ATS 2.x — https://github.com/wpmmcc/wpmmcc-ats
- Token thiết bị và route secret do plugin cấp
- Một endpoint nhà cung cấp dịch (bất kỳ API HTTP(S) nào)

## Cài đặt
1. Lấy kit có chữ ký hoặc trình cài đặt tại https://github.com/wpmmcc/wptsall-client-releases
2. Hoặc build từ mã nguồn: cargo build --release trong client-wpplugin/source (binary WebUI), hoặc chuỗi công cụ Tauri trong client-desktop (ứng dụng desktop)
3. Khởi động binary WebUI và mở http://127.0.0.1:8977

## Bắt đầu nhanh
1. Thêm site của bạn — dán client URL của plugin, route secret và token thiết bị
2. Cấu hình nhà cung cấp dịch — endpoint cùng thông tin xác thực, lưu cục bộ
3. Chạy Worker một lần — nó nhận một lô, dịch và ghi kết quả về

## Dữ liệu cục bộ & riêng tư
Mọi cấu hình và trạng thái tác vụ đều ở trên máy bạn (SQLite cộng tệp cục bộ). Client chỉ giao tiếp với site WordPress bạn cấu hình và endpoint nhà cung cấp bạn chọn.

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
Giao diện client có sẵn bản địa hóa riêng. README này có 16 ngôn ngữ — xem bảng ở đầu trang. Hoan nghênh đóng góp thêm ngôn ngữ.

## License
GPL-2.0-or-later. Xem [LICENSE](../LICENSE).


---

> This translation is an initial draft; corrections and improvements via pull requests are welcome.
