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
- Um pacote de conexão emitido pelo plugin (wp-admin → Gerenciamento de tarefas → Autorização, ou `wp wptsall security issue-pairing-pack`)
- Um endpoint de provedor de tradução (qualquer API HTTP(S))

## Porta padrão
- A WebUI escuta por padrão em 127.0.0.1:8977 — somente no loopback local, nunca exposta à sua rede
- A 8977 não está registrada na IANA e evita portas comuns de serviços e desenvolvimento (3306, 5432, 6379, 8080, 9000, 9200…), então conflitos são raros
- Se a porta já estiver em uso, a inicialização falha com bind web ui failed — defina WPTSALL_WEB_UI_PORT ou WPTSALL_WEB_UI_BIND para outra porta e reinicie o serviço

## Instalação
1. Pegue um kit assinado ou instalador em https://github.com/wpmmcc/wptsall-client-releases
2. Ou compile a partir do código: cargo build --release dentro de client-wpplugin/source (binário WebUI), ou o toolchain Tauri dentro de client-desktop (app desktop)
3. Inicie o binário WebUI e abra http://127.0.0.1:8977

## Início rápido
1. Adicione seu site — gere um pacote de conexão no wp-admin (Gerenciamento de tarefas → Autorização) e cole-o, junto com o código de pareamento, na página de sites do cliente
2. Configure um provedor de tradução — endpoint e credenciais, guardados localmente
3. Rode o worker uma vez — ele pega um lote, traduz e escreve os resultados de volta

## Dados locais e privacidade
Toda a configuração e o estado das tarefas ficam na sua máquina (SQLite mais arquivos locais). O cliente só conversa com o site WordPress que você configurou e o endpoint do provedor que escolheu. O registro de depuração fica desativado por padrão; a página Configurações permite ativar ou desativar os logs locais em tempo de execução.

## Atualização
- Abra Configurações na WebUI e use a verificação de atualizações — novas versões são baixadas como kits assinados e aplicadas no local
- Cada kit é verificado antes de substituir qualquer arquivo: assinatura minisign mais soma de verificação SHA-256, com proteção anti-rollback. O serviço reinicia automaticamente depois; no Windows o binário em execução é substituído com segurança, com reversão se a atualização falhar
- Alternativa manual: baixe o instalador mais recente em https://github.com/wpmmcc/wptsall-client-releases e execute-o sobre a instalação existente

## Desinstalação
- Os desinstaladores para cada produto e plataforma ficam no repositório de releases: uninstall-webui.sh / uninstall-desktop.sh (Linux, macOS) e os scripts .ps1 correspondentes (Windows)
- O desinstalador interrompe e remove o serviço em segundo plano (unidade de usuário systemd, LaunchAgent ou serviço do Windows), os atalhos de linha de comando e o diretório de instalação
- Seus dados — banco de dados SQLite local, configurações e logs — são mantidos por padrão. Adicione --purge-data (Linux/macOS) ou -PurgeData (Windows) para removê-los também
- Se você tiver instalados a WebUI e o app Desktop, o diretório de instalação compartilhado é mantido a menos que também passe --purge-shared

## Componentes de código aberto
- Núcleo Rust — tokio, reqwest (rustls TLS), serde/serde_json, rusqlite com SQLite embutido, aes-gcm/hkdf/sha2/hmac/rsa/jsonwebtoken para criptografia, aws-sdk-s3 para provedores compatíveis com S3, extism como runtime de componentes WASM
- App Desktop — Tauri 2 (shell nativo com webview do sistema)
- Web UI — Svelte 5, Vite, Tailwind CSS, svelte-i18n e ícones lucide
- Segurança nas atualizações — assinaturas minisign e SHA256SUMS assinados
- Listas completas com versões fixadas estão em Cargo.toml e frontend/package.json; cada componente é distribuído sob licenças tipo MIT / Apache-2.0 / ISC compatíveis com a GPL-2.0-or-later deste projeto

## Languages
A interface do cliente traz a própria localização. Este README está disponível em 16 idiomas — veja a tabela no topo. Contribuições de mais idiomas são bem-vindas.

## License
GPL-2.0-or-later. Veja [LICENSE](../LICENSE).

