# WPTSALL Client — عميل ترجمة محلي أولاً (WebUI + Desktop)

**عميل إضافة WPMMCC ATS لووردبريس — على جهازك، لا في السحابة.**

[English](../README.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Português (Brasil)](README.pt-BR.md) | [Italiano](README.it.md) | [Русский](README.ru.md) | **العربية** | [हिन्दी](README.hi.md) | [Türkçe](README.tr.md) | [Tiếng Việt](README.vi.md) | [Bahasa Indonesia](README.id.md)

WPTSALL Client هو العميل المرافق لإضافة WPMMCC ATS لووردبريس. يعمل على جهازك — بواجهة ويب محلية أو تطبيق سطح مكتب أصلي — ويحفظ كل الإعدادات في قاعدة بيانات محلية، ويتصل مباشرة بووردبريس الخاص بك برمز جهاز، ويشغّل أي مزوّد ترجمة HTTP(S) تضبطه. دون حساب أو ترخيص أو اعتماد على السحابة.

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

## Languages
واجهة العميل تأتي مع تعريبها الخاص. هذا README متوفر بـ16 لغة — انظر الجدول أعلاه. المساهمات بلغات أخرى مرحب بها.

## License
GPL-2.0-or-later. انظر [LICENSE](../LICENSE).


---

> This translation is an initial draft; corrections and improvements via pull requests are welcome.
