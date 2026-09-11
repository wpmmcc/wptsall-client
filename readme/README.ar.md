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
تبقى كل الإعدادات وحالة المهام على جهازك (SQLite مع ملفات محلية). يتواصل العميل فقط مع موقع ووردبريس الذي ضبطته ونقطة نهاية المزوّد التي اخترتها. سجل التصحيح معطّل افتراضيًا؛ وتتيح صفحة الإعدادات تشغيل أو إيقاف السجلات المحلية أثناء التشغيل.

## التحديث
- افتح الإعدادات في WebUI واستخدم فحص التحديثات — يتم تنزيل الإصدارات الجديدة كحزم kit موقّعة وتطبيقها في مكانها
- يتم التحقق من كل حزمة kit بدقة قبل استبدال أي ملف: توقيع minisign ومجموع تدقيق SHA-256، مع حماية ضد الرجوع إلى إصدار أقدم. يُعاد تشغيل الخدمة تلقائيًا بعدها؛ وفي نظام Windows يُستبدل الملف الثنائي المشغّل بأمان مع إمكانية التراجع عند الفشل
- بديل يدوي: نزّل أحدث مثبت من https://github.com/wpmmcc/wptsall-client-releases وشغّله فوق التثبيت الحالي

## إلغاء التثبيت
- تتوفر أدوات إلغاء التثبيت لكل منتج ومنصة في مستودع الإصدارات: uninstall-webui.sh / uninstall-desktop.sh (Linux وmacOS) وملفات .ps1 المقابلة (Windows)
- توقف أداة إلغاء التثبيت خدمة الخلفية وتزيلها (وحدة مستخدم systemd أو LaunchAgent أو خدمة Windows)، واختصارات سطر الأوامر ودليل التثبيت
- يتم الاحتفاظ ببياناتك (قاعدة بيانات SQLite المحلية، التكوين، السجلات) افتراضيًا. أضف --purge-data (Linux/macOS) أو -PurgeData (Windows) لحذفها أيضًا
- إذا كان لديك كل من WebUI وتطبيق Desktop، يُحتفظ بدليل التثبيت المشترك ما لم تُمرر أيضًا --purge-shared

## المكونات مفتوحة المصدر
- نواة Rust — مكتبات tokio وreqwest (عبر rustls TLS) وserde/serde_json وrusqlite مع SQLite مدمج، وaes-gcm/hkdf/sha2/hmac/rsa/jsonwebtoken للتشفير، وaws-sdk-s3 للمزودين المتوافقين مع S3، وextism لتشغيل مكونات WASM
- تطبيق Desktop — إطار Tauri 2 (واجهة أصلية تعتمد على webview النظام)
- واجهة Web UI — مكتبات Svelte 5 وVite وTailwind CSS وsvelte-i18n وأيقونات lucide
- أمان التحديثات — توقيعات minisign وملف SHA256SUMS موقّع
- القوائم الكاملة بالإصدارات المثبتة موجودة في Cargo.toml وfrontend/package.json؛ وتصدر جميع المكونات بتراخيص متوافقة مع GPL-2.0-or-later مثل MIT وApache-2.0 وISC

## Languages
واجهة العميل تأتي مع تعريبها الخاص. هذا README متوفر بـ16 لغة — انظر الجدول أعلاه. المساهمات بلغات أخرى مرحب بها.

## License
GPL-2.0-or-later. انظر [LICENSE](../LICENSE).


---

> This translation is an initial draft; corrections and improvements via pull requests are welcome.
