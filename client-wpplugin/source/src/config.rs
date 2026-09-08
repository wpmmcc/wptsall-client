use std::env;

pub(crate) const DEFAULT_LOG_FILE: &str = "./logs/wptsall-client.log";
pub(crate) const DEFAULT_COMPONENT_BINDINGS_FILE: &str = "./config/component-bindings.json";
pub(crate) const DEFAULT_DOMAIN_TOKEN_BINDINGS_FILE: &str = "./config/domain-token-bindings.json";
pub(crate) const DEFAULT_TASK_TYPE_COMPONENT_BINDINGS_FILE: &str =
    "./config/task-type-component-bindings.json";
pub(crate) const DEFAULT_VENDOR_KEYS_FILE: &str = "./config/vendor-keys.json";
pub(crate) const DEFAULT_VENDOR_OAUTH_FILE: &str = "./config/vendor-oauth.json";
pub(crate) const DEFAULT_PROXY_PROFILES_FILE: &str = "./config/proxy-profiles.json";
pub(crate) const DEFAULT_COMPONENTS_LOCAL_FILE: &str = "./config/components.json";
pub(crate) const DEFAULT_PROVIDER_CATALOG_FILE: &str = "./config/provider-catalog.json";
pub(crate) const DEFAULT_SESSION_TOKEN_FILE: &str = "./runtime/session-token.enc";
pub(crate) const DEFAULT_RULE_COMPONENT_BINDINGS_FILE: &str =
    "./config/rule-component-bindings.json";
pub(crate) const DEFAULT_DATA_DIR: &str = "./data";
pub(crate) const MAX_WEB_UI_WORKER_RUN_RECORDS: usize = 50;
pub(crate) const COMPONENT_CRYPTO_ALGO_AES: &str =
    client_runtime_core::component_crypto::COMPONENT_CRYPTO_ALGO_AES;
pub(crate) const COMPONENT_CRYPTO_ALGO_XOR_LEGACY: &str =
    client_runtime_core::component_crypto::COMPONENT_CRYPTO_ALGO_XOR_LEGACY;
pub(crate) const COMPONENT_KDF_VERSION_HKDF: &str =
    client_runtime_core::component_crypto::COMPONENT_KDF_VERSION_HKDF;
pub(crate) const REQUEST_ID_HEADER: &str = "X-Request-Id";
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

pub(crate) fn env_or(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

/// Session-token compatibility file. E2E slots override this path so OAuth
/// state cannot leak through the shared client source working directory.
pub(crate) fn session_token_file() -> String {
    env_or("WPTSALL_SESSION_TOKEN_FILE", DEFAULT_SESSION_TOKEN_FILE)
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
        let key = "WPTSALL_TEST_U64_NONEXISTENT_KEY_12345";
        env::remove_var(key);
        assert_eq!(env_u64(key, 42), 42);
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
