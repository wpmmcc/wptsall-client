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

## Cổng mặc định
- WebUI mặc định lắng nghe trên 127.0.0.1:8977 — chỉ trên loopback cục bộ, không bao giờ mở ra mạng
- Cổng 8977 chưa được IANA đăng ký và tránh các cổng dịch vụ/phát triển phổ biến (3306, 5432, 6379, 8080, 9000, 9200…), nên xung đột hiếm khi xảy ra
- Nếu cổng đã bị chiếm, khởi động sẽ lỗi bind web ui failed — đặt WPTSALL_WEB_UI_PORT hoặc WPTSALL_WEB_UI_BIND sang cổng khác rồi khởi động lại dịch vụ

## Cài đặt
1. Lấy kit có chữ ký hoặc trình cài đặt tại https://github.com/wpmmcc/wptsall-client-releases
2. Hoặc build từ mã nguồn: cargo build --release trong client-wpplugin/source (binary WebUI), hoặc chuỗi công cụ Tauri trong client-desktop (ứng dụng desktop)
3. Khởi động binary WebUI và mở http://127.0.0.1:8977

## Bắt đầu nhanh
1. Thêm site của bạn — dán client URL của plugin, route secret và token thiết bị
2. Cấu hình nhà cung cấp dịch — endpoint cùng thông tin xác thực, lưu cục bộ
3. Chạy Worker một lần — nó nhận một lô, dịch và ghi kết quả về

## Dữ liệu cục bộ & riêng tư
Mọi cấu hình và trạng thái tác vụ đều ở trên máy bạn (SQLite cộng tệp cục bộ). Client chỉ giao tiếp với site WordPress bạn cấu hình và endpoint nhà cung cấp bạn chọn. Nhật ký gỡ lỗi được tắt theo mặc định; trang Cài đặt có thể bật hoặc tắt nhật ký cục bộ trong thời gian chạy.

## Cập nhật
- Mở Cài đặt trong WebUI và sử dụng tính năng kiểm tra cập nhật — các phiên bản mới được tải về dưới dạng kit có chữ ký và áp dụng trực tiếp
- Mọi kit đều được xác minh nghiêm ngặt trước khi thay thế: chữ ký minisign và mã tổng kiểm SHA-256, cùng cơ chế chống rollback. Dịch vụ tự động khởi động lại sau đó; trên Windows, tệp thực thi đang chạy được thay thế an toàn và tự động hoàn tác nếu cập nhật thất bại
- Phương án thủ công: tải trình cài đặt mới nhất từ https://github.com/wpmmcc/wptsall-client-releases và chạy đè lên bản cài đặt hiện tại

## Gỡ cài đặt
- Bộ gỡ cài đặt cho từng sản phẩm và nền tảng có trong kho lưu trữ releases: uninstall-webui.sh / uninstall-desktop.sh (Linux, macOS) và script .ps1 tương ứng (Windows)
- Trình gỡ cài đặt sẽ dừng và gỡ bỏ dịch vụ nền (systemd user unit, LaunchAgent hoặc Windows service), các lối tắt dòng lệnh và thư mục cài đặt
- Dữ liệu của bạn — cơ sở dữ liệu SQLite cục bộ, cấu hình và nhật ký — được giữ lại theo mặc định. Thêm --purge-data (Linux/macOS) hoặc -PurgeData (Windows) để xóa toàn bộ
- Nếu bạn cài đặt cả WebUI và Desktop, thư mục cài đặt dùng chung sẽ được giữ lại trừ khi bạn truyền thêm tùy chọn --purge-shared

## Thành phần mã nguồn mở
- Lõi Rust — tokio, reqwest (rustls TLS), serde/serde_json, rusqlite tích hợp sẵn SQLite, aes-gcm/hkdf/sha2/hmac/rsa/jsonwebtoken cho mã hóa, aws-sdk-s3 cho nhà cung cấp tương thích S3, extism cho runtime thành phần WASM
- Ứng dụng Desktop — Tauri 2 (vỏ bọc gốc sử dụng webview của hệ thống)
- Web UI — Svelte 5, Vite, Tailwind CSS, svelte-i18n và bộ biểu tượng lucide
- Bảo mật cập nhật — chữ ký minisign và tệp SHA256SUMS có chữ ký
- Danh sách phiên bản cố định đầy đủ có trong Cargo.toml và frontend/package.json; mọi thành phần đều phát hành theo giấy phép tương thích GPL-2.0-or-later như MIT / Apache-2.0 / ISC

## Languages
Giao diện client có sẵn bản địa hóa riêng. README này có 16 ngôn ngữ — xem bảng ở đầu trang. Hoan nghênh đóng góp thêm ngôn ngữ.

## License
GPL-2.0-or-later. Xem [LICENSE](../LICENSE).


---

> This translation is an initial draft; corrections and improvements via pull requests are welcome.
