use hmac::{Hmac, Mac};
use rand::Rng;
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let result = hasher.finalize();
    format!("{:x}", result)
}

pub fn compute_string_to_sign(
    method: &str,
    canonical_uri: &str,
    timestamp: u64,
    nonce: &str,
    sender_uuid: &str,
    body_hash: &str,
) -> String {
    format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        method.to_uppercase(),
        if canonical_uri.starts_with('/') {
            canonical_uri.to_string()
        } else {
            format!("/{}", canonical_uri)
        },
        timestamp,
        nonce,
        sender_uuid,
        body_hash
    )
}

pub fn compute_hmac_signature(
    string_to_sign: &str,
    shared_secret: &[u8],
) -> anyhow::Result<String> {
    let mut mac = HmacSha256::new_from_slice(shared_secret)
        .map_err(|e| anyhow::anyhow!("HMAC key initialization failed: {e}"))?;
    mac.update(string_to_sign.as_bytes());
    let result = mac.finalize();
    Ok(format!("{:x}", result.into_bytes()))
}

pub fn generate_nonce() -> String {
    let mut rng = rand::thread_rng();
    let bytes: [u8; 8] = rng.gen();
    format!("{:016x}", u64::from_be_bytes(bytes))
}

pub fn response_string_to_sign(
    nonce: &str,
    timestamp: &str,
    sender_uuid: &str,
    method: &str,
    canonical_uri: &str,
    status: u16,
    body: &[u8],
) -> String {
    format!("WPMMCC-RESPONSE-v1\n{nonce}\n{timestamp}\n{sender_uuid}\n{}\n{canonical_uri}\n{status}\n{}",
        method.to_uppercase(), sha256_hex(body))
}

pub fn verify_response(signature: &str, signed: &str, secret: &[u8]) -> anyhow::Result<()> {
    anyhow::ensure!(signature.len() == 64 && signature.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "sync response signature is missing or invalid");
    let mut bytes = [0u8; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&signature[index * 2..index * 2 + 2], 16)
            .map_err(|_| anyhow::anyhow!("sync response signature is invalid"))?;
    }
    let mut mac = HmacSha256::new_from_slice(secret)
        .map_err(|_| anyhow::anyhow!("sync response key is invalid"))?;
    mac.update(signed.as_bytes());
    mac.verify_slice(&bytes).map_err(|_| anyhow::anyhow!("sync response proof differs; original intent retained"))
}

pub fn build_hmac_headers(
    sender_uuid: &str,
    shared_secret: &[u8],
    method: &str,
    canonical_uri: &str,
    body: &[u8],
    timestamp: u64,
) -> anyhow::Result<Vec<(String, String)>> {
    let body_hash = sha256_hex(body);
    let nonce = generate_nonce();
    let string_to_sign = compute_string_to_sign(
        method,
        canonical_uri,
        timestamp,
        &nonce,
        sender_uuid,
        &body_hash,
    );
    let signature = compute_hmac_signature(&string_to_sign, shared_secret)?;

    Ok(vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        ("X-WPMMCC-Site-UUID".to_string(), sender_uuid.to_string()),
        ("X-WPMMCC-Timestamp".to_string(), timestamp.to_string()),
        ("X-WPMMCC-Nonce".to_string(), nonce),
        ("X-WPMMCC-Signature".to_string(), signature),
    ])
}
