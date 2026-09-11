# WPTSALL Client — Cliente de tradução local-first (WebUI + Desktop)

**O cliente do plugin WordPress WPMMCC ATS — na sua máquina, não na nuvem.**

[English](../README.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | **Português (Brasil)** | [Italiano](README.it.md) | [Русский](README.ru.md) | [العربية](README.ar.md) | [हिन्दी](README.hi.md) | [Türkçe](README.tr.md) | [Tiếng Việt](README.vi.md) | [Bahasa Indonesia](README.id.md)

WPTSALL Client é o cliente complementar do plugin WordPress WPMMCC ATS. Ele roda na sua máquina — como interface web local ou aplicativo desktop nativo — guarda cada configuração em um banco de dados local, conecta direto ao seu próprio WordPress com um token de dispositivo e aciona qualquer provedor de tradução HTTP(S) que você configurar. Sem conta, sem licença, sem dependência de nuvem.

## Links
- Project website — https://www.wpmm.cc/
- Documentation and usage help (English / 简体中文) — https://www.wpmm.cc/docs/
- WPMMCC ATS plugin on WordPress.org — https://wordpress.org/plugins/wpmmcc-ats/
- Plugin source repository — https://github.com/wpmmcc/wpmmcc-ats
- Signed kits and installers — https://github.com/wpmmcc/wptsall-client-releases

## Funcionalidades
- Dois produtos, um núcleo — WebUI local em 127.0.0.1:8977 e app desktop nativo (Tauri) compartilhando o mesmo núcleo Rust
- Local-first — sites, provedores, componentes e regras ficam no seu banco local, nunca na nuvem
- Conexão direta — conversa com seu WordPress pela API Protocol v2 do plugin
- Qualquer provedor — traga seu próprio endpoint HTTP(S) de tradução e credenciais
- Worker — modos de execução única e contínua, com retentativas limitadas e callbacks idempotentes
- Empacotamento — kits multiplataforma (Linux / Windows / macOS) com atualizações OTA assinadas

## Requisitos
- Um site WordPress com o plugin WPMMCC ATS 2.x — https://github.com/wpmmcc/wpmmcc-ats
- Um token de dispositivo e route secret emitidos pelo plugin
- Um endpoint de provedor de tradução (qualquer API HTTP(S))

## Instalação
1. Pegue um kit assinado ou instalador em https://github.com/wpmmcc/wptsall-client-releases
2. Ou compile a partir do código: cargo build --release dentro de client-wpplugin/source (binário WebUI), ou o toolchain Tauri dentro de client-desktop (app desktop)
3. Inicie o binário WebUI e abra http://127.0.0.1:8977

## Início rápido
1. Adicione seu site — cole a URL de cliente do plugin, o route secret e o token de dispositivo
2. Configure um provedor de tradução — endpoint e credenciais, guardados localmente
3. Rode o worker uma vez — ele pega um lote, traduz e escreve os resultados de volta

## Dados locais e privacidade
Toda a configuração e o estado das tarefas ficam na sua máquina (SQLite mais arquivos locais). O cliente só conversa com o site WordPress que você configurou e o endpoint do provedor que escolheu.

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
A interface do cliente traz a própria localização. Este README está disponível em 16 idiomas — veja a tabela no topo. Contribuições de mais idiomas são bem-vindas.

## License
GPL-2.0-or-later. Veja [LICENSE](../LICENSE).

