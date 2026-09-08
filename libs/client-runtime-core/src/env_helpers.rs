//! Environment variable parsing helpers shared by all wptsall clients.
//!
//! Moved from per-client `config.rs` duplication in
//! `client-cloud-api-hub` and `client-github-deployer`
//! (Phase 2F: shared env helpers).
//!
//! Conventions:
//! - `env_or(key, default)`: return env var value or the literal default.
//! - `env_bool(key, default)`: treat "1", "true", "yes" (case-insensitive) as true.
//! - `env_u16(key, default)`: parse as `u16`, fall back to default on any error.
//!
//! Product-specific constants (bind address, product id, env var names)
//! remain in each client's own `config.rs`.

use std::env;

pub fn env_or(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

#[allow(dead_code)]
pub fn env_bool(key: &str, default: bool) -> bool {
    env::var(key)
        .map(|v| {
            let v = v.trim();
            v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(default)
}

#[allow(dead_code)]
pub fn env_u16(key: &str, default: u16) -> u16 {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::{env_bool, env_or, env_u16};

    #[test]
    fn env_parsing_behaviors() {
        std::env::set_var("WPTSALL_RUNTIME_TEST_ENV_OR", "abc");
        assert_eq!(env_or("WPTSALL_RUNTIME_TEST_ENV_OR", "fallback"), "abc");
        assert_eq!(
            env_or("WPTSALL_RUNTIME_TEST_ENV_OR_MISSING", "fallback"),
            "fallback"
        );

        for (idx, value) in ["1", "true", "TRUE", "yes", "YES", " true "]
            .iter()
            .enumerate()
        {
            let key = format!("WPTSALL_RUNTIME_TEST_ENV_BOOL_TRUE_{idx}");
            std::env::set_var(&key, value);
            assert!(env_bool(&key, false), "{value:?} should parse as true");
        }

        for (idx, value) in ["0", "false", "FALSE", "no", "NO", "", " truex "]
            .iter()
            .enumerate()
        {
            let key = format!("WPTSALL_RUNTIME_TEST_ENV_BOOL_FALSE_{idx}");
            std::env::set_var(&key, value);
            assert!(!env_bool(&key, true), "{value:?} should parse as false");
        }

        assert!(env_bool("WPTSALL_RUNTIME_TEST_ENV_BOOL_MISSING", true));

        std::env::set_var("WPTSALL_RUNTIME_TEST_ENV_U16", "3210");
        std::env::set_var("WPTSALL_RUNTIME_TEST_ENV_U16_BAD", "oops");
        assert_eq!(env_u16("WPTSALL_RUNTIME_TEST_ENV_U16", 99), 3210);
        assert_eq!(env_u16("WPTSALL_RUNTIME_TEST_ENV_U16_BAD", 99), 99);
        assert_eq!(env_u16("WPTSALL_RUNTIME_TEST_ENV_U16_MISSING", 99), 99);
    }
}
