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

## Languages
클라이언트 UI 자체 로컬라이제이션을 포함합니다. 이 README는 16개 언어로 제공됩니다 — 상단 표 참고. 더 많은 언어 기여를 환영합니다.

## License
GPL-2.0-or-later. [LICENSE](../LICENSE) 참고.

