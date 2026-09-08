#![allow(dead_code)]

use anyhow::{anyhow, Context};
use reqwest::Client;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard as StdMutexGuard, OnceLock};
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

use super::{routes, AccessControl};
use crate::db;
use crate::types::*;

fn test_env_lock() -> &'static StdMutex<()> {
    static LOCK: OnceLock<StdMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| StdMutex::new(()))
}

#[derive(Debug)]
struct EnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: impl Into<String>) -> Self {
        let previous = std::env::var(key).ok();
        std::env::set_var(key, value.into());
        Self { key, previous }
    }

    fn set_default(key: &'static str, value: impl Into<String>) -> Self {
        let previous = std::env::var(key).ok();
        if previous.is_none() {
            std::env::set_var(key, value.into());
        }
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.as_deref() {
            std::env::set_var(self.key, previous);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

#[derive(Debug, Clone)]
pub struct WebUiJsonResponse {
    pub status_line: String,
    pub body: Value,
    pub raw_body: String,
}

pub struct WebUiTestHarness {
    _env_lock: StdMutexGuard<'static, ()>,
    _env_guards: Vec<EnvVarGuard>,
    root_dir: PathBuf,
    pub db_path: PathBuf,
    pub data_dir: PathBuf,
    pub log_file: PathBuf,
    state: Arc<Mutex<WebUiState>>,
    runtime_control: WebUiRuntimeControl,
    access_control: AccessControl,
}

impl WebUiTestHarness {
    pub async fn new(server_base: &str, session_token: Option<&str>) -> anyhow::Result<Self> {
        let env_lock = test_env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let unique = format!(
            "wptsall-client-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        );
        let root_dir = std::env::temp_dir().join(unique);
        let data_dir = root_dir.join("data");
        let config_dir = root_dir.join("config");
        let runtime_dir = root_dir.join("runtime");
        std::fs::create_dir_all(&data_dir)?;
        std::fs::create_dir_all(&config_dir)?;
        std::fs::create_dir_all(&runtime_dir)?;

        let db_path = runtime_dir.join("wptsall.db");
        let log_file = runtime_dir.join("webui.log");
        let component_bindings_path = config_dir.join("component-bindings.json");
        let domain_token_bindings_path = config_dir.join("domain-token-bindings.json");
        let task_type_bindings_path = config_dir.join("task-type-component-bindings.json");
        let rule_bindings_path = config_dir.join("rule-component-bindings.json");
        let components_local_path = config_dir.join("components.json");
        let vendor_keys_path = config_dir.join("vendor-keys.json");
        let vendor_oauth_path = config_dir.join("vendor-oauth.json");
        let proxy_profiles_path = config_dir.join("proxy-profiles.json");

        let provider_allowlist = url::Url::parse(server_base)
            .ok()
            .and_then(|url| url.host_str().map(str::to_string))
            .unwrap_or_else(|| "127.0.0.1".to_string());
        let env_guards = vec![
            EnvVarGuard::set("WPTSALL_WEB_UI", "true"),
            // Integration backends bind to an ephemeral loopback port. The
            // production provider guard must remain strict; this test-only
            // harness explicitly allowlists its controlled mock host.
            EnvVarGuard::set("WPTSALL_PROVIDER_ALLOWLIST", provider_allowlist),
            EnvVarGuard::set("WPTSALL_DB_PATH", db_path.display().to_string()),
            EnvVarGuard::set(
                "WPTSALL_COMPONENTS_LOCAL_FILE",
                components_local_path.display().to_string(),
            ),
            EnvVarGuard::set(
                "WPTSALL_COMPONENT_BINDINGS_FILE",
                component_bindings_path.display().to_string(),
            ),
            EnvVarGuard::set(
                "WPTSALL_VENDOR_KEYS_FILE",
                vendor_keys_path.display().to_string(),
            ),
            EnvVarGuard::set(
                "WPTSALL_VENDOR_OAUTH_FILE",
                vendor_oauth_path.display().to_string(),
            ),
            EnvVarGuard::set(
                "WPTSALL_PROXY_PROFILES_FILE",
                proxy_profiles_path.display().to_string(),
            ),
            EnvVarGuard::set("WPTSALL_DATA_DIR", data_dir.display().to_string()),
            EnvVarGuard::set("WPTSALL_SKIP_SIGNATURE_CHECK", "true"),
            EnvVarGuard::set_default("WPTSALL_RUN_ONCE_MAX_ITERATIONS", "1"),
        ];

        let conn = db::open_db(db_path.to_string_lossy().as_ref())?;
        conn.execute(
            "INSERT OR REPLACE INTO system_config (key, value) VALUES ('json_migration_done', '1')",
            [],
        )?;
        let http_client = Client::builder().no_proxy().build()?;
        let state = Arc::new(Mutex::new(WebUiState {
            server_base: server_base.to_string(),
            device_id: "device-test".to_string(),
            session_token: session_token.map(|value| value.to_string()),
            oauth_code_verifier: None,
            oauth_state: None,
            domains: Vec::new(),
            components: Vec::new(),
            component_bindings_path: component_bindings_path.display().to_string(),
            component_bindings: ComponentBindingsDoc::default(),
            domain_token_bindings_path: domain_token_bindings_path.display().to_string(),
            domain_token_bindings: DomainTokenBindingsDoc::default(),
            task_type_component_bindings_path: task_type_bindings_path.display().to_string(),
            task_type_component_bindings: TaskTypeComponentBindingsDoc::default(),
            rule_component_bindings_path: rule_bindings_path.display().to_string(),
            rule_component_bindings: RuleComponentBindingsDoc::default(),
            worker_loop_running: false,
            worker_status: "idle".to_string(),
            worker_loop_poll_seconds: 20,
            worker_last_summary: json!({}),
            worker_recent_runs: Vec::new(),
            local_components_backfilled: 0,
            local_components_backfill_error: String::new(),
            last_error: String::new(),
            last_event: "test.ready".to_string(),
            updated_at: 0,
            log_enabled: false,
            log_min_level: "info".to_string(),
            vendor_oauth_pending: HashMap::new(),
            db: Arc::new(Mutex::new(conn)),
            update_in_progress: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            http_client,
        }));

        Ok(Self {
            _env_lock: env_lock,
            _env_guards: env_guards,
            root_dir,
            db_path,
            data_dir,
            log_file,
            state,
            runtime_control: WebUiRuntimeControl::new(),
            access_control: AccessControl::new(false, &[]),
        })
    }

    pub async fn post_json(&self, path: &str, body: Value) -> anyhow::Result<WebUiJsonResponse> {
        self.send_json_request("POST", path, Some(body)).await
    }

    pub async fn put_json(&self, path: &str, body: Value) -> anyhow::Result<WebUiJsonResponse> {
        self.send_json_request("PUT", path, Some(body)).await
    }

    pub async fn get_json(&self, path: &str) -> anyhow::Result<WebUiJsonResponse> {
        self.send_json_request("GET", path, None).await
    }

    pub async fn create_local_component(&self, body: Value) -> anyhow::Result<WebUiJsonResponse> {
        self.post_json("/api/components/local", body).await
    }

    pub async fn upsert_task_type_binding(
        &self,
        task_type: &str,
        component_id: &str,
    ) -> anyhow::Result<WebUiJsonResponse> {
        self.post_json(
            "/api/task-type-components/upsert",
            json!({
                "task_type": task_type,
                "component_id": component_id,
            }),
        )
        .await
    }

    pub async fn upsert_rule_component_binding(
        &self,
        scope: &str,
        scope_key: Option<&str>,
        slot_key: &str,
        component_id: &str,
    ) -> anyhow::Result<WebUiJsonResponse> {
        self.post_json(
            "/api/rule-component-bindings/upsert",
            json!({
                "scope": scope,
                "scope_key": scope_key,
                "slot_key": slot_key,
                "component_id": component_id,
            }),
        )
        .await
    }

    pub async fn upsert_domain_binding(
        &self,
        api_base_url: &str,
        wp_client_token: &str,
        route_secret: &str,
    ) -> anyhow::Result<WebUiJsonResponse> {
        self.post_json(
            "/api/domain-tokens/upsert",
            json!({
                "api_base_url": api_base_url,
                "wp_client_token": wp_client_token,
                "route_secret": route_secret,
            }),
        )
        .await
    }

    pub async fn run_worker_once(&self) -> anyhow::Result<WebUiJsonResponse> {
        self.post_json("/api/worker/run-once", json!({})).await
    }

    pub async fn list_jobs(&self) -> anyhow::Result<WebUiJsonResponse> {
        self.get_json("/api/jobs").await
    }

    pub async fn list_job_items(&self, job_id: i64) -> anyhow::Result<WebUiJsonResponse> {
        self.get_json(&format!("/api/jobs/{}/items", job_id)).await
    }

    pub async fn status(&self) -> anyhow::Result<WebUiJsonResponse> {
        self.get_json("/api/status").await
    }

    async fn send_json_request(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> anyhow::Result<WebUiJsonResponse> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let state = Arc::clone(&self.state);
        let runtime_control = self.runtime_control.clone();
        let access_control = self.access_control.clone();
        let log_file = self.log_file.display().to_string();

        let handler = tokio::spawn(async move {
            let (socket, _) = listener.accept().await?;
            routes::handle_web_ui_connection(
                socket,
                state,
                runtime_control,
                &log_file,
                crate::logging::unix_ts(),
                Instant::now(),
                access_control,
            )
            .await
        });

        let payload = body.map(|value| serde_json::to_vec(&value)).transpose()?;
        let mut request = format!(
            "{} {} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n",
            method, path
        );
        if method.eq_ignore_ascii_case("POST") {
            request.push_str("Origin: http://127.0.0.1:8977\r\n");
            request.push_str("Content-Type: application/json\r\n");
        }
        request.push_str(&format!(
            "Content-Length: {}\r\n\r\n",
            payload.as_ref().map(|value| value.len()).unwrap_or(0)
        ));

        let mut client = tokio::net::TcpStream::connect(addr).await?;
        client.write_all(request.as_bytes()).await?;
        if let Some(payload) = payload.as_ref() {
            client.write_all(payload).await?;
        }
        client.shutdown().await?;

        let mut response = Vec::new();
        client.read_to_end(&mut response).await?;
        handler
            .await
            .context("web ui handler join failed")?
            .context("web ui handler failed")?;

        let response_text = String::from_utf8(response).context("invalid utf-8 response")?;
        parse_json_response(&response_text)
    }
}

impl Drop for WebUiTestHarness {
    fn drop(&mut self) {
        if std::env::var("WPTSALL_KEEP_TEST_RUNTIME").ok().as_deref() == Some("1") {
            return;
        }
        let _ = std::fs::remove_dir_all(&self.root_dir);
    }
}

fn parse_json_response(response: &str) -> anyhow::Result<WebUiJsonResponse> {
    let mut lines = response.split("\r\n");
    let status_line = lines
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("missing status line"))?
        .to_string();
    let raw_body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .unwrap_or("")
        .to_string();
    let body = if raw_body.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&raw_body)
            .with_context(|| format!("invalid json body in response: {}", status_line))?
    };
    Ok(WebUiJsonResponse {
        status_line,
        body,
        raw_body,
    })
}

pub fn read_json_file(path: &Path) -> anyhow::Result<Value> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read json file {}", path.display()))?;
    serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse json file {}", path.display()))
}

pub fn sign_wp_plaintext_response(token: &str, plaintext_bytes: &[u8]) -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    type HmacSha256 = Hmac<Sha256>;

    let signing_key = crate::crypto::derive_signing_key(token);
    let mut mac =
        <HmacSha256 as Mac>::new_from_slice(&signing_key).expect("HMAC can take key of any size");
    mac.update(plaintext_bytes);
    URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
}

pub fn decode_wp_transport_request(token: &str, raw_body: &[u8]) -> anyhow::Result<Value> {
    if raw_body.is_empty() {
        return Ok(json!({}));
    }

    let envelope: Value =
        serde_json::from_slice(raw_body).context("parse wp transport request envelope failed")?;
    let encrypted_payload = envelope
        .get("encrypted_payload")
        .and_then(|value| value.as_str())
        .ok_or_else(|| anyhow!("missing encrypted_payload in wp transport request"))?;
    let nonce = envelope
        .get("nonce")
        .and_then(|value| value.as_str())
        .ok_or_else(|| anyhow!("missing nonce in wp transport request"))?;
    let decrypted = crate::crypto::transport_decrypt(encrypted_payload, nonce, token)
        .context("decrypt wp transport request failed")?;
    serde_json::from_slice(&decrypted).context("parse decrypted wp transport request failed")
}
