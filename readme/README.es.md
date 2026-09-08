# WPTSALL Client — Cliente de traducción local-first (WebUI + Desktop)

**El cliente del plugin WPMMCC ATS para WordPress — en tu máquina, no en la nube.**

[English](../README.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | **Español** | [Français](README.fr.md) | [Deutsch](README.de.md) | [Português (Brasil)](README.pt-BR.md) | [Italiano](README.it.md) | [Русский](README.ru.md) | [العربية](README.ar.md) | [हिन्दी](README.hi.md) | [Türkçe](README.tr.md) | [Tiếng Việt](README.vi.md) | [Bahasa Indonesia](README.id.md)

WPTSALL Client es el cliente complementario del plugin WPMMCC ATS para WordPress. Funciona en tu máquina — como interfaz web local o como aplicación de escritorio nativa — guarda cada ajuste en una base de datos local, se conecta directamente a tu propio WordPress con un token de dispositivo y maneja cualquier proveedor de traducción HTTP(S) que configures. Sin cuenta, sin licencia y sin dependencia de la nube.

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

## Languages
La interfaz del cliente incluye su propia localización. Este README está disponible en 16 idiomas — mira la tabla superior. Se agradecen contribuciones de más idiomas.

## License
GPL-2.0-or-later. Ver [LICENSE](../LICENSE).

