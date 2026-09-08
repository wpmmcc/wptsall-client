//! Minisign release artifact verification with compile-time pinned public key.

use anyhow::{anyhow, Context, Result};
use minisign::{PublicKey, SignatureBox};
use std::fs::File;
use std::io::BufReader;

/// Compile-time embedded minisign public key (set via build.rs / env).
pub fn embedded_minisign_pubkey_b64() -> &'static str {
    env!("WPTSALL_EMBEDDED_MINISIGN_PUBKEY")
}

/// Load the pinned release signing public key.
pub fn pinned_release_pubkey() -> Result<PublicKey> {
    let raw = embedded_minisign_pubkey_b64();
    PublicKey::from_base64(raw).map_err(|e| anyhow!("invalid pinned minisign pubkey: {e}"))
}

/// Verify a file against its `.minisig` companion using the pinned key.
pub fn verify_release_artifact(artifact_path: &str, sig_path: &str) -> Result<()> {
    let pk = pinned_release_pubkey()?;
    let sig_content = std::fs::read_to_string(sig_path)
        .with_context(|| format!("read signature file: {sig_path}"))?;
    let sig_box = SignatureBox::from_string(&sig_content)
        .map_err(|e| anyhow!("parse minisign signature: {e}"))?;
    let file =
        File::open(artifact_path).with_context(|| format!("open artifact: {artifact_path}"))?;
    let reader = BufReader::new(file);
    minisign::verify(&pk, &sig_box, reader, true, false, true)
        .map_err(|e| anyhow!("release signature verification failed: {e}"))
}

/// Verify SHA256SUMS content signature (minisign over the sums file).
pub fn verify_sha256sums_signature(sha256sums_path: &str, sig_path: &str) -> Result<()> {
    verify_release_artifact(sha256sums_path, sig_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_pubkey_api_callable() {
        let _ = embedded_minisign_pubkey_b64();
    }
}
