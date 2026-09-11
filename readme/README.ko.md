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
모든 설정과 작업 상태는 내 컴퓨터에 남습니다(SQLite + 로컬 파일). 클라이언트가 통신하는 대상은 설정한 WordPress 사이트와 선택한 번역 제공자 엔드포인트뿐입니다. 디버그 로깅은 기본적으로 꺼져 있으며, 설정 페이지에서 실행 중에 로컬 로깅을 켜거나 끌 수 있습니다.

## 업데이트
- WebUI의 설정 페이지에서 업데이트 확인을 사용하세요 — 새 버전이 서명된 kit로 다운로드되어 그 자리에서 적용됩니다
- 파일을 교체하기 전에 모든 kit을 철저히 검증합니다: minisign 서명과 SHA-256 체크섬 및 롤백 방지 기능. 완료 후 서비스가 자동으로 재시작되며, Windows에서는 실행 중인 바이너리가 안전하게 교체되고 업데이트 실패 시 자동 롤백됩니다
- 수동 대체 방법: https://github.com/wpmmcc/wptsall-client-releases 에서 최신 설치 프로그램을 다운로드하여 기존 설치본 위에 실행하세요

## 제거
- 각 제품 및 플랫폼용 제거 스크립트는 releases 저장소에 있습니다: uninstall-webui.sh / uninstall-desktop.sh(Linux, macOS) 및 해당 .ps1 스크립트(Windows)
- 제거 스크립트는 백그라운드 서비스(systemd 사용자 유닛, LaunchAgent 또는 Windows 서비스), 명령줄 바로가기 및 설치 디렉터리를 중지하고 제거합니다
- 데이터(로컬 SQLite 데이터베이스, 설정, 로그)는 기본적으로 보존됩니다. 데이터까지 함께 삭제하려면 --purge-data(Linux/macOS) 또는 -PurgeData(Windows)를 추가하세요
- WebUI와 Desktop 앱을 모두 설치한 경우, --purge-shared를 함께 전달하지 않는 한 공유 설치 디렉터리는 유지됩니다

## 오픈소스 구성 요소
- Rust 코어 — tokio, reqwest(rustls TLS), serde/serde_json, rusqlite(내장 SQLite), 암호화용 aes-gcm/hkdf/sha2/hmac/rsa/jsonwebtoken, S3 호환 제공자용 aws-sdk-s3, WASM 컴포넌트 런타임 extism
- Desktop 앱 — Tauri 2(시스템 webview를 사용하는 네이티브 셸)
- Web UI — Svelte 5, Vite, Tailwind CSS, svelte-i18n 및 lucide 아이콘
- 업데이트 보안 — minisign 서명 및 서명된 SHA256SUMS
- 버전이 고정된 전체 목록은 Cargo.toml과 frontend/package.json에 있으며, 모든 구성 요소는 GPL-2.0-or-later와 호환되는 MIT / Apache-2.0 / ISC 라이선스로 제공됩니다

## Languages
클라이언트 UI 자체 로컬라이제이션을 포함합니다. 이 README는 16개 언어로 제공됩니다 — 상단 표 참고. 더 많은 언어 기여를 환영합니다.

## License
GPL-2.0-or-later. [LICENSE](../LICENSE) 참고.

