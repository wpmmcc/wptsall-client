//! Headless OTA command E2E for the Desktop update commands.
//!
//! Closes the `run-ota-secure-smoke.sh` "full Tauri UI automation is TODO"
//! scaffold gap: the updater command layer (`commands::update`) is now
//! exercised end-to-end through the REAL Tauri command functions against a
//! local mock update server — no display server, WebKitGTK, or tauri-driver
//! required. The remaining GUI-automation gap is only the window/button
//! click layer; every security decision below it is asserted here.
//!
//! Fail-closed invariants asserted through the real command layer:
//! 1. `check_for_update` refuses an unsigned releases manifest in
//!    production posture (no debug valves): `.minisig` is required.
//! 2. `check_for_update` parses a UI-only update channel.
//! 3. `perform_update` refuses a UI-only update when no SecurityGate is
//!    bootstrapped and no debug valve is set (production OTA posture).
//! 4. `perform_update` applies a checksum-verified UI-only bundle through
//!    the explicit debug valve path (dev-only posture, guide 16 S11).
//!
//! Requires the crate `test` feature (tauri mock runtime):
//! `cargo test --manifest-path client-desktop/src-tauri/Cargo.toml \
//!   --features test --test ota_commands`

#![cfg(feature = "test")]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use wptsall_desktop_lib::commands::update::{check_for_update, perform_update};

/// Serializes the OTA tests: they mutate process-global env vars
/// (`WPTSALL_RELEASES_URL`, `WPTSALL_INSTALL_ROOT`, debug security valves)
/// that the updater reads at call time.
static OTA_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Restores the previous value (or absence) of an env var on drop.
struct EnvRestore {
    key: &'static str,
    previous: Option<String>,
}

impl EnvRestore {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

fn temp_test_dir(name: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "wptsall-desktop-ota-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

/// Static-file mock update server: maps request path -> (status, body).
/// The file set is built by `build_files` with the bound base URL, so the
/// manifest and checksum entries can point back at the live port. Serves
/// every method; the OTA flow only issues GETs.
async fn start_update_mock(
    build_files: impl FnOnce(&str) -> HashMap<String, (u16, Vec<u8>)>,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let base = format!("http://127.0.0.1:{port}");
    let files = build_files(&base);
    let handle = tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                break;
            };
            let files = files.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(socket);
                let mut request_path = String::new();
                let mut content_length: usize = 0;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    if line == "\r\n" {
                        break;
                    }
                    if request_path.is_empty() && line.starts_with("GET ") {
                        request_path = line
                            .split_whitespace()
                            .nth(1)
                            .unwrap_or_default()
                            .to_string();
                    }
                    let lower = line.to_lowercase();
                    if lower.starts_with("content-length:") {
                        content_length =
                            lower["content-length:".len()..].trim().parse().unwrap_or(0);
                    }
                }
                if content_length > 0 {
                    let mut buf = vec![0u8; content_length];
                    let _ = reader.read_exact(&mut buf).await;
                }
                let (status, body) = match files.get(&request_path) {
                    Some((status, body)) => (*status, body.clone()),
                    None => (404, b"not found".to_vec()),
                };
                let reason = match status {
                    200 => "OK",
                    404 => "Not Found",
                    _ => "Status",
                };
                let resp = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = reader.get_mut().write_all(resp.as_bytes()).await;
                let _ = reader.get_mut().write_all(&body).await;
            });
        }
    });
    tokio::time::sleep(Duration::from_millis(10)).await;
    (base, handle)
}

/// Build a UI bundle tarball (`ui/desktop/…` + root `VERSION-WEBUI`) plus its
/// SHA256SUMS entry, exactly the artifact shape the real OTA channel ships.
fn build_ui_bundle(dir: &Path) -> (Vec<u8>, String) {
    let staging = dir.join("bundle-src");
    let ui_desktop = staging.join("ui").join("desktop");
    std::fs::create_dir_all(&ui_desktop).unwrap();
    let marker = "<html><body>desktop ui 9.9.9 from ota_commands</body></html>";
    std::fs::write(ui_desktop.join("index.html"), marker).unwrap();
    std::fs::write(staging.join("VERSION-WEBUI"), "9.9.9").unwrap();

    let archive = dir.join("ui-bundle.tar.gz");
    let status = Command::new("tar")
        .args(["-czf"])
        .arg(&archive)
        .arg("-C")
        .arg(&staging)
        .args(["ui", "VERSION-WEBUI"])
        .status()
        .expect("failed to spawn tar for UI bundle fixture");
    assert!(status.success(), "tar must pack the UI bundle fixture");

    let bytes = std::fs::read(&archive).unwrap();
    let hash = sha256_of(&archive);
    (bytes, hash)
}

fn sha256_of(path: &Path) -> String {
    let output = Command::new("sha256sum")
        .arg(path)
        .output()
        .expect("failed to spawn sha256sum");
    assert!(output.status.success(), "sha256sum must succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .split_whitespace()
        .next()
        .expect("sha256sum output must start with the hash")
        .to_string()
}

/// Standard OTA env posture for the suite: releases URL + isolated install
/// root carrying an OLD `VERSION-WEBUI` (0.9.0), so the 9.9.9 UI channel is
/// strictly newer.
fn ota_env(dir: &Path) -> Vec<EnvRestore> {
    let install_root = dir.join("root");
    std::fs::create_dir_all(&install_root).unwrap();
    std::fs::write(install_root.join("VERSION-WEBUI"), "0.9.0").unwrap();
    vec![
        // Placeholder releases URL; each test binds its own mock and
        // re-points the variable inside the guard list order below.
        EnvRestore::set("WPTSALL_INSTALL_ROOT", &install_root.display().to_string()),
    ]
}

/// releases.json with a binary product pinned OLD (never triggers the
/// self-replace path) and a UI product at 9.9.9 (newer than the 0.9.0
/// VERSION-WEBUI the test installs), pointing at the mock server.
fn releases_manifest(base: &str) -> serde_json::Value {
    serde_json::json!({
        "data": {
            "schema_version": 1,
            "products": {
                "client-desktop": {
                    "latest_version": "0.0.1",
                    "min_supported_version": "0.0.1",
                    "release_notes_url": "",
                    "download_url_template": "",
                    "signature_url_template": "",
                    "mandatory": false
                },
                "client-desktop-webui": {
                    "latest_version": "9.9.9",
                    "min_supported_version": "0.0.1",
                    "release_notes_url": "",
                    "download_url_template": format!("{base}/ui-bundle.tar.gz"),
                    "signature_url_template": format!("{base}/ui-desktop-SHA256SUMS.minisig"),
                    "mandatory": false
                }
            }
        }
    })
}

/// Full mock file set: manifest + UI bundle + SHA256SUMS, no `.minisig`
/// companions (the suite exercises the unsigned-manifest debug valve and
/// the fail-closed refusal, not the pinned production signature — that key
/// pair is not testable offline by design).
fn mock_files(base: &str, bundle_bytes: &[u8], bundle_hash: &str) -> HashMap<String, (u16, Vec<u8>)> {
    let mut files = HashMap::new();
    files.insert(
        "/releases.json".to_string(),
        (200, serde_json::to_vec(&releases_manifest(base)).unwrap()),
    );
    files.insert("/ui-bundle.tar.gz".to_string(), (200, bundle_bytes.to_vec()));
    files.insert(
        "/ui-desktop-SHA256SUMS".to_string(),
        (
            200,
            format!("{bundle_hash}  ui-bundle.tar.gz\n").into_bytes(),
        ),
    );
    files
}

fn mock_app_handle() -> tauri::AppHandle<tauri::test::MockRuntime> {
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .expect("mock tauri app");
    app.handle().clone()
}

#[tokio::test]
async fn ota_check_fail_closed_on_unsigned_manifest() {
    let _env_guard = OTA_ENV_LOCK.lock().unwrap();
    let dir = temp_test_dir("unsigned-manifest");
    let _env = ota_env(&dir);

    // Production posture: NO debug valves. The manifest is served WITHOUT
    // its .minisig companion, so the check must fail closed.
    let _allow_unsigned = EnvRestore::set("WPTSALL_ALLOW_UNSIGNED_MANIFEST", "0");
    let _skip = EnvRestore::set("WPTSALL_SKIP_SECURITY", "0");

    let (base, mock) = start_update_mock(|base| {
        let mut files = HashMap::new();
        files.insert(
            "/releases.json".to_string(),
            (
                200,
                serde_json::to_vec(&releases_manifest(base)).unwrap(),
            ),
        );
        files
    })
    .await;
    let _releases = EnvRestore::set("WPTSALL_RELEASES_URL", &format!("{base}/releases.json"));

    let err = check_for_update()
        .await
        .expect_err("unsigned manifest must fail closed through the real command");
    assert!(
        err.contains("minisig"),
        "error must demand the manifest signature, got: {err}"
    );

    mock.abort();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn ota_check_parses_ui_only_channel() {
    let _env_guard = OTA_ENV_LOCK.lock().unwrap();
    let dir = temp_test_dir("ui-only-check");
    let _env = ota_env(&dir);

    let (bundle_bytes, bundle_hash) = build_ui_bundle(&dir);
    let _allow_unsigned = EnvRestore::set("WPTSALL_ALLOW_UNSIGNED_MANIFEST", "1");

    let (base, mock) =
        start_update_mock(|base| mock_files(base, &bundle_bytes, &bundle_hash)).await;
    let _releases = EnvRestore::set("WPTSALL_RELEASES_URL", &format!("{base}/releases.json"));

    let check = check_for_update().await.expect("ui-only channel parses");
    assert!(
        check.update_available,
        "ui-only channel must report update_available"
    );
    assert_eq!(check.update_kind, "ui");
    assert!(!check.mandatory);
    assert_eq!(check.ui_current_version, "0.9.0");
    assert_eq!(check.ui_latest_version, "9.9.9");
    assert!(check.ui_update_available);
    assert_eq!(
        check.ui_download_url_template,
        format!("{base}/ui-bundle.tar.gz")
    );

    mock.abort();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn ota_perform_update_fail_closed_without_gate() {
    let _env_guard = OTA_ENV_LOCK.lock().unwrap();
    let dir = temp_test_dir("refuse-no-gate");
    let _env = ota_env(&dir);

    let (bundle_bytes, bundle_hash) = build_ui_bundle(&dir);
    // Allow the (dev-lane) unsigned manifest so the check itself succeeds,
    // but keep the security-skip valve OFF: with no bootstrapped gate the
    // apply must refuse.
    let _allow_unsigned = EnvRestore::set("WPTSALL_ALLOW_UNSIGNED_MANIFEST", "1");
    let _skip = EnvRestore::set("WPTSALL_SKIP_SECURITY", "0");

    let (base, mock) =
        start_update_mock(|base| mock_files(base, &bundle_bytes, &bundle_hash)).await;
    let _releases = EnvRestore::set("WPTSALL_RELEASES_URL", &format!("{base}/releases.json"));

    let handle = mock_app_handle();
    let err = perform_update(handle)
        .await
        .expect_err("perform_update must refuse without gate or valve");
    assert!(
        err.contains("security gate inactive"),
        "refusal must name the security gate, got: {err}"
    );
    // Nothing may be applied: no ui/ tree, version file unchanged.
    let install_root = dir.join("root");
    assert!(!install_root.join("ui").exists());
    assert_eq!(
        std::fs::read_to_string(install_root.join("VERSION-WEBUI")).unwrap(),
        "0.9.0"
    );

    mock.abort();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn ota_perform_update_applies_ui_bundle_via_debug_valve() {
    let _env_guard = OTA_ENV_LOCK.lock().unwrap();
    let dir = temp_test_dir("apply-ui");
    let _env = ota_env(&dir);

    let (bundle_bytes, bundle_hash) = build_ui_bundle(&dir);
    let _allow_unsigned = EnvRestore::set("WPTSALL_ALLOW_UNSIGNED_MANIFEST", "1");
    // Dev-only posture (guide 16 S11): explicit debug valve honors the
    // checksum path. cargo test is a debug build, so the valve is live.
    let _skip = EnvRestore::set("WPTSALL_SKIP_SECURITY", "1");

    let (base, mock) =
        start_update_mock(|base| mock_files(base, &bundle_bytes, &bundle_hash)).await;
    let _releases = EnvRestore::set("WPTSALL_RELEASES_URL", &format!("{base}/releases.json"));

    let handle = mock_app_handle();
    let result = perform_update(handle)
        .await
        .expect("debug-valve UI-only update applies end-to-end");
    assert_eq!(result.update_kind, "ui");
    assert_eq!(result.target_version, "9.9.9");
    let ui_path = result.ui_path.expect("ui path reported");
    let install_root = dir.join("root");
    assert_eq!(
        Path::new(&ui_path),
        install_root.join("ui").join("desktop"),
        "UI bundle applies under the test install root"
    );
    let applied_marker =
        std::fs::read_to_string(Path::new(&ui_path).join("index.html"))
            .expect("applied ui/desktop/index.html exists");
    assert!(applied_marker.contains("9.9.9"));
    assert_eq!(
        std::fs::read_to_string(install_root.join("VERSION-WEBUI")).unwrap(),
        "9.9.9",
        "VERSION-WEBUI must advance to the applied UI version"
    );

    mock.abort();
    let _ = std::fs::remove_dir_all(&dir);
}
