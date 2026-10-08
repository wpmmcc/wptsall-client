use super::*;
mod boundaries;
mod process;
#[path = "../../../../../../tests/modules/client-wpplugin/unit/media_staging_validation.rs"]
pub(super) mod staging_validation;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Default)]
struct Observed {
    uploads: usize,
    imports: usize,
    statuses: usize,
    reconciles: usize,
    operation: String,
    sha: String,
    state: String,
    receipt_path: Option<std::path::PathBuf>,
}

async fn read_request(socket: &mut tokio::net::TcpStream) -> (String, Vec<u8>) {
    let mut request = Vec::new();
    let (end, size) = loop {
        let mut buffer = [0; 8192];
        let n = socket.read(&mut buffer).await.unwrap();
        assert!(n > 0);
        request.extend_from_slice(&buffer[..n]);
        if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
            let header = String::from_utf8_lossy(&request[..end]);
            let size = header
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            break (end + 4, size);
        }
    };
    while request.len() < end + size {
        let mut buffer = [0; 65536];
        let n = socket.read(&mut buffer).await.unwrap();
        assert!(n > 0);
        request.extend_from_slice(&buffer[..n]);
    }
    let headers = String::from_utf8_lossy(&request[..end]).into_owned();
    let mut body = request[end..end + size].to_vec();
    if !body.is_empty() {
        let envelope: Value = serde_json::from_slice(&body).unwrap();
        if let Some(payload) = envelope["encrypted_payload"].as_str() {
            body = crate::crypto::transport_decrypt(
                payload,
                envelope["nonce"].as_str().unwrap(),
                "owned-recovery-token",
            )
            .unwrap();
        }
    }
    (headers, body)
}

fn header<'a>(headers: &'a str, name: &str) -> &'a str {
    headers
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name).then(|| value.trim())
        })
        .unwrap()
}

async fn single_case(mode: &'static str) {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _data = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let asset = root.path().join("original.bin");
    std::fs::write(&asset, b"owned retained media").unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let observed = Arc::new(Mutex::new(Observed::default()));
    let state = observed.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let (headers, bytes) = read_request(&mut socket).await;
            let target = headers
                .lines()
                .next()
                .unwrap()
                .split_whitespace()
                .nth(1)
                .unwrap();
            let body = {
                let mut seen = state.lock().unwrap();
                if target.ends_with("/media-upload") {
                    seen.uploads += 1;
                    assert_eq!(bytes, b"owned retained media");
                    let id = header(&headers, "X-WPTSALL-Operation-ID");
                    let sha = header(&headers, "X-WPTSALL-Content-SHA256");
                    if !seen.operation.is_empty() {
                        assert_eq!(seen.operation, id);
                    }
                    seen.operation = id.into();
                    seen.sha = sha.into();
                    assert_eq!(sha, crate::sync_engine::hmac::sha256_hex(&bytes));
                    let signed = [
                        ("X-WPTSALL-Filename", header(&headers, "X-WPTSALL-Filename")),
                        ("X-WPTSALL-Source-ID", "77"),
                        ("X-WPTSALL-Task-ID", "22"),
                        ("X-WPTSALL-Relation-ID", "33"),
                        ("X-WPTSALL-Operation-ID", id),
                        ("X-WPTSALL-Content-SHA256", sha),
                    ];
                    let canonical = crate::crypto::build_canonical_string(
                        "POST",
                        target.strip_prefix("/wp-json").unwrap_or(target),
                        header(&headers, "X-WPTSALL-Timestamp"),
                        header(&headers, "X-WPTSALL-Signature-Nonce"),
                        &bytes,
                        &signed,
                    );
                    let signature = crate::crypto::compute_request_signature(
                        &crate::crypto::derive_signing_key("owned-recovery-token"),
                        &canonical,
                    );
                    assert_eq!(signature, header(&headers, "X-WPTSALL-Signature"));
                    if mode == "not_started" && seen.uploads == 1 {
                        continue;
                    }
                    seen.imports += 1;
                    seen.state = if mode == "explicit" {
                        "completion_unknown"
                    } else {
                        "result_ready"
                    }
                    .into();
                    if mode == "result_refused" && seen.uploads == 1 {
                        let store = RecoveryStore::configured().unwrap();
                        let row = operations().unwrap().pop().unwrap();
                        let path = root_receipt(&row.key);
                        std::fs::rename(&path, path.with_extension("owned-backup")).unwrap();
                        std::fs::create_dir(&path).unwrap();
                        seen.receipt_path = Some(path);
                        drop(store);
                    }
                    if !matches!(
                        mode,
                        "result_refused" | "not_started" | "wrong_immediate_scope"
                    ) {
                        continue;
                    }
                    json!({"success":true,"attachment_id":321,"operation_id":seen.operation,
                        "content_sha256":seen.sha,
                        "source_id":if mode=="wrong_immediate_scope"{78}else{77},"task_id":22,"relation_id":33})
                } else if target.ends_with("/reconcile") {
                    seen.reconciles += 1;
                    let request: Value = serde_json::from_slice(&bytes).unwrap();
                    let request = if let Some(payload) = request["encrypted_payload"].as_str() {
                        serde_json::from_slice::<Value>(
                            &crate::crypto::transport_decrypt(
                                payload,
                                request["nonce"].as_str().unwrap(),
                                "owned-recovery-token",
                            )
                            .unwrap(),
                        )
                        .unwrap()
                    } else {
                        request
                    };
                    assert_eq!(request["operation_id"], seen.operation);
                    seen.state = "result_ready".into();
                    json!({"success":true,"attachment_id":321})
                } else {
                    assert!(target.contains("/status?operation_id="));
                    seen.statuses += 1;
                    if seen.imports == 0 {
                        json!({"success":true,"data":{"state":"not_started","operation_id":seen.operation}})
                    } else {
                        json!({"success":true,"data":{
                            "state":seen.state,"attachment_id":321,"operation_id":seen.operation,
                            "content_sha256":seen.sha,"source_id":if matches!(mode,"wrong_scope"|"wrong_immediate_scope") {78} else {77},
                            "task_id":22,"relation_id":33,
                        }})
                    }
                }
            };
            let bytes = serde_json::to_vec(&body).unwrap();
            let signature = crate::web_ui::test_support::sign_wp_plaintext_response(
                if mode == "bad_signature" {
                    "wrong-token"
                } else {
                    "owned-recovery-token"
                },
                &bytes,
            );
            let response=format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nX-WPTSALL-Response-Signature: {signature}\r\nConnection: close\r\n\r\n",bytes.len());
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.write_all(&bytes).await.unwrap();
        }
    });
    let config = ChunkedUploadConfig {
        wp_base: format!("http://{address}/wp-json/wptsall/v2/owned/client"),
        token: "owned-recovery-token".into(),
        worker_id: "owned-worker".into(),
        device_id: "owned-device".into(),
        route_secret: Some("owned".into()),
        chunk_size: 0,
    };
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
    assert!(upload(
        &client,
        &config,
        asset.to_str().unwrap(),
        "owned.bin",
        "application/octet-stream",
        77,
        22,
        33,
        Some("owned-scope")
    )
    .await
    .is_err());
    if mode == "result_refused" {
        let path = observed.lock().unwrap().receipt_path.clone().unwrap();
        std::fs::remove_dir(&path).unwrap();
        std::fs::rename(path.with_extension("owned-backup"), &path).unwrap();
    }
    let row = operations().unwrap().pop().unwrap();
    assert_eq!(row.state, "complete_unknown");
    let encrypted = std::fs::read_to_string(root_receipt(&row.key)).unwrap();
    assert!(encrypted.starts_with("V1BUQw") && !encrypted.contains("owned-scope"));
    std::fs::remove_file(&asset).unwrap();
    if mode == "damaged_asset" {
        std::fs::write(&row.asset, b"tampered").unwrap();
    }
    #[cfg(unix)]
    if mode == "public_asset" {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&row.asset, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    if mode == "linked_asset" {
        std::fs::hard_link(&row.asset, root.path().join("outside-linked")).unwrap();
    }
    let resumed = resume_scope(&client, &config, "owned-scope").await;
    if matches!(
        mode,
        "wrong_scope"
            | "wrong_immediate_scope"
            | "bad_signature"
            | "damaged_asset"
            | "public_asset"
            | "linked_asset"
            | "explicit"
    ) {
        assert!(resumed.is_err());
        assert_eq!(observed.lock().unwrap().uploads, 1);
        if matches!(mode, "damaged_asset" | "public_asset" | "linked_asset") {
            assert_eq!(observed.lock().unwrap().statuses, 0);
        }
        if mode == "explicit" {
            let done = reconcile(&client, &config, &row.operation_id, None)
                .await
                .unwrap();
            assert_eq!(done.attachment_id, 321);
            assert_eq!(observed.lock().unwrap().reconciles, 1);
        }
    } else {
        assert_eq!(resumed.unwrap().unwrap().attachment_id, 321);
        assert_eq!(operations().unwrap()[0].state, "result_ready");
        assert_eq!(
            observed.lock().unwrap().uploads,
            if mode == "not_started" { 2 } else { 1 }
        );
    }
    assert_eq!(observed.lock().unwrap().imports, 1);
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}

fn root_receipt(key: &str) -> std::path::PathBuf {
    std::path::Path::new(&std::env::var("WPTSALL_DATA_DIR").unwrap())
        .join("media-recovery")
        .join(format!(
            "{}.receipt",
            crate::sync_engine::hmac::sha256_hex(key.as_bytes())
        ))
}

#[tokio::test]
async fn media_recovery_lost_single_response_reopens_without_source() {
    single_case("lost").await;
}
#[tokio::test]
async fn media_recovery_immediate_signed_result_must_match_operation_scope() {
    single_case("wrong_immediate_scope").await;
}
#[tokio::test]
async fn media_recovery_signed_not_started_reuses_same_operation() {
    single_case("not_started").await;
}
#[tokio::test]
async fn media_recovery_result_checkpoint_refusal_recovers_remote_receipt() {
    single_case("result_refused").await;
}
#[tokio::test]
async fn media_recovery_wrong_remote_scope_is_not_success() {
    single_case("wrong_scope").await;
}
#[tokio::test]
async fn media_recovery_bad_remote_signature_is_not_success() {
    single_case("bad_signature").await;
}
#[tokio::test]
async fn media_recovery_damaged_asset_stops_before_remote_lookup() {
    single_case("damaged_asset").await;
}
#[cfg(unix)]
#[tokio::test]
async fn media_recovery_public_asset_stops_before_remote_lookup() {
    single_case("public_asset").await;
}
#[cfg(unix)]
#[tokio::test]
async fn media_recovery_linked_asset_stops_before_remote_lookup() {
    single_case("linked_asset").await;
}
#[tokio::test]
async fn media_recovery_explicit_reconciliation_does_not_import_again() {
    single_case("explicit").await;
}

#[test]
fn media_recovery_file_lock_encryption_corruption_and_reopen() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let store = RecoveryStore::at(root.path().join("owned")).unwrap();
    store
        .update("owned-key", |old: Option<Value>| {
            assert!(old.is_none());
            Ok((json!({"private":"owned"}), ()))
        })
        .unwrap();
    let guard = store.guard("owned-key").unwrap();
    assert!(store.guard("owned-key").is_err());
    drop(guard);
    assert!(store.guard("owned-key").is_ok());
    let reopened = RecoveryStore::at(root.path().join("owned")).unwrap();
    assert_eq!(
        reopened.load::<Value>("owned-key").unwrap().unwrap()["private"],
        "owned"
    );
    let path = root.path().join("owned").join(format!(
        "{}.receipt",
        crate::sync_engine::hmac::sha256_hex(b"owned-key")
    ));
    let before = std::fs::read(&path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    std::fs::write(&path, b"{}").unwrap();
    assert!(reopened.load::<Value>("owned-key").is_err());
    assert!(reopened
        .update("owned-key", |_: Option<Value>| Ok((json!({}), ())))
        .is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"{}");
    std::fs::write(&path, before).unwrap();
    assert!(reopened.load::<Value>("owned-key").is_ok());
}

#[test]
fn media_recovery_operation_metadata_changes_request_signature() {
    let canonical = |id, sha| {
        crate::crypto::build_canonical_string(
            "POST",
            "/wptsall/v2/owned/client/media-upload",
            "123",
            "owned-nonce",
            b"owned",
            &[
                ("X-WPTSALL-Operation-ID", id),
                ("X-WPTSALL-Content-SHA256", sha),
            ],
        )
    };
    assert_ne!(
        canonical("operation-a", "hash-a"),
        canonical("operation-b", "hash-a")
    );
    assert_ne!(
        canonical("operation-a", "hash-a"),
        canonical("operation-a", "hash-b")
    );
}

#[tokio::test]
#[ignore = "requires explicitly prepared, verified owned WP HTTP fixture"]
async fn media_recovery_owned_wp_signed_http_single_and_50mib() {
    use base64::Engine;
    let _key = crate::db::owned_mock_bindings_key();
    let fixture: Value = serde_json::from_slice(
        &std::fs::read(
            std::env::var("WPTSALL_OWNED_MEDIA_HTTP_FIXTURE").expect("owned fixture required"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(fixture["format"], "owned-media-http-v1");
    let url = reqwest::Url::parse(fixture["wp_base"].as_str().unwrap()).unwrap();
    assert_eq!(url.host_str(), Some("127.0.0.1"));
    let label = std::process::Command::new("docker")
        .args([
            "inspect",
            fixture["container"].as_str().unwrap(),
            "--format",
            "{{index .Config.Labels \"com.wptsall.owned.run\"}}",
        ])
        .output()
        .unwrap();
    assert!(label.status.success());
    assert_eq!(
        String::from_utf8(label.stdout).unwrap().trim(),
        fixture["owner"].as_str().unwrap()
    );
    let root = tempfile::tempdir().unwrap();
    let _data = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let config = ChunkedUploadConfig {
        wp_base: url.into(),
        token: fixture["token"].as_str().unwrap().into(),
        worker_id: "owned-http-worker".into(),
        device_id: fixture["device_id"].as_str().unwrap().into(),
        route_secret: Some(fixture["route_secret"].as_str().unwrap().into()),
        chunk_size: 0,
    };
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let png = base64::engine::general_purpose::STANDARD
        .decode(fixture["png"].as_str().unwrap())
        .unwrap();
    for (size, scope) in [
        (png.len(), "owned-http-single"),
        (50 * 1024 * 1024, "owned-http-chunked"),
    ] {
        let file = root.path().join(format!("{scope}.png"));
        let mut bytes = png.clone();
        bytes.resize(size, 7);
        std::fs::write(&file, &bytes).unwrap();
        let result = upload(
            &client,
            &config,
            file.to_str().unwrap(),
            "owned-http.png",
            "image/png",
            fixture["source_id"].as_i64().unwrap(),
            0,
            fixture["relation_id"].as_i64().unwrap(),
            Some(scope),
        )
        .await;
        let diagnostic = result.as_ref().err().map(|error| {
            format!("{error:#}")
                .replace(&config.wp_base, "owned-wp")
                .replace(&config.token, "[owned-token]")
                .replace(config.route_secret.as_deref().unwrap(), "[owned-route]")
        });
        assert!(result.is_ok(), "owned WP {scope} failed: {diagnostic:?}");
        let id = result.unwrap().attachment_id;
        assert!(id > 0);
        std::fs::remove_file(file).unwrap();
        let row = operations()
            .unwrap()
            .into_iter()
            .find(|row| row.scope.as_deref() == Some(scope))
            .unwrap();
        let verified = reconcile(&client, &config, &row.operation_id, None).await;
        assert!(verified.is_ok(), "owned WP completion lookup failed");
        assert_eq!(verified.unwrap().attachment_id, id);
        assert_eq!(
            resume_scope(&client, &config, scope)
                .await
                .unwrap()
                .unwrap()
                .attachment_id,
            id
        );
    }
}

async fn chunk_restart_case(lost_init: bool) {
    use std::collections::BTreeSet;
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _data = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let file = root.path().join("owned-large.bin");
    let mut output = std::fs::File::create(&file).unwrap();
    for index in 0..10u8 {
        output.write_all(&vec![index + 1; 5 * 1024 * 1024]).unwrap();
    }
    drop(output);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let counts = Arc::new(Mutex::new((0, 0, 0)));
    let seen = counts.clone();
    let server = tokio::spawn(async move {
        let mut operation = String::new();
        let mut sha = String::new();
        let mut received = BTreeSet::new();
        let mut done = false;
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let (headers, bytes) = read_request(&mut socket).await;
            let target = headers
                .lines()
                .next()
                .unwrap()
                .split_whitespace()
                .nth(1)
                .unwrap();
            let body = if target.ends_with("/init") {
                seen.lock().unwrap().0 += 1;
                let init: Value = serde_json::from_slice(&bytes).unwrap();
                operation = init["operation_id"].as_str().unwrap().into();
                sha = init["content_sha256"].as_str().unwrap().into();
                if lost_init {
                    continue;
                }
                json!({"success":true,"upload_id":operation})
            } else if target.ends_with("/chunk") {
                let row = operations().unwrap().pop().unwrap();
                assert_eq!(row.upload_id.as_deref(), Some(operation.as_str()));
                assert_eq!(
                    row.state, "uploading",
                    "session checkpoint must commit before chunk writes"
                );
                let index = header(&headers, "X-WPTSALL-Chunk-Index")
                    .parse::<usize>()
                    .unwrap();
                assert!(
                    bytes.len() == 5 * 1024 * 1024 && bytes.iter().all(|b| *b == index as u8 + 1)
                );
                assert!(
                    received.insert(index),
                    "accepted chunks must not be resent after restart"
                );
                seen.lock().unwrap().1 += 1;
                json!({"success":true})
            } else if target.ends_with("/complete") {
                assert_eq!(received.len(), 10);
                seen.lock().unwrap().2 += 1;
                done = true;
                if !lost_init {
                    continue;
                }
                json!({"success":true,"attachment_id":321,"operation_id":operation,
                    "content_sha256":sha,"source_id":77,"task_id":22,"relation_id":33})
            } else {
                assert!(target.contains("/status?"));
                json!({"success":true,"data":{
                    "operation_id":operation,"upload_id":operation,"content_sha256":sha,
                    "source_id":77,"task_id":22,"relation_id":33,
                    "state":if done {"result_ready"} else {"uploading"},"attachment_id":if done {321} else {0},
                    "received_chunks":received.iter().copied().collect::<Vec<_>>(),"total_chunks":10,
                    "missing_chunks":(0..10).filter(|index|!received.contains(index)).collect::<Vec<_>>(),
                }})
            };
            let bytes = serde_json::to_vec(&body).unwrap();
            let signature = crate::web_ui::test_support::sign_wp_plaintext_response(
                "owned-recovery-token",
                &bytes,
            );
            let response=format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nX-WPTSALL-Response-Signature: {signature}\r\nConnection: close\r\n\r\n",bytes.len());
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.write_all(&bytes).await.unwrap();
        }
    });
    let config = ChunkedUploadConfig {
        wp_base: format!("http://{address}/wp-json/wptsall/v2/owned/client"),
        token: "owned-recovery-token".into(),
        worker_id: "owned-worker".into(),
        device_id: "owned-device".into(),
        route_secret: Some("owned".into()),
        chunk_size: 0,
    };
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    assert!(upload(
        &client,
        &config,
        file.to_str().unwrap(),
        "owned.bin",
        "application/octet-stream",
        77,
        22,
        33,
        Some("owned-large-scope")
    )
    .await
    .is_err());
    assert_eq!(
        operations().unwrap()[0].state,
        if lost_init {
            "init_unknown"
        } else {
            "complete_unknown"
        }
    );
    std::fs::remove_file(&file).unwrap();
    assert_eq!(
        resume_scope(&client, &config, "owned-large-scope")
            .await
            .unwrap()
            .unwrap()
            .attachment_id,
        321
    );
    assert_eq!(*counts.lock().unwrap(), (1, 10, 1));
    assert_eq!(
        resume_scope(&client, &config, "owned-large-scope")
            .await
            .unwrap()
            .unwrap()
            .attachment_id,
        321
    );
    assert_eq!(*counts.lock().unwrap(), (1, 10, 1));
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn media_recovery_lost_init_recovers_stable_session_before_chunks() {
    chunk_restart_case(true).await;
}
#[tokio::test]
async fn media_recovery_lost_chunk_completion_recovers_without_reimport() {
    chunk_restart_case(false).await;
}
