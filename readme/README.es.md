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
Toda la configuración y el estado de las tareas permanecen en tu máquina (SQLite y archivos locales). El cliente solo se comunica con el sitio WordPress que configuraste y el endpoint del proveedor que elegiste. El registro de depuración está desactivado por defecto; la página de Configuración permite activar o desactivar los registros locales en tiempo de ejecución.

## Actualización
- Abre Configuración en la WebUI y usa la comprobación de actualizaciones: las nuevas versiones se descargan como kits firmados y se aplican en el lugar
- Cada kit se verifica antes de reemplazar nada: firma minisign más suma de comprobación SHA-256, con protección antirretorno. El servicio se reinicia automáticamente después; en Windows el binario en ejecución se reemplaza de forma segura, con reversión si la actualización falla
- Alternativa manual: descarga el instalador más reciente desde https://github.com/wpmmcc/wptsall-client-releases y ejecútalo sobre la instalación existente

## Desinstalación
- Los desinstaladores para cada producto y plataforma están disponibles en el repositorio de releases: uninstall-webui.sh / uninstall-desktop.sh (Linux, macOS) y los scripts .ps1 correspondientes (Windows)
- Un desinstalador detiene y elimina el servicio en segundo plano (unidad de usuario systemd, LaunchAgent o servicio de Windows), los accesos directos de línea de comandos y el directorio de instalación
- Tus datos (base de datos SQLite local, configuración y registros) se conservan por defecto. Añade --purge-data (Linux/macOS) o -PurgeData (Windows) para eliminarlos también
- Si tienes instalados tanto la WebUI como la aplicación Desktop, el directorio de instalación compartido se conserva a menos que también pases --purge-shared

## Componentes de código abierto
- Núcleo Rust: tokio, reqwest (rustls TLS), serde/serde_json, rusqlite con SQLite integrado, aes-gcm/hkdf/sha2/hmac/rsa/jsonwebtoken para criptografía, aws-sdk-s3 para proveedores compatibles con S3, extism como entorno de componentes WASM
- Aplicación Desktop: Tauri 2 (entorno nativo con el webview del sistema)
- Web UI: Svelte 5, Vite, Tailwind CSS, svelte-i18n e iconos lucide
- Seguridad de actualización: firmas minisign y SHA256SUMS firmados
- Las listas completas con versiones fijadas están en Cargo.toml y frontend/package.json; cada componente se distribuye bajo licencias de tipo MIT / Apache-2.0 / ISC compatibles con la GPL-2.0-or-later de este proyecto

## Languages
La interfaz del cliente incluye su propia localización. Este README está disponible en 16 idiomas — mira la tabla superior. Se agradecen contribuciones de más idiomas.

## License
GPL-2.0-or-later. Ver [LICENSE](../LICENSE).

