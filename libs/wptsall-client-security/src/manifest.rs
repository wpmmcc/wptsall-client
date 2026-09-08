//! Signed release manifest parsing and anti-rollback enforcement.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::HashMap;

use crate::signing;

fn semver_cmp(a: &str, b: &str) -> Ordering {
    let parse = |s: &str| -> Vec<u64> {
        s.split('.')
            .filter_map(|p| p.split('-').next().unwrap_or("").parse::<u64>().ok())
            .collect()
    };
    let av = parse(a);
    let bv = parse(b);
    let n = av.len().max(bv.len()).max(3);
    for i in 0..n {
        let x = *av.get(i).unwrap_or(&0);
        let y = *bv.get(i).unwrap_or(&0);
        match x.cmp(&y) {
            Ordering::Equal => continue,
            non_eq => return non_eq,
        }
    }
    Ordering::Equal
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReleaseProduct {
    pub latest_version: String,
    pub min_supported_version: String,
    pub release_notes_url: String,
    pub download_url_template: String,
    pub signature_url_template: String,
    #[serde(default)]
    pub mandatory: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReleaseSigningMeta {
    pub key_id: String,
    #[serde(default)]
    pub algorithm: String,
    #[serde(default)]
    pub public_key_pem: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SignedReleaseManifest {
    #[serde(rename = "schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub generated_at: Option<String>,
    pub products: HashMap<String, ReleaseProduct>,
    #[serde(default)]
    pub signing: Option<ReleaseSigningMeta>,
}

impl SignedReleaseManifest {
    pub fn from_json(raw: &str) -> Result<Self> {
        serde_json::from_str(raw).context("parse release manifest JSON")
    }

    /// Reject downgrades: `candidate` must be >= `installed`.
    pub fn assert_not_downgrade(installed: &str, candidate: &str) -> Result<()> {
        match semver_cmp(candidate, installed) {
            Ordering::Less => Err(anyhow!(
                "anti-rollback: candidate {candidate} < installed {installed}"
            )),
            _ => Ok(()),
        }
    }

    /// Ensure installed version meets minimum supported.
    pub fn assert_min_supported(&self, product_id: &str, installed: &str) -> Result<()> {
        let product = self
            .products
            .get(product_id)
            .ok_or_else(|| anyhow!("product not in manifest: {product_id}"))?;
        if semver_cmp(installed, &product.min_supported_version) == Ordering::Less {
            return Err(anyhow!(
                "installed {installed} below min_supported {}",
                product.min_supported_version
            ));
        }
        Ok(())
    }
}

/// Canonical JSON bytes for signing (sorted object keys, compact).
pub fn canonical_json_bytes(value: &serde_json::Value) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(&sort_json(value))?)
}

fn sort_json(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::new();
            for k in keys {
                out.insert(k.clone(), sort_json(&map[k]));
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(sort_json).collect())
        }
        other => other.clone(),
    }
}

/// Canonical JSON bytes for manifest signing (sorted keys, no whitespace).
pub fn canonical_manifest_bytes(manifest: &SignedReleaseManifest) -> Result<Vec<u8>> {
    let value = serde_json::to_value(manifest)?;
    canonical_json_bytes(&value)
}

/// Verify detached minisign signature over manifest JSON file.
pub fn verify_signed_manifest_file(manifest_path: &str, sig_path: &str) -> Result<()> {
    signing::verify_release_artifact(manifest_path, sig_path)
}

/// Verify artifact hash against SHA256SUMS entry after signature check.
pub fn verify_artifact_hash(data: &[u8], expected_hex: &str) -> Result<()> {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let actual = format!("{:x}", hasher.finalize());
    if actual != expected_hex.to_ascii_lowercase() {
        return Err(anyhow!(
            "hash mismatch: expected {expected_hex}, got {actual}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anti_rollback_rejects_older() {
        assert!(SignedReleaseManifest::assert_not_downgrade("2.1.0", "2.0.9").is_err());
        assert!(SignedReleaseManifest::assert_not_downgrade("2.1.0", "2.1.0").is_ok());
        assert!(SignedReleaseManifest::assert_not_downgrade("2.1.0", "2.1.1").is_ok());
    }

    #[test]
    fn canonical_json_bytes_sorts_nested_keys_and_preserves_utf8() {
        let envelope: serde_json::Value = serde_json::from_str(include_str!(
            "../../../install-client/security/fixtures/releases-envelope.json"
        ))
        .unwrap();
        let value = envelope.get("data").unwrap();
        let expected =
            include_str!("../../../install-client/security/fixtures/releases-data.canonical.json")
                .trim_end();

        let canonical = String::from_utf8(canonical_json_bytes(&value).unwrap()).unwrap();
        assert_eq!(canonical, expected);
    }
}
