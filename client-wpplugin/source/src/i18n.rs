use serde_json::Value;
use std::collections::HashMap;
use std::sync::OnceLock;

type Catalog = HashMap<String, String>;

static EN: &str = include_str!("../locales/en.json");
static ZH_CN: &str = include_str!("../locales/zh-CN.json");

static CATALOGS: OnceLock<HashMap<String, Catalog>> = OnceLock::new();

fn catalogs() -> &'static HashMap<String, Catalog> {
    CATALOGS.get_or_init(|| {
        let mut m = HashMap::new();
        m.insert("en".into(), parse_flat(EN));
        m.insert("zh-CN".into(), parse_flat(ZH_CN));
        m
    })
}

fn parse_flat(json: &str) -> Catalog {
    let v: Value = serde_json::from_str(json).unwrap_or(Value::Null);
    let mut out = Catalog::new();
    flatten("", &v, &mut out);
    out
}

fn flatten(prefix: &str, val: &Value, out: &mut Catalog) {
    match val {
        Value::Object(map) => {
            for (k, v) in map {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten(&key, v, out);
            }
        }
        Value::String(s) => {
            out.insert(prefix.to_string(), s.clone());
        }
        _ => {}
    }
}

pub fn t(key: &str, lang: &str) -> String {
    catalogs()
        .get(lang)
        .and_then(|c| c.get(key))
        .or_else(|| catalogs().get("en")?.get(key))
        .cloned()
        .unwrap_or_else(|| key.to_string())
}

#[allow(dead_code)]
pub fn locale_json(lang: &str) -> String {
    let catalog = catalogs().get(lang).or_else(|| catalogs().get("en"));
    match catalog {
        Some(c) => serde_json::to_string(c).unwrap_or_else(|_| "{}".into()),
        None => "{}".into(),
    }
}

pub fn detect_cli_locale() -> String {
    std::env::var("WPTSALL_LOCALE")
        .or_else(|_| {
            std::env::var("LANG").map(|l| l.split('.').next().unwrap_or("en").replace('_', "-"))
        })
        .unwrap_or_else(|_| "en".into())
}
#[cfg(test)]
mod tests {
    // catalog: WEBUI-MOD-i18n-rs
    // oracle: L1
    use super::*;

    // P: flat 词典是 Rust 域独占层（CLI/OAuth eprintln + tray 菜单）。
    // 24 个键 = Rust 源码 `i18n::t("...")` 字面量全集（多行感知 rg 枚举定谳）。
    const RUST_DOMAIN_KEYS: [&str; 24] = [
        "cli.domains_fetch_failed",
        "cli.heartbeat_failed",
        "cli.invalid_config",
        "cli.login_failed",
        "cli.missing_wp_token",
        "cli.oauth_manual_url",
        "cli.oauth_opening_browser",
        "cli.oauth_success",
        "cli.oauth_timeout",
        "cli.oauth_waiting_callback",
        "cli.process_domain_failed",
        "cli.process_domain_warning",
        "cli.session_expired_relogin",
        "cli.webui_listening",
        "oauth_callback.close_window",
        "oauth_callback.login_failed",
        "oauth_callback.login_successful",
        "oauth_callback.no_auth_code",
        "oauth_callback.no_session",
        "oauth_callback.state_mismatch",
        "oauth_callback.token_exchange_error_prefix",
        "tray.show",
        "tray.run_once",
        "tray.quit",
    ];

    #[test]
    fn t_resolves_embedded_catalogs_per_language() {
        assert_eq!(
            t("cli.webui_listening", "en"),
            "WPTSALL client web ui listening on http://{}"
        );
        assert_eq!(
            t("cli.webui_listening", "zh-CN"),
            "WPTSALL 客户端 Web UI 监听地址 http://{}"
        );
        // P 批补齐的 7 个裸键疤之一：此前 CLI 面恒打印裸键。
        assert_eq!(t("cli.oauth_timeout", "en"), "Login timed out");
        assert_eq!(t("cli.oauth_timeout", "zh-CN"), "登录超时");
    }

    #[test]
    fn t_falls_back_to_english_for_unknown_language() {
        // An unknown language must resolve through the English catalog
        // rather than echoing the key.
        assert_eq!(
            t("cli.session_expired_relogin", "xx-XX"),
            "Session expired, please log in again"
        );
    }

    #[test]
    fn t_echoes_the_key_when_no_catalog_has_it() {
        assert_eq!(t("totally.missing.key", "zh-CN"), "totally.missing.key");
    }

    #[test]
    fn locale_json_serializes_the_flat_catalog() {
        let en = locale_json("en");
        let parsed: Value = serde_json::from_str(&en).unwrap();
        assert_eq!(
            parsed["cli.webui_listening"],
            "WPTSALL client web ui listening on http://{}"
        );
        // Unknown language resolves to the English catalog, never "{}".
        let fallback = locale_json("xx-XX");
        let parsed_fallback: Value = serde_json::from_str(&fallback).unwrap();
        assert_eq!(
            parsed_fallback["cli.webui_listening"],
            "WPTSALL client web ui listening on http://{}"
        );
    }

    #[test]
    fn flat_catalog_is_exactly_the_rust_domain() {
        // P 域门：flat 词典与 Rust 字面量集必须严格相等——多一个键即
        // UI 键漂回基层（复现 app.title 类历史事故），少一个键即 CLI
        // 裸键疤（t() fallback=key 恒回显键名给用户）。两语言同门。
        for lang in ["en", "zh-CN"] {
            let catalog = catalogs().get(lang).expect("catalog missing");
            let mut keys: Vec<&String> = catalog.keys().collect();
            keys.sort();
            let mut expected: Vec<&&str> = RUST_DOMAIN_KEYS.iter().collect();
            expected.sort();
            assert_eq!(
                keys, expected,
                "{lang} flat catalog must equal the 24-key Rust domain"
            );
        }
    }

    #[test]
    fn tray_menu_keys_resolve_in_both_catalogs() {
        // X-6: the desktop tray menu consumes these keys via
        // `wptsall_client::i18n::t` — both catalogs must carry them so no
        // locale renders a hard-coded fallback from the other language.
        for lang in ["en", "zh-CN"] {
            assert!(t("tray.show", lang).len() > 0, "tray.show missing in {lang}");
            assert!(t("tray.run_once", lang).len() > 0, "tray.run_once missing in {lang}");
            assert!(t("tray.quit", lang).len() > 0, "tray.quit missing in {lang}");
        }
        assert_eq!(t("tray.show", "zh-CN"), "打开主界面");
        assert_eq!(t("tray.show", "en"), "Open Main Window");
    }
}