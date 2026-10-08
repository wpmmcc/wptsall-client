//! Signature verification for each algorithm.
//!
//! Each function returns `Ok(())` on success or `Err(AuthError)` with diagnostic info.

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

use crate::config;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct AuthError {
    pub algorithm: &'static str,
    pub message: String,
}

impl AuthError {
    pub fn new(algorithm: &'static str, message: impl Into<String>) -> Self {
        Self {
            algorithm,
            message: message.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

fn hmac_sha256_bytes(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// Youdao truncation: if len > 20, use first10 + len + last10.
fn youdao_truncate(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() > 20 {
        let first10: String = chars[..10].iter().collect();
        let last10: String = chars[chars.len() - 10..].iter().collect();
        format!("{}{}{}", first10, chars.len(), last10)
    } else {
        text.to_string()
    }
}

/// Alibaba-style URL encoding (RFC 3986).
fn url_encode(input: &str) -> String {
    let mut result = String::new();
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(byte as char);
            }
            _ => {
                result.push_str(&format!("%{:02X}", byte));
            }
        }
    }
    result
}

/// Convert YYYYMMDD date string from a unix timestamp.
#[cfg(test)]
fn format_unix_date(unix_secs: u64) -> String {
    let secs = unix_secs as i64;
    let days = secs / 86400;
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}{:02}{:02}", y, m, d)
}

// ---------------------------------------------------------------------------
// 1. Baidu: MD5(appid + q + salt + secret)
// ---------------------------------------------------------------------------

/// Request fields sent by client in form/JSON body.
pub struct BaiduParams {
    pub appid: String,
    pub q: String,
    pub salt: String,
    pub sign: String,
}

pub fn verify_baidu(params: &BaiduParams) -> Result<(), AuthError> {
    if params.appid != config::BAIDU_APPID {
        return Err(AuthError::new(
            "md5",
            format!("Unknown appid: {}", params.appid),
        ));
    }
    let concat = format!(
        "{}{}{}{}",
        params.appid,
        params.q,
        params.salt,
        config::BAIDU_SECRET
    );
    let expected = format!("{:x}", md5::compute(concat.as_bytes()));
    if expected != params.sign {
        return Err(AuthError::new(
            "md5",
            format!(
                "MD5 signature mismatch: expected {}, got {}",
                expected, params.sign
            ),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 2. Youdao: SHA256(appKey + truncated(q) + salt + curtime + appSecret)
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub struct YoudaoParams {
    pub app_key: String,
    pub q: String,
    pub salt: String,
    pub curtime: String,
    pub sign: String,
    pub sign_type: String,
}

pub fn verify_youdao(params: &YoudaoParams) -> Result<(), AuthError> {
    if params.app_key != config::YOUDAO_APP_KEY {
        return Err(AuthError::new(
            "sha256",
            format!("Unknown appKey: {}", params.app_key),
        ));
    }
    let truncated = youdao_truncate(&params.q);
    let concat = format!(
        "{}{}{}{}{}",
        params.app_key,
        truncated,
        params.salt,
        params.curtime,
        config::YOUDAO_APP_SECRET
    );
    let hash = Sha256::digest(concat.as_bytes());
    let expected = to_hex(&hash);
    if expected != params.sign {
        return Err(AuthError::new(
            "sha256",
            format!(
                "SHA256 signature mismatch: expected {}, got {}",
                expected, params.sign
            ),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 3. HMAC-SHA256: HMAC(secret, concat_string)
// ---------------------------------------------------------------------------

pub struct HmacParams {
    pub api_key: String,
    pub text: String,
    pub salt: String,
    pub sign: String,
}

pub fn verify_hmac(params: &HmacParams) -> Result<(), AuthError> {
    if params.api_key != config::HMAC_API_KEY {
        return Err(AuthError::new(
            "hmac_sha256",
            format!("Unknown api_key: {}", params.api_key),
        ));
    }
    // The client concatenates fields and HMACs with the secret.
    // Concat order: api_key + text + salt (matching typical sign_config.concat)
    let concat = format!("{}{}{}", params.api_key, params.text, params.salt);
    let mut mac =
        Hmac::<Sha256>::new_from_slice(config::HMAC_SECRET.as_bytes()).expect("HMAC key length");
    mac.update(concat.as_bytes());
    let expected = to_hex(&mac.finalize().into_bytes());
    if expected != params.sign {
        return Err(AuthError::new(
            "hmac_sha256",
            format!(
                "HMAC-SHA256 signature mismatch: expected {}, got {}",
                expected, params.sign
            ),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 4. TC3-HMAC-SHA256 (Tencent Cloud)
// ---------------------------------------------------------------------------

pub struct Tc3Params {
    pub authorization: String,
    pub timestamp: String,
    pub body: String,
    pub host: String,
    /// Request URI path used in the canonical request. Empty defaults to `/`.
    /// When non-root, `/` is also accepted so legacy signers (path=`/`) keep working.
    pub path: String,
}

pub fn verify_tc3(params: &Tc3Params) -> Result<(), AuthError> {
    // Parse Authorization header:
    // TC3-HMAC-SHA256 Credential={id}/{scope}, SignedHeaders=content-type;host, Signature={sig}
    let auth = &params.authorization;
    if !auth.starts_with("TC3-HMAC-SHA256 ") {
        return Err(AuthError::new(
            "tc3_hmac_sha256",
            "Authorization header must start with TC3-HMAC-SHA256",
        ));
    }

    let parts_str = &auth["TC3-HMAC-SHA256 ".len()..];
    let credential = extract_field(parts_str, "Credential=")
        .ok_or_else(|| AuthError::new("tc3_hmac_sha256", "Missing Credential in Authorization"))?;
    let signature = extract_field(parts_str, "Signature=")
        .ok_or_else(|| AuthError::new("tc3_hmac_sha256", "Missing Signature in Authorization"))?;

    // Credential = {secret_id}/{date}/{service}/tc3_request
    let cred_parts: Vec<&str> = credential.splitn(2, '/').collect();
    if cred_parts.len() < 2 {
        return Err(AuthError::new(
            "tc3_hmac_sha256",
            "Invalid Credential format",
        ));
    }
    let secret_id = cred_parts[0];
    let credential_scope = cred_parts[1];

    if secret_id != config::TC3_SECRET_ID {
        return Err(AuthError::new(
            "tc3_hmac_sha256",
            format!("Unknown secret_id: {}", secret_id),
        ));
    }

    // Extract date and service from scope: {date}/{service}/tc3_request
    let scope_parts: Vec<&str> = credential_scope.split('/').collect();
    if scope_parts.len() < 3 {
        return Err(AuthError::new(
            "tc3_hmac_sha256",
            "Invalid credential scope",
        ));
    }
    let date = scope_parts[0];
    let service = scope_parts[1];

    let payload_hash = to_hex(&Sha256::digest(params.body.as_bytes()));
    let secret_date_key = hmac_sha256_bytes(
        format!("TC3{}", config::TC3_SECRET_KEY).as_bytes(),
        date.as_bytes(),
    );
    let secret_service_key = hmac_sha256_bytes(&secret_date_key, service.as_bytes());
    let signing_key = hmac_sha256_bytes(&secret_service_key, b"tc3_request");

    let mut expected_for_log = String::new();
    for path in canonical_path_candidates(&params.path) {
        let canonical_request = format!(
            "POST\n{}\n\ncontent-type:application/json\nhost:{}\n\ncontent-type;host\n{}",
            path, params.host, payload_hash
        );
        let string_to_sign = format!(
            "TC3-HMAC-SHA256\n{}\n{}\n{}",
            params.timestamp,
            credential_scope,
            to_hex(&Sha256::digest(canonical_request.as_bytes()))
        );
        let expected = to_hex(&hmac_sha256_bytes(&signing_key, string_to_sign.as_bytes()));
        if expected == signature {
            return Ok(());
        }
        if expected_for_log.is_empty() {
            expected_for_log = expected;
        }
    }

    Err(AuthError::new(
        "tc3_hmac_sha256",
        format!(
            "TC3 signature mismatch: expected {}, got {}",
            expected_for_log, signature
        ),
    ))
}

// ---------------------------------------------------------------------------
// 5. AWS SigV4
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub struct AwsSigV4Params {
    pub authorization: String,
    pub amz_date: String,
    pub body: String,
    pub host: String,
    /// Request URI path used in the canonical request. Empty defaults to `/`.
    /// When non-root, `/` is also accepted so legacy signers (path=`/`) keep working.
    pub path: String,
}

/// Paths to try when rebuilding the SigV4 / TC3 canonical request.
fn canonical_path_candidates(path: &str) -> Vec<&str> {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed == "/" {
        vec!["/"]
    } else {
        vec![trimmed, "/"]
    }
}

pub fn verify_aws_sigv4(params: &AwsSigV4Params) -> Result<(), AuthError> {
    verify_sigv4_variant(
        params,
        "AWS4-HMAC-SHA256",
        "AWS4",
        config::AWS_ACCESS_KEY,
        config::AWS_SECRET_KEY,
        "aws_sigv4",
    )
}

// ---------------------------------------------------------------------------
// 6. Volcengine HMAC-SHA256 (SigV4 variant)
// ---------------------------------------------------------------------------

pub fn verify_volcengine(params: &AwsSigV4Params) -> Result<(), AuthError> {
    // Official Volcengine uses "HMAC-SHA256 "; some clients emit AWS4-HMAC-SHA256.
    if params.authorization.starts_with("HMAC-SHA256 ") {
        return verify_sigv4_variant(
            params,
            "HMAC-SHA256",
            "AWS4",
            config::VOLC_ACCESS_KEY,
            config::VOLC_SECRET_KEY,
            "volcengine_hmac_sha256",
        );
    }
    if params.authorization.starts_with("AWS4-HMAC-SHA256 ") {
        return verify_sigv4_variant(
            params,
            "AWS4-HMAC-SHA256",
            "AWS4",
            config::VOLC_ACCESS_KEY,
            config::VOLC_SECRET_KEY,
            "volcengine_hmac_sha256",
        );
    }
    Err(AuthError::new(
        "volcengine_hmac_sha256",
        format!(
            "bad Authorization prefix={:?} (want HMAC-SHA256 or AWS4-HMAC-SHA256)",
            params.authorization.chars().take(48).collect::<String>()
        ),
    ))
}

fn verify_sigv4_variant(
    params: &AwsSigV4Params,
    algo_prefix: &str,
    secret_prefix: &str,
    expected_access_key: &str,
    secret_key: &str,
    algo_name: &'static str,
) -> Result<(), AuthError> {
    let auth = &params.authorization;
    let prefix = format!("{} ", algo_prefix);
    if !auth.starts_with(&prefix) {
        return Err(AuthError::new(
            algo_name,
            format!("Authorization header must start with {}", algo_prefix),
        ));
    }

    let parts_str = &auth[prefix.len()..];
    let credential = extract_field(parts_str, "Credential=")
        .ok_or_else(|| AuthError::new(algo_name, "Missing Credential"))?;
    let signature = extract_field(parts_str, "Signature=")
        .ok_or_else(|| AuthError::new(algo_name, "Missing Signature"))?;

    let cred_parts: Vec<&str> = credential.splitn(2, '/').collect();
    if cred_parts.len() < 2 {
        return Err(AuthError::new(algo_name, "Invalid Credential format"));
    }
    let access_key = cred_parts[0];
    let credential_scope = cred_parts[1]; // {date}/{service}/aws4_request

    if access_key != expected_access_key {
        return Err(AuthError::new(
            algo_name,
            format!("Unknown access_key: {}", access_key),
        ));
    }

    let scope_parts: Vec<&str> = credential_scope.split('/').collect();
    if scope_parts.len() < 3 {
        return Err(AuthError::new(algo_name, "Invalid credential scope"));
    }
    let date = scope_parts[0];
    let region = if scope_parts.len() >= 4 {
        scope_parts[1]
    } else {
        "us-east-1"
    };
    let service = if scope_parts.len() >= 4 {
        scope_parts[2]
    } else {
        scope_parts[1]
    };

    let payload_hash = to_hex(&Sha256::digest(params.body.as_bytes()));
    let string_date = if params.amz_date.trim().is_empty() {
        format!("{}T000000Z", date)
    } else {
        params.amz_date.clone()
    };

    let date_key = hmac_sha256_bytes(
        format!("{}{}", secret_prefix, secret_key).as_bytes(),
        date.as_bytes(),
    );
    let region_key = hmac_sha256_bytes(&date_key, region.as_bytes());
    let service_key = hmac_sha256_bytes(&region_key, service.as_bytes());
    let signing_key = hmac_sha256_bytes(&service_key, b"aws4_request");

    let mut expected_for_log = String::new();
    for path in canonical_path_candidates(&params.path) {
        let canonical_request = format!(
            "POST\n{}\n\ncontent-type:application/json\nhost:{}\n\ncontent-type;host\n{}",
            path, params.host, payload_hash
        );
        let string_to_sign = format!(
            "{}\n{}\n{}\n{}",
            algo_prefix,
            string_date,
            credential_scope,
            to_hex(&Sha256::digest(canonical_request.as_bytes()))
        );
        let expected = to_hex(&hmac_sha256_bytes(&signing_key, string_to_sign.as_bytes()));
        if expected == signature {
            return Ok(());
        }
        if expected_for_log.is_empty() {
            expected_for_log = expected;
        }
    }

    Err(AuthError::new(
        algo_name,
        format!(
            "Signature mismatch: expected {}, got {}",
            expected_for_log, signature
        ),
    ))
}

// ---------------------------------------------------------------------------
// 7. Alibaba V1: sorted params + HMAC-SHA1 + Base64
// ---------------------------------------------------------------------------

pub struct AlibabaParams {
    /// All query/form params as key-value pairs (includes Signature).
    pub params: Vec<(String, String)>,
    pub signature: String,
}

pub fn verify_alibaba(params: &AlibabaParams) -> Result<(), AuthError> {
    // Filter out Signature from params, sort the rest.
    let mut sorted: Vec<(String, String)> = params
        .params
        .iter()
        .filter(|(k, _)| k != "Signature" && k != "signature")
        .cloned()
        .collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));

    let canonical = sorted
        .iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join("&");
    let string_to_sign = format!("POST&%2F&{}", url_encode(&canonical));

    let sign_key = format!("{}&", config::ALI_SECRET_KEY);
    let mut mac =
        Hmac::<sha1::Sha1>::new_from_slice(sign_key.as_bytes()).expect("HMAC-SHA1 key length");
    mac.update(string_to_sign.as_bytes());
    let expected = BASE64_STANDARD.encode(mac.finalize().into_bytes());

    if expected != params.signature {
        return Err(AuthError::new(
            "alibaba_v1",
            format!(
                "Alibaba signature mismatch: expected {}, got {}",
                expected, params.signature
            ),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 8. Azure Subscription Key
// ---------------------------------------------------------------------------

pub fn verify_azure(subscription_key: &str) -> Result<(), AuthError> {
    if subscription_key != config::AZURE_SUB_KEY && subscription_key != config::BEARER_KEY {
        return Err(AuthError::new(
            "azure_subscription_key",
            format!(
                "Invalid subscription key: expected {} (or {}), got {}",
                config::AZURE_SUB_KEY,
                config::BEARER_KEY,
                subscription_key
            ),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 9. KakaoAK
// ---------------------------------------------------------------------------

pub fn verify_kakao(authorization: &str) -> Result<(), AuthError> {
    let expected = format!("KakaoAK {}", config::KAKAO_KEY);
    if authorization != expected {
        return Err(AuthError::new(
            "kakao_api_key",
            format!(
                "KakaoAK mismatch: expected '{}', got '{}'",
                expected, authorization
            ),
        ));
    }
    Ok(())
}

/// Bearer token used by OpenAI-compatible / generic HTTP MT mocks.
pub fn verify_bearer_api_key(authorization: &str) -> Result<(), AuthError> {
    let token = authorization
        .strip_prefix("Bearer ")
        .unwrap_or(authorization)
        .trim();
    if token.is_empty() {
        return Err(AuthError::new("bearer", "Missing Bearer API key"));
    }
    if token != config::BEARER_KEY {
        return Err(AuthError::new(
            "bearer",
            format!(
                "Invalid Bearer API key: expected {}, got {}",
                config::BEARER_KEY,
                token
            ),
        ));
    }
    Ok(())
}

/// DeepL: `Authorization: DeepL-Auth-Key <key>` (also accept bare key / Bearer for Lab overrides).
pub fn verify_deepl_auth_key(authorization: &str) -> Result<(), AuthError> {
    let raw = authorization.trim();
    if raw.is_empty() {
        return Err(AuthError::new("deepl", "Missing DeepL-Auth-Key header"));
    }
    let key = raw
        .strip_prefix("DeepL-Auth-Key ")
        .or_else(|| raw.strip_prefix("Bearer "))
        .unwrap_or(raw)
        .trim();
    if key != config::DEEPL_AUTH_KEY && key != config::BEARER_KEY {
        return Err(AuthError::new(
            "deepl",
            format!(
                "Invalid DeepL auth key: expected {} (or {}), got {}",
                config::DEEPL_AUTH_KEY,
                config::BEARER_KEY,
                key
            ),
        ));
    }
    Ok(())
}

/// Google Translate v2: API key in JSON/`form` field `key` or query `key`.
pub fn verify_google_api_key(api_key: &str) -> Result<(), AuthError> {
    let key = api_key.trim();
    if key.is_empty() {
        return Err(AuthError::new("google", "Missing Google API key"));
    }
    if key != config::GOOGLE_API_KEY && key != config::BEARER_KEY {
        return Err(AuthError::new(
            "google",
            format!(
                "Invalid Google API key: expected {} (or {}), got {}",
                config::GOOGLE_API_KEY,
                config::BEARER_KEY,
                key
            ),
        ));
    }
    Ok(())
}

/// Google Cloud Translation Advanced (v3): OAuth access token after login / service account exchange.
pub fn verify_google_access_token(authorization: &str) -> Result<(), AuthError> {
    let raw = authorization.trim();
    if raw.is_empty() {
        return Err(AuthError::new(
            "google_oauth",
            "Missing Authorization Bearer access_token (Google Cloud OAuth login required)",
        ));
    }
    let token = raw
        .strip_prefix("Bearer ")
        .unwrap_or(raw)
        .trim();
    if token != config::GOOGLE_ACCESS_TOKEN && token != config::BEARER_KEY {
        // Real Google bearers are tokens obtained from the OAuth token endpoint.
        // Accept mock-issued HS256 JWTs from POST /oauth/token so the full
        // client-side OAuth loop (fetch token -> use token) passes end-to-end.
        if verify_oauth_bearer(&format!("Bearer {}", token)).is_ok() {
            return Ok(());
        }
        return Err(AuthError::new(
            "google_oauth",
            format!(
                "Invalid Google OAuth access_token: expected {} (or {}) or a mock-issued JWT, got {}",
                config::GOOGLE_ACCESS_TOKEN,
                config::BEARER_KEY,
                token
            ),
        ));
    }
    Ok(())
}

/// Azure Translator OAuth (Entra): `Authorization: Bearer <access_token>`.
pub fn verify_azure_access_token(authorization: &str) -> Result<(), AuthError> {
    let raw = authorization.trim();
    if raw.is_empty() {
        return Err(AuthError::new(
            "azure_oauth",
            "Missing Authorization Bearer access_token (Azure OAuth login required)",
        ));
    }
    let token = raw.strip_prefix("Bearer ").unwrap_or(raw).trim();
    if token != config::AZURE_ACCESS_TOKEN && token != config::BEARER_KEY {
        // Real Azure Entra bearers are tokens obtained from the OAuth token
        // endpoint. Accept mock-issued HS256 JWTs from POST /oauth/token so
        // the full client-side OAuth loop (fetch token -> use token) passes.
        if verify_oauth_bearer(&format!("Bearer {}", token)).is_ok() {
            return Ok(());
        }
        return Err(AuthError::new(
            "azure_oauth",
            format!(
                "Invalid Azure OAuth access_token: expected {} (or {}) or a mock-issued JWT, got {}",
                config::AZURE_ACCESS_TOKEN,
                config::BEARER_KEY,
                token
            ),
        ));
    }
    Ok(())
}

/// Naver Papago: `X-Naver-Client-Id` + `X-Naver-Client-Secret`.
pub fn verify_papago(client_id: &str, client_secret: &str) -> Result<(), AuthError> {
    let id = client_id.trim();
    let secret = client_secret.trim();
    if id.is_empty() || secret.is_empty() {
        return Err(AuthError::new(
            "papago",
            "Missing X-Naver-Client-Id or X-Naver-Client-Secret",
        ));
    }
    let id_ok = id == config::PAPAGO_CLIENT_ID || id == config::BEARER_KEY;
    let secret_ok = secret == config::PAPAGO_CLIENT_SECRET || secret == config::BEARER_KEY;
    if !id_ok || !secret_ok {
        return Err(AuthError::new(
            "papago",
            format!(
                "Invalid Papago credentials: expected id={} secret={}",
                config::PAPAGO_CLIENT_ID,
                config::PAPAGO_CLIENT_SECRET
            ),
        ));
    }
    Ok(())
}

/// Yandex Translate: form/query `key`.
pub fn verify_yandex_api_key(api_key: &str) -> Result<(), AuthError> {
    let key = api_key.trim();
    if key.is_empty() {
        return Err(AuthError::new("yandex", "Missing Yandex API key"));
    }
    if key != config::YANDEX_API_KEY && key != config::BEARER_KEY {
        return Err(AuthError::new(
            "yandex",
            format!(
                "Invalid Yandex API key: expected {} (or {}), got {}",
                config::YANDEX_API_KEY,
                config::BEARER_KEY,
                key
            ),
        ));
    }
    Ok(())
}

/// Phrase TMS: `Authorization: token <api_key>`.
pub fn verify_phrase_token(authorization: &str) -> Result<(), AuthError> {
    let raw = authorization.trim();
    if raw.is_empty() {
        return Err(AuthError::new("phrase", "Missing Phrase token Authorization"));
    }
    let key = raw
        .strip_prefix("token ")
        .or_else(|| raw.strip_prefix("Token "))
        .or_else(|| raw.strip_prefix("Bearer "))
        .unwrap_or(raw)
        .trim();
    if key != config::BEARER_KEY {
        return Err(AuthError::new(
            "phrase",
            format!(
                "Invalid Phrase token: expected {}, got {}",
                config::BEARER_KEY,
                key
            ),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 10. OAuth client_credentials -> JWT token
// ---------------------------------------------------------------------------

pub fn verify_oauth_client_credentials(
    client_id: &str,
    client_secret: &str,
) -> Result<String, AuthError> {
    if client_id != config::OAUTH_CLIENT_ID || client_secret != config::OAUTH_CLIENT_SECRET {
        return Err(AuthError::new(
            "oauth",
            "Invalid client_id or client_secret",
        ));
    }

    // Issue an HS256 JWT
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let ttl = config::oauth_token_ttl_seconds();
    let claims = serde_json::json!({
        "sub": client_id,
        "iss": "mock-translate-api",
        "iat": now,
        "exp": now + ttl,
        "scope": "translate",
    });

    let key = jsonwebtoken::EncodingKey::from_secret(config::OAUTH_JWT_SECRET.as_bytes());
    let header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256);
    let token = jsonwebtoken::encode(&header, &claims, &key)
        .map_err(|e| AuthError::new("oauth", format!("JWT encode error: {}", e)))?;

    Ok(token)
}

// ---------------------------------------------------------------------------
// 11. OAuth Bearer (verify HS256 JWT from /oauth/token)
// ---------------------------------------------------------------------------

pub fn verify_oauth_bearer(authorization: &str) -> Result<(), AuthError> {
    let token = authorization
        .strip_prefix("Bearer ")
        .ok_or_else(|| AuthError::new("oauth_bearer", "Missing Bearer prefix"))?;

    let key = jsonwebtoken::DecodingKey::from_secret(config::OAUTH_JWT_SECRET.as_bytes());
    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256);
    validation.set_issuer(&["mock-translate-api"]);
    validation.validate_exp = true;
    validation.required_spec_claims.clear();

    jsonwebtoken::decode::<serde_json::Value>(token, &key, &validation)
        .map_err(|e| AuthError::new("oauth_bearer", format!("JWT verification failed: {}", e)))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// 12. JWT Bearer (RSA-SHA256) -- client signs JWT with private key
// ---------------------------------------------------------------------------

pub fn verify_jwt_bearer(authorization: &str) -> Result<(), AuthError> {
    let token = authorization
        .strip_prefix("Bearer ")
        .ok_or_else(|| AuthError::new("jwt_bearer", "Missing Bearer prefix"))?;

    let key = jsonwebtoken::DecodingKey::from_rsa_pem(config::JWT_RSA_PUBLIC_KEY_PEM.as_bytes())
        .map_err(|e| AuthError::new("jwt_bearer", format!("RSA public key error: {}", e)))?;

    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
    validation.validate_exp = true;
    // Accept tokens with any issuer/audience for testing flexibility
    validation.set_required_spec_claims::<String>(&[]);
    validation.validate_aud = false;

    jsonwebtoken::decode::<serde_json::Value>(token, &key, &validation).map_err(|e| {
        AuthError::new(
            "jwt_bearer",
            format!("JWT RS256 verification failed: {}", e),
        )
    })?;

    Ok(())
}

// ---------------------------------------------------------------------------
// 12. iFlytek V1: X-CheckSum header = MD5(APISecret + CurTime + Param)
//     X-Appid, X-CurTime, X-CheckSum, X-Param (base64 of params JSON)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct IflytekParams {
    pub app_id: String,
    pub cur_time: String,
    pub checksum: String,
    pub param: String,
    pub body: String,
}

pub fn verify_iflytek(params: &IflytekParams) -> Result<(), AuthError> {
    if params.app_id != config::IFLYTEK_APP_ID {
        return Err(AuthError::new(
            "iflytek_v1",
            format!(
                "Invalid X-Appid: expected {}, got {}",
                config::IFLYTEK_APP_ID, params.app_id
            ),
        ));
    }
    // Compute expected checksum: MD5(APISecret + CurTime + X-Param)
    let concat = format!(
        "{}{}{}",
        config::IFLYTEK_API_SECRET, params.cur_time, params.param
    );
    let expected = format!("{:x}", md5::compute(concat.as_bytes()));
    if expected != params.checksum {
        return Err(AuthError::new(
            "iflytek_v1",
            format!(
                "X-CheckSum mismatch: expected {}, got {}",
                expected, params.checksum
            ),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 13. Niutrans V2: sign in body = lowercase MD5(apikey + q + from + to + apikey)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct NiutransParams {
    pub api_key: String,
    pub q: String,
    pub from: String,
    pub to: String,
    pub sign: String,
}

pub fn verify_niutrans(params: &NiutransParams) -> Result<(), AuthError> {
    if params.api_key != config::NIUTRANS_API_KEY {
        return Err(AuthError::new(
            "niutrans_v2",
            format!(
                "Invalid api key: expected {}, got {}",
                config::NIUTRANS_API_KEY, params.api_key
            ),
        ));
    }
    let concat = format!("{}{}{}{}{}", params.api_key, params.q, params.from, params.to, params.api_key);
    let expected = format!("{:x}", md5::compute(concat.as_bytes()));
    if expected != params.sign {
        return Err(AuthError::new(
            "niutrans_v2",
            format!(
                "sign mismatch: expected {}, got {}",
                expected, params.sign
            ),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Shared parser helper
// ---------------------------------------------------------------------------

/// Extract a field value from Authorization header substring.
/// e.g. `extract_field("Credential=abc/def, SignedHeaders=x", "Credential=")` -> Some("abc/def")
fn extract_field<'a>(header: &'a str, prefix: &str) -> Option<&'a str> {
    let start = header.find(prefix)? + prefix.len();
    let rest = &header[start..];
    // Field ends at ',' or end-of-string
    let end = rest.find(',').unwrap_or(rest.len());
    let value = rest[..end].trim();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_verify_baidu_valid() {
        let q = "hello";
        let salt = "12345";
        let concat = format!(
            "{}{}{}{}",
            config::BAIDU_APPID,
            q,
            salt,
            config::BAIDU_SECRET
        );
        let sign = format!("{:x}", md5::compute(concat.as_bytes()));

        let params = BaiduParams {
            appid: config::BAIDU_APPID.to_string(),
            q: q.to_string(),
            salt: salt.to_string(),
            sign,
        };
        assert!(verify_baidu(&params).is_ok());
    }

    #[test]
    fn test_verify_baidu_invalid_sign() {
        let params = BaiduParams {
            appid: config::BAIDU_APPID.to_string(),
            q: "hello".to_string(),
            salt: "12345".to_string(),
            sign: "bad_signature".to_string(),
        };
        assert!(verify_baidu(&params).is_err());
    }

    #[test]
    fn test_verify_youdao_valid() {
        let q = "hello world";
        let salt = "uuid-test";
        let curtime = "1700000000";
        let truncated = youdao_truncate(q);
        let concat = format!(
            "{}{}{}{}{}",
            config::YOUDAO_APP_KEY,
            truncated,
            salt,
            curtime,
            config::YOUDAO_APP_SECRET
        );
        let hash = Sha256::digest(concat.as_bytes());
        let sign = to_hex(&hash);

        let params = YoudaoParams {
            app_key: config::YOUDAO_APP_KEY.to_string(),
            q: q.to_string(),
            salt: salt.to_string(),
            curtime: curtime.to_string(),
            sign,
            sign_type: "v3".to_string(),
        };
        assert!(verify_youdao(&params).is_ok());
    }

    #[test]
    fn test_verify_youdao_long_text_truncation() {
        // Text > 20 chars triggers truncation
        let q = "abcdefghijklmnopqrstuvwxyz";
        let salt = "test-salt";
        let curtime = "1700000000";
        let truncated = youdao_truncate(q);
        assert_eq!(truncated, "abcdefghij26qrstuvwxyz");

        let concat = format!(
            "{}{}{}{}{}",
            config::YOUDAO_APP_KEY,
            truncated,
            salt,
            curtime,
            config::YOUDAO_APP_SECRET
        );
        let hash = Sha256::digest(concat.as_bytes());
        let sign = to_hex(&hash);

        let params = YoudaoParams {
            app_key: config::YOUDAO_APP_KEY.to_string(),
            q: q.to_string(),
            salt: salt.to_string(),
            curtime: curtime.to_string(),
            sign,
            sign_type: "v3".to_string(),
        };
        assert!(verify_youdao(&params).is_ok());
    }

    #[test]
    fn test_verify_hmac_valid() {
        let text = "hello";
        let salt = "54321";
        let concat = format!("{}{}{}", config::HMAC_API_KEY, text, salt);
        let mut mac = Hmac::<Sha256>::new_from_slice(config::HMAC_SECRET.as_bytes()).unwrap();
        mac.update(concat.as_bytes());
        let sign = to_hex(&mac.finalize().into_bytes());

        let params = HmacParams {
            api_key: config::HMAC_API_KEY.to_string(),
            text: text.to_string(),
            salt: salt.to_string(),
            sign,
        };
        assert!(verify_hmac(&params).is_ok());
    }

    #[test]
    fn test_verify_azure_valid() {
        assert!(verify_azure(config::AZURE_SUB_KEY).is_ok());
    }

    #[test]
    fn test_verify_azure_invalid() {
        assert!(verify_azure("wrong-key").is_err());
    }

    #[test]
    fn test_verify_kakao_valid() {
        let auth = format!("KakaoAK {}", config::KAKAO_KEY);
        assert!(verify_kakao(&auth).is_ok());
    }

    #[test]
    fn test_verify_kakao_invalid() {
        assert!(verify_kakao("KakaoAK wrong-key").is_err());
    }

    #[test]
    fn test_oauth_client_credentials_valid() {
        let result =
            verify_oauth_client_credentials(config::OAUTH_CLIENT_ID, config::OAUTH_CLIENT_SECRET);
        assert!(result.is_ok());
        let token = result.unwrap();
        assert!(!token.is_empty());
        // Verify the issued token
        assert!(verify_oauth_bearer(&format!("Bearer {}", token)).is_ok());
    }

    #[test]
    fn test_oauth_client_credentials_invalid() {
        let result = verify_oauth_client_credentials("wrong-id", "wrong-secret");
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_field() {
        let header = "Credential=abc/def, SignedHeaders=content-type;host, Signature=xyz123";
        assert_eq!(extract_field(header, "Credential="), Some("abc/def"));
        assert_eq!(extract_field(header, "Signature="), Some("xyz123"));
        assert_eq!(
            extract_field(header, "SignedHeaders="),
            Some("content-type;host")
        );
        assert_eq!(extract_field(header, "NotPresent="), None);
    }

    #[test]
    fn test_youdao_truncate_short() {
        assert_eq!(youdao_truncate("hello"), "hello");
    }

    #[test]
    fn test_youdao_truncate_exactly_20() {
        let text = "12345678901234567890"; // exactly 20 chars
        assert_eq!(youdao_truncate(text), text);
    }

    #[test]
    fn test_youdao_truncate_long() {
        let text = "123456789012345678901"; // 21 chars
        assert_eq!(youdao_truncate(text), "1234567890212345678901");
    }

    #[test]
    fn test_format_unix_date() {
        // 2024-01-01 00:00:00 UTC = 1704067200
        assert_eq!(format_unix_date(1704067200), "20240101");
    }

    #[test]
    fn test_jwt_bearer_roundtrip() {
        // Sign a JWT with the test private key, then verify it
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let claims = serde_json::json!({
            "iss": "test",
            "sub": "test",
            "aud": "test",
            "iat": now,
            "exp": now + 3600,
        });

        let key =
            jsonwebtoken::EncodingKey::from_rsa_pem(config::JWT_RSA_PRIVATE_KEY_PEM.as_bytes())
                .expect("private key should parse");
        let header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
        let token = jsonwebtoken::encode(&header, &claims, &key).expect("encode should work");

        let auth = format!("Bearer {}", token);
        assert!(verify_jwt_bearer(&auth).is_ok());
    }

    #[test]
    fn test_jwt_bearer_invalid_token() {
        assert!(verify_jwt_bearer("Bearer invalid.jwt.token").is_err());
    }

    #[test]
    fn test_tc3_roundtrip() {
        let body = r#"{"text":"hello","source":"en","target":"zh"}"#;
        let host = "tmt.tencentcloudapi.com";
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let timestamp = now.to_string();
        let date = format_unix_date(now);
        let service = "tmt";

        let payload_hash = to_hex(&Sha256::digest(body.as_bytes()));
        let canonical_request = format!(
            "POST\n/\n\ncontent-type:application/json\nhost:{}\n\ncontent-type;host\n{}",
            host, payload_hash
        );
        let credential_scope = format!("{}/{}/tc3_request", date, service);
        let string_to_sign = format!(
            "TC3-HMAC-SHA256\n{}\n{}\n{}",
            timestamp,
            credential_scope,
            to_hex(&Sha256::digest(canonical_request.as_bytes()))
        );

        let secret_date_key = hmac_sha256_bytes(
            format!("TC3{}", config::TC3_SECRET_KEY).as_bytes(),
            date.as_bytes(),
        );
        let secret_service_key = hmac_sha256_bytes(&secret_date_key, service.as_bytes());
        let signing_key = hmac_sha256_bytes(&secret_service_key, b"tc3_request");
        let signature = to_hex(&hmac_sha256_bytes(&signing_key, string_to_sign.as_bytes()));

        let authorization = format!(
            "TC3-HMAC-SHA256 Credential={}/{}, SignedHeaders=content-type;host, Signature={}",
            config::TC3_SECRET_ID,
            credential_scope,
            signature
        );

        let params = Tc3Params {
            authorization,
            timestamp,
            body: body.to_string(),
            host: host.to_string(),
            path: "/".to_string(),
        };
        assert!(verify_tc3(&params).is_ok());
    }

    #[test]
    fn test_aws_sigv4_roundtrip() {
        let body = r#"{"Text":"hello","SourceLanguageCode":"en","TargetLanguageCode":"zh"}"#;
        let host = "translate.us-east-1.amazonaws.com";
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let date = format_unix_date(now);
        let service = "translate";
        let region = "us-east-1";

        let payload_hash = to_hex(&Sha256::digest(body.as_bytes()));
        let canonical_request = format!(
            "POST\n/\n\ncontent-type:application/json\nhost:{}\n\ncontent-type;host\n{}",
            host, payload_hash
        );
        let credential_scope = format!("{}/{}/{}/aws4_request", date, region, service);
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{}T000000Z\n{}\n{}",
            date,
            credential_scope,
            to_hex(&Sha256::digest(canonical_request.as_bytes()))
        );

        let date_key = hmac_sha256_bytes(
            format!("AWS4{}", config::AWS_SECRET_KEY).as_bytes(),
            date.as_bytes(),
        );
        let region_key = hmac_sha256_bytes(&date_key, region.as_bytes());
        let service_key = hmac_sha256_bytes(&region_key, service.as_bytes());
        let signing_key = hmac_sha256_bytes(&service_key, b"aws4_request");
        let signature = to_hex(&hmac_sha256_bytes(&signing_key, string_to_sign.as_bytes()));

        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders=content-type;host, Signature={}",
            config::AWS_ACCESS_KEY,
            credential_scope,
            signature
        );

        let params = AwsSigV4Params {
            authorization,
            amz_date: format!("{}T000000Z", date),
            body: body.to_string(),
            host: host.to_string(),
            path: "/".to_string(),
        };
        assert!(verify_aws_sigv4(&params).is_ok());
    }

    #[test]
    fn test_alibaba_roundtrip() {
        let params_list = vec![
            (
                "AccessKeyId".to_string(),
                config::ALI_ACCESS_KEY.to_string(),
            ),
            ("Action".to_string(), "TranslateGeneral".to_string()),
            ("Format".to_string(), "JSON".to_string()),
            ("FormatType".to_string(), "text".to_string()),
            ("SourceLanguage".to_string(), "en".to_string()),
            ("TargetLanguage".to_string(), "zh".to_string()),
        ];

        let mut sorted = params_list.clone();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        let canonical = sorted
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect::<Vec<_>>()
            .join("&");
        let string_to_sign = format!("POST&%2F&{}", url_encode(&canonical));

        let sign_key = format!("{}&", config::ALI_SECRET_KEY);
        let mut mac = Hmac::<sha1::Sha1>::new_from_slice(sign_key.as_bytes()).unwrap();
        mac.update(string_to_sign.as_bytes());
        let signature = BASE64_STANDARD.encode(mac.finalize().into_bytes());

        let ali_params = AlibabaParams {
            params: params_list,
            signature: signature.clone(),
        };
        assert!(verify_alibaba(&ali_params).is_ok());
    }

    #[test]
    fn test_verify_deepl_and_google_and_bearer() {
        assert!(verify_deepl_auth_key(&format!("DeepL-Auth-Key {}", config::DEEPL_AUTH_KEY)).is_ok());
        assert!(verify_deepl_auth_key(&format!("Bearer {}", config::BEARER_KEY)).is_ok());
        assert!(verify_deepl_auth_key("DeepL-Auth-Key wrong").is_err());
        assert!(verify_deepl_auth_key("").is_err());

        assert!(verify_google_api_key(config::GOOGLE_API_KEY).is_ok());
        assert!(verify_google_api_key(config::BEARER_KEY).is_ok());
        assert!(verify_google_api_key("wrong").is_err());
        assert!(verify_google_api_key("").is_err());

        assert!(verify_google_access_token(&format!(
            "Bearer {}",
            config::GOOGLE_ACCESS_TOKEN
        ))
        .is_ok());
        assert!(verify_google_access_token("Bearer wrong-oauth").is_err());
        assert!(verify_google_access_token("").is_err());

        assert!(verify_azure_access_token(&format!(
            "Bearer {}",
            config::AZURE_ACCESS_TOKEN
        ))
        .is_ok());
        assert!(verify_azure_access_token("Bearer wrong").is_err());

        // Mock-issued JWTs from POST /oauth/token are accepted as OAuth bearers
        // (full client-side OAuth loop: fetch token -> use token).
        let jwt = verify_oauth_client_credentials(config::OAUTH_CLIENT_ID, config::OAUTH_CLIENT_SECRET)
            .expect("issue jwt");
        assert!(jwt.split('.').count() == 3, "issued token is a JWT");
        assert!(verify_azure_access_token(&format!("Bearer {}", jwt)).is_ok());
        assert!(verify_google_access_token(&format!("Bearer {}", jwt)).is_ok());
        assert!(verify_azure_access_token("Bearer fake.jwt.token").is_err());
        assert!(verify_google_access_token("Bearer fake.jwt.token").is_err());

        assert!(verify_bearer_api_key(&format!("Bearer {}", config::BEARER_KEY)).is_ok());
        assert!(verify_bearer_api_key(config::BEARER_KEY).is_ok());
        assert!(verify_bearer_api_key("Bearer nope").is_err());

        assert!(verify_papago(config::PAPAGO_CLIENT_ID, config::PAPAGO_CLIENT_SECRET).is_ok());
        assert!(verify_papago("bad", config::PAPAGO_CLIENT_SECRET).is_err());
        assert!(verify_yandex_api_key(config::YANDEX_API_KEY).is_ok());
        assert!(verify_yandex_api_key("bad").is_err());
        assert!(verify_phrase_token(&format!("token {}", config::BEARER_KEY)).is_ok());
        assert!(verify_phrase_token("token nope").is_err());
    }
}
