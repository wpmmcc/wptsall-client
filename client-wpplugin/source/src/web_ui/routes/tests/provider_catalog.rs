use super::*;
use crate::bindings::{save_vendor_keys, save_vendor_oauth};
use crate::web_ui::test_support::WebUiTestHarness;

// catalog: WEBUI-API-GET-api-provider-catalog
// catalog: WEBUI-API-GET-api-provider-catalog-versions
// catalog: WEBUI-API-POST-api-components-local-import
// catalog: WEBUI-API-POST-api-components-local-install-from-catalog
// catalog: WEBUI-API-POST-api-provider-catalog-install-version
// catalog: WEBUI-API-POST-api-provider-catalog-refresh
// catalog: WEBUI-API-POST-api-integrations-pack-export
// catalog: WEBUI-API-POST-api-integrations-pack-preview
// oracle: L2
// (route-level catalog flow tests incl. install/import roundtrips and pack export/preview; F-T1 annotation batch 2026-09-22)

/// Build a v2 catalog with authenticated full-document version metadata.
#[allow(dead_code)]
fn signed_v2_catalog(
    version: &str,
    entry_id: &str,
    template_id: &str,
    private: &rsa::RsaPrivateKey,
) -> Value {
    let entries = json!([{
        "id": entry_id,
        "kind": "provider-template-pack",
        "vendor_id": "openai",
        "family": "openai_compatible",
        "source": "official-repo",
        "templates": [{
            "id": template_id,
            "name": format!("Test {template_id}"),
            "version": "1.0.0",
            "type": "text",
            "api_version": "1.0.0",
            "forked_from": null,
            "auth": { "fields": [{ "name": "api_key", "required": true }] },
            "request": {
                "method": "POST",
                "url": "https://api.openai.com/v1/chat/completions",
                "headers": { "Authorization": "Bearer {{auth.api_key}}" },
                "body": { "model": "gpt-4o-mini", "messages": [] }
            },
            "response": { "translated_text_path": "choices.0.message.content" },
            "constraints": {
                "split_strategy": "paragraph",
                "supported_content_formats": ["plain_text"]
            },
            "editable_params": [],
            "evidence": {
                "tier": "mock-verified",
                "verified_at": "2026-09-12T00:00:00Z",
                "cases": ["dual-ui-catalog-112:catalog:version-test"]
            },
            "examples": {
                "request": { "method": "POST", "url": "https://api.openai.com/v1/chat/completions" },
                "response": { "choices": [{ "message": { "content": "ok" } }] }
            }
        }]
    }]);
    signed_v2_catalog_entries(version, entries, private)
}

/// Sign an arbitrary entries array into a v2 manifest (same envelope shape
/// as the official repo; used by the X-11 media-lane gate tests).
#[allow(dead_code)]
fn signed_v2_catalog_entries(version: &str, entries: Value, private: &rsa::RsaPrivateKey) -> Value {
    use base64::Engine;
    use rsa::pkcs1v15::SigningKey;
    use rsa::signature::{SignatureEncoding, SignerMut};
    use sha2::Sha256;

    let entries_json = serde_json::to_vec(&entries).unwrap();
    let mut signing_key = SigningKey::<Sha256>::new_unprefixed(private.clone());
    let entries_sha256: String = {
        use sha2::Digest;
        let mut hasher = sha2::Sha256::new();
        hasher.update(&entries_json);
        hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    };
    let mut document = json!({
        "schema": "wptsall-provider-catalog-manifest.v2",
        "template_schema": "wptsall-provider-template.v2",
        "catalog_version": version,
        "created_at": "2026-09-24T00:00:00Z",
        "entries": entries,
        "entries_count": entries.as_array().map(Vec::len).unwrap_or(0),
        "entries_sha256": entries_sha256,
        "signature_scope": "provider-catalog-document-json-v2",
        "signature_algorithm": "RSA-SHA256",
        "signature_contract_version": "2",
        "signing_key_id": "test"
    });
    let payload = serde_json::to_vec(&document).unwrap();
    document["signature"] = json!(base64::engine::general_purpose::STANDARD
        .encode(signing_key.sign(&payload).to_bytes()));
    document
}

fn write_frozen_fixture_release(repo: &std::path::Path, catalog: &Value) {
    let version = catalog["catalog_version"].as_str().unwrap();
    let dir = repo.join("versions").join(version);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("catalog.json"),
        serde_json::to_vec_pretty(catalog).unwrap(),
    ).unwrap();
}

#[tokio::test]
async fn ui01_provider_catalog_filters_over_real_http() {
    use rsa::pkcs8::{EncodePublicKey, LineEnding};
    use rsa::{RsaPrivateKey, RsaPublicKey};

    let _env_guard = components_env_lock().lock().unwrap();
    let harness = WebUiTestHarness::new("", None).await.unwrap();
    let fixture_dir = tempfile::tempdir().unwrap();
    let base_path = fixture_dir.path().join("provider-catalog.json");
    let cache_path = fixture_dir.path().join("provider-catalog.current.json");
    let key_path = fixture_dir.path().join("catalog.pub.pem");
    let private = RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap();
    std::fs::write(
        &key_path,
        RsaPublicKey::from(&private)
            .to_public_key_pem(LineEnding::LF)
            .unwrap(),
    )
    .unwrap();
    let mut entries = Vec::new();
    for (entry_id, template_id, name, vendor, family) in [
        (
            "fixture-openai",
            "alpha-template",
            "Alpha Text",
            "openai",
            "openai_compatible",
        ),
        (
            "fixture-custom",
            "image-special",
            "图片 + &? #=%",
            "custom_http_mt",
            "custom_family",
        ),
        (
            "fixture-backup",
            "backup-template",
            "Backup Text",
            "openai_backup",
            "backup_family",
        ),
    ] {
        let mut entry =
            signed_v2_catalog("1.0.0", entry_id, template_id, &private)["entries"][0].clone();
        entry["vendor_id"] = json!(vendor);
        entry["family"] = json!(family);
        entry["templates"][0]["name"] = json!(name);
        entries.push(entry);
    }
    let catalog = signed_v2_catalog_entries("1.0.0", json!(entries), &private);
    let cache_bytes = serde_json::to_vec(&catalog).unwrap();
    std::fs::write(&cache_path, &cache_bytes).unwrap();
    let _catalog_guard = EnvVarGuard::set(
        "WPTSALL_PROVIDER_CATALOG_FILE",
        base_path.display().to_string(),
    );
    let _key_guard = EnvVarGuard::set(
        "WPTSALL_PROVIDER_CATALOG_PUBLIC_KEY_FILE",
        key_path.display().to_string(),
    );

    let encoded_name = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("q", "图片 + &? #=%")
        .finish();
    for (query, expected) in [
        (
            "",
            vec!["alpha-template", "image-special", "backup-template"],
        ),
        ("q=ALPHA", vec!["alpha-template"]),
        ("q=fixture-custom", vec!["image-special"]),
        ("q=image-special", vec!["image-special"]),
        ("q=custom_http_mt", vec!["image-special"]),
        ("q=OPENAI_COMPATIBLE", vec!["alpha-template"]),
        (encoded_name.as_str(), vec!["image-special"]),
        (
            "vendor_id=OPENAI",
            vec!["alpha-template", "backup-template"],
        ),
        ("vendor_id=custom", vec!["image-special"]),
        ("q=Alpha&vendor_id=OPENAI", vec!["alpha-template"]),
        ("q=Alpha&vendor_id=custom", vec![]),
        ("q=not-present", vec![]),
        ("vendor_id=not-present", vec![]),
        (
            "q=&vendor_id=",
            vec!["alpha-template", "image-special", "backup-template"],
        ),
    ] {
        let path = if query.is_empty() {
            "/api/provider-catalog".to_string()
        } else {
            format!("/api/provider-catalog?{query}")
        };
        let response = harness.get_json(&path).await.unwrap();
        assert!(
            response.status_line.starts_with("HTTP/1.1 200"),
            "{path}: {}",
            response.status_line
        );
        assert_eq!(response.body["success"], true, "{path}");
        assert_eq!(response.body["data"]["cache_source"], "current_cache");
        assert_eq!(response.body["data"]["catalog_version"], "1.0.0");
        let items = response.body["data"]["items"].as_array().unwrap();
        let actual: Vec<_> = items
            .iter()
            .map(|item| {
                assert_eq!(item["verified"], true, "{path}");
                item["template_id"].as_str().unwrap()
            })
            .collect();
        assert_eq!(actual, expected, "{path}");
    }
    assert_eq!(std::fs::read(cache_path).unwrap(), cache_bytes);
}

/// Historical-version flow (decision 3): browse the repository's frozen
/// releases, install an older one, and get correct install provenance.
#[tokio::test]
async fn catalog_versions_browse_and_install_historical_version() {
    use rsa::pkcs8::{EncodePublicKey, LineEnding};
    use rsa::{RsaPrivateKey, RsaPublicKey};

    let _env_guard = components_env_lock().lock().unwrap();
    let harness = WebUiTestHarness::new("", None).await.unwrap();

    // A miniature official repository on disk: current catalog + two frozen
    // releases behind versions.json.
    let repo_dir = tempfile::tempdir().expect("tempdir");
    let mut rng = rand::thread_rng();
    let private = RsaPrivateKey::new(&mut rng, 2048).expect("rsa");
    let public_pem = RsaPublicKey::from(&private)
        .to_public_key_pem(LineEnding::LF)
        .expect("pem");

    let current = signed_v2_catalog("1.0.0", "cur-openai", "cur-openai-v1", &private);
    let historical = signed_v2_catalog("0.9.0", "hist-openai", "hist-openai-v1", &private);
    std::fs::write(
        repo_dir.path().join("catalog.json"),
        serde_json::to_vec_pretty(&current).unwrap(),
    )
    .unwrap();
    let versions_dir = repo_dir.path().join("versions/0.9.0");
    std::fs::create_dir_all(&versions_dir).unwrap();
    std::fs::write(
        versions_dir.join("catalog.json"),
        serde_json::to_vec_pretty(&historical).unwrap(),
    )
    .unwrap();
    let entries_sha = historical["entries_sha256"].as_str().unwrap().to_string();
    std::fs::write(
        repo_dir.path().join("versions.json"),
        serde_json::to_vec_pretty(&json!({
            "versions": [
                {
                    "version": "0.9.0",
                    "released_at": "2026-09-01T00:00:00Z",
                    "entries_count": 1,
                    "entries_sha256": entries_sha,
                    "catalog_path": "versions/0.9.0/catalog.json"
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    // Point the (test-gated) source override at the miniature repository.
    let _source_guard = EnvVarGuard::set(
        "WPTSALL_PROVIDER_CATALOG_SOURCE_URL",
        format!("file://{}/catalog.json", repo_dir.path().display()),
    );
    let _key_guard = EnvVarGuard::set("WPTSALL_PROVIDER_CATALOG_PUBLIC_KEY_FILE", {
        let path = repo_dir.path().join("catalog.pub.pem");
        std::fs::write(&path, &public_pem).unwrap();
        path.display().to_string()
    });

    // 1) Browse frozen releases.
    let versions = harness
        .get_json("/api/provider-catalog/versions")
        .await
        .unwrap();
    assert!(versions.status_line.starts_with("HTTP/1.1 200"));
    assert_eq!(versions.body["data"]["source"], "remote");
    let list = versions.body["data"]["versions"].as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["version"], "0.9.0");

    // 2) Install the historical version; the current cache flips to it.
    let install = harness
        .post_json(
            "/api/provider-catalog/install-version",
            json!({ "version": "0.9.0", "confirm": true }),
        )
        .await
        .unwrap();
    assert!(
        install.status_line.starts_with("HTTP/1.1 200"),
        "install-version failed: {} body={}",
        install.status_line,
        serde_json::to_string(&install.body).unwrap_or_default()
    );
    assert_eq!(install.body["data"]["installed_version"], "0.9.0");
    assert_eq!(install.body["data"]["verified"], true);

    let listed = harness.get_json("/api/provider-catalog").await.unwrap();
    assert_eq!(listed.body["data"]["catalog_version"], "0.9.0");
    assert_eq!(listed.body["data"]["cache_state"], "current");
    assert!(
        listed.body["data"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["entry_id"] == "hist-openai"),
        "historical entries must be listed after install-version"
    );

    // 3) Install from the historical catalog; provenance must point at the
    //    historical template (acceptance: 溯源字段正确).
    let installed = harness
        .post_json(
            "/api/components/local/install-from-catalog",
            json!({ "entry_id": "hist-openai", "local_id": "from-history" }),
        )
        .await
        .unwrap();
    assert!(
        installed.status_line.starts_with("HTTP/1.1 200"),
        "install-from-catalog failed: {} body={}",
        installed.status_line,
        serde_json::to_string(&installed.body).unwrap_or_default()
    );
    let detail = harness
        .get_json("/api/components/local/from-history")
        .await
        .unwrap();
    assert_eq!(
        detail.body["data"]["template_json"]["id"], "hist-openai-v1",
        "installed component must carry the historical template"
    );
}

/// Signature tamper is fail-closed: a mutated signed catalog is rejected and
/// the previously cached current catalog is preserved untouched.
#[tokio::test]
async fn tampered_catalog_signature_fails_closed_and_keeps_cache() {
    use rsa::pkcs8::{EncodePublicKey, LineEnding};
    use rsa::{RsaPrivateKey, RsaPublicKey};

    let _env_guard = components_env_lock().lock().unwrap();
    let harness = WebUiTestHarness::new("", None).await.unwrap();

    let repo_dir = tempfile::tempdir().expect("tempdir");
    let mut rng = rand::thread_rng();
    let private = RsaPrivateKey::new(&mut rng, 2048).expect("rsa");
    let public_pem = RsaPublicKey::from(&private)
        .to_public_key_pem(LineEnding::LF)
        .expect("pem");

    let good = signed_v2_catalog("1.0.0", "good-openai", "good-openai-v1", &private);
    write_frozen_fixture_release(repo_dir.path(), &good);
    let mut tampered = good.clone();
    tampered["entries"][0]["templates"][0]["name"] = json!("Evil Renamed");
    std::fs::write(
        repo_dir.path().join("catalog.json"),
        serde_json::to_vec_pretty(&good).unwrap(),
    )
    .unwrap();

    let _source_guard = EnvVarGuard::set(
        "WPTSALL_PROVIDER_CATALOG_SOURCE_URL",
        format!("file://{}/catalog.json", repo_dir.path().display()),
    );
    let _key_guard = EnvVarGuard::set("WPTSALL_PROVIDER_CATALOG_PUBLIC_KEY_FILE", {
        let path = repo_dir.path().join("catalog.pub.pem");
        std::fs::write(&path, &public_pem).unwrap();
        path.display().to_string()
    });

    // Check without installing, then explicitly confirm the exact version.
    let refresh = harness
        .post_json("/api/provider-catalog/refresh", json!({}))
        .await
        .unwrap();
    assert!(refresh.status_line.starts_with("HTTP/1.1 200"));
    assert_eq!(refresh.body["data"]["available_version"], "1.0.0");
    assert_eq!(refresh.body["data"]["installed"], false);
    let installed = harness
        .post_json("/api/provider-catalog/install-version", json!({"version":"1.0.0","confirm":true}))
        .await.unwrap();
    assert!(installed.status_line.starts_with("HTTP/1.1 200"), "{installed:?}");

    // Serve the tampered catalog and refresh again: must fail closed.
    std::fs::write(
        repo_dir.path().join("catalog.json"),
        serde_json::to_vec_pretty(&tampered).unwrap(),
    )
    .unwrap();
    let rejected = harness
        .post_json("/api/provider-catalog/refresh", json!({}))
        .await
        .unwrap();
    assert!(
        rejected.status_line.starts_with("HTTP/1.1 422"),
        "tampered catalog must be rejected, got {} body={}",
        rejected.status_line,
        serde_json::to_string(&rejected.body).unwrap_or_default()
    );
    // S6 (batch G): the integrity DETAIL (signature failure vs entries-hash
    // mismatch) now goes to the client log via err_public; the response
    // envelope must carry the stable refresh-failure code so fail-closed
    // behavior stays assertable without leaking the chain outward.
    assert_eq!(
        rejected.body["error"]["code"], "PROVIDER_CATALOG_REFRESH_FAILED",
        "tampered catalog must be rejected with the refresh-failure code, got: {}",
        rejected.body
    );

    // The cached current catalog is unchanged.
    let listed = harness.get_json("/api/provider-catalog").await.unwrap();
    assert_eq!(listed.body["data"]["catalog_version"], "1.0.0");
    assert_eq!(listed.body["data"]["cache_state"], "current");
    assert!(
        listed.body["data"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["name"] == "Test good-openai-v1"),
        "cached catalog must still serve the untampered entry"
    );
}

#[tokio::test]
async fn clean_client_first_run_offline_then_cached_catalog_works() {
    let _env_guard = components_env_lock().lock().unwrap();
    let harness = WebUiTestHarness::new("", None).await.unwrap();

    // 1) Clean first run with an unreachable source: loads 15 embedded seed
    //    provider templates (WBS 3.4 / Revised Decision 2).
    let catalog = harness.get_json("/api/provider-catalog").await.unwrap();
    assert!(catalog.status_line.starts_with("HTTP/1.1 200"));
    assert_eq!(catalog.body["data"]["offline"], true);
    assert_eq!(catalog.body["data"]["first_fetch_required"], false);
    assert_eq!(
        catalog.body["data"]["cache_state"], "seed",
        "clean client must load embedded seed templates"
    );
    assert_eq!(
        catalog.body["data"]["items"].as_array().map(Vec::len),
        Some(15),
        "clean client must ship 15 core seed templates out of the box"
    );
    assert!(
        catalog.body["data"]["first_fetch_hint"]
            .as_str()
            .unwrap_or("")
            .contains("embedded seed"),
        "first-run hint must inform the user seed templates are active"
    );

    // 2) Offline refresh attempt: soft-fallback with a recorded error.
    let refresh = harness
        .post_json("/api/provider-catalog/refresh", json!({}))
        .await
        .unwrap();
    assert!(refresh.status_line.starts_with("HTTP/1.1 200"));
    assert_eq!(refresh.body["data"]["offline"], true);
    assert_eq!(refresh.body["data"]["fallback"], true);
    assert!(
        refresh.body["data"]["refresh_metadata"]["last_error"]
            .as_str()
            .is_some_and(|v| !v.is_empty()),
        "offline refresh must record the last error"
    );

    // 3) Once a local cache exists it keeps working offline (cached offline
    //    usability acceptance): seed the legacy cache path with a valid v1
    //    document, then install from it and export a pack — all offline.
    let _unsigned_guard = EnvVarGuard::set("WPTSALL_ALLOW_UNSIGNED_CATALOG", "1");
    let catalog_path = crate::config::provider_catalog_file();
    let seeded = json!({
        "schema": "wptsall-provider-catalog-manifest.v1",
        "catalog_version": "seed-1",
        "created_at": "2026-09-14T00:00:00Z",
        "entries": [{
            "id": "test-openai",
            "kind": "provider-template-pack",
            "vendor_id": "openai",
            "family": "openai_compatible",
            "source": "remote",
            "templates": [{
                "id": "test-openai-v1",
                "name": "Test OpenAI",
                "version": "1.0.0",
                "type": "text",
                "auth": { "fields": [{ "name": "api_key", "required": true }] },
                "request": {
                    "method": "POST",
                    "url": "https://api.openai.com/v1/chat/completions",
                    "headers": { "Authorization": "Bearer {{auth.api_key}}" },
                    "body": { "model": "gpt-4o-mini", "messages": [] }
                },
                "response": { "translated_text_path": "choices.0.message.content" },
                "constraints": {
                    "split_strategy": "paragraph",
                    "supported_content_formats": ["plain_text"]
                },
                "editable_params": [],
                "evidence_tier": "schema-only"
            }]
        }]
    });
    std::fs::write(&catalog_path, serde_json::to_vec_pretty(&seeded).unwrap()).unwrap();

    let cached = harness.get_json("/api/provider-catalog").await.unwrap();
    assert!(cached.status_line.starts_with("HTTP/1.1 200"));
    assert_eq!(cached.body["data"]["cache_state"], "local_file");
    assert_eq!(cached.body["data"]["first_fetch_required"], false);
    assert!(
        cached.body["data"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["entry_id"] == "test-openai" && item["verified"] == false),
        "cached catalog must list the seeded entry (unsigned seed => unverified)"
    );

    let installed = harness
        .post_json(
            "/api/components/local/install-from-catalog",
            json!({ "entry_id": "test-openai", "local_id": "catalog-text" }),
        )
        .await
        .unwrap();
    assert!(installed.status_line.starts_with("HTTP/1.1 200"));
    assert_eq!(installed.body["data"]["enabled"], false);

    let local = harness
        .get_json("/api/components/local/catalog-text")
        .await
        .unwrap();
    assert_eq!(local.body["data"]["template_json"]["id"], "test-openai-v1");

    let exported = harness
        .post_json("/api/integrations/pack/export", json!({}))
        .await
        .unwrap();
    assert!(exported.status_line.starts_with("HTTP/1.1 200"));
    assert_eq!(exported.body["data"]["export_mode"], "public");
    let pack = exported.body["data"]["pack"].clone();
    let text = serde_json::to_string(&pack).unwrap();
    assert!(!text.contains("wp_client_token"));
    assert!(!text.contains("auth_values"));
    assert!(!text.contains("Bearer "));

    let preview = harness
        .post_json("/api/integrations/pack/preview", json!({ "pack": pack }))
        .await
        .unwrap();
    if !preview.status_line.starts_with("HTTP/1.1 200") {
        panic!(
            "preview failed: {} body={}",
            preview.status_line,
            serde_json::to_string(&preview.body).unwrap_or_default()
        );
    }
    assert_eq!(preview.body["data"]["safe_to_import"], true);

    let _ = std::fs::remove_file(&catalog_path);
}

#[tokio::test]
async fn private_integration_pack_is_explicitly_encrypted() {
    let _env_guard = components_env_lock().lock().unwrap();
    let _device_guard =
        EnvVarGuard::set("WPTSALL_DEVICE_ID", "integration-pack-private-test-device");
    let harness = WebUiTestHarness::new("", None).await.unwrap();
    let response = harness
        .post_json(
            "/api/integrations/pack/export",
            json!({
                "mode": "private",
                "confirm": true,
                "passphrase": "portable-backup-passphrase"
            }),
        )
        .await
        .unwrap();
    assert!(response.status_line.starts_with("HTTP/1.1 200"));
    assert_eq!(response.body["data"]["encrypted"], true);
    assert!(
        response.body["data"]["payload_base64"]
            .as_str()
            .unwrap()
            .len()
            > 20
    );

    let preview = harness
        .post_json("/api/integrations/pack/preview", {
            let mut encrypted = response.body["data"].clone();
            encrypted["passphrase"] = json!("portable-backup-passphrase");
            encrypted
        })
        .await
        .unwrap();
    assert!(preview.status_line.starts_with("HTTP/1.1 200"));
    assert_eq!(preview.body["data"]["encrypted"], true);

    let wrong_passphrase = harness
        .post_json("/api/integrations/pack/preview", {
            let mut encrypted = response.body["data"].clone();
            encrypted["passphrase"] = json!("wrong-passphrase");
            encrypted
        })
        .await
        .unwrap();
    assert!(wrong_passphrase
        .status_line
        .starts_with("HTTP/1.1 400 Bad Request"));
}

/// Client todo #3 audit: no auth values may ride along any public export
/// channel, and private backups must never echo secrets as plaintext.
/// Plants distinct markers in every credential channel (pool key
/// auth_values, oauth client/cached/refresh tokens, static binding auth,
/// template auth field defaults) and asserts their absence end to end.
#[tokio::test]
async fn public_exports_strip_planted_auth_values_in_every_channel() {
    let _env_guard = components_env_lock().lock().unwrap();
    // The harness owns an isolated config dir; plant into it AFTER creation.
    let harness = WebUiTestHarness::new("", None).await.unwrap();
    let config_dir = harness.data_dir.parent().unwrap().join("config");
    let components_path = config_dir.join("components.json").display().to_string();
    let vendor_keys_path_value = config_dir.join("vendor-keys.json").display().to_string();
    let vendor_oauth_path_value = config_dir.join("vendor-oauth.json").display().to_string();

    let mut keys_doc = VendorKeysDoc::default();
    keys_doc.keys.insert(
        "key-audit".to_string(),
        VendorKey {
            vendor_id: "openai".to_string(),
            label: "Audit Key".to_string(),
            auth_values: std::collections::HashMap::from([
                ("api_key".to_string(), "PLANTED-POOL-KEY-SECRET".to_string()),
                (
                    "secret_key".to_string(),
                    "PLANTED-POOL-SECRETKEY".to_string(),
                ),
            ]),
            max_concurrent: 5,
            requests_per_second: 3.0,
            weight: 1,
            enabled: true,
            max_input_chars: 0,
            max_file_size_mb: 0.0,
        },
    );
    save_vendor_keys(&vendor_keys_path_value, &keys_doc).unwrap();

    let mut oauth_doc = VendorOAuthDoc::default();
    oauth_doc.configs.insert(
        "oauth-audit".to_string(),
        OAuthConfig {
            vendor_id: "openai".to_string(),
            label: "Audit OAuth".to_string(),
            grant_type: "client_credentials".to_string(),
            auth_url: String::new(),
            token_url: "https://login.example.com/token".to_string(),
            client_id: "audit-client".to_string(),
            client_secret: "PLANTED-OAUTH-CLIENT-SECRET".to_string(),
            scopes: String::new(),
            extra_params: std::collections::HashMap::new(),
            auth_extra_params: std::collections::HashMap::new(),
            cached_token: Some("PLANTED-OAUTH-CACHED-TOKEN".to_string()),
            cached_token_expires_at: 0,
            refresh_token: Some("PLANTED-OAUTH-REFRESH-TOKEN".to_string()),
            max_concurrent: 5,
            weight: 1,
            max_input_chars: 0,
            max_file_size_mb: 0.0,
            token_field: "access_token".to_string(),
        },
    );
    save_vendor_oauth(&vendor_oauth_path_value, &oauth_doc).unwrap();

    // The harness enables sqlite component storage, so the component must be
    // planted through the import API (which persists via the runtime doc),
    // not by writing the JSON mirror.
    let import_payload = json!({
        "export_version": 2,
        "redacted": false,
        "id": "comp-openai",
        "component": {
            "name": "Comp OpenAI",
            "template_id": "tpl-export-audit",
            "source_template_id": "tpl-export-audit",
            "source_template_api_version": "1.0.0",
            "vendor_id": "openai",
            "kind": "text",
            "enabled": true,
            "created_at": "1",
            "versions": {},
            "template_json": {
                "id": "tpl-export-audit",
                "name": "Tpl Export Audit",
                "version": "1.0.0",
                "type": "text_translation",
                "auth": {"fields": [{"name": "api_key", "value": "PLANTED-TPL-DEFAULT-SECRET"}]},
                "request": {"method": "POST", "url": "https://api.example.com/v1/translate"},
                "response": {"translated_text_path": "data.0.translated"}
            }
        }
    });
    let imported = harness
        .post_json("/api/components/local/import", import_payload)
        .await
        .unwrap();
    assert!(
        imported.status_line.starts_with("HTTP/1.1 200"),
        "component import failed: {} body={}",
        imported.status_line,
        serde_json::to_string(&imported.body).unwrap_or_default()
    );

    let bound = harness
        .post_json(
            "/api/components/bindings/upsert",
            json!({
                "component_id": "comp-openai",
                "key_ids": ["key-audit"],
                "oauth_ids": ["oauth-audit"],
                "auth": {"access_token": "PLANTED-BINDING-AUTH-SECRET"}
            }),
        )
        .await
        .unwrap();
    assert!(
        bound.status_line.starts_with("HTTP/1.1 200"),
        "binding upsert failed: {} body={}",
        bound.status_line,
        serde_json::to_string(&bound.body).unwrap_or_default()
    );

    let planted = [
        "PLANTED-TPL-DEFAULT-SECRET",
        "PLANTED-POOL-KEY-SECRET",
        "PLANTED-POOL-SECRETKEY",
        "PLANTED-OAUTH-CLIENT-SECRET",
        "PLANTED-OAUTH-CACHED-TOKEN",
        "PLANTED-OAUTH-REFRESH-TOKEN",
        "PLANTED-BINDING-AUTH-SECRET",
    ];

    // 1) Public integration pack export.
    let exported = harness
        .post_json("/api/integrations/pack/export", json!({}))
        .await
        .unwrap();
    assert!(exported.status_line.starts_with("HTTP/1.1 200"));
    assert_eq!(exported.body["data"]["export_mode"], "public");
    let pack_text = serde_json::to_string(&exported.body["data"]["pack"]).unwrap();
    for marker in planted {
        assert!(!pack_text.contains(marker), "public pack leaked {marker}");
    }
    // The pool-key value container must be absent from the pack structurally;
    // "auth_values" survives only as a *path string* in the redaction report.
    assert!(
        exported.body["data"]["pack"]["vendor_keys"]["keys"]["key-audit"]
            .get("auth_values")
            .is_none(),
        "vendor key auth_values must be stripped from the public pack"
    );
    assert!(
        exported.body["data"]["pack"]["vendor_oauth"]["configs"]["oauth-audit"]
            .get("client_secret")
            .is_none(),
        "oauth client_secret must be stripped from the public pack"
    );

    // 2) Single-component public export.
    let comp_export = harness
        .get_json("/api/components/local/comp-openai/export")
        .await
        .unwrap();
    assert!(comp_export.status_line.starts_with("HTTP/1.1 200"));
    assert_eq!(comp_export.body["data"]["redacted"], true);
    assert_eq!(comp_export.body["data"]["component"]["enabled"], false);
    let comp_text = serde_json::to_string(&comp_export.body).unwrap();
    for marker in planted {
        assert!(
            !comp_text.contains(marker),
            "component export leaked {marker}"
        );
    }

    // 3) Private pack: encrypted payload, no plaintext echo of secrets.
    let _device_guard = EnvVarGuard::set("WPTSALL_DEVICE_ID", "export-audit-device");
    let private = harness
        .post_json(
            "/api/integrations/pack/export",
            json!({
                "mode": "private",
                "confirm": true,
                "passphrase": "export-audit-passphrase"
            }),
        )
        .await
        .unwrap();
    assert!(private.status_line.starts_with("HTTP/1.1 200"));
    assert_eq!(private.body["data"]["encrypted"], true);
    let private_text = serde_json::to_string(&private.body).unwrap();
    for marker in planted {
        assert!(
            !private_text.contains(marker),
            "private pack response echoed {marker} as plaintext"
        );
    }

    let _ = std::fs::remove_file(&components_path);
    let _ = std::fs::remove_file(&vendor_keys_path_value);
    let _ = std::fs::remove_file(&vendor_oauth_path_value);
}

/// X-11 (批 S, 冻结表销账): non-text (image) and async (video dubbing)
/// media templates pass the v2 install gate end-to-end — the non-text input
/// vocabulary ({{input.source_ref}} etc.) mirrors the runner's
/// insert_component_non_text_context, and {{computed.job_id}} is admitted
/// inside an async_poll block; the stale-vocabulary rejections (unknown
/// input key, async computed key without async_poll) stay in force.
#[tokio::test]
async fn non_text_and_async_media_templates_pass_v2_install_gate() {
    use rsa::pkcs8::{EncodePublicKey, LineEnding};
    use rsa::{RsaPrivateKey, RsaPublicKey};

    let _env_guard = components_env_lock().lock().unwrap();
    let harness = WebUiTestHarness::new("", None).await.unwrap();

    let repo_dir = tempfile::tempdir().expect("tempdir");
    let mut rng = rand::thread_rng();
    let private = RsaPrivateKey::new(&mut rng, 2048).expect("rsa");
    let public_pem = RsaPublicKey::from(&private)
        .to_public_key_pem(LineEnding::LF)
        .expect("pem");

    let media_entries = json!([
        {
            "id": "custom-media-image-mt",
            "kind": "provider-template-pack",
            "vendor_id": "custom_media_image_mt",
            "family": "media_mt",
            "source": "custom",
            "templates": [{
                "id": "custom-media-image-localize-v1",
                "name": "Custom Media MT — Image Localization",
                "version": "1.0.0",
                "type": "image",
                "api_version": "1.0.0",
                "forked_from": null,
                "auth": { "fields": [{ "name": "api_key", "required": false }] },
                "constraints": {
                    "split_strategy": "none",
                    "supported_content_formats": ["media_ref"],
                    "input_artifact_kind": "image_file",
                    "output_artifact_kinds": ["translated_image_file"]
                },
                "editable_params": [
                    { "path": "request.url", "required": true, "scope": "config", "type": "string" }
                ],
                "request": {
                    "method": "POST",
                    "url": "https://example.com/media/translate/image",
                    "headers": {
                        "Authorization": "Bearer {{auth.api_key}}",
                        "Content-Type": "application/json"
                    },
                    "body": {
                        "source_ref": "{{input.source_ref}}",
                        "source_lang": "{{input.source_lang}}",
                        "target_lang": "{{input.target_lang}}"
                    }
                },
                "response": { "translated_image_ref_path": "translated_ref" },
                "evidence": {
                    "tier": "mock-verified",
                    "verified_at": "2026-09-24T00:00:00Z",
                    "cases": ["x11-media-mock:custom-media-image-mt"]
                },
                "examples": {
                    "request": { "method": "POST", "url": "https://example.com/media/translate/image" },
                    "response": { "translated_ref": "https://example.com/translated/image-zh.png" }
                }
            }]
        },
        {
            "id": "heygen",
            "kind": "provider-template-pack",
            "vendor_id": "heygen",
            "family": "media_mt",
            "source": "official-repo",
            "templates": [{
                "id": "heygen-video-translate-v2",
                "name": "HeyGen Video Translate (dubbing)",
                "version": "1.0.0",
                "type": "video",
                "api_version": "1.0.0",
                "forked_from": null,
                "auth": { "fields": [{ "name": "api_key", "required": true }] },
                "constraints": {
                    "split_strategy": "none",
                    "supported_content_formats": ["media_ref"],
                    "input_artifact_kind": "video_file",
                    "output_artifact_kinds": ["dubbed_video_file"]
                },
                "editable_params": [],
                "request": {
                    "method": "POST",
                    "url": "https://api.heygen.com/v2/video_translate",
                    "headers": {
                        "x-api-key": "{{auth.api_key}}",
                        "Content-Type": "application/json"
                    },
                    "body": {
                        "video_url": "{{input.source_url}}",
                        "source_language": "{{input.source_lang}}",
                        "target_language": "{{input.target_lang}}"
                    }
                },
                "response": {},
                "async_poll": {
                    "job_id_path": "data.video_translate_id",
                    "request": {
                        "method": "GET",
                        "url": "https://api.heygen.com/v2/video_translate/{{computed.job_id}}",
                        "headers": { "x-api-key": "{{auth.api_key}}" }
                    },
                    "status_path": "data.status",
                    "pending_values": ["pending", "processing"],
                    "done_values": ["success", "complete", "completed"],
                    "failed_values": ["failed", "error"],
                    "interval_seconds": 5,
                    "timeout_seconds": 600,
                    "result_ref_path": "data.video_url"
                },
                "evidence": {
                    "tier": "mock-verified",
                    "verified_at": "2026-09-24T00:00:00Z",
                    "cases": ["x11-media-mock:heygen-video-translate-v2"]
                },
                "examples": {
                    "request": { "method": "POST", "url": "https://api.heygen.com/v2/video_translate" },
                    "response": { "data": { "video_translate_id": "id-1", "status": "pending" } }
                }
            }]
        }
    ]);

    let good = signed_v2_catalog_entries("1.1.0", media_entries, &private);
    write_frozen_fixture_release(repo_dir.path(), &good);
    std::fs::write(
        repo_dir.path().join("catalog.json"),
        serde_json::to_vec_pretty(&good).unwrap(),
    )
    .unwrap();
    let _source_guard = EnvVarGuard::set(
        "WPTSALL_PROVIDER_CATALOG_SOURCE_URL",
        format!("file://{}/catalog.json", repo_dir.path().display()),
    );
    let _key_guard = EnvVarGuard::set("WPTSALL_PROVIDER_CATALOG_PUBLIC_KEY_FILE", {
        let path = repo_dir.path().join("catalog.pub.pem");
        std::fs::write(&path, &public_pem).unwrap();
        path.display().to_string()
    });

    // Positive arm: the whole media catalog refreshes through the v2 gate…
    let refresh = harness
        .post_json("/api/provider-catalog/refresh", json!({}))
        .await
        .unwrap();
    assert!(
        refresh.status_line.starts_with("HTTP/1.1 200"),
        "media catalog must pass the v2 gate: {} body={}",
        refresh.status_line,
        serde_json::to_string(&refresh.body).unwrap_or_default()
    );
    assert_eq!(refresh.body["data"]["available_version"], "1.1.0");
    assert_eq!(refresh.body["data"]["installed"], false);
    let installed_version = harness
        .post_json("/api/provider-catalog/install-version", json!({"version":"1.1.0","confirm":true}))
        .await.unwrap();
    assert!(installed_version.status_line.starts_with("HTTP/1.1 200"), "{installed_version:?}");

    // …and both templates install as components (async_poll + non-text
    // response paths deserialize into the client ComponentTemplate).
    for (entry_id, local_id) in [
        ("custom-media-image-mt", "x11-image-local"),
        ("heygen", "x11-heygen-video"),
    ] {
        let installed = harness
            .post_json(
                "/api/components/local/install-from-catalog",
                json!({ "entry_id": entry_id, "local_id": local_id }),
            )
            .await
            .unwrap();
        assert!(
            installed.status_line.starts_with("HTTP/1.1 200"),
            "{entry_id} must install: {} body={}",
            installed.status_line,
            serde_json::to_string(&installed.body).unwrap_or_default()
        );
    }
    let heygen_detail = harness
        .get_json("/api/components/local/x11-heygen-video")
        .await
        .unwrap();
    assert_eq!(
        heygen_detail.body["data"]["template_json"]["async_poll"]["job_id_path"],
        "data.video_translate_id",
        "the async_poll block must survive the install roundtrip"
    );

    // Negative arms (stale-vocabulary rejections stay in force):
    let reject_entries = json!([{
        "id": "reject-media-bogus",
        "kind": "provider-template-pack",
        "vendor_id": "reject_media_bogus",
        "family": "media_mt",
        "source": "custom",
        "templates": [{
            "id": "reject-media-bogus-v1",
            "name": "Reject",
            "version": "1.0.0",
            "type": "image",
            "api_version": "1.0.0",
            "forked_from": null,
            "auth": { "fields": [] },
            "request": {
                "method": "POST",
                "url": "https://example.com/media",
                "body": { "ref": "{{input.bogus_ref}}" }
            },
            "response": { "translated_image_ref_path": "translated_ref" },
            "evidence": {
                "tier": "mock-verified",
                "verified_at": "2026-09-24T00:00:00Z",
                "cases": ["x11-media-mock:negative"]
            },
            "examples": {
                "request": { "method": "POST", "url": "https://example.com/media" },
                "response": { "translated_ref": "https://example.com/t.png" }
            }
        }]
    }]);
    let reject_catalog = signed_v2_catalog_entries("1.2.0", reject_entries, &private);
    std::fs::write(
        repo_dir.path().join("catalog.json"),
        serde_json::to_vec_pretty(&reject_catalog).unwrap(),
    )
    .unwrap();
    let rejected = harness
        .post_json("/api/provider-catalog/refresh", json!({}))
        .await
        .unwrap();
    assert!(
        rejected.status_line.starts_with("HTTP/1.1 422"),
        "unknown input placeholder must fail closed, got {} body={}",
        rejected.status_line,
        serde_json::to_string(&rejected.body).unwrap_or_default()
    );

    // Async computed key WITHOUT an async_poll block is equally rejected.
    let reject_async_entries = json!([{
        "id": "reject-async-no-poll",
        "kind": "provider-template-pack",
        "vendor_id": "reject_async_no_poll",
        "family": "media_mt",
        "source": "custom",
        "templates": [{
            "id": "reject-async-no-poll-v1",
            "name": "Reject Async",
            "version": "1.0.0",
            "type": "video",
            "api_version": "1.0.0",
            "forked_from": null,
            "auth": { "fields": [{ "name": "api_key", "required": true }] },
            "request": {
                "method": "GET",
                "url": "https://example.com/job/{{computed.job_id}}",
                "headers": { "x-api-key": "{{auth.api_key}}" }
            },
            "response": { "translated_video_ref_path": "data.video_url" },
            "evidence": {
                "tier": "mock-verified",
                "verified_at": "2026-09-24T00:00:00Z",
                "cases": ["x11-media-mock:negative-async"]
            },
            "examples": {
                "request": { "method": "GET", "url": "https://example.com/job/id" },
                "response": { "data": { "video_url": "https://example.com/v.mp4" } }
            }
        }]
    }]);
    let reject_async = signed_v2_catalog_entries("1.3.0", reject_async_entries, &private);
    std::fs::write(
        repo_dir.path().join("catalog.json"),
        serde_json::to_vec_pretty(&reject_async).unwrap(),
    )
    .unwrap();
    let rejected_async = harness
        .post_json("/api/provider-catalog/refresh", json!({}))
        .await
        .unwrap();
    assert!(
        rejected_async.status_line.starts_with("HTTP/1.1 422"),
        "async computed key without async_poll must fail closed, got {} body={}",
        rejected_async.status_line,
        serde_json::to_string(&rejected_async.body).unwrap_or_default()
    );
}
