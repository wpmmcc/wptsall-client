use std::env;
use std::path::PathBuf;

pub(crate) const DEFAULT_LOG_FILE: &str = "./logs/wptsall-client.log";
pub(crate) const DEFAULT_COMPONENT_BINDINGS_FILE: &str = "./config/component-bindings.json";
pub(crate) const DEFAULT_DOMAIN_TOKEN_BINDINGS_FILE: &str = "./config/domain-token-bindings.json";
pub(crate) const DEFAULT_TASK_TYPE_COMPONENT_BINDINGS_FILE: &str =
    "./config/task-type-component-bindings.json";
pub(crate) const DEFAULT_VENDOR_KEYS_FILE: &str = "./config/vendor-keys.json";
pub(crate) const DEFAULT_VENDOR_OAUTH_FILE: &str = "./config/vendor-oauth.json";
pub(crate) const DEFAULT_PROXY_PROFILES_FILE: &str = "./config/proxy-profiles.json";
pub(crate) const DEFAULT_SIGNING_PUBLIC_KEY_FILE: &str = "./config/signing-public-key.pem";
pub(crate) const DEFAULT_COMPONENTS_LOCAL_FILE: &str = "./config/components.json";
pub(crate) const DEFAULT_PROVIDER_CATALOG_FILE: &str = "./config/provider-catalog.json";
pub(crate) const DEFAULT_SESSION_TOKEN_FILE: &str = "./runtime/session-token.enc";
pub(crate) const DEFAULT_RULE_COMPONENT_BINDINGS_FILE: &str =
    "./config/rule-component-bindings.json";
pub(crate) const DEFAULT_SYNC_PAIRS_FILE: &str = "./config/sync-pairs.json";
pub(crate) const DEFAULT_SYNC_PEER_CREDENTIALS_FILE: &str = "./config/sync-peer-credentials.json";
pub(crate) const DEFAULT_SYNC_STATE_FILE: &str = "./config/sync-state.json";
pub(crate) const DEFAULT_SYNC_REVIEW_FILE: &str = "./config/sync-review.json";
pub(crate) const DEFAULT_DATA_DIR: &str = "./data";
pub(crate) const MAX_WEB_UI_WORKER_RUN_RECORDS: usize = 50;
pub(crate) const COMPONENT_CRYPTO_ALGO_AES: &str =
    client_runtime_core::component_crypto::COMPONENT_CRYPTO_ALGO_AES;
pub(crate) const COMPONENT_CRYPTO_ALGO_XOR_LEGACY: &str =
    client_runtime_core::component_crypto::COMPONENT_CRYPTO_ALGO_XOR_LEGACY;
pub(crate) const COMPONENT_KDF_VERSION_HKDF: &str =
    client_runtime_core::component_crypto::COMPONENT_KDF_VERSION_HKDF;
pub(crate) const REQUEST_ID_HEADER: &str = "X-Request-Id";
/// Run-scoped outbound trace header (GAP-06 收尾). Attached to WP-bound and
/// vendor-bound requests while a discovery/sync run is active, so client,
/// WP plugin and mock/ provider logs can be correlated on one run id.
/// Unrelated to the per-request `X-Request-Id`: that one identifies a single
/// HTTP exchange; this one identifies the whole run that issued it.
pub(crate) const TRACE_ID_HEADER: &str = "X-WPTSALL-Trace-Id";
pub(crate) const BINDINGS_CRYPTO_ALGO: &str = "aes-256-gcm-v1";
pub(crate) const TASK_CALLBACK_SCHEMA_VERSION: u64 = 2;
pub(crate) const DEFAULT_MAX_INPUT_CHARS: u64 = 0; // 0 = no limit
pub(crate) const DEFAULT_SPLIT_STRATEGY: &str = "none";

/// Product identifier sent during OAuth and heartbeat.
/// Aligns with the convention used by client-cloud-api-hub and
/// client-github-deployer. The web server uses this to tag the session
/// (`wptsall_server.routes::oauth::TokenExchangeRequest.product_id`)
/// so downstream authorization decisions can be product-aware.
pub(crate) const PRODUCT_ID: &str = "wptsall-plugin";
/// OAuth client_id (matches what the web server expects in the
/// /oauth/authorize page and /api/v1/oauth/token exchange).
pub(crate) const CLIENT_ID: &str = "wptsall-client";

/// Env var convention (intentionally per-client, not a shared `WPTSALL_SERVER_BASE`):
///   - `client-wpplugin` reads `WPTSALL_SERVER_BASE` (or the legacy alias
///     `WPTSALL_SERVER_URL`).
///   - `client-cloud-api-hub` reads `CLOUD_API_HUB_SERVER_BASE`.
///   - `client-github-deployer` reads `GITHUB_DEPLOYER_SERVER_BASE`.
///   `tests/infra/test-host/install-runtime.sh` and `config-drift-audit.sh` reference
///   these names; renaming is a cross-cutting change. The asymmetry exists so that
///   each client can point at a different upstream for staged/prod cutover.
///
/// There is deliberately no built-in or official-site fallback. The normal client
/// is local-first; a control-plane URL is only available when explicitly
/// configured for the legacy/test lane.
#[allow(dead_code)]
pub(crate) const SERVER_BASE_ENV_VAR: &str = "WPTSALL_SERVER_BASE";
#[allow(dead_code)]
pub(crate) const PRODUCT_ID_FOR_OAUTH: &str = "wptsall-plugin";

pub(crate) fn configured_server_base() -> String {
    for key in ["WPTSALL_SERVER_BASE", "WPTSALL_SERVER_URL"] {
        if let Ok(value) = env::var(key) {
            let value = value.trim();
            if !value.is_empty() {
                return value.to_string();
            }
        }
    }
    String::new()
}

/// Single authority for the legacy website/control-plane opt-in gate.
///
/// Every website/control-plane decision (startup session restore, worker
/// mode selection, route dispatch, server fallback helpers, status
/// projection) must call this function instead of re-parsing
/// `WPTSALL_USE_SERVER_CONTROL_PLANE`. Only the documented truthy values
/// opt in; unset, `0`, `false`, or malformed values resolve to local-first
/// mode. Configuring `WPTSALL_SERVER_BASE`/`WPTSALL_SERVER_URL` alone never
/// enables the legacy lane.
pub(crate) fn server_control_plane_enabled() -> bool {
    env_bool("WPTSALL_USE_SERVER_CONTROL_PLANE", false)
}

/// Runtime mode identifier for `/api/status`. Safe to expose: it carries no
/// server URL or session material.
pub(crate) fn runtime_mode() -> &'static str {
    if server_control_plane_enabled() {
        "legacy_server_control_plane"
    } else {
        "local"
    }
}

/// opus5 A-03 (AF-03): boot-level WP device identity resolution. The explicit
/// `WPTSALL_WP_DEVICE_ID` override (test lane / deployments that intentionally
/// use a distinct WP device identity) wins over the generic
/// `WPTSALL_DEVICE_ID` seed; when neither is set the caller falls back to the
/// stable DB identity. The HTTP layer threads this resolved value from
/// WorkerConfig/AppState and never re-reads env at request time, so every
/// binding's requests carry the device id its token was paired with on the
/// WP side (`verify_client_token($token, $device, true)`).
pub(crate) fn wp_device_id_override() -> Option<String> {
    for key in ["WPTSALL_WP_DEVICE_ID", "WPTSALL_DEVICE_ID"] {
        if let Ok(value) = std::env::var(key) {
            let trimmed = value.trim().to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }
    None
}

pub(crate) fn env_or(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

// ---------------------------------------------------------------------------
// Persistent-state path resolution (BUG-RT-01 remediation, v2.1.3)
// ---------------------------------------------------------------------------
// The DEFAULT_* paths above are CWD-relative ("./runtime", "./logs",
// "./config"). That is correct for source-tree/dev runs but broken for GUI
// launches (macOS Finder/Launchpad starts apps with CWD "/", a read-only
// volume; Windows shortcuts without a start-in folder may start in
// System32) and scattered for service installs (systemd user units default
// to CWD $HOME, so the WebUI service littered ~/runtime, ~/logs, ~/config).
//
// Fix: service installers and the Desktop shell set WPTSALL_DATA_DIR; every
// persistent default below then resolves under it, keeping the legacy
// sub-layout (runtime/, logs/, config/) intact. Priority per key:
//   1. explicit per-key env override (tests, e2e slots, power users)
//   2. WPTSALL_DATA_DIR subpath (service / desktop installs)
//   3. legacy CWD-relative default (dev terminal runs, unchanged)

/// Base directory for persistent state, when configured.
pub(crate) fn data_dir() -> Option<PathBuf> {
    match env::var("WPTSALL_DATA_DIR") {
        Ok(value) if !value.trim().is_empty() => Some(PathBuf::from(value.trim())),
        _ => None,
    }
}

/// Resolve one persistent path with the documented priority order.
pub(crate) fn resolve_data_path(env_key: &str, data_subpath: &str, legacy_default: &str) -> String {
    if let Ok(value) = env::var(env_key) {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    if let Some(dir) = data_dir() {
        return dir.join(data_subpath).to_string_lossy().into_owned();
    }
    legacy_default.to_string()
}

pub(crate) fn db_path() -> String {
    resolve_data_path(
        "WPTSALL_DB_PATH",
        "runtime/wptsall.db",
        "./runtime/wptsall.db",
    )
}

pub(crate) fn log_file_path() -> String {
    resolve_data_path(
        "WPTSALL_LOG_FILE",
        "logs/wptsall-client.log",
        DEFAULT_LOG_FILE,
    )
}

pub(crate) fn component_bindings_file() -> String {
    resolve_data_path(
        "WPTSALL_COMPONENT_BINDINGS_FILE",
        "config/component-bindings.json",
        DEFAULT_COMPONENT_BINDINGS_FILE,
    )
}

pub(crate) fn domain_token_bindings_file() -> String {
    resolve_data_path(
        "WPTSALL_DOMAIN_TOKEN_BINDINGS_FILE",
        "config/domain-token-bindings.json",
        DEFAULT_DOMAIN_TOKEN_BINDINGS_FILE,
    )
}

pub(crate) fn task_type_component_bindings_file() -> String {
    resolve_data_path(
        "WPTSALL_TASK_TYPE_COMPONENT_BINDINGS_FILE",
        "config/task-type-component-bindings.json",
        DEFAULT_TASK_TYPE_COMPONENT_BINDINGS_FILE,
    )
}

pub(crate) fn rule_component_bindings_file() -> String {
    resolve_data_path(
        "WPTSALL_RULE_COMPONENT_BINDINGS_FILE",
        "config/rule-component-bindings.json",
        DEFAULT_RULE_COMPONENT_BINDINGS_FILE,
    )
}

pub(crate) fn sync_pairs_file() -> String {
    resolve_data_path(
        "WPTSALL_SYNC_PAIRS_FILE",
        "config/sync-pairs.json",
        DEFAULT_SYNC_PAIRS_FILE,
    )
}

/// Per-domain WPMMCC peer credentials (HMAC shared secrets derived at
/// pairing time). Secrets at rest — stored through the bindings crypto
/// envelope (WPTC/AES-256-GCM when a bindings secret is configured).
pub(crate) fn sync_peer_credentials_file() -> String {
    resolve_data_path(
        "WPTSALL_SYNC_PEER_CREDENTIALS_FILE",
        "config/sync-peer-credentials.json",
        DEFAULT_SYNC_PEER_CREDENTIALS_FILE,
    )
}

/// Per-pair incremental sync state (known canonical UUIDs, fingerprints,
/// target-side mappings, digest keyset cursor). Non-secret bookkeeping.
pub(crate) fn sync_state_file() -> String {
    resolve_data_path(
        "WPTSALL_SYNC_STATE_FILE",
        "config/sync-state.json",
        DEFAULT_SYNC_STATE_FILE,
    )
}

/// Parked sync packets awaiting human approve → push.
pub(crate) fn sync_review_file() -> String {
    resolve_data_path(
        "WPTSALL_SYNC_REVIEW_FILE",
        "config/sync-review.json",
        DEFAULT_SYNC_REVIEW_FILE,
    )
}

pub(crate) fn vendor_keys_file() -> String {
    resolve_data_path(
        "WPTSALL_VENDOR_KEYS_FILE",
        "config/vendor-keys.json",
        DEFAULT_VENDOR_KEYS_FILE,
    )
}

pub(crate) fn vendor_oauth_file() -> String {
    resolve_data_path(
        "WPTSALL_VENDOR_OAUTH_FILE",
        "config/vendor-oauth.json",
        DEFAULT_VENDOR_OAUTH_FILE,
    )
}

pub(crate) fn proxy_profiles_file() -> String {
    resolve_data_path(
        "WPTSALL_PROXY_PROFILES_FILE",
        "config/proxy-profiles.json",
        DEFAULT_PROXY_PROFILES_FILE,
    )
}

/// P7 unification: the component signing public key resolves through the
/// same single authority as every other persistent config path. Consumers
/// (component_rt loader) must call this instead of deriving sibling paths
/// from the bindings file directory.
pub(crate) fn signing_public_key_file() -> String {
    resolve_data_path(
        "WPTSALL_SIGNING_PUBLIC_KEY_FILE",
        "config/signing-public-key.pem",
        DEFAULT_SIGNING_PUBLIC_KEY_FILE,
    )
}

pub(crate) fn components_local_file() -> String {
    resolve_data_path(
        "WPTSALL_COMPONENTS_LOCAL_FILE",
        "config/components.json",
        DEFAULT_COMPONENTS_LOCAL_FILE,
    )
}

pub(crate) fn provider_catalog_file() -> String {
    resolve_data_path(
        "WPTSALL_PROVIDER_CATALOG_FILE",
        "config/provider-catalog.json",
        DEFAULT_PROVIDER_CATALOG_FILE,
    )
}

/// Session-token compatibility file. E2E slots override this path so OAuth
/// state cannot leak through the shared client source working directory.
pub(crate) fn session_token_file() -> String {
    resolve_data_path(
        "WPTSALL_SESSION_TOKEN_FILE",
        "runtime/session-token.enc",
        DEFAULT_SESSION_TOKEN_FILE,
    )
}

pub(crate) fn env_u64(key: &str, default: u64) -> u64 {
    env::var(key)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(default)
}

pub(crate) fn env_u32(key: &str, default: u32) -> u32 {
    env::var(key)
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(default)
}

pub(crate) fn env_usize(key: &str, default: usize) -> usize {
    env::var(key)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(default)
}

pub(crate) fn env_bool(key: &str, default: bool) -> bool {
    client_runtime_core::env_helpers::env_bool(key, default)
}

pub(crate) fn parse_csv_env(key: &str) -> Vec<String> {
    env::var(key)
        .ok()
        .map(|v| {
            v.split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect::<Vec<String>>()
        })
        .unwrap_or_default()
}
