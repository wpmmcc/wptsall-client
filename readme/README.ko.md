# WPTSALL Client — 로컬 우선 번역 클라이언트(WebUI + Desktop)

**WPMMCC ATS WordPress 플러그인의 클라이언트 — 클라우드가 아니라 내 컴퓨터에서.**

[English](../README.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [日本語](README.ja.md) | **한국어** | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Português (Brasil)](README.pt-BR.md) | [Italiano](README.it.md) | [Русский](README.ru.md) | [العربية](README.ar.md) | [हिन्दी](README.hi.md) | [Türkçe](README.tr.md) | [Tiếng Việt](README.vi.md) | [Bahasa Indonesia](README.id.md)

WPTSALL Client는 WPMMCC ATS WordPress 플러그인의 클라이언트입니다. 로컬 웹 UI 또는 네이티브 데스크톱 앱으로 내 컴퓨터에서 동작하고, 모든 설정을 로컬 데이터베이스에 저장하며, 기기 토큰으로 내 WordPress에 직접 연결되고, 직접 설정한 HTTP(S) 번역 제공자를 구동합니다. 계정도, 라이선스도, 클라우드 의존도 없습니다.

## Links
- Project website — https://www.wpmm.cc/
- Documentation and usage help (English / 简体中文) — https://www.wpmm.cc/docs/
- WPMMCC ATS plugin on WordPress.org — https://wordpress.org/plugins/wpmmcc-ats/
- Plugin source repository — https://github.com/wpmmcc/wpmmcc-ats
- Signed kits and installers — https://github.com/wpmmcc/wptsall-client-releases

## 기능
- 두 제품, 하나의 코어 — 로컬 WebUI(127.0.0.1:8977)와 네이티브 Desktop 앱(Tauri)이 같은 Rust 코어 공유
- 로컬 우선 — 사이트·제공자·컴포넌트·규칙은 로컬 DB에 저장, 클라우드로 보내지 않음
- 직접 연결 — 플러그인의 Protocol v2 API로 내 WordPress와 통신
- 모든 제공자 — HTTP(S) 번역 엔드포인트와 자격 증명을 직접 준비
- Worker — 1회 실행 및 연속 모드, 제한된 재시도와 멱등 콜백
- 패키징 — 크로스 플랫폼 kit(Linux / Windows / macOS)와 서명된 OTA 업데이트

## 요구 사항
- WPMMCC ATS 플러그인 2.x가 동작하는 WordPress 사이트 — https://github.com/wpmmcc/wpmmcc-ats
- 플러그인이 발급한 기기 토큰과 route secret
- 번역 제공자 엔드포인트(아무 HTTP(S) API)

## 설치
1. https://github.com/wpmmcc/wptsall-client-releases 에서 서명된 kit 또는 설치 프로그램 받기
2. 또는 소스에서 빌드: client-wpplugin/source에서 cargo build --release(WebUI 바이너리), client-desktop에서 Tauri 도구 체인(Desktop 앱)
3. WebUI 바이너리를 시작하고 http://127.0.0.1:8977 열기

## 빠른 시작
1. 사이트 추가 — 플러그인의 client URL, route secret, 기기 토큰 붙여넣기
2. 번역 제공자 설정 — 엔드포인트와 자격 증명, 모두 로컬 저장
3. Worker 1회 실행 — 배치를 가져와 번역하고 다시 기록

## 로컬 데이터와 프라이버시
모든 설정과 작업 상태는 내 컴퓨터에 남습니다(SQLite + 로컬 파일). 클라이언트가 통신하는 대상은 설정한 WordPress 사이트와 선택한 번역 제공자 엔드포인트뿐입니다.

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
클라이언트 UI 자체 로컬라이제이션을 포함합니다. 이 README는 16개 언어로 제공됩니다 — 상단 표 참고. 더 많은 언어 기여를 환영합니다.

## License
GPL-2.0-or-later. [LICENSE](../LICENSE) 참고.

