# WPTSALL Client — स्थानीय-प्रथम अनुवाद क्लाइंट (WebUI + Desktop)

**WPMMCC ATS WordPress प्लगइन का क्लाइंट — आपकी मशीन पर, क्लाउड पर नहीं।**

[English](../README.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Português (Brasil)](README.pt-BR.md) | [Italiano](README.it.md) | [Русский](README.ru.md) | [العربية](README.ar.md) | **हिन्दी** | [Türkçe](README.tr.md) | [Tiếng Việt](README.vi.md) | [Bahasa Indonesia](README.id.md)

WPTSALL Client, WPMMCC ATS WordPress प्लगइन का साथी क्लाइंट है। यह आपकी मशीन पर चलता है — स्थानीय वेब UI या नेटिव डेस्कटॉप ऐप के रूप में — हर सेटिंग स्थानीय डेटाबेस में रखता है, डिवाइस टोकन से आपके WordPress से सीधे जुड़ता है, और आपके विन्यस्त किसी भी HTTP(S) अनुवाद प्रोवाइडर को चलाता है। बिना खाते, बिना लाइसेंस, बिना क्लाउड निर्भरता।

## Links
- Project website — https://www.wpmm.cc/
- Documentation and usage help (English / 简体中文) — https://www.wpmm.cc/docs/
- WPMMCC ATS plugin on WordPress.org — https://wordpress.org/plugins/wpmmcc-ats/
- Plugin source repository — https://github.com/wpmmcc/wpmmcc-ats
- Signed kits and installers — https://github.com/wpmmcc/wptsall-client-releases

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

## डिफ़ॉल्ट पोर्ट
- WebUI डिफ़ॉल्ट रूप से 127.0.0.1:8977 पर सुनती है —— केवल लोकल लूपबैक पर, आपके नेटवर्क पर कभी खुली नहीं
- 8977, IANA में पंजीकृत नहीं है और आम सेवा/डेवलपमेंट पोर्ट (3306, 5432, 6379, 8080, 9000, 9200…) से बचता है, इसलिए टकराव दुर्लभ हैं
- यदि पोर्ट पहले से व्यस्त है, तो स्टार्टअप bind web ui failed के साथ विफल होता है —— WPTSALL_WEB_UI_PORT या WPTSALL_WEB_UI_BIND में दूसरा पोर्ट सेट करें और सेवा पुनः आरंभ करें

## स्थापना
1. https://github.com/wpmmcc/wptsall-client-releases से हस्ताक्षरित kit या इंस्टॉलर लें
2. या स्रोत से बिल्ड करें: client-wpplugin/source में cargo build --release (WebUI बाइनरी), या client-desktop में Tauri टूलचेन (Desktop ऐप)
3. WebUI बाइनरी चालू करें और http://127.0.0.1:8977 खोलें

## त्वरित शुरुआत
1. अपनी साइट जोड़ें — प्लगइन का client URL, route secret और डिवाइस टोकन पेस्ट करें
2. अनुवाद प्रोवाइडर विन्यस्त करें — एंडपॉइंट और क्रेडेंशियल, सब स्थानीय रूप से सहेजे जाते हैं
3. Worker एक बार चलाएँ — यह बैच लेता है, अनुवाद करता है और परिणाम लिखता है

## स्थानीय डेटा और गोपनीयता
सारी विन्यास और कार्य स्थिति आपकी मशीन पर रहती है (SQLite + स्थानीय फ़ाइलें)। क्लाइंट केवल आपके विन्यस्त WordPress साइट और चुने गए प्रोवाइडर एंडपॉइंट से संवाद करता है। डिबग लॉगिंग डिफ़ॉल्ट रूप से बंद है; सेटिंग्स पृष्ठ से रनटाइम पर स्थानीय लॉगिंग चालू या बंद की जा सकती है।

## अपडेट करना
- WebUI में सेटिंग्स खोलें और अपडेट जांच का उपयोग करें — नए संस्करण हस्ताक्षरित kit के रूप में डाउनलोड होते हैं और उसी स्थान पर लागू होते हैं
- फ़ाइलों को बदलने से पहले प्रत्येक kit को सत्यापित किया जाता है: minisign हस्ताक्षर और SHA-256 चेकसम, साथ ही एंटी-रोलबैक सुरक्षा। इसके बाद सेवा स्वतः पुनरारंभ होती है; Windows पर चल रही बाइनरी सुरक्षित रूप से बदल दी जाती है और विफलता पर स्वतः रोलबैक होता है
- मैन्युअल विकल्प: https://github.com/wpmmcc/wptsall-client-releases से नवीनतम इंस्टॉलर डाउनलोड करें और मौजूदा इंस्टॉलेशन पर चलाएं

## अनइंस्टॉल करना
- प्रत्येक उत्पाद और प्लेटफ़ॉर्म के अनइंस्टॉलर रिलीज़ रिपॉज़िटरी में उपलब्ध हैं: uninstall-webui.sh / uninstall-desktop.sh (Linux, macOS) और संबंधित .ps1 स्क्रिप्ट (Windows)
- अनइंस्टॉलर बैकग्राउंड सर्विस (systemd यूजर यूनिट, LaunchAgent या Windows सर्विस), कमांड-लाइन शॉर्टकट और इंस्टॉल डायरेक्टरी को रोकता और हटाता है
- आपका डेटा — स्थानीय SQLite डेटाबेस, कॉन्फ़िगरेशन और लॉग — डिफ़ॉल्ट रूप से सुरक्षित रहता है। इसे भी हटाने के लिए --purge-data (Linux/macOS) या -PurgeData (Windows) जोड़ें
- यदि आपके पास WebUI और Desktop ऐप दोनों हैं, तो साझा इंस्टॉल डायरेक्टरी सुरक्षित रहती है जब तक कि आप --purge-shared भी पास न करें

## ओपन-सोर्स घटक
- Rust कोर — tokio, reqwest (rustls TLS), serde/serde_json, rusqlite बंडल SQLite सहित, क्रिप्टोग्राफी के लिए aes-gcm/hkdf/sha2/hmac/rsa/jsonwebtoken, S3-संगत प्रोवाइडर के लिए aws-sdk-s3, WASM घटक रनटाइम extism
- Desktop ऐप — Tauri 2 (सिस्टम वेबव्यू का उपयोग करने वाला नेटिव शेल)
- Web UI — Svelte 5, Vite, Tailwind CSS, svelte-i18n और lucide आइकन
- अपडेट सुरक्षा — minisign हस्ताक्षर और हस्ताक्षरित SHA256SUMS
- संस्करण-पिन की गई पूरी सूची Cargo.toml और frontend/package.json में है; प्रत्येक घटक GPL-2.0-or-later संगत MIT / Apache-2.0 / ISC लाइसेंस के तहत जारी किया गया है

## Languages
क्लाइंट UI अपना स्थानीयकरण साथ लाता है। यह README 16 भाषाओं में उपलब्ध है — शीर्ष की तालिका देखें। अधिक भाषाओं के योगदान का स्वागत है।

## License
GPL-2.0-or-later। [LICENSE](../LICENSE) देखें।


---

> This translation is an initial draft; corrections and improvements via pull requests are welcome.
