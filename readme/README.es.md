# WPTSALL Client — Cliente de traducción local-first (WebUI + Desktop)

**El cliente del plugin WPMMCC ATS para WordPress — en tu máquina, no en la nube.**

[English](../README.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | **Español** | [Français](README.fr.md) | [Deutsch](README.de.md) | [Português (Brasil)](README.pt-BR.md) | [Italiano](README.it.md) | [Русский](README.ru.md) | [العربية](README.ar.md) | [हिन्दी](README.hi.md) | [Türkçe](README.tr.md) | [Tiếng Việt](README.vi.md) | [Bahasa Indonesia](README.id.md)

WPTSALL Client es el cliente complementario del plugin WPMMCC ATS para WordPress. Funciona en tu máquina — como interfaz web local o como aplicación de escritorio nativa — guarda cada ajuste en una base de datos local, se conecta directamente a tu propio WordPress con un token de dispositivo y maneja cualquier proveedor de traducción HTTP(S) que configures. Sin cuenta, sin licencia y sin dependencia de la nube.

## Links
- Project website — https://www.wpmm.cc/
- Documentation and usage help (English / 简体中文) — https://www.wpmm.cc/docs/
- WPMMCC ATS plugin on WordPress.org — https://wordpress.org/plugins/wpmmcc-ats/
- Plugin source repository — https://github.com/wpmmcc/wpmmcc-ats
- Signed kits and installers — https://github.com/wpmmcc/wptsall-client-releases

## Características
- Dos productos, un núcleo — WebUI local en 127.0.0.1:8977 y aplicación de escritorio nativa (Tauri) compartiendo el mismo núcleo Rust
- Local-first — sitios, proveedores, componentes y reglas viven en tu base de datos local, nunca en la nube
- Conexión directa — habla con tu WordPress mediante la API Protocol v2 del plugin
- Cualquier proveedor — aporta tu propio endpoint de traducción HTTP(S) y credenciales
- Worker — modos de una pasada y continuo, con reintentos acotados y callbacks idempotentes
- Empaquetado — kits multiplataforma (Linux / Windows / macOS) con actualizaciones OTA firmadas

## Requisitos
- Un sitio WordPress con el plugin WPMMCC ATS 2.x — https://github.com/wpmmcc/wpmmcc-ats
- Un token de dispositivo y route secret emitidos por el plugin
- Un endpoint de proveedor de traducción (cualquier API HTTP(S))

## Instalación
1. Consigue un kit firmado o instalador en https://github.com/wpmmcc/wptsall-client-releases
2. O compila desde el código: cargo build --release dentro de client-wpplugin/source (binario WebUI), o el toolchain de Tauri dentro de client-desktop (aplicación de escritorio)
3. Arranca el binario WebUI y abre http://127.0.0.1:8977

## Inicio rápido
1. Añade tu sitio — pega la URL de cliente del plugin, el route secret y el token de dispositivo
2. Configura un proveedor de traducción — endpoint y credenciales, guardados localmente
3. Ejecuta el worker una vez — reclama un lote, lo traduce y escribe los resultados

## Datos locales y privacidad
Toda la configuración y el estado de las tareas permanecen en tu máquina (SQLite y archivos locales). El cliente solo se comunica con el sitio WordPress que configuraste y el endpoint del proveedor que elegiste.

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
La interfaz del cliente incluye su propia localización. Este README está disponible en 16 idiomas — mira la tabla superior. Se agradecen contribuciones de más idiomas.

## License
GPL-2.0-or-later. Ver [LICENSE](../LICENSE).

