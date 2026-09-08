//! Shared security primitives for WPTSALL Desktop and WebUI clients.
//!
//! Phases implemented:
//! 1. Release signing (minisign, pinned pubkey)
//! 2. Signed manifest verification + anti-rollback
//! 3. Device identity + secure key storage
//! 4. Short-lived token request helpers
//! 5. Unified [`SecurityGate`] facade
//! 6. String obfuscation via `obfstr`
//! 7. Anti-debug + self-integrity checks
//! 8. In-memory module loading (Linux, feature `mem-module`)
//! 9. Commercial packer hooks documented in install-client/security/

pub mod antitamper;
pub mod device;
pub mod gate;
pub mod manifest;
pub mod mem_module;
pub mod signing;
pub mod token;
pub mod updater_secure;

pub use gate::{SecurityConfig, SecurityGate};
pub use updater_secure::{
    download_and_verify_apply_ui, download_and_verify_signed_artifact, download_and_verify_update,
    fetch_verified_releases_data, VerifiedArtifact, VerifiedUpdate,
};
