//! P0-LF-02: legacy-route gate tests.
//!
//! Behaviour tests drive the full router through `WebUiTestHarness` against a
//! counting loopback "server" and assert that, with the legacy gate disabled,
//! every legacy route is rejected (`LEGACY_CONTROL_PLANE_DISABLED`, HTTP 404)
//! BEFORE any network I/O happens (request count on the mock stays zero).
//!
//! The route-classification audit test re-parses the router source and fails
//! when a handler that performs legacy network I/O becomes reachable without
//! being covered by `legacy_routes::classify_legacy_route` (or without gating
//! itself through `server_control_plane_enabled`).

use super::*;
use crate::web_ui::test_support::WebUiTestHarness;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// A minimal counting loopback server: every accepted connection counts as
/// one request, whatever its method or path.
async fn spawn_counting_server() -> (String, Arc<AtomicUsize>) {
    use tokio::io::AsyncWriteExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let counter = std::sync::Arc::new(AtomicUsize::new(0));
    let counter_task = std::sync::Arc::clone(&counter);
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            counter_task.fetch_add(1, Ordering::SeqCst);
            // Read until the request (headers + Content-Length body) is done.
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 8192];
            loop {
                match tokio::io::AsyncReadExt::read(&mut socket, &mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        buffer.extend_from_slice(&chunk[..n]);
                        if request_complete(&buffer) {
                            break;
                        }
                    }
                }
            }
            let body = r#"{"success":true,"data":[]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    });
    (format!("http://{}", addr), counter)
}

/// True when `buffer` holds a complete HTTP request (headers present and the
/// declared Content-Length fully received).
fn request_complete(buffer: &[u8]) -> bool {
    let text = String::from_utf8_lossy(buffer);
    let Some(header_end) = text.find("\r\n\r\n") else {
        return false;
    };
    let content_length = text[..header_end]
        .lines()
        .find_map(|line| {
            let lower = line.to_ascii_lowercase();
            let value = lower.strip_prefix("content-length:")?;
            value.trim().parse::<usize>().ok()
        })
        .unwrap_or(0);
    buffer.len() >= header_end + 4 + content_length
}

/// Every legacy route from the P0-LF-02 table plus the `/api/v1/platform/*`
/// alias and the dynamic refresh-snapshot route, as request pairs.
fn legacy_request_cases() -> Vec<(&'static str, &'static str)> {
    vec![
        ("POST", "/api/components/refresh"),
        ("POST", "/api/components/template"),
        ("POST", "/api/domains/refresh"),
        ("POST", "/api/logout"),
        ("POST", "/api/oauth/start"),
        ("GET", "/oauth/callback"),
        ("GET", "/api/platform/products"),
        ("GET", "/api/platform/entitlements"),
        ("GET", "/api/vendors"),
        ("GET", "/api/wp-translation-providers"),
        ("GET", "/api/cloud-api-types"),
        ("GET", "/api/components/server-search"),
        ("POST", "/api/components/local/install-from-server"),
        // Obsolete frontend spellings must fail with the same code.
        ("GET", "/api/v1/platform/products"),
        ("GET", "/api/v1/platform/entitlements"),
        // Dynamic legacy route.
        ("POST", "/api/components/local/demo-component/refresh-snapshot"),
    ]
}

#[tokio::test]
async fn legacy_routes_are_rejected_without_network_io_when_gate_is_off() {
    let _env_guard = components_env_lock().lock().unwrap();
    let _gate_guard = EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "0".to_string());
    let (server_base, hits) = spawn_counting_server().await;
    let harness = WebUiTestHarness::new(&server_base, Some("sess_test"))
        .await
        .unwrap();

    for (method, path) in legacy_request_cases() {
        let response = match method {
            "POST" => harness
                .post_json(path, serde_json::json!({}))
                .await
                .unwrap(),
            _ => harness.get_json(path).await.unwrap(),
        };
        assert!(
            response.status_line.starts_with("HTTP/1.1 404"),
            "{method} {path} must be rejected with 404, got {}",
            response.status_line
        );
        assert_eq!(
            response.body["error"]["code"],
            serde_json::json!("LEGACY_CONTROL_PLANE_DISABLED"),
            "{method} {path} must report the disabled legacy code, got {}",
            response.body
        );
        assert_eq!(
            response.body["success"],
            serde_json::json!(false),
            "{method} {path} must not claim success"
        );
    }

    // The decisive assertion: not a single request reached the server.
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "legacy routes must be rejected before any network I/O"
    );
}

#[tokio::test]
async fn legacy_routes_reach_the_server_when_gate_is_on() {
    let _env_guard = components_env_lock().lock().unwrap();
    let _gate_guard = EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "1".to_string());
    let (server_base, hits) = spawn_counting_server().await;
    let harness = WebUiTestHarness::new(&server_base, Some("sess_test"))
        .await
        .unwrap();

    let response = harness.get_json("/api/vendors").await.unwrap();
    assert!(
        !response.status_line.starts_with("HTTP/1.1 404"),
        "with the gate on, /api/vendors must not be blocked by the classifier (got {})",
        response.status_line
    );
    assert_ne!(
        response.body["error"]["code"],
        serde_json::json!("LEGACY_CONTROL_PLANE_DISABLED"),
        "with the gate on the legacy error code must not be returned: {}",
        response.body
    );
    assert!(
        hits.load(Ordering::SeqCst) >= 1,
        "with the gate on the handler must reach the configured server"
    );
}

#[tokio::test]
async fn vendor_oauth_callback_is_never_blocked_by_the_gate() {
    let _env_guard = components_env_lock().lock().unwrap();
    let _gate_guard = EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "0".to_string());
    let (server_base, _hits) = spawn_counting_server().await;
    let harness = WebUiTestHarness::new(&server_base, None).await.unwrap();

    // The vendor callback handler may answer with a non-JSON body for an
    // unknown state, in which case `get_json` surfaces the raw response in
    // the error message. Either way the response must NOT be the legacy
    // gate's 404 JSON error.
    let result = harness.get_json("/oauth/vendor/callback?state=missing").await;
    let rendered = match result {
        Ok(response) => format!("{} {}", response.status_line, response.raw_body),
        Err(err) => format!("{err:#}"),
    };
    assert!(
        !rendered.contains("LEGACY_CONTROL_PLANE_DISABLED"),
        "/oauth/vendor/callback must not be misclassified as website OAuth: {rendered}"
    );
    assert!(
        !rendered.contains("HTTP/1.1 404"),
        "/oauth/vendor/callback must not be blocked by the gate: {rendered}"
    );
}

// ---------------------------------------------------------------------------
// Route-classification audit (static source analysis).
// ---------------------------------------------------------------------------

/// Function names that intentionally keep legacy fallbacks inside local
/// flows. Each entry is owned by another task; when that work lands, remove
/// the entry so the audit stays strict.
const AUDIT_ALLOWLIST: &[&str] = &[
    // P0-LF-03 §5.3: local component testing must not fall back to the
    // website for missing components.
    "handle_local_component_test",
    // Application self-update endpoints use update infrastructure, not the
    // website control plane.
    "handle_update_check",
    "handle_perform_update",
    // P0-LF-03 5.3 closed: `handle_bindings_upsert` now gates its legacy
    // fallback through `server_control_plane_enabled` and
    // `handle_task_type_components_upsert` delegates to the gated
    // `validate_task_type_component_target`, so both left this list.
    //
    // P0-LF-03 5.4 closed: `handle_rule_component_binding_discovery` now
    // discovers domains from local bindings and gates its legacy website
    // fallback through `server_control_plane_enabled`.
    //
    // P0-LF-03 5.4 (remaining): the domain-token test handler still contains
    // an ungated legacy fallback; removing it is that task's scope.
    "handle_domain_tokens_test",
];

/// Routes handled outside the static `match` table in `routes.rs`
/// (`handle_dynamic_routes`): `(method, path pattern, handler)`.
const DYNAMIC_ROUTES: &[(&str, &str, &str)] = &[(
    "POST",
    "/api/components/local/:id/refresh-snapshot",
    "handle_local_component_refresh_snapshot",
)];

/// Routes the classifier treats as legacy, mirroring
/// `legacy_routes::classify_legacy_route` (static + v1 prefix + dynamic).
fn function_name_is_flaggy(functions: &BTreeMap<String, String>, name: &str) -> bool {
    const FORBIDDEN_MARKERS: [&str; 4] = [
        "fetch_",
        "request_component_download",
        "fetch_server_template_snapshot",
        "refresh_local_component_snapshot_from_server",
    ];
    let Some(body) = functions.get(name) else {
        return false;
    };
    if body.contains("server_control_plane_enabled") {
        // The handler gates itself before legacy I/O; allowed in non-legacy
        // routes (e.g. worker flows that switch modes at runtime).
        return false;
    }
    // `fetch_*_for_session(` is the dominant legacy call shape.
    (body.contains("fetch_") && body.contains("_for_session("))
        || FORBIDDEN_MARKERS[1..]
            .iter()
            .any(|marker| body.contains(marker))
}

/// Routes declared in `routes.rs`: `(method, path, handler)` triples parsed
/// from the `match (method, target)` arms.
fn parse_router_routes(routes_rs: &str) -> Vec<(String, String, String)> {
    let mut routes = Vec::new();
    let mut lines = routes_rs.lines().peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("(\"") {
            continue;
        }
        let parse = parse_route_arm(trimmed);
        let Some((method, path)) = parse else {
            continue;
        };
        // The handler call appears on the same line or within the next few
        // lines (rustfmt wraps long calls).
        let mut handler = String::new();
        let mut lookahead = 0;
        let mut scan = line.to_string();
        while lookahead < 6 {
            if let Some(name) = first_handler_call(&scan) {
                handler = name;
                break;
            }
            let Some(next) = lines.next() else { break };
            scan.push_str(next);
            lookahead += 1;
        }
        if !handler.is_empty() {
            routes.push((method, path, handler));
        }
    }
    routes
}

fn parse_route_arm(line: &str) -> Option<(String, String)> {
    let open = line.find("(\"")?;
    let rest = &line[open + 2..]; // starts right after the opening quote
    let method_end = rest.find('"')?;
    let method = &rest[..method_end];
    if !method.chars().all(|c| c.is_ascii_uppercase()) {
        return None;
    }
    // Skip `, ` and the path's opening quote: `", "`.
    let after_method = &rest[method_end + 1..]; // starts at the closing quote
    let comma = after_method.find(',')?;
    let after_comma = &after_comma_slice(after_method, comma);
    let quote = after_comma.find('"')?;
    let path_rest = &after_comma[quote + 1..];
    let path_end = path_rest.find('"')?;
    let path = &path_rest[..path_end];
    Some((method.to_string(), path.to_string()))
}

fn after_comma_slice(s: &str, comma: usize) -> &str {
    &s[comma + 1..]
}

fn first_handler_call(text: &str) -> Option<String> {
    for candidate in text.split(|c: char| c == ',' || c == ';' || c == '{') {
        let trimmed = candidate.trim();
        if let Some(name) = trimmed.strip_suffix('(') {
            let name = name.trim();
            if name.starts_with("handle_") {
                return Some(name.to_string());
            }
        }
        if let Some(pos) = trimmed.find("handle_") {
            let rest = &trimmed[pos..];
            let name: String = rest
                .chars()
                .take_while(|c| char::is_alphanumeric(*c) || *c == '_')
                .collect();
            if rest.len() > name.len() && rest[name.len()..].starts_with('(') {
                return Some(name);
            }
        }
    }
    None
}

fn strip_fn_prefixes(trimmed: &str) -> Option<&str> {
    let mut rest = trimmed;
    for prefix in ["pub(crate) ", "pub(super) ", "pub "] {
        if let Some(stripped) = rest.strip_prefix(prefix) {
            rest = stripped;
            break;
        }
    }
    let rest = rest.strip_prefix("async ").unwrap_or(rest);
    rest.strip_prefix("fn ")
}

/// Parse every function definition in `source` into `name -> body` using the
/// convention that a function ends at the next `fn` line with indentation
/// less than or equal to its own.
fn parse_functions(source: &str) -> BTreeMap<String, String> {
    let mut functions: BTreeMap<String, String> = BTreeMap::new();
    let mut current: Option<(String, usize, String)> = None;
    for line in source.lines() {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        let is_fn_line = strip_fn_prefixes(trimmed)
            .map(|rest| {
                rest.split(|c: char| !char::is_alphanumeric(c) && c != '_')
                    .next()
                    .is_some_and(|name| !name.is_empty())
            })
            .unwrap_or(false);
        if let Some(name) = strip_fn_prefixes(trimmed).and_then(|rest| {
            let name: String = rest
                .chars()
                .take_while(|c| char::is_alphanumeric(*c) || *c == '_')
                .collect();
            if name.is_empty() {
                None
            } else {
                Some(name)
            }
        }) {
            if let Some((name, _, body)) = current.take() {
                functions.entry(name).or_insert(body);
            }
            current = Some((name, indent, String::new()));
            continue;
        }
        if let Some((_, _, body)) = current.as_mut() {
            body.push_str(line);
            body.push('\n');
        }
        let _ = is_fn_line;
    }
    if let Some((name, _, body)) = current.take() {
        functions.entry(name).or_insert(body);
    }
    functions
}

#[test]
fn classifier_covers_every_route_with_legacy_io() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let src_dir = std::path::Path::new(manifest_dir).join("src");

    let routes_rs = std::fs::read_to_string(src_dir.join("web_ui").join("routes.rs"))
        .expect("read routes.rs");
    let routes = parse_router_routes(&routes_rs);
    assert!(
        routes.len() >= 60,
        "router parse found only {} routes; the parse helpers must track the router format",
        routes.len()
    );

    // Collect every .rs source under src/ for the function bodies.
    let mut sources: BTreeMap<String, String> = BTreeMap::new();
    collect_sources(&src_dir, &mut sources);
    let mut functions: BTreeMap<String, String> = BTreeMap::new();
    for source in sources.values() {
        for (name, body) in parse_functions(source) {
            functions.entry(name).or_insert(body);
        }
    }

    // The classifier's legacy set, mirroring `legacy_routes.rs`.
    let legacy_routes = classifier_legacy_set();
    // Routes that the router handles dynamically.
    let mut all_routes: Vec<(String, String, String)> = routes
        .into_iter()
        .map(|(m, p, h)| (m, p, h))
        .collect();
    for (method, path, handler) in DYNAMIC_ROUTES {
        let route = path.replace(":id", "test-id");
        if legacy_routes.contains(&(method.to_string(), route.clone())) {
            all_routes.push((method.to_string(), route, handler.to_string()));
        }
    }

    let mut violations: Vec<String> = Vec::new();
    for (method, path, handler) in &all_routes {
        let in_classifier = legacy_routes.contains(&(method.clone(), path.clone()))
            || (method == "POST"
                && path.starts_with("/api/components/local/")
                && path.ends_with("/refresh-snapshot"))
            || path.starts_with("/api/v1/platform/");
        if in_classifier {
            continue;
        }
        if !function_name_is_flaggy(&functions, handler) {
            continue;
        }
        if AUDIT_ALLOWLIST.contains(&handler.as_str()) {
            continue;
        }
        violations.push(format!(
            "{method} {path} -> {handler} performs legacy network I/O but is not in the classifier"
        ));
    }
    assert!(
        violations.is_empty(),
        "legacy handlers discovered outside the classifier (P0-LF-02):\n{}",
        violations.join("\n")
    );
}

fn classifier_legacy_set() -> BTreeSet<(String, String)> {
    let mut set = BTreeSet::new();
    for (method, path) in [
        ("POST", "/api/components/refresh"),
        ("POST", "/api/components/template"),
        ("POST", "/api/domains/refresh"),
        ("POST", "/api/logout"),
        ("POST", "/api/oauth/start"),
        ("GET", "/oauth/callback"),
        ("GET", "/api/platform/products"),
        ("GET", "/api/platform/entitlements"),
        ("GET", "/api/vendors"),
        ("GET", "/api/wp-translation-providers"),
        ("GET", "/api/cloud-api-types"),
        ("GET", "/api/components/server-search"),
        ("POST", "/api/components/local/install-from-server"),
    ] {
        set.insert((method.to_string(), path.to_string()));
    }
    set
}

fn collect_sources(dir: &std::path::Path, out: &mut BTreeMap<String, String>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "target") {
                continue;
            }
            collect_sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            if let Ok(source) = std::fs::read_to_string(&path) {
                out.insert(path.display().to_string(), source);
            }
        }
    }
}
