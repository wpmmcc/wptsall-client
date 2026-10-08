//! Owned Linux subprocess proof without the runtime-wide lease.
use super::*;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const WORKER: &str = "component_rt::oauth::tests::process::owned_oauth_process_worker";

struct OwnedChild(Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn spawn(root: &Path, label: &str, hold: bool) -> OwnedChild {
    let log = std::fs::File::create(root.join(format!("{label}.log"))).unwrap();
    OwnedChild(
        Command::new("/proc/self/exe")
            .args([
                "--exact",
                WORKER,
                "--ignored",
                "--test-threads=1",
                "--nocapture",
            ])
            .current_dir(root)
            .env("WPTSALL_OWNED_OAUTH_PROCESS", "owned-oauth-process-v1")
            .env("WPTSALL_OWNED_OAUTH_LABEL", label)
            .env("WPTSALL_OWNED_OAUTH_HOLD", if hold { "1" } else { "0" })
            .env(
                "WPTSALL_COMPONENT_BINDINGS_SECRET",
                std::env::var("WPTSALL_COMPONENT_BINDINGS_SECRET").unwrap(),
            )
            .env("WPTSALL_DATA_DIR", root)
            .env("WPTSALL_USE_SERVER_CONTROL_PLANE", "0")
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap(),
    )
}

async fn wait(child: &mut OwnedChild) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                assert!(status.success(), "owned OAuth subprocess failed");
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "owned subprocess harness only; mock token endpoint and isolated SQLite"]
async fn owned_oauth_process_worker() {
    assert_eq!(
        std::env::var("WPTSALL_OWNED_OAUTH_PROCESS").unwrap(),
        "owned-oauth-process-v1"
    );
    let root = std::path::PathBuf::from(std::env::var("WPTSALL_DATA_DIR").unwrap());
    let label = std::env::var("WPTSALL_OWNED_OAUTH_LABEL").unwrap();
    assert!(label
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || c == b'-'));
    let configs: HashMap<String, OAuthConfig> = serde_json::from_str(
        &crate::bindings::load_encrypted_or_plain(&root.join("owned-frozen-oauth.json")).unwrap(),
    )
    .unwrap();
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(root.join("owned.sqlite").to_str().unwrap()).unwrap(),
    ));
    let manager = OAuthTokenManager::from_snapshot(configs, OAuthHttpClient::direct().unwrap())
        .with_recovery_db(db);
    let result = match manager.get_token("owned").await {
        Ok(token) => serde_json::json!({"success":true,"token":token}),
        Err(error) => serde_json::json!({"success":false,"error":error.to_string()}),
    };
    crate::bindings::atomic_file::install_new(
        &root.join(format!("{label}.response")),
        &serde_json::to_vec(&result).unwrap(),
    )
    .unwrap();
    if std::env::var("WPTSALL_OWNED_OAUTH_HOLD").unwrap() == "1" {
        std::future::pending::<()>().await;
    }
}

fn seed(root: &Path, config: OAuthConfig) {
    let db = crate::db::open_db(root.join("owned.sqlite").to_str().unwrap()).unwrap();
    let doc = crate::types::VendorOAuthDoc {
        version: 1,
        configs: HashMap::from([("owned".into(), config)]),
    };
    crate::db::vendor::save_vendor_oauth_doc(&db, &doc).unwrap();
    let raw =
        crate::bindings::encrypt_for_save(&serde_json::to_string(&doc.configs).unwrap()).unwrap();
    crate::bindings::atomic_file::install_new(&root.join("owned-frozen-oauth.json"), &raw).unwrap();
}

fn response(root: &Path, label: &str) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(root.join(format!("{label}.response"))).unwrap()).unwrap()
}

#[tokio::test]
async fn remaining_oauth_real_kill_after_issue_retains_unknown_and_refuses_competitor() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap().keep();
    eprintln!("owned OAuth process evidence: {}", root.display());
    let endpoint = RemainingTokenEndpoint::with_ttl(true, 3600).await;
    seed(&root, endpoint.config("authorization_code"));
    let mut original = spawn(&root, "original", false);
    tokio::time::timeout(Duration::from_secs(5), async {
        while endpoint.requests.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let mut competitor = spawn(&root, "competitor", false);
    wait(&mut competitor).await;
    assert!(response(&root, "competitor")["error"]
        .as_str()
        .unwrap()
        .contains("OAUTH_REFRESH_BUSY"));
    original.0.kill().unwrap();
    assert!(
        !original.0.wait().unwrap().success(),
        "the original token issuer must actually be killed"
    );
    let mut restarted = spawn(&root, "restarted", false);
    wait(&mut restarted).await;
    assert!(response(&root, "restarted")["error"]
        .as_str()
        .unwrap()
        .contains("OAUTH_REFRESH_UNKNOWN"));
    assert_eq!(
        endpoint.requests.lock().unwrap().len(),
        1,
        "unknown rotation cannot be issued again"
    );
    crate::bindings::atomic_file::install_new(&root.join("proof.json"),
        br#"{"case":"after-issue","original_killed":true,"competitor":"busy","restart":"unknown","token_requests":1,"runtime_wide_lease":false}"#).unwrap();
}

async fn ready_case(short_lifetime: bool) {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap().keep();
    eprintln!("owned OAuth process evidence: {}", root.display());
    let endpoint =
        RemainingTokenEndpoint::with_ttl(false, if short_lifetime { 5 } else { 3600 }).await;
    seed(&root, endpoint.config("authorization_code"));
    let before = std::fs::read(root.join("owned-frozen-oauth.json")).unwrap();
    let mut original = spawn(&root, "original", true);
    tokio::time::timeout(Duration::from_secs(5), async {
        while !root.join("original.response").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(response(&root, "original")["token"], "owned-token-1");
    original.0.kill().unwrap();
    assert!(!original.0.wait().unwrap().success());
    let mut restarted = spawn(&root, "restarted", false);
    wait(&mut restarted).await;
    let expected = if short_lifetime {
        "owned-token-2"
    } else {
        "owned-token-1"
    };
    assert_eq!(response(&root, "restarted")["token"], expected);
    let requests = endpoint.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), if short_lifetime { 2 } else { 1 });
    if short_lifetime {
        assert_eq!(
            form_field(&requests[1], "refresh_token").as_deref(),
            Some("owned-rotation-1"),
            "fresh process must use the durable rotated credential, never the consumed original"
        );
    }
    assert_eq!(
        std::fs::read(root.join("owned-frozen-oauth.json")).unwrap(),
        before
    );
    crate::bindings::atomic_file::install_new(
        &root.join("proof.json"),
        &serde_json::to_vec(&serde_json::json!({
            "case":if short_lifetime { "ready-rotation" } else { "ready-replay" },
            "original_killed":true,"token_requests":requests.len(),"restart_token":expected,
            "frozen_configuration_unchanged":true,"runtime_wide_lease":false,
        }))
        .unwrap(),
    )
    .unwrap();
}

#[tokio::test]
async fn remaining_oauth_real_kill_after_ready_replays_without_token_issue() {
    ready_case(false).await;
}

#[tokio::test]
async fn remaining_oauth_real_kill_after_ready_resumes_original_rotated_credential() {
    ready_case(true).await;
}
