# WPTSALL Client — Client de traduction local-first (WebUI + Desktop)

**Le client du plugin WordPress WPMMCC ATS — sur votre machine, pas dans le cloud.**

[English](../README.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | **Français** | [Deutsch](README.de.md) | [Português (Brasil)](README.pt-BR.md) | [Italiano](README.it.md) | [Русский](README.ru.md) | [العربية](README.ar.md) | [हिन्दी](README.hi.md) | [Türkçe](README.tr.md) | [Tiếng Việt](README.vi.md) | [Bahasa Indonesia](README.id.md)

WPTSALL Client est le client compagnon du plugin WordPress WPMMCC ATS. Il tourne sur votre machine — en interface web locale ou en application bureautique native — conserve chaque réglage dans une base de données locale, se connecte directement à votre propre WordPress avec un jeton d'appareil et pilote n'importe quel fournisseur de traduction HTTP(S) que vous configurez. Sans compte, sans licence, sans dépendance au cloud.

## Links
- Project website — https://www.wpmm.cc/
- Documentation and usage help (English / 简体中文) — https://www.wpmm.cc/docs/
- WPMMCC ATS plugin on WordPress.org — https://wordpress.org/plugins/wpmmcc-ats/
- Plugin source repository — https://github.com/wpmmcc/wpmmcc-ats
- Signed kits and installers — https://github.com/wpmmcc/wptsall-client-releases

## Fonctionnalités
- Deux produits, un cœur — une WebUI locale sur 127.0.0.1:8977 et une application bureautique native (Tauri) partageant le même cœur Rust
- Local-first — sites, fournisseurs, composants et règles restent dans votre base locale, jamais dans le cloud
- Connexion directe — dialogue avec votre WordPress via l'API Protocol v2 du plugin
- N'importe quel fournisseur — apportez votre endpoint de traduction HTTP(S) et vos identifiants
- Worker — modes ponctuel et continu, avec tentatives bornées et rappels idempotents
- Empaquetage — kits multiplateformes (Linux / Windows / macOS) avec mises à jour OTA signées

## Prérequis
- Un site WordPress faisant tourner le plugin WPMMCC ATS 2.x — https://github.com/wpmmcc/wpmmcc-ats
- Un pack de connexion émis par le plugin (wp-admin → Gestion des tâches → Autorisation, ou `wp wptsall security issue-pairing-pack`)
- Un endpoint de fournisseur de traduction (toute API HTTP(S))

## Port par défaut
- La WebUI écoute par défaut sur 127.0.0.1:8977 — en boucle locale uniquement, jamais exposée à votre réseau
- Le 8977 n'est pas enregistré auprès de l'IANA et évite les ports courants de services et de développement (3306, 5432, 6379, 8080, 9000, 9200…), les conflits sont donc rares
- Si le port est déjà pris, le démarrage échoue avec bind web ui failed — définissez WPTSALL_WEB_UI_PORT ou WPTSALL_WEB_UI_BIND sur un autre port et redémarrez le service

## Installation
1. Récupérez un kit signé ou un installateur sur https://github.com/wpmmcc/wptsall-client-releases
2. Ou compilez depuis les sources : cargo build --release dans client-wpplugin/source (binaire WebUI), ou la chaîne Tauri dans client-desktop (application bureautique)
3. Démarrez le binaire WebUI et ouvrez http://127.0.0.1:8977

## Démarrage rapide
1. Ajoutez votre site — générez un pack de connexion dans wp-admin (Gestion des tâches → Autorisation) et collez-le, avec le code d'appairage, dans la page Sites du client
2. Configurez un fournisseur de traduction — endpoint et identifiants, stockés localement
3. Lancez le worker une fois — il réclame un lot, le traduit et écrit les résultats

## Données locales et confidentialité
Toute la configuration et l'état des tâches restent sur votre machine (SQLite et fichiers locaux). Le client ne communique qu'avec le site WordPress configuré et l'endpoint du fournisseur choisi. La journalisation de débogage est désactivée par défaut ; la page Paramètres permet d'activer ou désactiver les journaux locaux à l'exécution.

## Mises à jour
- Ouvrez les Paramètres dans la WebUI et lancez la vérification des mises à jour — les nouvelles versions sont téléchargées sous forme de kits signés et appliquées sur place
- Chaque kit est vérifié avant tout remplacement : signature minisign et somme de contrôle SHA-256, avec protection anti-rollback. Le service redémarre automatiquement ensuite ; sous Windows, le binaire en cours d'exécution est remplacé en toute sécurité, avec retour en arrière en cas d'échec
- Alternative manuelle : téléchargez le dernier programme d'installation depuis https://github.com/wpmmcc/wptsall-client-releases et lancez-le par-dessus l'installation existante

## Désinstallation
- Les désinstallateurs pour chaque produit et plateforme sont disponibles dans le dépôt des versions : uninstall-webui.sh / uninstall-desktop.sh (Linux, macOS) et les scripts .ps1 correspondants (Windows)
- Un désinstallateur arrête et supprime le service en arrière-plan (unité utilisateur systemd, LaunchAgent ou service Windows), les raccourcis en ligne de commande et le répertoire d'installation
- Vos données (base de données SQLite locale, configuration et journaux) sont conservées par défaut. Ajoutez --purge-data (Linux/macOS) ou -PurgeData (Windows) pour les supprimer également
- Si vous avez à la fois la WebUI et l'application Desktop, le répertoire d'installation partagé est conservé à moins de passer également --purge-shared

## Composants open source
- Cœur Rust — tokio, reqwest (rustls TLS), serde/serde_json, rusqlite avec SQLite intégré, aes-gcm/hkdf/sha2/hmac/rsa/jsonwebtoken pour la cryptographie, aws-sdk-s3 pour les fournisseurs compatibles S3, extism comme runtime de composants WASM
- Application Desktop — Tauri 2 (enveloppe native utilisant le webview du système)
- Web UI — Svelte 5, Vite, Tailwind CSS, svelte-i18n et icônes lucide
- Sécurité des mises à jour — signatures minisign et SHA256SUMS signés
- La liste complète des versions verrouillées se trouve dans Cargo.toml et frontend/package.json ; chaque composant est distribué sous une licence compatible GPL-2.0-or-later de type MIT / Apache-2.0 / ISC

## Languages
L'interface du client embarque sa propre localisation. Ce README est disponible en 16 langues — voir le tableau en haut. Les contributions pour d'autres langues sont bienvenues.

## License
GPL-2.0-or-later. Voir [LICENSE](../LICENSE).

