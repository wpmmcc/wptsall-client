//! Golden wire fixtures (opus5 T-01, doc 05 §9.10/§9.12).
//!
//! The six fixtures under docs/architecture/current/contracts/golden-wire/
//! are captured from the live WP lab with
//! tests/infra/tools/check-golden-wire.py (double-cycle determinism proof,
//! explicit volatile-field normalization). They freeze the exact wire JSON
//! of the client REST surfaces this crate consumes.
//!
//! These tests make CLIENT-SIDE drift red: if a serde struct renames,
//! retypes, or drops a field the frozen wire still carries, deserialization
//! fails here. WP-side drift goes red in the Python compare gate instead
//! (fresh capture vs the tracked fixtures) — the two directions together
//! are the T-01 golden-wire contract.
//!
//! Run: cargo test golden_wire --lib

use serde_json::{json, Value};

use crate::types::{
    ContentClaimResponse, ContentResponse, RelationsResponse, RulesResponse,
    TranslationCallbackPayload,
};

const FIXTURE_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/architecture/current/contracts/golden-wire"
);

fn fixture(name: &str) -> Value {
    let path = format!("{}/{}", FIXTURE_DIR, name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("golden-wire: cannot read {}: {}", path, err));
    serde_json::from_str(&text)
        .unwrap_or_else(|err| panic!("golden-wire: {} is not valid JSON: {}", path, err))
}

#[test]
fn golden_wire_site_relations_fixture_deserializes() {
    let value = fixture("site-relations.response.json");
    let parsed: RelationsResponse = serde_json::from_value(value)
        .expect("client must still accept the frozen site-relations wire");
    assert_eq!(
        parsed.relations.len(),
        1,
        "exactly the owned fixture relation"
    );
    let relation = &parsed.relations[0];
    assert_eq!(relation.id, 111_111);
    assert_eq!(relation.source_lang, "en_US");
    assert_eq!(relation.target_lang, "zh_CN");
    assert_eq!(relation.target_site_type, "virtual");
    assert_eq!(relation.sync_mode, "new_only");
    assert_eq!(relation.template, "p2fxgw-golden");
    assert_eq!(relation.models.len(), 1);
    assert_eq!(relation.models[0].plugin_slug, "p2fxgw-golden");
    assert_eq!(
        relation.models[0].post_types,
        vec!["p2fxgw_gwpost".to_string()]
    );
    // Preflight axes must stay on the wire (T-01 surface contract).
    assert_eq!(relation.preflight_policy, "warn");
    assert_eq!(
        relation.missing_component_behavior, "confirm_continue",
        "missing_component_behavior must stay on the discovery wire"
    );
    assert!(
        relation.i18n_config.is_some(),
        "i18n_config envelope required"
    );
    assert!(
        relation
            .source_group_config
            .as_ref()
            .expect("source_group_config required")
            .translate_content_objects
    );
}

#[test]
fn golden_wire_rules_fixture_deserializes() {
    let value = fixture("rules.response.json");
    let parsed: RulesResponse =
        serde_json::from_value(value).expect("client must still accept the frozen rules wire");
    assert_eq!(parsed.rules.len(), 1, "exactly the owned fixture rule");
    let rule = &parsed.rules[0];
    assert_eq!(rule.id, 111_111);
    assert_eq!(rule.data_type, "post");
    assert_eq!(rule.object_name, "p2fxgw_gwpost");
    // X-Client-Version >= 2.1 contract axes: content formats + storage map.
    assert_eq!(
        rule.field_content_formats
            .get("post_content")
            .map(String::as_str),
        Some("rich_html"),
        "field_content_formats axis missing from the rules wire"
    );
    assert_eq!(
        rule.field_storage_map
            .get("post_content")
            .map(String::as_str),
        Some("post_column"),
        "field_storage_map axis missing from the rules wire"
    );
    // Derived semantics stay on the wire for routing decisions.
    assert_eq!(rule.source_group, "content_object");
    assert_eq!(rule.routing_profile, "post_content_default");
    assert_eq!(rule.delivery_target, "object_writeback");
    assert_eq!(
        rule.required_content_formats,
        vec!["plain_text".to_string(), "rich_html".to_string()]
    );
}

#[test]
fn golden_wire_content_fixture_deserializes() {
    let value = fixture("content.response.json");
    let parsed: ContentResponse =
        serde_json::from_value(value).expect("client must still accept the frozen content wire");
    assert_eq!(parsed.total, 1);
    assert_eq!(parsed.page, 1);
    assert_eq!(parsed.per_page, 20);
    assert_eq!(parsed.items.len(), 1);
    let item = &parsed.items[0];
    assert_eq!(item.object_type, "post_type");
    assert_eq!(item.subtype, "p2fxgw_gwpost");
    assert_eq!(item.object_id, 111_111);
    assert!(!item.needs_resync);
    // The job snapshot the callback must echo (A-04) rides complete_data.
    let snapshot = &item.complete_data["__wptsall_job_snapshot"];
    assert!(snapshot["source_revision"].is_string());
    assert!(snapshot["policy_version"].is_string());
    assert_eq!(
        item.complete_data["post_title"],
        json!("P2FXGW Golden Post"),
        "flattened post fields must ride complete_data"
    );
}

#[test]
fn golden_wire_content_claim_fixture_deserializes() {
    let value = fixture("content-claim.response.json");
    let parsed: ContentClaimResponse = serde_json::from_value(value)
        .expect("client must still accept the frozen content/claim wire");
    assert_eq!(parsed.claimed_count, Some(1));
    let claimed = parsed
        .claimed_items
        .as_ref()
        .and_then(|items| items.first())
        .expect("claimed_items required on the claim wire");
    assert_eq!(claimed.post_type, "p2fxgw_gwpost");
    assert_eq!(claimed.object_id, 111_111);
}

#[test]
fn golden_wire_callback_payload_round_trip_is_byte_stable() {
    let fixture_value = fixture("translation-callback.payload.json");
    let payload: TranslationCallbackPayload = serde_json::from_value(fixture_value.clone())
        .expect("client must still serialize/accept the frozen callback payload");
    // Serialize-side contract: re-serializing the deserialized payload must
    // reproduce the frozen wire value exactly (serde_json Value equality is
    // key-order independent; skip_serializing_if empties stay absent).
    let reserialized = serde_json::to_value(&payload)
        .expect("callback payload must round-trip through serialization");
    assert_eq!(
        reserialized, fixture_value,
        "TranslationCallbackPayload serialization drifted from the frozen wire"
    );
    assert_eq!(
        payload.schema_version,
        crate::config::TASK_CALLBACK_SCHEMA_VERSION
    );
    assert_eq!(payload.business_line, "post_content");
    assert_eq!(
        payload
            .translated_fields
            .get("post_title")
            .map(String::as_str),
        Some("P2FXGW 金標")
    );
}

#[test]
fn golden_wire_callback_response_envelope_shape_is_frozen() {
    // The callback response has no dedicated Rust type (the submitter reads
    // it as raw JSON); freeze the envelope shape so silent drift is still
    // visible from the client side.
    let value = fixture("translation-callback.response.json");
    assert_eq!(value["success"], json!(true));
    assert_eq!(value["protocol"], json!("v2"));
    assert_eq!(value["result_status"], json!("synced"));
    assert_eq!(value["queued"], json!(true));
    assert!(value["result_id"].is_number());
    assert!(value["sync_task_id"].is_number());
    assert_eq!(value["sync_result"]["success"], json!(true));
    assert!(value["sync_result"]["target_id"].is_number());
}
