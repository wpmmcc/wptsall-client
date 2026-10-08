use super::*;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const WORKER: &str =
    "web_ui::routes::tests::integrations::process::owned_vendor_oauth_process_worker";

struct OwnedChild(Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn spawn(root: &Path, label: &str, stage: &str) -> OwnedChild {
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
            .env(
                "WPTSALL_OWNED_VENDOR_OAUTH_PROCESS",
                "owned-vendor-oauth-process-v1",
            )
            .env("WPTSALL_OWNED_VENDOR_OAUTH_LABEL", label)
            .env("WPTSALL_OWNED_VENDOR_OAUTH_STAGE", stage)
            .env(
                "WPTSALL_COMPONENT_BINDINGS_SECRET",
                std::env::var("WPTSALL_COMPONENT_BINDINGS_SECRET").unwrap(),
            )
            .env("WPTSALL_DATA_DIR", root)
            .env("WPTSALL_VENDOR_OAUTH_FILE", root.join("owned-oauth.json"))
            .env("WPTSALL_DB_PATH", root.join("owned.sqlite"))
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
                assert!(status.success(), "owned OAuth callback child failed");
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

fn response(root: &Path, label: &str) -> String {
    std::fs::read_to_string(root.join(format!("{label}.response"))).unwrap()
}

#[tokio::test]
#[ignore = "owned subprocess harness only; isolated SQLite and loopback OAuth endpoint"]
async fn owned_vendor_oauth_process_worker() {
    assert_eq!(
        std::env::var("WPTSALL_OWNED_VENDOR_OAUTH_PROCESS").unwrap(),
        "owned-vendor-oauth-process-v1"
    );
    let root = std::path::PathBuf::from(std::env::var("WPTSALL_DATA_DIR").unwrap());
    let label = std::env::var("WPTSALL_OWNED_VENDOR_OAUTH_LABEL").unwrap();
    assert!(label
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'));
    let oauth_state: String = serde_json::from_str(
        &crate::bindings::load_encrypted_or_plain(&root.join("owned-control.json")).unwrap(),
    )
    .unwrap();
    let state = build_test_web_ui_state("", None);
    state.lock().await.db = Arc::new(Mutex::new(
        crate::db::open_db(root.join("owned.sqlite").to_str().unwrap()).unwrap(),
    ));
    let received = callback_integration_fixture(&state, &oauth_state).await;
    crate::bindings::atomic_file::install_new(
        &root.join(format!("{label}.response")),
        received.as_bytes(),
    )
    .unwrap();
}

async fn process_case(ready: bool) {
    let _env = components_env_lock().lock().unwrap();
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap().keep();
    eprintln!("owned vendor OAuth process evidence: {}", root.display());
    let path = root.join("owned-oauth.json");
    let _path = EnvVarGuard::set("WPTSALL_VENDOR_OAUTH_FILE", path.display().to_string());
    let server = OwnedIntegrationTokenServer::start(!ready).await;
    let state = build_test_web_ui_state("", None);
    state.lock().await.db = Arc::new(Mutex::new(
        crate::db::open_db(root.join("owned.sqlite").to_str().unwrap()).unwrap(),
    ));
    let oauth_state = owned_code_flow_fixture(&state, &server, &path).await;
    crate::bindings::save_encrypted_file(
        &root.join("owned-control.json"),
        &serde_json::to_string(&oauth_state).unwrap(),
    )
    .unwrap();
    let original = std::fs::read(&path).unwrap();
    let stage = if ready { "ready" } else { "intent" };
    let mut first = spawn(&root, "original", stage);
    tokio::time::timeout(Duration::from_secs(10), async {
        while !root.join(format!("original.{stage}")).exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(server.requests.load(std::sync::atomic::Ordering::SeqCst), 1);
    let mut competitor = spawn(&root, "competitor", "");
    wait(&mut competitor).await;
    assert!(response(&root, "competitor").starts_with("HTTP/1.1 409"));
    first.0.kill().unwrap();
    assert!(
        !first.0.wait().unwrap().success(),
        "the original must actually receive SIGKILL"
    );
    let mut restarted = spawn(&root, "restarted", "");
    wait(&mut restarted).await;
    assert!(response(&root, "restarted").starts_with(if ready {
        "HTTP/1.1 200"
    } else {
        "HTTP/1.1 409"
    }));
    assert_eq!(server.requests.load(std::sync::atomic::Ordering::SeqCst), 1);
    if !ready {
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }
    crate::bindings::atomic_file::install_new(
        &root.join("proof.json"),
        &serde_json::to_vec(&json!({
            "case":if ready { "after-ready" } else { "after-accepted-lost-reply" },
            "original_killed":true,"token_requests":1,"runtime_wide_lease":false,
            "restart":if ready { "original-ready-applied" } else { "original-unknown-refused" },
        }))
        .unwrap(),
    )
    .unwrap();
}

#[tokio::test]
async fn integration_durable_authorization_code_real_kill_after_ready_replays_original_without_new_exchange(
) {
    process_case(true).await;
}

#[tokio::test]
async fn integration_durable_authorization_code_real_kill_after_accepted_refuses_without_new_exchange(
) {
    process_case(false).await;
}

const CONFIG_WORKER: &str =
    "web_ui::routes::tests::integrations::process::owned_config_process_worker";

fn spawn_config(root: &Path, label: &str, stage: &str, save: bool) -> OwnedChild {
    let log = std::fs::File::create(root.join(format!("{label}.log"))).unwrap();
    OwnedChild(
        Command::new("/proc/self/exe")
            .args([
                "--exact",
                CONFIG_WORKER,
                "--ignored",
                "--test-threads=1",
                "--nocapture",
            ])
            .current_dir(root)
            .env("WPTSALL_OWNED_CONFIG_PROCESS", "owned-config-process-v1")
            .env("WPTSALL_OWNED_CONFIG_LABEL", label)
            .env("WPTSALL_OWNED_CONFIG_STAGE", stage)
            .env("WPTSALL_OWNED_CONFIG_SAVE", if save { "1" } else { "0" })
            .env(
                "WPTSALL_COMPONENT_BINDINGS_SECRET",
                std::env::var("WPTSALL_COMPONENT_BINDINGS_SECRET").unwrap(),
            )
            .env("WPTSALL_DATA_DIR", root)
            .env("WPTSALL_VENDOR_OAUTH_FILE", root.join("owned-oauth.json"))
            .env("WPTSALL_DB_PATH", root.join("owned.sqlite"))
            .env("WPTSALL_USE_SERVER_CONTROL_PLANE", "0")
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap(),
    )
}

#[tokio::test]
#[ignore = "owned configuration subprocess harness only; no HTTP or runtime-wide lease"]
async fn owned_config_process_worker() {
    use crate::web_ui::routes::integrations::config_store::{load_config, save_config};
    assert_eq!(
        std::env::var("WPTSALL_OWNED_CONFIG_PROCESS").unwrap(),
        "owned-config-process-v1"
    );
    let root = std::path::PathBuf::from(std::env::var("WPTSALL_DATA_DIR").unwrap());
    let label = std::env::var("WPTSALL_OWNED_CONFIG_LABEL").unwrap();
    assert!(label
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'));
    let path = root.join("owned-oauth.json");
    let state = build_test_web_ui_state("", None);
    state.lock().await.db = Arc::new(Mutex::new(
        crate::db::open_db(root.join("owned.sqlite").to_str().unwrap()).unwrap(),
    ));
    let result = if std::env::var("WPTSALL_OWNED_CONFIG_SAVE").unwrap() == "1" {
        let before =
            load_config::<VendorOAuthDoc>(&state, path.to_str().unwrap(), "vendor_oauth_doc")
                .await
                .unwrap();
        let after = VendorOAuthDoc {
            version: 1,
            configs: HashMap::from([(
                "owned".into(),
                make_test_oauth_config("http://127.0.0.1:1/token".into()),
            )]),
        };
        save_config(
            &state,
            path.to_str().unwrap(),
            "vendor_oauth_doc",
            &before,
            &after,
        )
        .await
        .map(|_| true)
    } else {
        load_config::<VendorOAuthDoc>(&state, path.to_str().unwrap(), "vendor_oauth_doc")
            .await
            .map(|doc| doc.configs.contains_key("owned"))
    };
    let result = match result {
        Ok(confirmed) => json!({"success":true,"confirmed":confirmed}),
        Err(_) => json!({"success":false}),
    };
    crate::bindings::atomic_file::install_new(
        &root.join(format!("{label}.response")),
        &serde_json::to_vec(&result).unwrap(),
    )
    .unwrap();
}

async fn config_process_case(stage: &str) {
    let _env = components_env_lock().lock().unwrap();
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap().keep();
    eprintln!("owned configuration process evidence: {}", root.display());
    let path = root.join("owned-oauth.json");
    crate::bindings::save_vendor_oauth(path.to_str().unwrap(), &VendorOAuthDoc::default()).unwrap();
    let original = std::fs::read(&path).unwrap();
    let conn = crate::db::open_db(root.join("owned.sqlite").to_str().unwrap()).unwrap();
    drop(conn);
    let mut first = spawn_config(&root, "original", stage, true);
    tokio::time::timeout(Duration::from_secs(10), async {
        while !root.join(format!("original.{stage}")).exists() {
            assert!(
                first.0.try_wait().unwrap().is_none(),
                "owned configuration child exited before its committed boundary"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    if stage == "prepared" {
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }
    let mut competitor = spawn_config(&root, "competitor", "", false);
    wait(&mut competitor).await;
    assert_eq!(
        serde_json::from_str::<Value>(&response(&root, "competitor")).unwrap()["success"],
        false
    );
    first.0.kill().unwrap();
    assert!(!first.0.wait().unwrap().success());
    let mut restarted = spawn_config(&root, "restarted", "", false);
    wait(&mut restarted).await;
    let result: Value = serde_json::from_str(&response(&root, "restarted")).unwrap();
    assert_eq!(result["success"], true);
    assert_eq!(result["confirmed"], true);
    let conn = crate::db::open_db(root.join("owned.sqlite").to_str().unwrap()).unwrap();
    assert!(crate::db::system::get_system_config_checked(
        &conn,
        "integration-save-v1:vendor_oauth_doc"
    )
    .unwrap()
    .is_none());
    assert!(crate::bindings::load_vendor_oauth(path.to_str().unwrap())
        .unwrap()
        .configs
        .contains_key("owned"));
    crate::bindings::atomic_file::install_new(&root.join("proof.json"),&serde_json::to_vec(&json!({
        "stage":stage,"original_killed":true,"competitor":"refused","restart":"committed-snapshot",
        "runtime_wide_lease":false,"network_requests":0,
    })).unwrap()).unwrap();
}

#[tokio::test]
async fn integration_durable_configuration_real_kill_after_prepared_recovers_committed_snapshot() {
    config_process_case("prepared").await;
}

#[tokio::test]
async fn integration_durable_configuration_real_kill_after_projected_recovers_committed_snapshot() {
    config_process_case("projected").await;
}
