//! P0-LF-03 §5.4: rule discovery in local mode must discover domains from
//! the LOCAL domain-token bindings only, never call
//! `fetch_domains_for_session`, and return an empty success with a local
//! configuration issue (never `SESSION_REQUIRED`) when no local sites exist.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A loopback listener that counts accepted connections. Connections are
/// dropped immediately: any dial by the code under test is counted, and the
/// peer sees a broken response (which must never happen in the no-network
/// assertions).
async fn spawn_counting_listener() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();
    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                return;
            };
            counter_clone.fetch_add(1, Ordering::SeqCst);
            drop(socket);
        }
    });
    (format!("http://{}", addr), counter)
}

/// A loopback WP stand-in: counts requests, reads each HTTP request to its
/// declared Content-Length, and answers with a Protocol-v2 signed plaintext
/// `body` (signed with `wp_client_token`).
async fn spawn_wp_stub(wp_client_token: &str, body: String) -> (String, Arc<AtomicUsize>) {
    use tokio::io::AsyncWriteExt;
    let signature = crate::web_ui::test_support::sign_wp_plaintext_response(
        wp_client_token,
        body.as_bytes(),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            counter_clone.fetch_add(1, Ordering::SeqCst);
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 8192];
            loop {
                match tokio::io::AsyncReadExt::read(&mut socket, &mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        buffer.extend_from_slice(&chunk[..n]);
                        let text = String::from_utf8_lossy(&buffer);
                        let Some(header_end) = text.find("\r\n\r\n") else {
                            continue;
                        };
                        let content_length = text[..header_end]
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if buffer.len() >= header_end + 4 + content_length {
                            break;
                        }
                    }
                }
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-WPTSALL-Transport: plaintext\r\nX-WPTSALL-Response-Signature: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                signature,
                body.len(),
                body
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    });
    (format!("http://{}", addr), counter)
}

async fn run_discovery(state: &Arc<Mutex<WebUiState>>) -> String {
    let downstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let downstream_addr = downstream.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut client = tokio::net::TcpStream::connect(downstream_addr)
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).to_string()
    });
    let (mut socket, _) = downstream.accept().await.unwrap();
    handle_rule_component_binding_discovery(&mut socket, state)
        .await
        .unwrap();
    drop(socket);
    reader.await.unwrap()
}

#[tokio::test]
async fn rule_discovery_local_mode_without_sites_is_empty_success_without_network() {
    let _env_guard = components_env_lock().lock().unwrap();
    let _gate_guard = EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "0".to_string());

    // Any accidental dial lands on the counting listener (and is counted).
    let state = build_test_web_ui_state("http://127.0.0.1:1", Some("sess_test"));
    let (counting_base, counter) = spawn_counting_listener().await;
    {
        let mut guard = state.lock().await;
        guard.server_base = counting_base;
    }

    let response = run_discovery(&state).await;
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "no local sites must be an empty SUCCESS in local mode, got: {}",
        response
    );
    assert!(
        !response.contains("SESSION_REQUIRED"),
        "local mode must never answer SESSION_REQUIRED for rule discovery: {}",
        response
    );
    let payload = parse_http_json_body(&response);
    assert_eq!(
        payload["data"]["summary"]["domains_checked"],
        serde_json::json!(0),
        "no domains without local sites: {}",
        response
    );
    assert_eq!(payload["data"]["items"], serde_json::json!([]));
    let issues = payload["data"]["issues"].as_array().expect("issues array");
    assert_eq!(
        issues.len(),
        1,
        "exactly one local configuration issue expected: {}",
        response
    );
    assert_eq!(issues[0]["stage"], serde_json::json!("local_config"));
    assert_eq!(
        counter.load(Ordering::SeqCst),
        0,
        "local mode must never fetch domains from the website"
    );
}

#[tokio::test]
async fn rule_discovery_local_mode_discovers_from_local_bindings_only() {
    let _env_guard = components_env_lock().lock().unwrap();
    let _gate_guard = EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "0".to_string());

    // The "user's WP": a loopback stub answering valid (empty) relations so
    // the discovery flow completes. The website control plane is a counting
    // listener that must stay at zero.
    let (wp_base, wp_counter) =
        spawn_wp_stub("wptc-test-token", "{\"relations\":[]}".to_string()).await;
    let (website_base, website_counter) = spawn_counting_listener().await;
    let state = build_test_web_ui_state(&website_base, Some("sess_test"));
    {
        let mut guard = state.lock().await;
        guard.domain_token_bindings.domains.insert(
            wp_base.clone(),
            DomainTokenBindingEntry {
                wp_client_token: "wptc-test-token".to_string(),
                route_secret: "feedc0ffee".to_string(),
            },
        );
    }

    let response = run_discovery(&state).await;
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "local discovery with a configured site must succeed, got: {}",
        response
    );
    let payload = parse_http_json_body(&response);
    assert_eq!(
        payload["data"]["summary"]["domains_checked"],
        serde_json::json!(1),
        "the locally configured site must be discovered from the local bindings: {}",
        response
    );
    assert_eq!(
        payload["data"]["summary"]["issues"],
        serde_json::json!(0),
        "the stub WP answers valid empty relations, so no issues expected: {}",
        response
    );
    assert!(
        wp_counter.load(Ordering::SeqCst) > 0,
        "discovery must talk to the USER's WP directly (the counting WP stand-in)"
    );
    assert_eq!(
        website_counter.load(Ordering::SeqCst),
        0,
        "discovery must never contact the website control plane in local mode"
    );
}
