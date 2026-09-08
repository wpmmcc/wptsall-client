# WPTSALL Client — स्थानीय-प्रथम अनुवाद क्लाइंट (WebUI + Desktop)

**WPMMCC ATS WordPress प्लगइन का क्लाइंट — आपकी मशीन पर, क्लाउड पर नहीं।**

[English](../README.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Português (Brasil)](README.pt-BR.md) | [Italiano](README.it.md) | [Русский](README.ru.md) | [العربية](README.ar.md) | **हिन्दी** | [Türkçe](README.tr.md) | [Tiếng Việt](README.vi.md) | [Bahasa Indonesia](README.id.md)

WPTSALL Client, WPMMCC ATS WordPress प्लगइन का साथी क्लाइंट है। यह आपकी मशीन पर चलता है — स्थानीय वेब UI या नेटिव डेस्कटॉप ऐप के रूप में — हर सेटिंग स्थानीय डेटाबेस में रखता है, डिवाइस टोकन से आपके WordPress से सीधे जुड़ता है, और आपके विन्यस्त किसी भी HTTP(S) अनुवाद प्रोवाइडर को चलाता है। बिना खाते, बिना लाइसेंस, बिना क्लाउड निर्भरता।

## विशेषताएँ
- दो उत्पाद, एक कोर — स्थानीय WebUI (127.0.0.1:8977) और नेटिव Desktop ऐप (Tauri) एक ही Rust कोर साझा करते हैं
- स्थानीय-प्रथम — साइट, प्रोवाइडर, कंपोनेंट और नियम आपके स्थानीय डेटाबेस में रहते हैं, कभी क्लाउड में नहीं
- सीधा कनेक्शन — प्लगइन के Protocol v2 API से आपके WordPress से बात करता है
- कोई भी प्रोवाइडर — अपना HTTP(S) अनुवाद एंडपॉइंट और क्रेडेंशियल लाएँ
- Worker — एक-बार और निरंतर मोड, सीमित पुनःप्रयास और idempotent कॉलबैक के साथ
- पैकेजिंग — क्रॉस-प्लेटफ़ॉर्म kit (Linux / Windows / macOS) और हस्ताक्षरित OTA अपडेट

## आवश्यकताएँ
- WPMMCC ATS प्लगइन 2.x चलाती WordPress साइट — https://github.com/wpmmcc/wpmmcc-ats
- प्लगइन जारी किया डिवाइस टोकन और route secret
- एक अनुवाद प्रोवाइडर एंडपॉइंट (कोई भी HTTP(S) API)

## स्थापना
1. https://github.com/wpmmcc/wptsall-client-releases से हस्ताक्षरित kit या इंस्टॉलर लें
2. या स्रोत से बिल्ड करें: client-wpplugin/source में cargo build --release (WebUI बाइनरी), या client-desktop में Tauri टूलचेन (Desktop ऐप)
3. WebUI बाइनरी चालू करें और http://127.0.0.1:8977 खोलें

## त्वरित शुरुआत
1. अपनी साइट जोड़ें — प्लगइन का client URL, route secret और डिवाइस टोकन पेस्ट करें
2. अनुवाद प्रोवाइडर विन्यस्त करें — एंडपॉइंट और क्रेडेंशियल, सब स्थानीय रूप से सहेजे जाते हैं
3. Worker एक बार चलाएँ — यह बैच लेता है, अनुवाद करता है और परिणाम लिखता है

## स्थानीय डेटा और गोपनीयता
सारी विन्यास और कार्य स्थिति आपकी मशीन पर रहती है (SQLite + स्थानीय फ़ाइलें)। क्लाइंट केवल आपके विन्यस्त WordPress साइट और चुने गए प्रोवाइडर एंडपॉइंट से संवाद करता है।

## Languages
क्लाइंट UI अपना स्थानीयकरण साथ लाता है। यह README 16 भाषाओं में उपलब्ध है — शीर्ष की तालिका देखें। अधिक भाषाओं के योगदान का स्वागत है।

## License
GPL-2.0-or-later। [LICENSE](../LICENSE) देखें।


---

> This translation is an initial draft; corrections and improvements via pull requests are welcome.
