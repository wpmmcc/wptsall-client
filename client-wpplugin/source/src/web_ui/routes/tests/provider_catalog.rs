use super::*;
use crate::web_ui::test_support::WebUiTestHarness;

#[tokio::test]
async fn local_catalog_install_and_public_integration_pack_are_offline() {
    let _env_guard = components_env_lock().lock().unwrap();
    let harness = WebUiTestHarness::new("", None).await.unwrap();

    let catalog = harness.get_json("/api/provider-catalog").await.unwrap();
    assert!(catalog.status_line.starts_with("HTTP/1.1 200"));
    assert_eq!(catalog.body["data"]["offline"], true);
    assert!(catalog.body["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| { item["entry_id"] == "openai-compatible" && item["verified"] == true }));

    let installed = harness
        .post_json(
            "/api/components/local/install-from-catalog",
            json!({ "entry_id": "openai-compatible", "local_id": "catalog-text" }),
        )
        .await
        .unwrap();
    assert!(installed.status_line.starts_with("HTTP/1.1 200"));
    assert_eq!(installed.body["data"]["enabled"], false);

    let local = harness
        .get_json("/api/components/local/catalog-text")
        .await
        .unwrap();
    assert_eq!(
        local.body["data"]["template_json"]["id"],
        "openai-compatible-chat-completions-v1"
    );

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
    assert!(preview.status_line.starts_with("HTTP/1.1 200"));
    assert_eq!(preview.body["data"]["safe_to_import"], true);
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
