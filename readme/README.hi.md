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
क्लाइंट UI अपना स्थानीयकरण साथ लाता है। यह README 16 भाषाओं में उपलब्ध है — शीर्ष की तालिका देखें। अधिक भाषाओं के योगदान का स्वागत है।

## License
GPL-2.0-or-later। [LICENSE](../LICENSE) देखें।


---

> This translation is an initial draft; corrections and improvements via pull requests are welcome.
