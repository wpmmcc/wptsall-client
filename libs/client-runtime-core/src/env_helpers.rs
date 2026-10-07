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
