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

    #[test]
    fn t_resolves_embedded_catalogs_per_language() {
        assert_eq!(t("app.title", "en"), "WPTSALL Client");
        assert_eq!(t("app.title", "zh-CN"), "WPTSALL 客户端");
    }

    #[test]
    fn t_falls_back_to_english_for_unknown_language() {
        // An unknown language must resolve through the English catalog
        // rather than echoing the key.
        assert_eq!(t("app.title", "xx-XX"), "WPTSALL Client");
    }

    #[test]
    fn t_echoes_the_key_when_no_catalog_has_it() {
        assert_eq!(t("totally.missing.key", "zh-CN"), "totally.missing.key");
    }

    #[test]
    fn locale_json_serializes_the_flat_catalog() {
        let en = locale_json("en");
        let parsed: Value = serde_json::from_str(&en).unwrap();
        assert_eq!(parsed["app.title"], "WPTSALL Client");
        // Unknown language resolves to the English catalog, never "{}".
        let fallback = locale_json("xx-XX");
        let parsed_fallback: Value = serde_json::from_str(&fallback).unwrap();
        assert_eq!(parsed_fallback["app.title"], "WPTSALL Client");
    }
}
