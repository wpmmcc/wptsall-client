#![allow(dead_code)]

use anyhow::{anyhow, Context};
use reqwest::Client;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

use super::{routes, AccessControl};
use crate::db;
use crate::types::*;

#[derive(Debug)]
struct EnvVarGuard {
    /// Delegates to the db test-env guard so env mutation mutually excludes
    /// against every other env-mutating test (review/retranslate etc. use
    /// the same lock). The depth counter there makes same-thread nesting
    /// cheap; cross-thread it blocks until the holder finishes.
    ///
    /// NOTE (deadlock fix): the harness deliberately does NOT take a second
    /// private env mutex. Routes tests that nest a harness inside
    /// `components_env_lock` acquire (components_env_lock → db env lock →
    /// harness locks); harness-only tests used to acquire a private
    /// `test_env_lock` BEFORE the db env lock, inverting the order and
    /// deadlocking the two families when run in parallel. The db env lock
    /// alone already provides the mutual exclusion this harness needs, so
    /// the redundant private lock is gone and a single lock order remains.
    inner: crate::db::TestEnvVarGuard,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: impl Into<String>) -> Self {
        Self {
            inner: crate::db::TestEnvVarGuard::set(key, value),
        }
    }

    fn set_default(key: &'static str, value: impl Into<String>) -> Self {
        // Keep the "only if absent" semantics: when the variable already
        // exists, set it to its own current value (restores identically).
        let existing = std::env::var(key).ok();
        let effective = existing.unwrap_or_else(|| value.into());
        Self::set(key, effective)
    }
}

#[derive(Debug, Clone)]
pub struct WebUiJsonResponse {
    pub status_line: String,
    pub body: Value,
    pub raw_body: String,
}

pub struct WebUiTestHarness {
    _env_guards: Vec<EnvVarGuard>,
    root_dir: PathBuf,
    pub db_path: PathBuf,
    pub data_dir: PathBuf,
    pub log_file: PathBuf,
    pub(crate) state: Arc<Mutex<WebUiState>>,
    runtime_control: WebUiRuntimeControl,
    access_control: AccessControl,
}

impl WebUiTestHarness {
    pub async fn new(server_base: &str, session_token: Option<&str>) -> anyhow::Result<Self> {
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
            EnvVarGuard::set_default(
                "WPTSALL_COMPONENT_BINDINGS_SECRET",
                "owned-private-fixture-key",
            ),
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
            // Clean-release catalog: without a cache the list endpoint would
            // auto-fetch the OFFICIAL repository — tests must stay offline and
            // deterministic, so point the source at a missing local file and
            // allow the file scheme (test-only override gating).
            EnvVarGuard::set(
                "WPTSALL_PROVIDER_CATALOG_SOURCE_URL",
                format!("file://{}/nonexistent-catalog.json", root_dir.display()),
            ),
            EnvVarGuard::set("WPTSALL_PROVIDER_CATALOG_ALLOW_FILE_SOURCE", "true"),
        ];

        // 2026-09-25 lane flake hardening: WebUiTestHarness::new once hit
        // open-time SQLITE_BUSY (DDL + WAL init) under back-to-back
        // saturated lane runs (host_guard_rejects_missing_host_header,
        // lane 2 of 3 — isolated reruns green, third lane fully green).
        // Root-cause class is the one already documented on
        // open_db_with_busy_timeout: saturation can blow past the 5s
        // default during open-time statements. This harness is a
        // lock-heavy caller per that precedent (db/mod.rs's own
        // concurrent-writers probe uses 30s per-thread opens), so take
        // the explicit 30s budget for the whole harness DB lifetime
        // (busy_timeout is per-connection: every later statement on this
        // conn — including the seed INSERT below — waits up to 30s
        // instead of 5s under load). Test-support only; production keeps
        // the 5s default.
        let conn = db::open_db_with_busy_timeout(
            db_path.to_string_lossy().as_ref(),
            std::time::Duration::from_secs(30),
        )?;
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

    pub async fn delete_json(&self, path: &str) -> anyhow::Result<WebUiJsonResponse> {
        self.send_json_request("DELETE", path, None).await
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

    /// Raw-request variant of the JSON plumbing above: no automatic Origin
    /// or Content-Type headers, arbitrary extra headers, non-JSON bodies
    /// allowed. Returns `(status_line, raw_body)` with the body split at
    /// the first `\r\n\r\n`. Goes through the same real dispatcher
    /// (`routes::handle_web_ui_connection`), so CSRF/legacy/route gates all
    /// apply exactly as in production.
    pub async fn send_raw_request(
        &self,
        method: &str,
        path: &str,
        body: Option<&[u8]>,
        extra_headers: &[(&str, &str)],
    ) -> anyhow::Result<(String, String)> {
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

        let mut request = format!(
            "{} {} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n",
            method, path
        );
        for (name, value) in extra_headers {
            request.push_str(&format!("{}: {}\r\n", name, value));
        }
        request.push_str(&format!(
            "Content-Length: {}\r\n\r\n",
            body.map(|payload| payload.len()).unwrap_or(0)
        ));

        let mut client = tokio::net::TcpStream::connect(addr).await?;
        client.write_all(request.as_bytes()).await?;
        if let Some(payload) = body {
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
        let status_line = response_text
            .split("\r\n")
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("missing status line"))?
            .to_string();
        let raw_body = response_text
            .split_once("\r\n\r\n")
            .map(|(_, body)| body)
            .unwrap_or("")
            .to_string();
        Ok((status_line, raw_body))
    }

    /// Host-less variant of `send_raw_request` for the S4 Host-allowlist
    /// gate: no default `Host` header is injected, so the request exercises
    /// the fail-closed path for a missing Host header. Extra headers are
    /// still appended verbatim (the caller controls any Origin header).
    pub async fn send_raw_request_without_host(
        &self,
        method: &str,
        path: &str,
        body: Option<&[u8]>,
        extra_headers: &[(&str, &str)],
    ) -> anyhow::Result<(String, String)> {
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

        let mut request = format!("{} {} HTTP/1.1\r\nConnection: close\r\n", method, path);
        for (name, value) in extra_headers {
            request.push_str(&format!("{}: {}\r\n", name, value));
        }
        request.push_str(&format!(
            "Content-Length: {}\r\n\r\n",
            body.map(|payload| payload.len()).unwrap_or(0)
        ));

        let mut client = tokio::net::TcpStream::connect(addr).await?;
        client.write_all(request.as_bytes()).await?;
        if let Some(payload) = body {
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
        let status_line = response_text
            .split("\r\n")
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("missing status line"))?
            .to_string();
        let raw_body = response_text
            .split_once("\r\n\r\n")
            .map(|(_, body)| body)
            .unwrap_or("")
            .to_string();
        Ok((status_line, raw_body))
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
    let raw = crate::bindings::load_encrypted_or_plain(path)
        .with_context(|| format!("failed to read json file {}", path.display()))?;
    serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse json file {}", path.display()))
}

pub fn media_operation_response_from_headers(headers: &str, attachment_id: i64) -> Value {
    let header = |name: &str| {
        headers
            .lines()
            .find_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.eq_ignore_ascii_case(name).then(|| value.trim())
            })
            .expect("owned media header required")
    };
    json!({"success":true,"attachment_id":attachment_id,
        "operation_id":header("X-WPTSALL-Operation-ID"),
        "content_sha256":header("X-WPTSALL-Content-SHA256"),
        "source_id":header("X-WPTSALL-Source-ID").parse::<i64>().unwrap(),
        "task_id":header("X-WPTSALL-Task-ID").parse::<i64>().unwrap(),
        "relation_id":header("X-WPTSALL-Relation-ID").parse::<i64>().unwrap(),
    })
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
