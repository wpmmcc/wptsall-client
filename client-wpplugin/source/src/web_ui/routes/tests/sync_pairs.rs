use serde_json::json;

use crate::sync_engine::SYNC_PAIRS_SCHEMA_VERSION;
use crate::web_ui::test_support::WebUiTestHarness;

// catalog: WEBUI-API-POST-api-sync-pairs-delete
// catalog: WEBUI-API-PREFIX-api-sync-pairs-credentials
// oracle: L2
// (route-level pair lifecycle: delete roundtrip, credentials list, pause/resume/run routes; F-T1 annotation batch 2026-09-22)

/// Minimal encodeURIComponent mirror for path segments in tests.
fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[tokio::test]
async fn sync_pairs_list_empty_by_default() {
    let harness = WebUiTestHarness::new("", None).await.unwrap();

    let res = harness.get_json("/api/sync-pairs").await.unwrap();
    assert!(res.status_line.starts_with("HTTP/1.1 200"));
    assert_eq!(res.body["success"], true);
    assert_eq!(
        res.body["data"]["schema_version"],
        SYNC_PAIRS_SCHEMA_VERSION
    );
    assert_eq!(res.body["data"]["pairs"].as_array().map(Vec::len), Some(0));
}

#[tokio::test]
async fn sync_pairs_validations_and_identity_gates() {
    let harness = WebUiTestHarness::new("", None).await.unwrap();

    // 1) Missing source/target domain
    let res = harness
        .post_json(
            "/api/sync-pairs",
            json!({
                "source_domain": "",
                "target_domain": "https://site-b.example.com"
            }),
        )
        .await
        .unwrap();
    assert!(res.status_line.starts_with("HTTP/1.1 400"));
    assert_eq!(res.body["error"]["code"], "INVALID_SOURCE_DOMAIN");

    // 2) Self-pairing prohibited
    let res = harness
        .post_json(
            "/api/sync-pairs",
            json!({
                "source_domain": "https://site-a.example.com",
                "target_domain": "https://site-a.example.com"
            }),
        )
        .await
        .unwrap();
    assert!(res.status_line.starts_with("HTTP/1.1 400"));
    assert_eq!(res.body["error"]["code"], "SELF_PAIRING_PROHIBITED");

    // 3) Domains not bound in domain-tokens.json
    let res = harness
        .post_json(
            "/api/sync-pairs",
            json!({
                "source_domain": "https://site-a.example.com",
                "target_domain": "https://site-b.example.com"
            }),
        )
        .await
        .unwrap();
    assert!(res.status_line.starts_with("HTTP/1.1 400"));
    assert_eq!(res.body["error"]["code"], "SOURCE_DOMAIN_NOT_BOUND");

    // Seed domain tokens with one ATS site and one WPMMCC site through the
    // REAL upsert route (state + runtime store stay consistent — the exact
    // integration the sqlite-backed WebUI relies on).
    for (domain, token, secret, identity) in [
        (
            "https://ats.example.com",
            "token-ats",
            "secret-ats",
            "wpmmcc_ats",
        ),
        (
            "https://sync-target.example.com",
            "token-target",
            "secret-target",
            "wpmmcc",
        ),
    ] {
        let seeded = harness
            .post_json(
                "/api/domain-tokens/upsert",
                json!({
                    "api_base_url": domain,
                    "wp_client_token": token,
                    "route_secret": secret,
                    "plugin_identity": identity,
                }),
            )
            .await
            .unwrap();
        assert!(
            seeded.status_line.starts_with("HTTP/1.1 200"),
            "seed upsert failed: {}",
            seeded.raw_body
        );
    }

    // 4) Pairing ATS with WPMMCC site is rejected by Identity Contract §5
    let res = harness
        .post_json(
            "/api/sync-pairs",
            json!({
                "source_domain": "https://ats.example.com",
                "target_domain": "https://sync-target.example.com"
            }),
        )
        .await
        .unwrap();
    assert!(res.status_line.starts_with("HTTP/1.1 400"));
    assert_eq!(res.body["error"]["code"], "identity_mismatch");
    assert!(
        res.body["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("WPMMCC ATS"),
        "error message must inform operator that one site runs ATS"
    );
}

#[tokio::test]
async fn sync_pairs_full_crud_and_lifecycle() {
    let harness = WebUiTestHarness::new("", None).await.unwrap();

    // Seed domain tokens with two valid WPMMCC sites through the REAL
    // upsert route (state + runtime store stay consistent).
    for (domain, token, secret) in [
        ("https://site-source.example.com", "token-src", "sec-src"),
        ("https://site-target.example.com", "token-tgt", "sec-tgt"),
    ] {
        let seeded = harness
            .post_json(
                "/api/domain-tokens/upsert",
                json!({
                    "api_base_url": domain,
                    "wp_client_token": token,
                    "route_secret": secret,
                    "plugin_identity": "wpmmcc",
                }),
            )
            .await
            .unwrap();
        assert!(
            seeded.status_line.starts_with("HTTP/1.1 200"),
            "seed upsert failed: {}",
            seeded.raw_body
        );
    }

    // 1a) sync_and_translate without a translate component is rejected —
    // the engine refuses to ship untranslated content under that mode.
    let res = harness
        .post_json(
            "/api/sync-pairs",
            json!({
                "name": "Bad translate pair",
                "source_domain": "https://site-source.example.com",
                "target_domain": "https://site-target.example.com",
                "sync_mode": "sync_and_translate",
                "post_types": ["post"]
            }),
        )
        .await
        .unwrap();
    assert!(res.status_line.starts_with("HTTP/1.1 400"));
    assert_eq!(res.body["error"]["code"], "TRANSLATE_COMPONENT_REQUIRED");

    // 1b) Create a valid sync_only pair
    let res = harness
        .post_json(
            "/api/sync-pairs",
            json!({
                "name": "Site Source -> Site Target",
                "source_domain": "https://site-source.example.com",
                "target_domain": "https://site-target.example.com",
                "direction": "unidirectional",
                "sync_mode": "sync_only",
                "source_lang": "en_US",
                "target_lang": "zh_CN",
                "conflict_strategy": "lww",
                "sync_frequency": "hourly",
                "post_types": ["post", "product"]
            }),
        )
        .await
        .unwrap();
    assert!(res.status_line.starts_with("HTTP/1.1 200"));
    assert_eq!(res.body["success"], true);

    let pair = &res.body["data"]["pair"];
    let pair_id = pair["id"].as_str().unwrap().to_string();
    assert!(!pair_id.is_empty());
    assert_eq!(pair["name"], "Site Source -> Site Target");
    assert_eq!(pair["sync_mode"], "sync_only");
    assert_eq!(pair["conflict_strategy"], "lww");
    assert_eq!(pair["status"], "active");

    // 2) List SyncPairs (+ credential section, empty by default)
    let list_res = harness.get_json("/api/sync-pairs").await.unwrap();
    assert_eq!(list_res.body["data"]["pairs"].as_array().unwrap().len(), 1);
    assert_eq!(
        list_res.body["data"]["credentials"]
            .as_array()
            .map(Vec::len),
        Some(0)
    );

    // 3) Pause SyncPair
    let pause_res = harness
        .post_json(&format!("/api/sync-pairs/{pair_id}/pause"), json!({}))
        .await
        .unwrap();
    assert!(pause_res.status_line.starts_with("HTTP/1.1 200"));
    assert_eq!(pause_res.body["data"]["pair"]["status"], "paused");

    // 4) Run on a paused pair is rejected
    let paused_run = harness
        .post_json(&format!("/api/sync-pairs/{pair_id}/run"), json!({}))
        .await
        .unwrap();
    assert!(paused_run.status_line.starts_with("HTTP/1.1 400"));
    assert_eq!(paused_run.body["error"]["code"], "PAIR_PAUSED");

    // 5) Resume SyncPair
    let resume_res = harness
        .post_json(&format!("/api/sync-pairs/{pair_id}/resume"), json!({}))
        .await
        .unwrap();
    assert!(resume_res.status_line.starts_with("HTTP/1.1 200"));
    assert_eq!(resume_res.body["data"]["pair"]["status"], "active");

    // 6) Run before pairing is rejected with an explicit precondition error
    let unpaired_run = harness
        .post_json(&format!("/api/sync-pairs/{pair_id}/run"), json!({}))
        .await
        .unwrap();
    assert!(unpaired_run.status_line.starts_with("HTTP/1.1 400"));
    assert_eq!(unpaired_run.body["error"]["code"], "SOURCE_NOT_PAIRED");

    // 7) Pairing endpoint: invalid role rejected before any network call
    let bad_role = harness
        .post_json(
            "/api/sync-pairs/pair",
            json!({
                "domain": "https://site-source.example.com",
                "pairing_code": "0f1a2b3c4d5e6f708192a3b4c5d6e7f8",
                "role": "sideways"
            }),
        )
        .await
        .unwrap();
    assert!(bad_role.status_line.starts_with("HTTP/1.1 400"));
    assert_eq!(bad_role.body["error"]["code"], "INVALID_ROLE");

    // 8) Pairing an ATS-identity site is rejected (identity gate)
    let ats_seeded = harness
        .post_json(
            "/api/domain-tokens/upsert",
            json!({
                "api_base_url": "https://ats-again.example.com",
                "wp_client_token": "token-ats2",
                "route_secret": "sec-ats2",
                "plugin_identity": "wpmmcc_ats",
            }),
        )
        .await
        .unwrap();
    assert!(ats_seeded.status_line.starts_with("HTTP/1.1 200"));
    let ats_pair = harness
        .post_json(
            "/api/sync-pairs/pair",
            json!({
                "domain": "https://ats-again.example.com",
                "pairing_code": "0f1a2b3c4d5e6f708192a3b4c5d6e7f8",
                "role": "source"
            }),
        )
        .await
        .unwrap();
    assert!(ats_pair.status_line.starts_with("HTTP/1.1 400"));
    assert_eq!(ats_pair.body["error"]["code"], "IDENTITY_MISMATCH");

    // 9) Deleting a credential that was never stored is a 404-style error
    let missing_cred = harness
        .delete_json("/api/sync-pairs/credentials/https://site-source.example.com")
        .await
        .unwrap();
    assert!(missing_cred.status_line.starts_with("HTTP/1.1 400"));
    assert_eq!(missing_cred.body["error"]["code"], "CREDENTIAL_NOT_FOUND");

    // 9b) The frontend sends encodeURIComponent(domain); the wire path is
    // percent-encoded, so the router must decode before matching. Store a
    // real credential and delete it through the encoded path.
    {
        use crate::config::sync_peer_credentials_file;
        use crate::sync_engine::credentials::{
            save_peer_credentials, PeerCredential, PeerCredentialsDoc,
            PEER_CREDENTIALS_SCHEMA_VERSION,
        };
        use std::collections::HashMap;

        let domain = "https://site-source.example.com";
        let mut peers = HashMap::new();
        peers.insert(
            domain.to_string(),
            PeerCredential {
                domain: domain.to_string(),
                peer_uuid: "peer-uuid-1".to_string(),
                peer_name: "Site Source".to_string(),
                shared_secret_hex: "00".repeat(32),
                key_scheme: "hmac_v1".to_string(),
                negotiated_direction: "push_only".to_string(),
                install_signature: "sig".to_string(),
                paired_at: 1,
                paired_as: "source".to_string(),
            },
        );
        let doc = PeerCredentialsDoc {
            schema_version: PEER_CREDENTIALS_SCHEMA_VERSION.to_string(),
            client_origin_uuid: "client-uuid-1".to_string(),
            client_origin_url: "http://client.local".to_string(),
            client_origin_name: "Test Client".to_string(),
            peers,
            updated_at: 1,
        };
        save_peer_credentials(&sync_peer_credentials_file(), &doc).unwrap();

        let encoded = urlencode(domain);
        let deleted = harness
            .delete_json(&format!("/api/sync-pairs/credentials/{encoded}"))
            .await
            .unwrap();
        assert!(
            deleted.status_line.starts_with("HTTP/1.1 200"),
            "encoded-domain credential delete failed: {}",
            deleted.raw_body
        );
        assert_eq!(deleted.body["data"]["domain"], domain);
        assert_eq!(deleted.body["data"]["deleted"], true);
    }

    // 10) Delete SyncPair
    let del_res = harness
        .post_json(
            "/api/sync-pairs/delete",
            json!({
                "id": pair_id
            }),
        )
        .await
        .unwrap();
    assert!(del_res.status_line.starts_with("HTTP/1.1 200"));
    assert_eq!(del_res.body["data"]["deleted"], true);

    // 11) Verify empty list after delete
    let list_after = harness.get_json("/api/sync-pairs").await.unwrap();
    assert_eq!(
        list_after.body["data"]["pairs"].as_array().unwrap().len(),
        0
    );
}

// X-1 (tasks/5.3falsh2/12 批 B): the pairing payload carries the canonical
// conflict strategy; the fifth value `merge` deserializes and legacy
// vocabulary (source_dominant) is rejected at the serde boundary — the
// client only ever speaks the canonical five-value wire.
#[test]
fn sync_pair_pair_request_conflict_strategy_wire_vocabulary() {
    use crate::web_ui::routes::sync_pairs::SyncPairPairRequest;

    for strategy in [
        "lww",
        "source_wins",
        "target_wins",
        "manual_review",
        "merge",
    ] {
        let req: SyncPairPairRequest = serde_json::from_value(json!({
            "domain": "https://site.example.com",
            "pairing_code": "0f1a2b3c4d5e6f708192a3b4c5d6e7f8",
            "role": "source",
            "conflict_strategy": strategy,
        }))
        .expect("canonical strategy must deserialize");
        assert_eq!(
            req.conflict_strategy.map(|s| s.as_wire_str()),
            Some(strategy)
        );
    }

    let err = serde_json::from_value::<SyncPairPairRequest>(json!({
        "domain": "https://site.example.com",
        "pairing_code": "0f1a2b3c4d5e6f708192a3b4c5d6e7f8",
        "role": "source",
        "conflict_strategy": "source_dominant",
    }));
    assert!(
        err.is_err(),
        "legacy/unknown strategy must be rejected at the wire boundary"
    );
}

#[tokio::test]
async fn sync_storage_corrupt_pairs_http_never_returns_empty_success_or_overwrites() {
    let harness = WebUiTestHarness::new("", None).await.unwrap();
    for domain in ["https://source.example.test", "https://target.example.test"] {
        let seeded = harness
            .post_json(
                "/api/domain-tokens/upsert",
                json!({
                    "api_base_url": domain, "wp_client_token": "owned-mock-token",
                    "route_secret": "owned-mock-secret", "plugin_identity": "wpmmcc"
                }),
            )
            .await
            .unwrap();
        assert!(seeded.status_line.contains("200"), "seed failed");
    }
    let path = crate::config::sync_pairs_file();
    let state_path = crate::config::sync_state_file();
    let raw = b"{broken pairs must survive}";
    std::fs::create_dir_all(std::path::Path::new(&path).parent().unwrap()).unwrap();
    std::fs::write(&path, raw).unwrap();
    let list = harness.get_json("/api/sync-pairs").await.unwrap();
    assert!(
        list.status_line.contains("500"),
        "bad read returned success: {list:?}"
    );
    assert_eq!(list.body["success"], false);
    let upsert = harness
        .post_json(
            "/api/sync-pairs",
            json!({
                "id": "new-pair", "source_domain": "https://source.example.test",
                "target_domain": "https://target.example.test", "sync_mode": "sync_only"
            }),
        )
        .await
        .unwrap();
    assert!(
        upsert.status_line.contains("500"),
        "upsert discarded corrupt source: {upsert:?}"
    );
    for (route, code) in [
        ("/api/sync-pairs/delete", "SYNC_PAIRS_DELETE_FAILED"),
        ("/api/sync-pairs/new-pair/pause", "SYNC_PAIRS_SAVE_FAILED"),
        ("/api/sync-pairs/new-pair/resume", "SYNC_PAIRS_SAVE_FAILED"),
        ("/api/sync-pairs/new-pair/run", "SYNC_PAIRS_READ_FAILED"),
    ] {
        let result = harness
            .post_json(route, json!({"id": "new-pair"}))
            .await
            .unwrap();
        assert!(
            result.status_line.contains("500"),
            "bad read hidden at {route}: {result:?}"
        );
        assert_eq!(result.body["success"], false);
        assert_eq!(
            result.body["error"]["code"], code,
            "verify the intended handler at {route}"
        );
    }
    assert_eq!(std::fs::read(&path).unwrap(), raw);
    assert!(
        !std::path::Path::new(&state_path).exists(),
        "failed requests must not initialize state"
    );
}

#[tokio::test]
async fn sync_storage_corrupt_state_blocks_run_and_delete_before_any_change() {
    let harness = WebUiTestHarness::new("", None).await.unwrap();
    let pair: crate::sync_engine::SyncPair = serde_json::from_value(json!({
        "id": "owned-corrupt-state", "name": "Owned",
        "source_domain": "https://source.example.test", "target_domain": "https://target.example.test",
        "sync_mode": "sync_only", "created_at": 1, "updated_at": 1
    })).unwrap();
    let mut doc = crate::sync_engine::SyncPairsDoc::default();
    crate::sync_engine::upsert_pair_in_doc(&mut doc, pair);
    let pairs_path = crate::config::sync_pairs_file();
    crate::sync_engine::save_sync_pairs(&pairs_path, &doc).unwrap();
    let before = std::fs::read(&pairs_path).unwrap();
    let state_path = crate::config::sync_state_file();
    std::fs::write(&state_path, b"{broken state}").unwrap();
    for (route, code) in [
        (
            "/api/sync-pairs/owned-corrupt-state/run",
            "SYNC_STATE_READ_FAILED",
        ),
        ("/api/sync-pairs/delete", "SYNC_PAIRS_DELETE_FAILED"),
    ] {
        let result = harness
            .post_json(route, json!({"id":"owned-corrupt-state"}))
            .await
            .unwrap();
        assert!(result.status_line.contains("500"), "{route}: {result:?}");
        assert_eq!(result.body["success"], false);
        assert_eq!(result.body["error"]["code"], code);
    }
    assert_eq!(std::fs::read(&pairs_path).unwrap(), before);
    assert_eq!(std::fs::read(&state_path).unwrap(), b"{broken state}");
    assert!(!std::path::Path::new(&crate::config::sync_peer_credentials_file()).exists());
}
