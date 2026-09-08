# WPTSALL Client — Client dịch thuật local-first (WebUI + Desktop)

**Client của plugin WordPress WPMMCC ATS — trên máy bạn, không phải trên cloud.**

[English](../README.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Português (Brasil)](README.pt-BR.md) | [Italiano](README.it.md) | [Русский](README.ru.md) | [العربية](README.ar.md) | [हिन्दी](README.hi.md) | [Türkçe](README.tr.md) | **Tiếng Việt** | [Bahasa Indonesia](README.id.md)

WPTSALL Client là client đi kèm của plugin WordPress WPMMCC ATS. Nó chạy trên máy của bạn — dưới dạng giao diện web cục bộ hoặc ứng dụng desktop gốc — lưu mọi cấu hình trong cơ sở dữ liệu cục bộ, kết nối trực tiếp tới WordPress của bạn bằng token thiết bị, và điều khiển bất kỳ nhà cung cấp dịch HTTP(S) nào bạn cấu hình. Không tài khoản, không giấy phép, không phụ thuộc cloud.

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

## Languages
Giao diện client có sẵn bản địa hóa riêng. README này có 16 ngôn ngữ — xem bảng ở đầu trang. Hoan nghênh đóng góp thêm ngôn ngữ.

## License
GPL-2.0-or-later. Xem [LICENSE](../LICENSE).


---

> This translation is an initial draft; corrections and improvements via pull requests are welcome.
