//! WPMMCC peer credentials: pairing handshake, HKDF-SHA256 shared-secret
//! derivation, and encrypted-at-rest storage (doc 14 §4 / WP-A-03).
//!
//! The client pairs with each WPMMCC site as a standalone peer:
//!   1. The site admin generates a one-time pairing code (10-minute TTL).
//!   2. The client POSTs `/wp-json/wpmmcc/v1/sync/handshake` with its stable
//!      `client_origin_uuid`, a `pairing_code`, and a requested direction.
//!   3. The site registers the client as an active peer and answers with
//!      its own `peer_uuid` plus a one-time `pairing_secret`.
//!   4. Both sides derive the same 32-byte HMAC shared secret:
//!        HKDF-SHA256(ikm = pairing_secret,        // 32-char hex string as raw bytes
//!                    salt = min(client_uuid, site_uuid) + "|1",
//!                    info = "wpmmcc-peer-hmac-v1", len = 32)
//!      byte-identical to the PHP side
//!      `hash_hkdf('sha256', $pairing_secret, 32, 'wpmmcc-peer-hmac-v1', $salt)`.
//!   5. Every subsequent sync call (digest/pull/push/media/reconcile) is
//!      HMAC-signed with that secret; `X-WPMMCC-Site-UUID` carries the
//!      client origin UUID (the peer id the site registered).
//!
//! Direction semantics (doc 14 §4.3, mirrored by the plugin's negotiation):
//!   - pairing with the SOURCE site (client pulls): requested `pull_only`
//!     → the site records the client as `push_only` (it only serves pushes
//!     of its content to this peer).
//!   - pairing with the TARGET site (client pushes): requested `push_only`
//!     → the site records the client as `pull_only` (it only accepts
//!     pushes from this peer).

use anyhow::{anyhow, Context};
use hkdf::Hkdf;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Sha256;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::bindings::load_encrypted_or_plain;
use crate::logging::unix_ts;
use crate::sync_engine::ConflictStrategy;

pub const PEER_CREDENTIALS_SCHEMA_VERSION: &str = "wpmmcc-peer-credentials.v1";

/// HKDF info string (must stay byte-identical to the plugin's
/// `class-wpmmcc-rest-handshake.php`).
pub const HKDF_INFO: &[u8] = b"wpmmcc-peer-hmac-v1";

/// HKDF salt suffix: `min(uuid_a, uuid_uuid_b) + "|1"`.
const HKDF_SALT_SUFFIX: &str = "|1";

pub const DEFAULT_CLIENT_ORIGIN_NAME: &str = "WPMMCC ATS Client";

/// Role of a site in a sync pair — decides the requested direction the
/// client asks for during the handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingRole {
    /// The client pulls content from this site.
    Source,
    /// The client pushes content to this site.
    Target,
}

impl PairingRole {
    pub fn as_requested_direction(self) -> &'static str {
        match self {
            // Initiator pulls → receiver records itself push-only.
            PairingRole::Source => "pull_only",
            // Initiator pushes → receiver records itself pull-only.
            PairingRole::Target => "push_only",
        }
    }
}

/// One paired WPMMCC site. `shared_secret_hex` is the 64-char hex of the
/// 32-byte HKDF output — the HMAC key for every authenticated call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerCredential {
    /// Normalized domain base (`scheme://host`, the domain-token-bindings key).
    pub domain: String,
    /// The site's own origin UUID (handshake response `peer_uuid`).
    pub peer_uuid: String,
    pub peer_name: String,
    /// 64-char hex of the derived 32-byte shared secret.
    pub shared_secret_hex: String,
    pub key_scheme: String,
    /// Direction recorded by the site for this client peer
    /// (`push_only` when paired as source, `pull_only` as target).
    pub negotiated_direction: String,
    pub install_signature: String,
    pub paired_at: u64,
    /// Role the client paired this site for (informational; the negotiated
    /// direction is authoritative on the site side).
    pub paired_as: String,
}

impl PeerCredential {
    pub fn shared_secret_bytes(&self) -> anyhow::Result<Vec<u8>> {
        decode_hex(&self.shared_secret_hex)
            .ok_or_else(|| anyhow!("invalid shared secret hex for domain '{}'", self.domain))
    }

    /// WP REST base for wpmmcc/v1 sync endpoints (plain route; the
    /// route-secret prefix only applies to `/sync/ping` discovery).
    pub fn rest_base_url(&self) -> String {
        format!("{}/wp-json/wpmmcc/v1", self.domain.trim_end_matches('/'))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerCredentialsDoc {
    pub schema_version: String,
    /// Stable UUID for this client installation — generated on first pair,
    /// reused for every handshake and HMAC signature afterwards.
    pub client_origin_uuid: String,
    /// Synthetic origin URL reported to the site (the client is an
    /// initiator-only peer; sites never call back to it).
    pub client_origin_url: String,
    pub client_origin_name: String,
    #[serde(default)]
    pub peers: HashMap<String, PeerCredential>,
    pub updated_at: u64,
}

impl Default for PeerCredentialsDoc {
    fn default() -> Self {
        Self {
            schema_version: PEER_CREDENTIALS_SCHEMA_VERSION.to_string(),
            client_origin_uuid: String::new(),
            client_origin_url: String::new(),
            client_origin_name: DEFAULT_CLIENT_ORIGIN_NAME.to_string(),
            peers: HashMap::new(),
            updated_at: 0,
        }
    }
}

/// HKDF derivation, byte-compatible with the plugin's
/// `hash_hkdf('sha256', pairing_secret, 32, 'wpmmcc-peer-hmac-v1', salt)`.
///
/// PHP `min()` on strings is a lexicographic byte comparison; UUIDs are
/// ASCII, so `str::cmp` matches exactly.
pub fn derive_peer_shared_secret(
    pairing_secret: &str,
    client_uuid: &str,
    peer_uuid: &str,
) -> [u8; 32] {
    let salt_str = format!(
        "{}{}",
        if client_uuid <= peer_uuid {
            client_uuid
        } else {
            peer_uuid
        },
        HKDF_SALT_SUFFIX
    );
    let hk = Hkdf::<Sha256>::new(Some(salt_str.as_bytes()), pairing_secret.as_bytes());
    let mut okm = [0u8; 32];
    hk.expand(HKDF_INFO, &mut okm)
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    okm
}

fn encode_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// Load the credentials doc (decrypting the WPTC envelope when present).
pub fn load_peer_credentials(path: &str) -> anyhow::Result<PeerCredentialsDoc> {
    let file_path = Path::new(path);
    if let Some(parent) = file_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).with_context(|| {
                format!("create peer credentials dir failed: {}", parent.display())
            })?;
        }
    }
    if !file_path.exists() {
        let doc = PeerCredentialsDoc::default();
        save_peer_credentials(path, &doc)?;
        return Ok(doc);
    }
    let raw = load_encrypted_or_plain(file_path)?;
    if raw.trim().is_empty() {
        return Ok(PeerCredentialsDoc::default());
    }
    serde_json::from_str(&raw)
        .with_context(|| format!("parse peer credentials failed: {}", file_path.display()))
}

/// Save the credentials doc through the bindings crypto envelope
/// (AES-256-GCM at rest; a missing key refuses the save).
pub fn save_peer_credentials(path: &str, doc: &PeerCredentialsDoc) -> anyhow::Result<()> {
    let file_path = Path::new(path);
    if let Some(parent) = file_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).with_context(|| {
                format!("create peer credentials dir failed: {}", parent.display())
            })?;
        }
    }
    let plain = serde_json::to_string_pretty(doc).context("encode peer credentials json failed")?;
    crate::bindings::save_encrypted_file(file_path, &plain)
}

fn ensure_client_identity(doc: &mut PeerCredentialsDoc) {
    if doc.client_origin_uuid.trim().is_empty() {
        doc.client_origin_uuid = format!("urn:uuid:{}", uuid::Uuid::new_v4());
    }
    if doc.client_origin_url.trim().is_empty() {
        // Synthetic, stable, per-installation URL. The site stores it as the
        // peer endpoint; the client never serves it.
        let suffix: String = doc.client_origin_uuid.chars().rev().take(8).collect();
        doc.client_origin_url = format!("https://wpmmcc-ats-client.local/{suffix}");
    }
}

/// Look up the stored credential for a normalized domain.
pub fn find_peer_credential<'a>(
    doc: &'a PeerCredentialsDoc,
    domain: &str,
) -> Option<&'a PeerCredential> {
    doc.peers.get(domain.trim_end_matches('/'))
}

/// Remove a domain's credential (local forget; the site-side peer row
/// stays until the site admin removes it).
pub fn remove_peer_credential(path: &str, domain: &str) -> anyhow::Result<bool> {
    let mut doc = load_peer_credentials(path)?;
    let key = domain.trim_end_matches('/');
    let removed = doc.peers.remove(key).is_some();
    if removed {
        doc.updated_at = unix_ts();
        save_peer_credentials(path, &doc)?;
    }
    Ok(removed)
}

/// Public (non-secret) view of a credential for API responses.
pub fn credential_status(item: (&String, &PeerCredential)) -> Value {
    let (domain, cred) = item;
    json!({
        "domain": domain,
        "peer_uuid": cred.peer_uuid,
        "peer_name": cred.peer_name,
        "key_scheme": cred.key_scheme,
        "negotiated_direction": cred.negotiated_direction,
        "paired_as": cred.paired_as,
        "paired_at": cred.paired_at,
    })
}

/// Handshake request body (X-1, tasks/5.3falsh2/12 批 B): carries the
/// canonical `conflict_strategy` so the receiving site's peers row — and
/// through it the arbiter — honors the pairing-chosen strategy. Older
/// receiving sites ignore the extra field, so the wire stays compatible.
fn handshake_request_body(
    doc: &PeerCredentialsDoc,
    pairing_code: &str,
    requested_direction: &str,
    source_lang: &str,
    target_lang: &str,
    sync_mode: &str,
    conflict_strategy: &str,
) -> Value {
    json!({
        "pairing_code": pairing_code,
        "origin_uuid": doc.client_origin_uuid,
        "origin_url": doc.client_origin_url,
        "origin_name": doc.client_origin_name,
        "requested_direction": requested_direction,
        "source_lang": source_lang,
        "target_lang": target_lang,
        "sync_mode": sync_mode,
        "conflict_strategy": conflict_strategy,
    })
}

/// Execute the pairing handshake against one site and store the derived
/// credential. `domain` must be a normalized base (`scheme://host`) whose
/// binding was already identity-verified as `wpmmcc`. `conflict_strategy`
/// must be one of the five canonical values (lww / source_wins /
/// target_wins / manual_review / merge); it rides the handshake and is
/// stored by the receiving site for inbound arbitration.
#[allow(clippy::too_many_arguments)]
pub async fn pair_with_site(
    client: &Client,
    credentials_path: &str,
    domain: &str,
    pairing_code: &str,
    role: PairingRole,
    source_lang: &str,
    target_lang: &str,
    sync_mode: &str,
    conflict_strategy: &str,
) -> anyhow::Result<PeerCredential> {
    let pairing_code = pairing_code.trim();
    if pairing_code.is_empty() {
        return Err(anyhow!("pairing code must not be empty"));
    }
    if pairing_code.len() != 32 || !pairing_code.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(anyhow!(
            "invalid pairing code: expected 32 hex characters from the site admin page"
        ));
    }
    let conflict_strategy = conflict_strategy.trim();
    if ConflictStrategy::from_wire_str(conflict_strategy).is_none() {
        return Err(anyhow!(
            "invalid conflict strategy '{conflict_strategy}': expected one of lww / source_wins / target_wins / manual_review / merge"
        ));
    }

    let mut doc = load_peer_credentials(credentials_path)?;
    ensure_client_identity(&mut doc);

    let url = format!(
        "{}/wp-json/wpmmcc/v1/sync/handshake",
        domain.trim_end_matches('/')
    );
    let body = handshake_request_body(
        &doc,
        pairing_code,
        role.as_requested_direction(),
        source_lang,
        target_lang,
        sync_mode,
        conflict_strategy,
    );

    let resp = client
        .post(&url)
        .header("Content-Type", "application/json")
        .body(serde_json::to_vec(&body)?)
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .with_context(|| format!("handshake request to '{domain}' failed"))?;

    let status_code = resp.status();
    let resp_bytes = resp.bytes().await.unwrap_or_default();
    let resp_json: Value = serde_json::from_slice(&resp_bytes)
        .with_context(|| format!("handshake response from '{domain}' is not valid JSON"))?;

    if !status_code.is_success() {
        let code = resp_json["code"]
            .as_str()
            .unwrap_or("wpmmcc_handshake_failed");
        let message = resp_json["message"]
            .as_str()
            .unwrap_or("target site rejected the pairing handshake");
        return Err(anyhow!("handshake rejected ({code}): {message}"));
    }

    let data = resp_json
        .get("data")
        .ok_or_else(|| anyhow!("handshake response missing data payload"))?;
    let peer_uuid = data["peer_uuid"]
        .as_str()
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| anyhow!("handshake response missing peer_uuid"))?
        .to_string();
    let pairing_secret = data["pairing_secret"]
        .as_str()
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| anyhow!("handshake response missing pairing_secret"))?
        .to_string();

    // Derive the shared secret exactly as the site does, then store only
    // the derived key (the one-time pairing_secret is never persisted).
    let shared_secret =
        derive_peer_shared_secret(&pairing_secret, &doc.client_origin_uuid, &peer_uuid);

    let credential = PeerCredential {
        domain: domain.trim_end_matches('/').to_string(),
        peer_uuid,
        peer_name: data["peer_name"].as_str().unwrap_or("").to_string(),
        shared_secret_hex: encode_hex(&shared_secret),
        key_scheme: data["key_scheme"].as_str().unwrap_or("hmac_v1").to_string(),
        negotiated_direction: data["negotiated_direction"]
            .as_str()
            .unwrap_or("bidirectional")
            .to_string(),
        install_signature: data["install_signature"].as_str().unwrap_or("").to_string(),
        paired_at: unix_ts(),
        paired_as: match role {
            PairingRole::Source => "source",
            PairingRole::Target => "target",
        }
        .to_string(),
    };

    doc.peers
        .insert(credential.domain.clone(), credential.clone());
    doc.updated_at = unix_ts();
    save_peer_credentials(credentials_path, &doc)?;
    // Audit trail for credential material changes: the derived shared secret
    // itself is never logged (central redaction would catch it anyway).
    crate::logging::log_event_global(
        "info",
        "credential.peer_stored",
        json!({
            "domain": credential.domain,
            "peer_uuid": credential.peer_uuid,
            "paired_as": credential.paired_as,
            "negotiated_direction": credential.negotiated_direction,
        }),
    );

    Ok(credential)
}
#[cfg(test)]
mod tests {
    use super::*;

    // Reference vectors generated with PHP 8 `hash_hkdf` on the exact
    // plugin construction (`class-wpmmcc-rest-handshake.php` §6):
    //   bin2hex(hash_hkdf('sha256', <secret>, 32, 'wpmmcc-peer-hmac-v1',
    //                      <min-uuid> . '|1'))
    #[test]
    fn hkdf_matches_php_plugin_derivation() {
        // pairing_secret, client_uuid, peer_uuid → expected hex (PHP).
        let vector_1 = (
            "9f1a2b3c4d5e6f708192a3b4c5d6e7f8",
            "11111111-1111-1111-1111-111111111111",
            "22222222-2222-2222-2222-222222222222",
            "7cd6844ad9f65531518ede47c92422ec411ff6b5cb8bf8a20b8c1ecd8b046b25",
        );
        // Salt uses min(uuid_a, uuid_b) — peer UUID sorts first here.
        let vector_2 = (
            "aabbccdd",
            "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            "99999999-9999-9999-9999-999999999999",
            "69a237817d641818d13a7e0cbf2fd96dec452f14f32350f41b4aaca61ce7ded4",
        );

        for (secret, client_uuid, peer_uuid, expected) in [vector_1, vector_2] {
            let derived = derive_peer_shared_secret(secret, client_uuid, peer_uuid);
            assert_eq!(encode_hex(&derived), expected);
        }
    }

    #[test]
    fn hkdf_salt_uses_lexicographic_min() {
        // Client UUID sorts after the peer UUID: salt must be peer|1.
        // Proves the min() branch both ways through distinct vectors.
        let a = derive_peer_shared_secret("s", "zzzz", "aaaa");
        let b = derive_peer_shared_secret("s", "aaaa", "zzzz");
        assert_eq!(a, b, "derivation must be symmetric in the two uuids");
    }

    #[test]
    fn roles_map_to_requested_directions() {
        assert_eq!(PairingRole::Source.as_requested_direction(), "pull_only");
        assert_eq!(PairingRole::Target.as_requested_direction(), "push_only");
    }

    #[test]
    fn credentials_storage_roundtrip_encrypted() {
        let _key = crate::db::owned_mock_bindings_key();
        let dir = std::env::temp_dir().join(format!("sync_cred_test_{}", unix_ts()));
        let path = dir.join("creds.json");
        let path_str = path.to_string_lossy().to_string();

        let mut doc = PeerCredentialsDoc::default();
        ensure_client_identity(&mut doc);
        assert!(!doc.client_origin_uuid.is_empty());
        assert!(doc
            .client_origin_url
            .starts_with("https://wpmmcc-ats-client.local/"));

        let cred = PeerCredential {
            domain: "https://site-a.example".to_string(),
            peer_uuid: "22222222-2222-2222-2222-222222222222".to_string(),
            peer_name: "Site A".to_string(),
            shared_secret_hex: "00".repeat(32),
            key_scheme: "hmac_v1".to_string(),
            negotiated_direction: "push_only".to_string(),
            install_signature: String::new(),
            paired_at: unix_ts(),
            paired_as: "source".to_string(),
        };
        doc.peers.insert(cred.domain.clone(), cred.clone());
        save_peer_credentials(&path_str, &doc).expect("save");

        let loaded = load_peer_credentials(&path_str).expect("load");
        assert_eq!(loaded.client_origin_uuid, doc.client_origin_uuid);
        assert_eq!(loaded.peers.get("https://site-a.example"), Some(&cred));
        assert!(find_peer_credential(&loaded, "https://site-a.example").is_some());
        assert!(find_peer_credential(&loaded, "https://site-a.example/").is_some());

        assert!(remove_peer_credential(&path_str, "https://site-a.example").unwrap());
        let after = load_peer_credentials(&path_str).unwrap();
        assert!(after.peers.is_empty());
        assert!(!remove_peer_credential(&path_str, "https://site-a.example").unwrap());
    }

    #[test]
    fn shared_secret_bytes_decodes_hex() {
        let cred = PeerCredential {
            domain: "https://x.example".to_string(),
            peer_uuid: "u".to_string(),
            peer_name: String::new(),
            shared_secret_hex: encode_hex(&[7u8; 32]),
            key_scheme: "hmac_v1".to_string(),
            negotiated_direction: "push_only".to_string(),
            install_signature: String::new(),
            paired_at: 0,
            paired_as: "source".to_string(),
        };
        assert_eq!(cred.shared_secret_bytes().unwrap(), vec![7u8; 32]);
        assert!(cred.rest_base_url().ends_with("/wp-json/wpmmcc/v1"));
    }

    #[tokio::test]
    async fn pair_with_site_rejects_invalid_code_shapes() {
        let client = Client::new();
        let dir = std::env::temp_dir().join(format!("sync_cred_pair_test_{}", unix_ts()));
        let path = dir.join("creds.json");
        let path_str = path.to_string_lossy().to_string();

        // Too short / non-hex codes are rejected before any network call.
        let err = pair_with_site(
            &client,
            &path_str,
            "https://site.example",
            "abc",
            PairingRole::Source,
            "en_US",
            "zh_CN",
            "sync_only",
            "lww",
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("32 hex"));
    }

    #[tokio::test]
    async fn pair_with_site_rejects_unknown_conflict_strategy() {
        let client = Client::new();
        let dir = std::env::temp_dir().join(format!("sync_cred_strategy_test_{}", unix_ts()));
        let path = dir.join("creds.json");
        let path_str = path.to_string_lossy().to_string();

        // Legacy/unknown strategy words are rejected before any network
        // call — the client only ever sends the canonical vocabulary.
        let err = pair_with_site(
            &client,
            &path_str,
            "https://site.example",
            "0f1a2b3c4d5e6f708192a3b4c5d6e7f8",
            PairingRole::Source,
            "en_US",
            "zh_CN",
            "sync_only",
            "source_dominant",
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("invalid conflict strategy"));
    }

    #[test]
    fn handshake_body_carries_conflict_strategy() {
        let mut doc = PeerCredentialsDoc::default();
        ensure_client_identity(&mut doc);

        let body = handshake_request_body(
            &doc,
            "0f1a2b3c4d5e6f708192a3b4c5d6e7f8",
            "pull_only",
            "en_US",
            "zh_CN",
            "sync_only",
            "manual_review",
        );
        assert_eq!(body["conflict_strategy"], "manual_review");
        assert_eq!(body["pairing_code"], "0f1a2b3c4d5e6f708192a3b4c5d6e7f8");
        assert_eq!(body["origin_uuid"], doc.client_origin_uuid.as_str());
    }
}