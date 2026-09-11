# WPTSALL Client — عميل ترجمة محلي أولاً (WebUI + Desktop)

**عميل إضافة WPMMCC ATS لووردبريس — على جهازك، لا في السحابة.**

[English](../README.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Português (Brasil)](README.pt-BR.md) | [Italiano](README.it.md) | [Русский](README.ru.md) | **العربية** | [हिन्दी](README.hi.md) | [Türkçe](README.tr.md) | [Tiếng Việt](README.vi.md) | [Bahasa Indonesia](README.id.md)

WPTSALL Client هو العميل المرافق لإضافة WPMMCC ATS لووردبريس. يعمل على جهازك — بواجهة ويب محلية أو تطبيق سطح مكتب أصلي — ويحفظ كل الإعدادات في قاعدة بيانات محلية، ويتصل مباشرة بووردبريس الخاص بك برمز جهاز، ويشغّل أي مزوّد ترجمة HTTP(S) تضبطه. دون حساب أو ترخيص أو اعتماد على السحابة.

## Links
- Project website — https://www.wpmm.cc/
- Documentation and usage help (English / 简体中文) — https://www.wpmm.cc/docs/
- WPMMCC ATS plugin on WordPress.org — https://wordpress.org/plugins/wpmmcc-ats/
- Plugin source repository — https://github.com/wpmmcc/wpmmcc-ats
- Signed kits and installers — https://github.com/wpmmcc/wptsall-client-releases

## الميزات
- منتجان ونواة واحدة — واجهة ويب محلية على 127.0.0.1:8977 وتطبيق سطح مكتب أصلي (Tauri) يتشاركان نواة Rust نفسها
- محلي أولاً — المواقع والمزوّدون والمكونات والقواعد تبقى في قاعدتك المحلية، لا في السحابة أبدًا
- اتصال مباشر — يتحدث مع ووردبريس عبر واجهة Protocol v2 الخاصة بالإضافة
- أي مزوّد — أحضر نقطة نهاية الترجمة HTTP(S) الخاصة بك وبيانات الاعتماد
- العامل — وضعا التشغيل مرة واحدة والمستمر، مع إعادة محاولة محدودة وردود ختامية idempotent
- التغليف — حزم متعددة المنصات (Linux / Windows / macOS) مع تحديثات OTA موقّعة

## المتطلبات
- موقع ووردبريس يعمل بإضافة WPMMCC ATS 2.x — https://github.com/wpmmcc/wpmmcc-ats
- رمز جهاز وroute secret تصدرهما الإضافة
- نقطة نهاية مزوّد ترجمة (أي HTTP(S) API)

## التثبيت
1. احصل على kit موقّع أو مثبّت من https://github.com/wpmmcc/wptsall-client-releases
2. أو ابنِ من المصدر: cargo build --release داخل client-wpplugin/source (ثنائي WebUI)، أو سلسلة أدوات Tauri داخل client-desktop (تطبيق سطح المكتب)
3. شغّل ثنائي WebUI وافتح http://127.0.0.1:8977

## البداية السريعة
1. أضف موقعك — الصق عنوان client الخاص بالإضافة وroute secret ورمز الجهاز
2. اضبط مزوّد ترجمة — نقطة النهاية مع بيانات الاعتماد، محفوظة محليًا
3. شغّل العامل مرة واحدة — يستلم دفعة ويترجمها ويكتب النتائج

## البيانات المحلية والخصوصية
تبقى كل الإعدادات وحالة المهام على جهازك (SQLite مع ملفات محلية). يتواصل العميل فقط مع موقع ووردبريس الذي ضبطته ونقطة نهاية المزوّد التي اخترتها.

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
واجهة العميل تأتي مع تعريبها الخاص. هذا README متوفر بـ16 لغة — انظر الجدول أعلاه. المساهمات بلغات أخرى مرحب بها.

## License
GPL-2.0-or-later. انظر [LICENSE](../LICENSE).


---

> This translation is an initial draft; corrections and improvements via pull requests are welcome.
