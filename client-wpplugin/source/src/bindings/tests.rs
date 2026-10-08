use super::*;
use std::collections::HashMap;

#[path = "../../../../tests/modules/client-wpplugin/unit/physical_readonly_component_capacity.rs"]
mod physical_readonly_component_capacity;

// -----------------------------------------------------------------------
// normalize_api_base_url_key
// -----------------------------------------------------------------------

#[test]
fn normalize_url_trims_slash() {
    assert_eq!(
        normalize_api_base_url_key("https://example.com/"),
        "https://example.com"
    );
}

#[test]
fn normalize_url_lowercases() {
    assert_eq!(
        normalize_api_base_url_key("HTTPS://Example.COM/api"),
        "https://example.com/api"
    );
}

#[test]
fn normalize_url_trims_whitespace() {
    assert_eq!(
        normalize_api_base_url_key("  https://example.com  "),
        "https://example.com"
    );
}

// -----------------------------------------------------------------------
// normalize_domain_base
// -----------------------------------------------------------------------

#[test]
fn domain_base_strips_wp_path() {
    assert_eq!(
        normalize_domain_base("https://blog.wpmm.cc/wp-json/wptsall/v2/SECRET/client"),
        "https://blog.wpmm.cc"
    );
}

#[test]
fn domain_base_plain_domain() {
    assert_eq!(
        normalize_domain_base("blog.wpmm.cc"),
        "https://blog.wpmm.cc"
    );
}

#[test]
fn domain_base_https_no_path() {
    assert_eq!(
        normalize_domain_base("https://blog.wpmm.cc"),
        "https://blog.wpmm.cc"
    );
}

#[test]
fn domain_base_http_localhost() {
    assert_eq!(
        normalize_domain_base("http://127.0.0.1:9090/api/v1"),
        "http://127.0.0.1:9090"
    );
}

#[test]
fn domain_base_lowercases() {
    assert_eq!(
        normalize_domain_base("HTTPS://Blog.WPMM.CC/path"),
        "https://blog.wpmm.cc"
    );
}

#[test]
fn domain_base_empty_returns_empty() {
    assert_eq!(normalize_domain_base(""), "");
    assert_eq!(normalize_domain_base("   "), "");
}

// -----------------------------------------------------------------------
// build_wp_base_url
// -----------------------------------------------------------------------

#[test]
fn build_wp_url_with_secret() {
    assert_eq!(
        build_wp_base_url("https://blog.wpmm.cc", "abc123"),
        Some("https://blog.wpmm.cc/wp-json/wptsall/v2/abc123/client".to_string())
    );
}

#[test]
fn build_wp_url_empty_secret_returns_none() {
    assert_eq!(build_wp_base_url("https://blog.wpmm.cc", ""), None);
    assert_eq!(build_wp_base_url("https://blog.wpmm.cc", "  "), None);
}

// -----------------------------------------------------------------------
// parse_task_type_binding_key
// -----------------------------------------------------------------------

#[test]
fn parse_task_type_text_variants() {
    assert_eq!(
        parse_task_type_binding_key("text"),
        Some("text".to_string())
    );
    assert_eq!(
        parse_task_type_binding_key("Text"),
        Some("text".to_string())
    );
    assert_eq!(
        parse_task_type_binding_key("field"),
        Some("text".to_string())
    );
    assert_eq!(
        parse_task_type_binding_key("fields"),
        Some("text".to_string())
    );
}

#[test]
fn parse_task_type_image() {
    assert_eq!(
        parse_task_type_binding_key("image"),
        Some("image".to_string())
    );
    assert_eq!(
        parse_task_type_binding_key("images"),
        Some("image".to_string())
    );
}

#[test]
fn parse_task_type_document() {
    assert_eq!(
        parse_task_type_binding_key("document"),
        Some("document".to_string())
    );
    assert_eq!(
        parse_task_type_binding_key("doc"),
        Some("document".to_string())
    );
    assert_eq!(
        parse_task_type_binding_key("file"),
        Some("document".to_string())
    );
}

#[test]
fn parse_task_type_unknown() {
    assert_eq!(parse_task_type_binding_key("unknown"), None);
    assert_eq!(parse_task_type_binding_key(""), None);
}

#[test]
fn parse_content_format_variants() {
    assert_eq!(
        parse_content_format_binding_key("plain_text"),
        Some("plain_text".to_string())
    );
    assert_eq!(
        parse_content_format_binding_key("html"),
        Some("rich_html".to_string())
    );
    assert_eq!(
        parse_content_format_binding_key("json"),
        Some("json_structured".to_string())
    );
    assert_eq!(
        parse_content_format_binding_key("serialized"),
        Some("serialized_php".to_string())
    );
    assert_eq!(
        parse_content_format_binding_key("media"),
        Some("media_ref".to_string())
    );
    assert_eq!(parse_content_format_binding_key("unknown"), None);
}

#[test]
fn parse_rule_component_slot_variants() {
    assert_eq!(
        parse_rule_component_slot_binding_key("rich_html"),
        Some("rich_html".to_string())
    );
    assert_eq!(
        parse_rule_component_slot_binding_key("media_ref:video"),
        Some("media_ref:video".to_string())
    );
    assert_eq!(
        parse_rule_component_slot_binding_key("text"),
        Some("plain_text".to_string())
    );
    assert_eq!(parse_rule_component_slot_binding_key("video"), None);
}

#[test]
fn normalize_rule_bindings_prefers_canonical_keys() {
    let mut doc = RuleComponentBindingsDoc::default();
    doc.global_defaults
        .insert("text".to_string(), "comp-old".to_string());
    doc.global_defaults
        .insert("plain_text".to_string(), "comp-new".to_string());
    doc.global_defaults
        .insert("Rich_HTML".to_string(), "comp-html".to_string());

    doc.plugin_bindings.insert(
        "Yoast-Seo".to_string(),
        HashMap::from([
            ("text".to_string(), "comp-plugin-old".to_string()),
            ("plain_text".to_string(), "comp-plugin-new".to_string()),
            ("media".to_string(), "comp-plugin-media".to_string()),
        ]),
    );
    doc.relation_bindings.insert(
        "0012".to_string(),
        HashMap::from([("text".to_string(), "comp-relation".to_string())]),
    );
    doc.relation_bindings.insert(
        "invalid".to_string(),
        HashMap::from([("plain_text".to_string(), "comp-invalid".to_string())]),
    );

    let normalized = normalize_rule_component_bindings_doc(&doc);
    assert_eq!(
        normalized.global_defaults.get("plain_text"),
        Some(&"comp-new".to_string())
    );
    assert_eq!(
        normalized.global_defaults.get("rich_html"),
        Some(&"comp-html".to_string())
    );
    assert!(!normalized.global_defaults.contains_key("text"));

    let plugin = normalized
        .plugin_bindings
        .get("yoast-seo")
        .expect("normalized plugin slug should exist");
    assert_eq!(
        plugin.get("plain_text"),
        Some(&"comp-plugin-new".to_string())
    );
    assert_eq!(
        plugin.get("media_ref"),
        Some(&"comp-plugin-media".to_string())
    );
    assert!(!plugin.contains_key("text"));

    assert!(normalized.relation_bindings.contains_key("12"));
    assert!(!normalized.relation_bindings.contains_key("0012"));
    assert!(!normalized.relation_bindings.contains_key("invalid"));
}

#[test]
fn status_items_dedup_legacy_alias_slots() {
    let mut doc = RuleComponentBindingsDoc::default();
    doc.global_defaults
        .insert("text".to_string(), "comp-old".to_string());
    doc.global_defaults
        .insert("plain_text".to_string(), "comp-new".to_string());

    let items = rule_component_binding_status_items(&doc);
    let global_plain: Vec<&RuleComponentBindingStatusItem> = items
        .iter()
        .filter(|it| it.scope == "global" && it.slot_key == "plain_text")
        .collect();
    assert_eq!(global_plain.len(), 1);
    assert_eq!(global_plain[0].component_id, "comp-new");
}

#[test]
fn resolve_rule_bindings_prefers_media_subtype_slot() {
    let mut doc = RuleComponentBindingsDoc::default();
    doc.rule_bindings.insert(
        "9".to_string(),
        HashMap::from([
            ("media_ref".to_string(), "comp-media-generic".to_string()),
            (
                "media_ref:video".to_string(),
                "comp-media-video".to_string(),
            ),
        ]),
    );

    let resolved = resolve_component_id_from_rule_bindings(
        &doc,
        Some(9),
        None,
        None,
        "video",
        Some("media_ref"),
    );
    assert_eq!(resolved, Some("comp-media-video"));
}

#[test]
fn resolve_rule_bindings_uses_relation_between_plugin_and_global() {
    let mut doc = RuleComponentBindingsDoc::default();
    doc.global_defaults
        .insert("plain_text".to_string(), "comp-global".to_string());
    doc.relation_bindings.insert(
        "12".to_string(),
        HashMap::from([("plain_text".to_string(), "comp-relation".to_string())]),
    );

    let resolved = resolve_component_id_from_rule_bindings(
        &doc,
        None,
        Some(12),
        None,
        "text",
        Some("plain_text"),
    );
    assert_eq!(resolved, Some("comp-relation"));
}

// -----------------------------------------------------------------------
// parse_business_line_key
// -----------------------------------------------------------------------

#[test]
fn parse_business_line_post() {
    assert_eq!(
        parse_business_line_key("post"),
        Some("post_content".to_string())
    );
    assert_eq!(
        parse_business_line_key("post_type"),
        Some("post_content".to_string())
    );
}

#[test]
fn parse_business_line_taxonomy() {
    assert_eq!(
        parse_business_line_key("taxonomy"),
        Some("taxonomy_content".to_string())
    );
    assert_eq!(
        parse_business_line_key("term"),
        Some("taxonomy_content".to_string())
    );
}

#[test]
fn parse_business_line_theme() {
    assert_eq!(
        parse_business_line_key("theme"),
        Some("theme_i18n".to_string())
    );
}

#[test]
fn parse_business_line_plugin() {
    assert_eq!(
        parse_business_line_key("plugin"),
        Some("plugin_i18n".to_string())
    );
    assert_eq!(
        parse_business_line_key("language_pack"),
        Some("plugin_i18n".to_string())
    );
}

#[test]
fn parse_business_line_config() {
    assert_eq!(
        parse_business_line_key("config"),
        Some("config_i18n".to_string())
    );
    assert_eq!(
        parse_business_line_key("config_i18n"),
        Some("config_i18n".to_string())
    );
}

#[test]
fn parse_business_line_unknown() {
    assert_eq!(parse_business_line_key("unknown"), None);
    assert_eq!(parse_business_line_key(""), None);
}

// -----------------------------------------------------------------------
// parse_business_line_task_type_binding_key
// -----------------------------------------------------------------------

#[test]
fn parse_scoped_binding_key_valid() {
    assert_eq!(
        parse_business_line_task_type_binding_key("post:text"),
        Some("post_content:text".to_string())
    );
    assert_eq!(
        parse_business_line_task_type_binding_key("theme:image"),
        Some("theme_i18n:image".to_string())
    );
}

#[test]
fn parse_scoped_binding_key_invalid() {
    assert_eq!(parse_business_line_task_type_binding_key("post"), None);
    assert_eq!(parse_business_line_task_type_binding_key(""), None);
    assert_eq!(parse_business_line_task_type_binding_key("a:b:c"), None);
    assert_eq!(
        parse_business_line_task_type_binding_key("unknown:text"),
        None
    );
}

// -----------------------------------------------------------------------
// resolve_wp_client_token_for_domain
// -----------------------------------------------------------------------

#[test]
fn resolve_token_from_bindings_domain_base_key() {
    let mut doc = DomainTokenBindingsDoc::default();
    // v2 key: scheme://host
    doc.domains.insert(
        "https://example.com".to_string(),
        DomainTokenBindingEntry {
            wp_client_token: "token-from-bindings".to_string(),
            route_secret: String::new(),
            ..Default::default()
        },
    );
    // Look up using full WP URL → should extract domain base and find it
    let result = resolve_wp_client_token_for_domain(
        "https://example.com/wp-json/wptsall/v2/SECRET/client",
        &doc,
        "fallback",
    );
    assert_eq!(result, Some("token-from-bindings".to_string()));
}

#[test]
fn resolve_token_from_bindings_exact_domain() {
    let mut doc = DomainTokenBindingsDoc::default();
    doc.domains.insert(
        "https://example.com".to_string(),
        DomainTokenBindingEntry {
            wp_client_token: "token-from-bindings".to_string(),
            route_secret: String::new(),
            ..Default::default()
        },
    );
    let result = resolve_wp_client_token_for_domain("https://example.com", &doc, "fallback");
    assert_eq!(result, Some("token-from-bindings".to_string()));
}

#[test]
fn resolve_token_fallback() {
    let doc = DomainTokenBindingsDoc::default();
    let result = resolve_wp_client_token_for_domain("https://example.com", &doc, "fallback-token");
    assert_eq!(result, Some("fallback-token".to_string()));
}

#[test]
fn resolve_token_no_fallback_returns_none() {
    let doc = DomainTokenBindingsDoc::default();
    let result = resolve_wp_client_token_for_domain("https://example.com", &doc, "");
    assert_eq!(result, None);
}

// -----------------------------------------------------------------------
// has_any_domain_token_bindings
// -----------------------------------------------------------------------

#[test]
fn has_bindings_empty_doc() {
    let doc = DomainTokenBindingsDoc::default();
    assert!(!has_any_domain_token_bindings(&doc));
}

#[test]
fn has_bindings_with_token() {
    let mut doc = DomainTokenBindingsDoc::default();
    doc.domains.insert(
        "https://example.com".to_string(),
        DomainTokenBindingEntry {
            wp_client_token: "token".to_string(),
            route_secret: String::new(),
            ..Default::default()
        },
    );
    assert!(has_any_domain_token_bindings(&doc));
}

#[test]
fn has_bindings_with_empty_token() {
    let mut doc = DomainTokenBindingsDoc::default();
    doc.domains.insert(
        "https://example.com".to_string(),
        DomainTokenBindingEntry {
            wp_client_token: "  ".to_string(),
            route_secret: String::new(),
            ..Default::default()
        },
    );
    assert!(!has_any_domain_token_bindings(&doc));
}

// -----------------------------------------------------------------------
// resolve_local_dev_binding
// -----------------------------------------------------------------------

#[test]
fn resolve_local_dev_binding_prefers_non_server_domain_binding() {
    let mut doc = DomainTokenBindingsDoc::default();
    doc.domains.insert(
        "https://example.com".to_string(),
        DomainTokenBindingEntry {
            wp_client_token: "token-main".to_string(),
            route_secret: "main-secret".to_string(),
            ..Default::default()
        },
    );
    doc.domains.insert(
        "http://127.0.0.1:8080".to_string(),
        DomainTokenBindingEntry {
            wp_client_token: "token-local".to_string(),
            route_secret: "local-secret".to_string(),
            ..Default::default()
        },
    );
    let occupied = HashSet::from(["https://example.com".to_string()]);
    let resolved = resolve_local_dev_binding(&doc, &occupied).expect("local binding");
    assert_eq!(resolved.0, "http://127.0.0.1:8080");
    assert_eq!(resolved.1, "token-local");
    assert_eq!(resolved.2.as_deref(), Some("local-secret"));
}

#[test]
fn resolve_local_dev_binding_returns_none_when_no_extra_binding() {
    let mut doc = DomainTokenBindingsDoc::default();
    doc.domains.insert(
        "https://example.com".to_string(),
        DomainTokenBindingEntry {
            wp_client_token: "token-main".to_string(),
            route_secret: String::new(),
            ..Default::default()
        },
    );
    let occupied = HashSet::from(["https://example.com".to_string()]);
    assert!(resolve_local_dev_binding(&doc, &occupied).is_none());
}

// -----------------------------------------------------------------------
// domain_token_binding_status_items
// -----------------------------------------------------------------------

#[test]
fn status_items_sorted_by_url() {
    let mut doc = DomainTokenBindingsDoc::default();
    doc.domains.insert(
        "https://b.com".to_string(),
        DomainTokenBindingEntry {
            wp_client_token: "token-b".to_string(),
            route_secret: String::new(),
            ..Default::default()
        },
    );
    doc.domains.insert(
        "https://a.com".to_string(),
        DomainTokenBindingEntry {
            wp_client_token: "token-a".to_string(),
            route_secret: "mysecret".to_string(),
            ..Default::default()
        },
    );

    let items = domain_token_binding_status_items(&doc);
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].api_base_url, "https://a.com");
    assert!(items[0].route_secret_set);
    assert_eq!(items[1].api_base_url, "https://b.com");
    assert!(!items[1].route_secret_set);
}

// -----------------------------------------------------------------------
// task_type_component_binding_status_items
// -----------------------------------------------------------------------

#[test]
fn task_type_binding_status_items() {
    let mut doc = TaskTypeComponentBindingsDoc::default();
    doc.task_types.insert(
        "text".to_string(),
        TaskTypeComponentBindingEntry {
            component_id: "comp-text".to_string(),
        },
    );
    doc.business_line_task_types.insert(
        "post_content:image".to_string(),
        TaskTypeComponentBindingEntry {
            component_id: "comp-post-image".to_string(),
        },
    );

    let items = task_type_component_binding_status_items(&doc);
    assert_eq!(items.len(), 2);
}

// -----------------------------------------------------------------------
// Component bindings file I/O
// -----------------------------------------------------------------------

#[test]
fn component_bindings_save_and_load() {
    let _key = crate::db::owned_mock_bindings_key();
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("bindings.json");
    let path_str = path.to_string_lossy().to_string();

    let mut doc = ComponentBindingsDoc {
        version: 1,
        components: HashMap::new(),
    };
    let mut entry = ComponentBindingEntry::default();
    entry
        .auth
        .insert("api_key".to_string(), "sk-test".to_string());
    doc.components.insert("comp-1".to_string(), entry);

    save_component_bindings(&path_str, &doc).expect("save");
    let loaded = load_component_bindings(&path_str).expect("load");
    assert_eq!(loaded.components.len(), 1);
    assert_eq!(
        loaded.components.get("comp-1").unwrap().auth.get("api_key"),
        Some(&"sk-test".to_string())
    );
}

#[test]
fn component_bindings_load_creates_new_file() {
    let _key = crate::db::owned_mock_bindings_key();
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("new-bindings.json");
    let path_str = path.to_string_lossy().to_string();

    let doc = load_component_bindings(&path_str).expect("load");
    assert!(doc.components.is_empty());
    assert!(path.exists());
}

// -----------------------------------------------------------------------
// Domain token bindings file I/O
// -----------------------------------------------------------------------

#[test]
fn domain_token_bindings_save_and_load() {
    let _key = crate::db::owned_mock_bindings_key();
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("domain-tokens.json");
    let path_str = path.to_string_lossy().to_string();

    let mut doc = DomainTokenBindingsDoc {
        version: 2,
        domains: HashMap::new(),
    };
    doc.domains.insert(
        "HTTPS://Example.COM/path/to/something".to_string(),
        DomainTokenBindingEntry {
            wp_client_token: "token-123".to_string(),
            route_secret: "my-secret".to_string(),
            ..Default::default()
        },
    );

    save_domain_token_bindings(&path_str, &doc).expect("save");
    let loaded = load_domain_token_bindings(&path_str).expect("load");

    // Key should be normalized to scheme://host (lowercase, path stripped)
    assert!(loaded.domains.contains_key("https://example.com"));
    let entry = loaded.domains.get("https://example.com").unwrap();
    assert_eq!(entry.wp_client_token, "token-123");
    assert_eq!(entry.route_secret, "my-secret");
}

#[test]
fn domain_token_bindings_v1_migration() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("domain-tokens-v1.json");
    let path_str = path.to_string_lossy().to_string();

    // Simulate a v1 file with full api_base_url key
    let v1_json = r#"{
            "version": 1,
            "domains": {
                "https://blog.wpmm.cc/wp-json/wptsall/v2/OLD_SECRET/client": {
                    "wp_client_token": "wptc1.xxx",
                    "route_secret": ""
                }
            }
        }"#;
    std::fs::write(&path, v1_json).expect("write v1 file");

    let loaded = load_domain_token_bindings(&path_str).expect("load");
    // Should have migrated key to scheme://host and schema to v3
    // (Identity Contract v1.1 §4: v1 → v2 key normalization → v3 identity
    // backfill in one pass).
    assert_eq!(loaded.version, 3);
    assert!(
        loaded.domains.contains_key("https://blog.wpmm.cc"),
        "expected 'https://blog.wpmm.cc' key, got: {:?}",
        loaded.domains.keys().collect::<Vec<_>>()
    );
    let migrated = loaded.domains.get("https://blog.wpmm.cc").unwrap();
    assert_eq!(migrated.wp_client_token, "wptc1.xxx");
    // Legacy binding: identity defaults to wpmmcc_ats, pending re-verification.
    assert_eq!(migrated.plugin_identity, Some(PluginIdentity::WpmmccAts));
    assert_eq!(migrated.identity_verified_at, None);
}

// -----------------------------------------------------------------------
// Identity Contract v1.1 (C-1, T-ID-1): golden fixtures, v3 migration,
// wire shapes. Contract dir: tasks/wpmmcc/contract/identity-v1/
// -----------------------------------------------------------------------

/// Identity-aware verification base (T-ID-4 prerequisite, contract §2/§3):
/// the expected identity selects the endpoint family the live ping hits.
/// ATS → wptsall/v2/{secret}/client; wpmmcc → wpmmcc/v1/{secret}/sync.
#[test]
fn identity_aware_verify_base_selects_endpoint_family_per_identity() {
    use crate::types::PluginIdentity;

    // ATS family: the secret-bearing wptsall client base (existing shape).
    assert_eq!(
        build_verify_base_url(
            "https://site-a.example",
            "sec_ats",
            &PluginIdentity::WpmmccAts,
        )
        .as_deref(),
        Some("https://site-a.example/wp-json/wptsall/v2/sec_ats/client")
    );

    // wpmmcc family: the secret-prefixed wpmmcc/v1 sync base (the plugin
    // registers /{route_secret}/sync/ping alongside /sync/ping).
    assert_eq!(
        build_verify_base_url("https://site-b.example/", "sec_mm", &PluginIdentity::Wpmmcc,)
            .as_deref(),
        Some("https://site-b.example/wp-json/wpmmcc/v1/sec_mm/sync")
    );

    // Fail-closed: no route secret, no base — the binding cannot verify and
    // therefore never dispatches (never falls back to guessing an endpoint).
    assert_eq!(
        build_verify_base_url("https://site-b.example", "", &PluginIdentity::Wpmmcc),
        None
    );
    assert_eq!(
        build_verify_base_url("https://site-a.example", "  ", &PluginIdentity::WpmmccAts),
        None
    );
}

/// Contract fixtures dir (17 号工作包 §3.2: all three roles read the same
/// files straight from the contract directory).
fn identity_contract_fixtures_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tasks/wpmmcc/contract/identity-v1/fixtures")
}

#[test]
fn identity_contract_domain_binding_v3_fixture_loads() {
    let raw =
        std::fs::read_to_string(identity_contract_fixtures_dir().join("domain-binding-v3.json"))
            .expect("read domain-binding-v3.json fixture");
    let doc: DomainTokenBindingsDoc = serde_json::from_str(&raw).expect("parse v3 fixture");

    assert_eq!(doc.version, 3);
    assert_eq!(doc.domains.len(), 2);

    let ats = doc
        .domains
        .get("https://site-a.example")
        .expect("site-a entry");
    assert_eq!(ats.plugin_identity, Some(PluginIdentity::WpmmccAts));
    assert_eq!(
        ats.identity_verified_at.as_deref(),
        Some("2026-09-20T10:00:00Z")
    );
    let ats_caps = ats.identity_capabilities.as_ref().expect("ats caps");
    assert_eq!(ats_caps.plugin_version, "2.1.4");
    assert_eq!(ats_caps.protocol_min, Some(1));
    assert_eq!(ats_caps.protocol_current, Some(1));

    let wpmmcc = doc
        .domains
        .get("https://site-b.example")
        .expect("site-b entry");
    assert_eq!(wpmmcc.plugin_identity, Some(PluginIdentity::Wpmmcc));
    assert_eq!(
        wpmmcc.identity_verified_at.as_deref(),
        Some("2026-09-20T10:05:00Z")
    );
    let wpmmcc_caps = wpmmcc.identity_capabilities.as_ref().expect("wpmmcc caps");
    assert_eq!(wpmmcc_caps.plugin_version, "1.0.0");
    assert_eq!(wpmmcc_caps.protocol_min, Some(1));
    assert_eq!(wpmmcc_caps.protocol_current, Some(1));
}

#[test]
fn identity_contract_ping_fixtures_carry_exact_identity_values() {
    // wpmmcc-sync-ping.json: plugin_identity EXACTLY "wpmmcc" (contract §2).
    let wpmmcc_raw =
        std::fs::read_to_string(identity_contract_fixtures_dir().join("wpmmcc-sync-ping.json"))
            .expect("read wpmmcc-sync-ping.json fixture");
    let wpmmcc_ping: serde_json::Value =
        serde_json::from_str(&wpmmcc_raw).expect("parse wpmmcc ping fixture");
    let identity = wpmmcc_ping["data"]["plugin_identity"]
        .as_str()
        .expect("plugin_identity present");
    assert_eq!(
        PluginIdentity::from_wire_str(identity),
        Some(PluginIdentity::Wpmmcc),
        "wire value must parse byte-exact to the enum"
    );

    // wpmmcc-ats-identity.json: the additive ats data block (contract §3).
    let ats_raw =
        std::fs::read_to_string(identity_contract_fixtures_dir().join("wpmmcc-ats-identity.json"))
            .expect("read wpmmcc-ats-identity.json fixture");
    let ats_block: serde_json::Value =
        serde_json::from_str(&ats_raw).expect("parse ats identity fixture");
    assert_eq!(
        ats_block["data"]["plugin_identity"].as_str(),
        Some("wpmmcc_ats")
    );
    assert_eq!(ats_block["data"]["plugin_version"].as_str(), Some("2.1.4"));
    assert_eq!(
        PluginIdentity::from_wire_str(ats_block["data"]["plugin_identity"].as_str().unwrap_or("")),
        Some(PluginIdentity::WpmmccAts)
    );
}

#[test]
fn domain_token_bindings_v2_to_v3_migration_defaults_and_idempotence() {
    let _key = crate::db::owned_mock_bindings_key();
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("domain-tokens-v2.json");
    let path_str = path.to_string_lossy().to_string();

    let v2_json = r#"{
            "version": 2,
            "domains": {
                "https://blog.wpmm.cc": {
                    "wp_client_token": "wptc1.xxx",
                    "route_secret": "my-secret"
                }
            }
        }"#;
    std::fs::write(&path, v2_json).expect("write v2 file");

    let loaded = load_domain_token_bindings(&path_str).expect("load");
    assert_eq!(loaded.version, 3);
    let entry = loaded.domains.get("https://blog.wpmm.cc").unwrap();
    // Contract §4: 存量缺省 wpmmcc_ats + identity_verified_at = null (待重验).
    assert_eq!(entry.plugin_identity, Some(PluginIdentity::WpmmccAts));
    assert_eq!(entry.identity_verified_at, None);
    assert_eq!(entry.identity_capabilities, None);
    // Token/secret survive the migration untouched.
    assert_eq!(entry.wp_client_token, "wptc1.xxx");
    assert_eq!(entry.route_secret, "my-secret");

    // Idempotence: re-loading the same v2 source yields the same state.
    let reloaded = load_domain_token_bindings(&path_str).expect("reload");
    assert_eq!(reloaded.version, 3);
    let re_entry = reloaded.domains.get("https://blog.wpmm.cc").unwrap();
    assert_eq!(re_entry.plugin_identity, Some(PluginIdentity::WpmmccAts));
    assert_eq!(re_entry.identity_verified_at, None);

    // And a persisted v3 file round-trips without re-migration.
    save_domain_token_bindings(&path_str, &loaded).expect("save v3");
    let persisted = load_domain_token_bindings(&path_str).expect("load v3");
    assert_eq!(persisted.version, 3);
    let p_entry = persisted.domains.get("https://blog.wpmm.cc").unwrap();
    assert_eq!(p_entry.plugin_identity, Some(PluginIdentity::WpmmccAts));
    assert_eq!(p_entry.identity_verified_at, None);
}

#[test]
fn domain_token_bindings_v3_unknown_stored_identity_is_pending_not_fatal() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("domain-tokens-v3.json");
    let path_str = path.to_string_lossy().to_string();

    // T-ID-6 shape: a forged/out-of-enum stored value maps to None (pending
    // re-verification) instead of failing the whole bindings file.
    let v3_json = r#"{
            "version": 3,
            "domains": {
                "https://blog.wpmm.cc": {
                    "wp_client_token": "wptc1.xxx",
                    "route_secret": "my-secret",
                    "plugin_identity": "wpmmcc_pro"
                }
            }
        }"#;
    std::fs::write(&path, v3_json).expect("write v3 file");

    let loaded = load_domain_token_bindings(&path_str).expect("load");
    assert_eq!(loaded.version, 3);
    let entry = loaded.domains.get("https://blog.wpmm.cc").unwrap();
    assert_eq!(entry.plugin_identity, None);
    assert_eq!(entry.identity_verified_at, None);
}

#[test]
fn domain_token_binding_entry_serializes_contract_wire_shape() {
    let entry = DomainTokenBindingEntry {
        wp_client_token: "tok".to_string(),
        route_secret: "sec".to_string(),
        plugin_identity: Some(PluginIdentity::WpmmccAts),
        identity_verified_at: Some("2026-09-20T10:00:00Z".to_string()),
        identity_capabilities: Some(IdentityCapabilities {
            plugin_version: "2.1.4".to_string(),
            protocol_min: Some(1),
            protocol_current: Some(1),
        }),
    };
    let value = serde_json::to_value(&entry).expect("serialize entry");
    assert_eq!(value["plugin_identity"], "wpmmcc_ats");
    assert_eq!(value["identity_verified_at"], "2026-09-20T10:00:00Z");
    assert_eq!(value["identity_capabilities"]["plugin_version"], "2.1.4");
    assert_eq!(value["identity_capabilities"]["protocol_min"], 1);
    assert_eq!(value["identity_capabilities"]["protocol_current"], 1);

    // Round-trip keeps the identity byte-exact.
    let back: DomainTokenBindingEntry = serde_json::from_value(value).expect("deserialize entry");
    assert_eq!(back.plugin_identity, Some(PluginIdentity::WpmmccAts));
    assert_eq!(
        back.identity_verified_at.as_deref(),
        Some("2026-09-20T10:00:00Z")
    );

    // Unverified entries omit the identity fields entirely (contract §4:
    // 验证前不落值).
    let pending = DomainTokenBindingEntry {
        wp_client_token: "tok".to_string(),
        route_secret: "sec".to_string(),
        ..Default::default()
    };
    let pending_value = serde_json::to_value(&pending).expect("serialize pending");
    assert!(pending_value.get("plugin_identity").is_none());
    assert!(pending_value.get("identity_verified_at").is_none());
    assert!(pending_value.get("identity_capabilities").is_none());
}

// -----------------------------------------------------------------------
// Task type component bindings file I/O
// -----------------------------------------------------------------------

#[test]
fn task_type_component_bindings_save_and_load() {
    let _key = crate::db::owned_mock_bindings_key();
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("task-type-bindings.json");
    let path_str = path.to_string_lossy().to_string();

    let mut doc = TaskTypeComponentBindingsDoc::default();
    doc.task_types.insert(
        "text".to_string(),
        TaskTypeComponentBindingEntry {
            component_id: "comp-text".to_string(),
        },
    );

    save_task_type_component_bindings(&path_str, &doc).expect("save");
    let loaded = load_task_type_component_bindings(&path_str).expect("load");
    assert_eq!(
        loaded.task_types.get("text").unwrap().component_id,
        "comp-text"
    );
}

// -----------------------------------------------------------------------
// derive_bindings_crypto_key
// -----------------------------------------------------------------------

#[test]
fn bindings_key_deterministic() {
    let key1 = crypto::derive_bindings_crypto_key("secret-1");
    let key2 = crypto::derive_bindings_crypto_key("secret-1");
    assert_eq!(key1, key2);
}

#[test]
fn bindings_key_different_secrets() {
    let key1 = crypto::derive_bindings_crypto_key("secret-a");
    let key2 = crypto::derive_bindings_crypto_key("secret-b");
    assert_ne!(key1, key2);
}

// -----------------------------------------------------------------------
// Vendor keys file I/O
// -----------------------------------------------------------------------

#[test]
fn vendor_keys_save_and_load() {
    let _key = crate::db::owned_mock_bindings_key();
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("vendor-keys.json");
    let path_str = path.to_string_lossy().to_string();

    let mut doc = VendorKeysDoc {
        version: 1,
        keys: HashMap::new(),
    };
    doc.keys.insert(
        "key-1".to_string(),
        VendorKey {
            vendor_id: "vendor-1".to_string(),
            label: "Test Key".to_string(),
            auth_values: {
                let mut m = HashMap::new();
                m.insert("api_key".to_string(), "sk-test-123".to_string());
                m
            },
            max_concurrent: 5,
            requests_per_second: 3.0,
            weight: 1,
            enabled: true,
            max_input_chars: 0,
            max_file_size_mb: 0.0,
        },
    );

    save_vendor_keys(&path_str, &doc).expect("save");
    let loaded = load_vendor_keys(&path_str).expect("load");
    assert_eq!(loaded.keys.len(), 1);
    assert_eq!(loaded.keys.get("key-1").unwrap().vendor_id, "vendor-1");
}

#[test]
fn vendor_keys_load_creates_new_file() {
    let _key = crate::db::owned_mock_bindings_key();
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("new-vendor-keys.json");
    let path_str = path.to_string_lossy().to_string();

    let doc = load_vendor_keys(&path_str).expect("load");
    assert!(doc.keys.is_empty());
    assert!(path.exists());
}

// -----------------------------------------------------------------------
// Vendor OAuth file I/O
// -----------------------------------------------------------------------

#[test]
fn vendor_oauth_save_and_load() {
    let _key = crate::db::owned_mock_bindings_key();
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("vendor-oauth.json");
    let path_str = path.to_string_lossy().to_string();

    let mut doc = VendorOAuthDoc {
        version: 1,
        configs: HashMap::new(),
    };
    doc.configs.insert(
        "oauth-1".to_string(),
        OAuthConfig {
            vendor_id: "vendor-1".to_string(),
            label: "Test OAuth".to_string(),
            grant_type: "client_credentials".to_string(),
            auth_url: String::new(),
            token_url: "https://example.com/oauth/token".to_string(),
            client_id: "client-id".to_string(),
            client_secret: "client-secret".to_string(),
            scopes: "translate".to_string(),
            extra_params: HashMap::new(),
            auth_extra_params: HashMap::new(),
            cached_token: None,
            cached_token_expires_at: 0,
            refresh_token: None,
            max_concurrent: 5,
            weight: 1,
            max_input_chars: 0,
            max_file_size_mb: 0.0,
            token_field: "access_token".to_string(),
        },
    );

    save_vendor_oauth(&path_str, &doc).expect("save");
    let loaded = load_vendor_oauth(&path_str).expect("load");
    assert_eq!(loaded.configs.len(), 1);
    assert_eq!(
        loaded.configs.get("oauth-1").unwrap().client_id,
        "client-id"
    );
}

// -----------------------------------------------------------------------
// Proxy profiles file I/O
// -----------------------------------------------------------------------

#[test]
fn proxy_profiles_save_and_load() {
    let _key = crate::db::owned_mock_bindings_key();
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("proxy-profiles.json");
    let path_str = path.to_string_lossy().to_string();

    let mut doc = ProxyProfilesDoc {
        version: 1,
        profiles: HashMap::new(),
    };
    doc.profiles.insert(
        "proxy-1".to_string(),
        ProxyProfile {
            name: "US Proxy".to_string(),
            protocol: "http".to_string(),
            host: "127.0.0.1".to_string(),
            port: 8080,
            username: String::new(),
            password: String::new(),
            enabled: true,
        },
    );

    save_proxy_profiles(&path_str, &doc).expect("save");
    let loaded = load_proxy_profiles(&path_str).expect("load");
    assert_eq!(loaded.profiles.len(), 1);
    assert_eq!(loaded.profiles.get("proxy-1").unwrap().port, 8080);
}

// -----------------------------------------------------------------------
// Components local file I/O
// -----------------------------------------------------------------------

#[test]
fn components_local_save_and_load() {
    let _key = crate::db::owned_mock_bindings_key();
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("components-local.json");
    let path_str = path.to_string_lossy().to_string();

    let mut doc = ComponentsLocalDoc {
        version: 1,
        components: HashMap::new(),
    };
    doc.components.insert(
        "comp-1".to_string(),
        ComponentInstanceLocal {
            name: "Test Component".to_string(),
            template_id: "tmpl-1".to_string(),
            source_template_id: "tmpl-1".to_string(),
            source_template_updated_at: Some("2026-01-01T00:00:00Z".to_string()),
            source_template_api_version: Some("1.0.0".to_string()),
            vendor_id: "vendor-1".to_string(),
            vendor_name: "Test Vendor".to_string(),
            kind: "text".to_string(),
            remarks: String::new(),
            enabled: true,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: Some("2026-01-01T00:00:00Z".to_string()),
            component_overrides: None,
            versions: HashMap::new(),
            active_version: None,
            template_json: None,
        },
    );

    save_components_local(&path_str, &doc).expect("save");
    let loaded = load_components_local(&path_str).expect("load");
    assert_eq!(loaded.components.len(), 1);
    assert_eq!(
        loaded.components.get("comp-1").unwrap().name,
        "Test Component"
    );
}

// -----------------------------------------------------------------------
// Identity Contract v1.1 §3: verify_identity parses the ping envelope
// -----------------------------------------------------------------------

#[tokio::test]
async fn verify_identity_parses_live_ping_envelope_shapes() {
    use std::io::{Read, Write};
    use std::net::TcpListener as StdListener;

    // Serve one connection with the exact envelope the ATS plugin's
    // /client/ping returns (class-client-tasks-rest-controller.php::ping),
    // then one with the wpmmcc plugin's /sync/ping envelope. Both are signed
    // with the client Protocol v2 response signature (HMAC-SHA256 over the
    // body with the HKDF signing key derived from the token) — the client
    // transport rejects unsigned plaintext on http.
    let listener = StdListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = std::thread::spawn(move || {
        let envelopes = [
            "{\"success\":true,\"data\":{\"plugin_identity\":\"wpmmcc_ats\",\"plugin_version\":\"1.0.0\",\"site_platform\":\"wp\"}}",
            "{\"success\":true,\"data\":{\"plugin_identity\":\"wpmmcc\",\"plugin_version\":\"1.0.0\",\"site_platform\":\"wp\"}}",
        ];
        for body in envelopes {
            let (mut socket, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 4096];
            let _n = socket.read(&mut buf).expect("read");
            let signing_key = crate::crypto::derive_signing_key("tok");
            let signature = crate::crypto::compute_request_signature(&signing_key, body);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-WPTSALL-Transport: plaintext\r\nX-WPTSALL-Response-Signature: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                signature,
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).expect("write");
        }
    });

    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("client");
    let base = format!("http://{}", addr);

    let ats = verify_identity(
        &client,
        &base,
        "tok",
        "worker-1",
        "worker-1",
        Some("secret"),
    )
    .await;
    assert!(
        matches!(
            ats,
            VerifyOutcome::Verified(crate::types::PluginIdentity::WpmmccAts, _)
        ),
        "expected Verified(wpmmcc_ats), got {:?}",
        ats
    );

    let wpmmcc = verify_identity(
        &client,
        &base,
        "tok",
        "worker-1",
        "worker-1",
        Some("secret"),
    )
    .await;
    assert!(
        matches!(
            wpmmcc,
            VerifyOutcome::Verified(crate::types::PluginIdentity::Wpmmcc, _)
        ),
        "expected Verified(wpmmcc), got {:?}",
        wpmmcc
    );

    handle.join().expect("mock thread");
}
