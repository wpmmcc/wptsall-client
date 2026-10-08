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
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_or_returns_default_when_unset() {
        let key = "WPTSALL_TEST_ENV_OR_NONEXISTENT_KEY_12345";
        env::remove_var(key);
        assert_eq!(env_or(key, "fallback"), "fallback");
    }

    #[test]
    fn env_bool_returns_default_when_unset() {
        let key = "WPTSALL_TEST_BOOL_NONEXISTENT_KEY_12345";
        env::remove_var(key);
        assert!(!env_bool(key, false));
        assert!(env_bool(key, true));
    }

    #[test]
    fn env_bool_only_explicit_true_values_enable_flags() {
        for (idx, value) in ["1", "true", "TRUE", "yes", "YES", " true "]
            .iter()
            .enumerate()
        {
            let key = format!("WPTSALL_TEST_BOOL_TRUE_{idx}");
            env::set_var(&key, value);
            assert!(env_bool(&key, false), "{value:?} should parse as true");
        }

        for (idx, value) in ["0", "false", "FALSE", "no", "NO", "", " truex "]
            .iter()
            .enumerate()
        {
            let key = format!("WPTSALL_TEST_BOOL_FALSE_{idx}");
            env::set_var(&key, value);
            assert!(!env_bool(&key, true), "{value:?} should parse as false");
        }
    }

    #[test]
    fn env_u64_returns_default_when_unset() {
        let key = "**************************************";
        env::remove_var(key);
        assert_eq!(env_u64(key, 42), 42);
    }

    /// P7 write/read same-source: vendor credential and signing-key paths
    /// must resolve through this single authority (env override → data-dir
    /// layout → legacy default), and a save→load roundtrip through the same
    /// authority path must return the same document. The original P7 bug was
    /// the read side deriving sibling paths from the bindings file directory
    /// while the write side used these functions, which emptied the OAuth
    /// pool and triggered a 401 retry storm.
    #[test]
    fn vendor_credential_paths_single_authority_roundtrip() {
        let _key = crate::db::owned_mock_bindings_key();

        struct RestoreEnv(&'static str, Option<String>);
        impl Drop for RestoreEnv {
            fn drop(&mut self) {
                match self.1.as_deref() {
                    Some(value) => std::env::set_var(self.0, value),
                    None => std::env::remove_var(self.0),
                }
            }
        }
        const VARS: [&str; 5] = [
            "WPTSALL_DATA_DIR",
            "WPTSALL_VENDOR_OAUTH_FILE",
            "WPTSALL_VENDOR_KEYS_FILE",
            "WPTSALL_PROXY_PROFILES_FILE",
            "WPTSALL_SIGNING_PUBLIC_KEY_FILE",
        ];
        let restores: Vec<RestoreEnv> = VARS
            .iter()
            .map(|k| RestoreEnv(k, std::env::var(k).ok()))
            .collect();
        for key in VARS {
            std::env::remove_var(key);
        }

        // 1. Data-dir relocation: every authority follows WPTSALL_DATA_DIR.
        let base = std::env::temp_dir().join(format!("wptsall-config-p7-{}", std::process::id()));
        let data_dir = base.join("data");
        std::fs::create_dir_all(data_dir.join("config")).unwrap();
        std::env::set_var("WPTSALL_DATA_DIR", data_dir.display().to_string());
        assert_eq!(
            vendor_oauth_file(),
            data_dir
                .join("config/vendor-oauth.json")
                .display()
                .to_string()
        );
        assert_eq!(
            vendor_keys_file(),
            data_dir
                .join("config/vendor-keys.json")
                .display()
                .to_string()
        );
        assert_eq!(
            proxy_profiles_file(),
            data_dir
                .join("config/proxy-profiles.json")
                .display()
                .to_string()
        );
        assert_eq!(
            signing_public_key_file(),
            data_dir
                .join("config/signing-public-key.pem")
                .display()
                .to_string()
        );

        // 2. Explicit env override wins over the data-dir layout.
        let override_path = base.join("elsewhere/vendor-oauth.json");
        std::env::set_var(
            "WPTSALL_VENDOR_OAUTH_FILE",
            override_path.display().to_string(),
        );
        assert_eq!(vendor_oauth_file(), override_path.display().to_string());

        // 3. Write/read same-source roundtrip through the authority path.
        std::env::remove_var("WPTSALL_VENDOR_OAUTH_FILE");
        let mut doc = crate::types::VendorOAuthDoc::default();
        doc.configs.insert(
            "p7-roundtrip".to_string(),
            crate::types::OAuthConfig {
                vendor_id: "azure".to_string(),
                label: "P7 roundtrip".to_string(),
                grant_type: "client_credentials".to_string(),
                auth_url: String::new(),
                token_url: "https://login.example.com/token".to_string(),
                client_id: "roundtrip-client".to_string(),
                client_secret: "roundtrip-secret".to_string(),
                scopes: String::new(),
                extra_params: Default::default(),
                auth_extra_params: Default::default(),
                cached_token: None,
                cached_token_expires_at: 0,
                refresh_token: None,
                max_concurrent: 5,
                weight: 1,
                max_input_chars: 0,
                max_file_size_mb: 0.0,
                token_field: "access_token".to_string(),
            },
        );
        let target = vendor_oauth_file();
        crate::bindings::save_vendor_oauth(&target, &doc).unwrap();
        let loaded = crate::bindings::load_vendor_oauth(&target).unwrap();
        assert!(
            loaded.configs.contains_key("p7-roundtrip"),
            "save→load through the same authority path must roundtrip"
        );
        let _ = std::fs::remove_file(&target);

        drop(restores);
    }

    /// Path resolution shares process-global env vars with other tests, so
    /// hold the shared test-env lock for the whole test and restore values.
    #[test]
    fn resolve_data_path_priority_env_then_data_dir_then_legacy() {
        let lock = crate::db::test_env_lock();
        let _lock_guard = lock.lock().unwrap_or_else(|e| e.into_inner());

        const DATA_DIR_KEY: &str = "WPTSALL_DATA_DIR";
        const DB_KEY: &str = "WPTSALL_DB_PATH";

        struct RestoreEnv(&'static str, Option<String>);
        impl Drop for RestoreEnv {
            fn drop(&mut self) {
                match self.1.as_deref() {
                    Some(value) => std::env::set_var(self.0, value),
                    None => std::env::remove_var(self.0),
                }
            }
        }
        let _data_restore = RestoreEnv(DATA_DIR_KEY, std::env::var(DATA_DIR_KEY).ok());
        let _db_restore = RestoreEnv(DB_KEY, std::env::var(DB_KEY).ok());

        // 1. Legacy: nothing set → CWD-relative default (dev behavior unchanged).
        std::env::remove_var(DATA_DIR_KEY);
        std::env::remove_var(DB_KEY);
        assert_eq!(db_path(), "./runtime/wptsall.db");

        // 2. Data dir set, no explicit override → subpath under data dir
        //    (service/desktop installs; BUG-RT-01 remediation).
        let tmp = std::env::temp_dir().join(format!("wptsall-test-datadir-{}", std::process::id()));
        std::env::set_var(DATA_DIR_KEY, tmp.to_string_lossy().to_string());
        assert_eq!(
            db_path(),
            tmp.join("runtime/wptsall.db").to_string_lossy().to_string()
        );
        assert_eq!(
            log_file_path(),
            tmp.join("logs/wptsall-client.log")
                .to_string_lossy()
                .to_string()
        );

        // 3. Explicit per-key env wins over data dir (tests, e2e slots).
        std::env::set_var(DB_KEY, "/tmp/explicit-wptsall.db");
        assert_eq!(db_path(), "/tmp/explicit-wptsall.db");

        // 4. Blank values are treated as unset, not as an override.
        std::env::set_var(DB_KEY, "   ");
        assert_eq!(
            db_path(),
            tmp.join("runtime/wptsall.db").to_string_lossy().to_string()
        );
    }

    #[test]
    fn env_u32_returns_default_when_unset() {
        let key = "WPTSALL_TEST_U32_NONEXISTENT_KEY_12345";
        env::remove_var(key);
        assert_eq!(env_u32(key, 10), 10);
    }

    #[test]
    fn env_usize_returns_default_when_unset() {
        let key = "WPTSALL_TEST_USIZE_NONEXISTENT_KEY_12345";
        env::remove_var(key);
        assert_eq!(env_usize(key, 100), 100);
    }

    /// The gate reads a fixed process-global variable that other tests also
    /// mutate through `TestEnvVarGuard`, so hold the shared test-env lock
    /// for the whole test and restore the original value on the way out.
    #[test]
    fn server_control_plane_gate_resolves_modes() {
        let lock = crate::db::test_env_lock();
        let _lock_guard = lock.lock().unwrap_or_else(|e| e.into_inner());

        const MODE_KEY: &str = "WPTSALL_USE_SERVER_CONTROL_PLANE";
        const BASE_KEY: &str = "WPTSALL_SERVER_BASE";

        struct RestoreEnv(&'static str, Option<String>);
        impl Drop for RestoreEnv {
            fn drop(&mut self) {
                match self.1.as_deref() {
                    Some(value) => std::env::set_var(self.0, value),
                    None => std::env::remove_var(self.0),
                }
            }
        }
        let _mode_restore = RestoreEnv(MODE_KEY, std::env::var(MODE_KEY).ok());
        let _base_restore = RestoreEnv(BASE_KEY, std::env::var(BASE_KEY).ok());

        // Unset resolves to local mode.
        env::remove_var(MODE_KEY);
        assert!(!server_control_plane_enabled());
        assert_eq!(runtime_mode(), "local");

        // `0`, `false`, and malformed values stay local.
        for value in ["0", "false", "FALSE", "no", "", " yes-no ", "2"] {
            env::set_var(MODE_KEY, value);
            assert!(!server_control_plane_enabled(), "{value:?} must stay local");
        }
        assert_eq!(runtime_mode(), "local");

        // Only documented true values opt in.
        for value in ["1", "true", "TRUE", "YES", " yes "] {
            env::set_var(MODE_KEY, value);
            assert!(server_control_plane_enabled(), "{value:?} must opt in");
            assert_eq!(runtime_mode(), "legacy_server_control_plane");
        }

        // A configured server base alone never enables the legacy lane.
        env::remove_var(MODE_KEY);
        env::set_var(BASE_KEY, "http://127.0.0.1:8787");
        assert!(!server_control_plane_enabled());
        assert_eq!(runtime_mode(), "local");
    }
}