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

## Languages
A interface do cliente traz a própria localização. Este README está disponível em 16 idiomas — veja a tabela no topo. Contribuições de mais idiomas são bem-vindas.

## License
GPL-2.0-or-later. Veja [LICENSE](../LICENSE).

