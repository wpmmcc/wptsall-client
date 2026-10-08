//! Private owned W3 persistence and overlapping-run regressions.

use crate::db::TestEnvVarGuard;
use sha2::{Digest, Sha256};

#[test]
fn successful_encrypted_install_has_secret_free_writer_receipt() {
    let _key = TestEnvVarGuard::set("WPTSALL_COMPONENT_BINDINGS_SECRET", "owned-w3-key");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owned-route-marker.json");
    let plain = r#"{"token":"owned-token-marker"}"#;
    let mut receipt = None;
    crate::bindings::save_encrypted_file_with_audit(&path, plain, |value| {
        receipt = Some(value);
    })
    .unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(crate::bindings::decrypt_from_bytes(&bytes).unwrap(), plain);
    let receipt = receipt.expect("successful atomic install must emit a writer receipt");
    assert_eq!(receipt["writer_pid"], std::process::id());
    assert_eq!(
        receipt["ciphertext_sha256"],
        format!("{:x}", Sha256::digest(&bytes))
    );
    assert_eq!(receipt["ciphertext_bytes"], bytes.len());
    assert_eq!(receipt["storage_format"], "WPTC-v1");
    let rendered = receipt.to_string();
    for marker in ["owned-token-marker", "owned-route-marker", "owned-w3-key"] {
        assert!(
            !rendered.contains(marker),
            "receipt must not contain credentials or paths"
        );
    }
    assert!(!rendered.contains(dir.path().to_str().unwrap()));
}

#[test]
fn rejected_encrypted_install_emits_no_success_receipt() {
    let _key = TestEnvVarGuard::set("WPTSALL_COMPONENT_BINDINGS_SECRET", "owned-w3-key");
    let dir = tempfile::tempdir().unwrap();
    let mut calls = 0;
    let result = crate::bindings::save_encrypted_file_with_audit(dir.path(), "owned", |_| {
        calls += 1;
    });
    assert!(result.is_err());
    assert_eq!(calls, 0);
    assert!(dir.path().is_dir());
}

#[tokio::test]
async fn overlapping_discovery_runs_keep_independent_item_limits() {
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::{Barrier, Mutex};

    let _key = TestEnvVarGuard::set("WPTSALL_COMPONENT_BINDINGS_SECRET", "owned-w3-key");
    let _transport = TestEnvVarGuard::set("WPTSALL_WP_TRANSPORT_ENCRYPT", "off");
    let dir = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("WPTSALL_DATA_DIR", dir.path().display().to_string());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let barrier = Arc::new(Barrier::new(2));
    let server = tokio::spawn(async move {
        let mut handlers = tokio::task::JoinSet::new();
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let barrier = Arc::clone(&barrier);
            handlers.spawn(async move {
                let mut raw = Vec::new();
                let mut buffer = [0; 8192];
                let end = loop {
                    let n = socket.read(&mut buffer).await.unwrap();
                    assert!(n > 0);
                    raw.extend_from_slice(&buffer[..n]);
                    assert!(raw.len() < 64 * 1024);
                    if let Some(end) = raw.windows(4).position(|b| b == b"\r\n\r\n") {
                        break end + 4;
                    }
                };
                let headers = std::str::from_utf8(&raw[..end]).unwrap();
                let target = headers.lines().next().unwrap().split_whitespace().nth(1).unwrap();
                let length = headers.lines().find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                }).unwrap_or(0);
                let target = target.to_owned();
                while raw.len() < end + length {
                    let n = socket.read(&mut buffer).await.unwrap();
                    assert!(n > 0);
                    raw.extend_from_slice(&buffer[..n]);
                }
                let body = if target.ends_with("/site-relations") {
                    barrier.wait().await;
                    serde_json::json!({"relations": [{
                        "id": 7, "source_site_id": 1, "target_site_id": 2,
                        "source_lang": "en", "target_lang": "zh",
                        "target_site_type": "site", "sync_mode": "push", "models": []
                    }]})
                } else if target.contains("/content-changes") {
                    serde_json::json!({"success": true, "data": {"items": [], "schema_version": 1}})
                } else if target.contains("/rules?") {
                    serde_json::json!({"rules": [{
                        "id": 1, "model_id": 1, "name": "owned-post",
                        "data_type": "post", "object_name": "post", "translate_fields": []
                    }]})
                } else if target.ends_with("/content/claim") {
                    serde_json::json!({"claimed_count": 4, "claimed_items":
                        (1..=4).map(|id| serde_json::json!({"object_id": id, "post_type": "post"})).collect::<Vec<_>>()})
                } else if target.contains("/content?") {
                    serde_json::json!({"items":
                        (1..=4).map(|id| serde_json::json!({
                            "object_type": "post_type", "subtype": "post", "object_id": id,
                            "complete_data": {"post_title": "Owned"}
                        })).collect::<Vec<_>>(), "total": 4, "page": 1, "per_page": 20})
                } else {
                    panic!("unexpected owned fixture route");
                };
                let bytes = serde_json::to_vec(&body).unwrap();
                let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len());
                socket.write_all(response.as_bytes()).await.unwrap();
                socket.write_all(&bytes).await.unwrap();
            });
        }
    });
    let run = |name: &'static str, limit: usize| {
        let base = base.clone();
        let log = dir.path().join(format!("{name}.log")).display().to_string();
        async move {
            let client = reqwest::Client::builder().no_proxy().build().unwrap();
            let db = Arc::new(Mutex::new(crate::db::open_db(":memory:").unwrap()));
            let pending = Arc::new(Mutex::new(
                crate::persistence::PendingCallbackStore::open(":memory:").unwrap(),
            ));
            let mut config = crate::worker::build_worker_config(name);
            config.apply_discovery_item_limit(Some(limit));
            crate::task_engine::discoverer::discover_and_translate(
                &client,
                &format!("{base}/{name}/wp-json/wptsall/v2/owned/client"),
                "owned-token",
                &log,
                None,
                None,
                "",
                &[],
                None,
                None,
                &config,
                Some("owned"),
                &pending,
                None,
                None,
                None,
                Some(db),
                crate::types::PluginIdentity::WpmmccAts,
            )
            .await
            .unwrap()
        }
    };
    let reports = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        tokio::join!(run("first", 1), run("second", 3))
    })
    .await;
    server.abort();
    let _ = server.await;
    let (first, second) = reports.expect("both owned HTTP runs must complete");
    assert_eq!((first.processed, second.processed), (1, 3));
}

#[test]
fn run_limit_configuration_preserves_default_zero_compatibility() {
    let _default = TestEnvVarGuard::set("WPTSALL_DISCOVERY_MAX_ITEMS_PER_RUN", "7");
    let mut config = crate::worker::build_worker_config("owned-budget");
    config.apply_discovery_item_limit(None);
    config.apply_discovery_item_limit(Some(0));
    assert_eq!(config.discovery_max_items_per_run, 7);
    config.apply_discovery_item_limit(Some(2));
    assert_eq!(config.discovery_max_items_per_run, 2);
    let _unbounded = TestEnvVarGuard::set("WPTSALL_DISCOVERY_MAX_ITEMS_PER_RUN", "0");
    assert_eq!(
        crate::worker::build_worker_config("owned-budget").discovery_max_items_per_run,
        0
    );
}
