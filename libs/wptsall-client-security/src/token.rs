//! Short-lived token and request signing for client ↔ server communication.

use anyhow::{anyhow, Result};
use hmac::{Hmac, Mac};
use obfstr::obfstr;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::time::{SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionToken {
    pub token: String,
    pub expires_at: i64,
    #[serde(default)]
    pub scopes: Vec<String>,
}

impl SessionToken {
    pub fn is_expired(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        self.expires_at <= now
    }

    pub fn assert_valid(&self) -> Result<()> {
        if self.token.trim().is_empty() {
            return Err(anyhow!("empty session token"));
        }
        if self.is_expired() {
            return Err(anyhow!("session token expired"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenRequest {
    pub device_id: String,
    pub client_version: String,
    pub product_id: String,
    pub nonce: String,
    pub timestamp: i64,
    pub signature: String,
}

/// Build signed token request body for server secondary authorization.
pub fn build_token_request(
    device_id: &str,
    product_id: &str,
    client_version: &str,
    device_sign: impl Fn(&[u8]) -> String,
) -> TokenRequest {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let nonce = uuid::Uuid::new_v4().to_string();
    let payload = format!("{device_id}|{product_id}|{client_version}|{timestamp}|{nonce}");
    let signature = device_sign(payload.as_bytes());
    TokenRequest {
        device_id: device_id.to_string(),
        product_id: product_id.to_string(),
        client_version: client_version.to_string(),
        nonce,
        timestamp,
        signature,
    }
}

/// HMAC-SHA256 request signature for API calls (defense-in-depth with TLS).
pub fn sign_api_request(
    secret: &str,
    method: &str,
    path: &str,
    body: &[u8],
    timestamp: i64,
) -> Result<String> {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).map_err(|_| anyhow!("invalid HMAC key"))?;
    let body_hash = {
        use sha2::Digest;
        format!("{:x}", Sha256::digest(body))
    };
    let msg = format!("{method}\n{path}\n{timestamp}\n{body_hash}");
    mac.update(msg.as_bytes());
    Ok(hex::encode(mac.finalize().into_bytes()))
}

/// Attach auth headers for server-side secondary validation.
pub fn auth_headers(
    token: &SessionToken,
    request_sig: &str,
    timestamp: i64,
) -> Vec<(String, String)> {
    vec![
        (
            obfstr!("Authorization").to_string(),
            format!("Bearer {}", token.token),
        ),
        (
            obfstr!("X-WPTSALL-Timestamp").to_string(),
            timestamp.to_string(),
        ),
        (
            obfstr!("X-WPTSALL-Request-Signature").to_string(),
            request_sig.to_string(),
        ),
    ]
}

mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_expiry() {
        let t = SessionToken {
            token: "abc".into(),
            expires_at: 0,
            scopes: vec![],
        };
        assert!(t.is_expired());
    }

    #[test]
    fn request_signature_deterministic() {
        let sig1 = sign_api_request("secret", "GET", "/api/v1/client/domains", b"", 123).unwrap();
        let sig2 = sign_api_request("secret", "GET", "/api/v1/client/domains", b"", 123).unwrap();
        assert_eq!(sig1, sig2);
    }
}
