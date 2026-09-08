use anyhow::Context;

use super::*;
use crate::component_rt::sign_plugin;
use crate::crypto::to_hex;

fn current_unix_duration() -> anyhow::Result<std::time::Duration> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|err| anyhow::anyhow!("system clock is before UNIX_EPOCH: {}", err))
}

pub(super) fn process_sign_config(
    sign_config: Option<&serde_json::Value>,
    ctx: &mut HashMap<String, String>,
    http_method: &str,
    request_url: &str,
) -> anyhow::Result<SignResult> {
    let config = match sign_config {
        Some(c) => c,
        None => return Ok(SignResult::ContextOnly(HashMap::new())),
    };

    let algorithm = config
        .get("algorithm")
        .and_then(|v| v.as_str())
        .unwrap_or("none");
    if algorithm == "none" {
        return Ok(SignResult::ContextOnly(HashMap::new()));
    }

    // Try WASM sign plugin first (if loaded).
    // Falls through to built-in on: plugin not loaded, call error, unsupported,
    // or host_function_required (Category D algorithms like jwt_bearer/rsa_sha256).
    if sign_plugin::is_sign_plugin_loaded() {
        if let Some(output) = sign_plugin::call_process_sign(algorithm, config, ctx) {
            if output.success {
                // Merge computed values back into context
                for (k, v) in &output.computed {
                    ctx.insert(k.clone(), v.clone());
                }
                return match output.result_type.as_str() {
                    "authorization" => Ok(SignResult::AuthorizationHeader(
                        output.authorization.unwrap_or_default(),
                    )),
                    "with_headers" => Ok(SignResult::WithHeaders(output.computed, output.headers)),
                    _ => {
                        // "context_only" or any other success type
                        Ok(SignResult::ContextOnly(output.computed))
                    }
                };
            }
            // Not successful -- check if we should fall through to built-in
            match output.result_type.as_str() {
                "unsupported" | "host_function_required" => {
                    // Fall through to built-in implementation
                }
                _ => {
                    // Plugin returned a real error for a known algorithm
                    return Err(anyhow::anyhow!(
                        "WASM sign plugin error ({}): {}",
                        algorithm,
                        output.error.unwrap_or_default()
                    ));
                }
            }
        }
        // call_process_sign returned None (plugin call failed) -- fall through
    }

    // Built-in fallback implementation

    // Category C: header injection (no concat/hash needed)
    match algorithm {
        "bearer" => {
            let api_key = ctx.get("auth.api_key").cloned().unwrap_or_default();
            return Ok(SignResult::AuthorizationHeader(format!(
                "Bearer {}",
                api_key
            )));
        }
        "azure_subscription_key" => {
            let key = ctx
                .get("auth.subscription_key")
                .or_else(|| ctx.get("auth.api_key"))
                .cloned()
                .unwrap_or_default();
            let headers = vec![("Ocp-Apim-Subscription-Key".to_string(), key)];
            return Ok(SignResult::WithHeaders(HashMap::new(), headers));
        }
        "kakao_api_key" => {
            let api_key = ctx.get("auth.api_key").cloned().unwrap_or_default();
            return Ok(SignResult::AuthorizationHeader(format!(
                "KakaoAK {}",
                api_key
            )));
        }
        "custom_header" => {
            return process_custom_header_sign(config, ctx);
        }
        _ => {}
    }

    // Category D: token generation
    match algorithm {
        "jwt_bearer" => {
            return process_jwt_bearer_sign(config, ctx);
        }
        "rsa_sha256" => {
            return process_rsa_sha256_sign(config, ctx);
        }
        _ => {}
    }

    // Category B: canonical request signing
    match algorithm {
        "tc3_hmac_sha256"
        | "aws_sigv4"
        | "volcengine_hmac_sha256"
        | "alibaba_v1"
        | "iflytek_v1" => {
            return process_canonical_request_sign(
                algorithm,
                config,
                ctx,
                http_method,
                request_url,
            );
        }
        _ => {}
    }

    if algorithm == "niutrans_v2" {
        return process_niutrans_v2_sign(ctx, request_url);
    }

    // Category A: concat-and-hash (existing pattern + new algorithms)
    prime_category_a_sign_context(config, ctx)?;

    // 3. Build concat string
    let concat_str = if let Some(concat) = config.get("concat").and_then(|v| v.as_array()) {
        concat
            .iter()
            .filter_map(|v| v.as_str())
            .map(|key| ctx.get(key).cloned().unwrap_or_default())
            .collect::<Vec<_>>()
            .join("")
    } else {
        return Ok(SignResult::ContextOnly(HashMap::new()));
    };

    let output_encoding = config
        .get("output_encoding")
        .and_then(|v| v.as_str())
        .unwrap_or("hex");

    // 4. Compute sign
    let sign_value = match algorithm {
        "md5" => format!("{:x}", md5::compute(concat_str.as_bytes())),
        "sha256" => {
            use sha2::{Digest, Sha256};
            let hash = Sha256::digest(concat_str.as_bytes());
            encode_hash_output(&hash, output_encoding)
        }
        "sha1" => {
            use sha1::Digest;
            let hash = sha1::Sha1::digest(concat_str.as_bytes());
            encode_hash_output(&hash, output_encoding)
        }
        "hmac_sha256" => {
            use hmac::{Hmac, Mac};
            use sha2::Sha256;
            let secret = resolve_sign_secret(ctx);
            let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
                .map_err(|e| anyhow::anyhow!("HMAC key error: {}", e))?;
            mac.update(concat_str.as_bytes());
            let result = mac.finalize();
            encode_hash_output(&result.into_bytes(), output_encoding)
        }
        "hmac_sha1" => {
            use hmac::{Hmac, Mac};
            let secret = resolve_sign_secret(ctx);
            let mut mac = Hmac::<sha1::Sha1>::new_from_slice(secret.as_bytes())
                .map_err(|e| anyhow::anyhow!("HMAC-SHA1 key error: {}", e))?;
            mac.update(concat_str.as_bytes());
            let result = mac.finalize();
            encode_hash_output(&result.into_bytes(), output_encoding)
        }
        "hmac_md5" => {
            use hmac::{Hmac, Mac};
            let secret = resolve_sign_secret(ctx);
            let mut mac = Hmac::<md5_digest::Md5>::new_from_slice(secret.as_bytes())
                .map_err(|e| anyhow::anyhow!("HMAC-MD5 key error: {}", e))?;
            mac.update(concat_str.as_bytes());
            let result = mac.finalize();
            encode_hash_output(&result.into_bytes(), output_encoding)
        }
        "base64_hmac_sha256" => {
            use hmac::{Hmac, Mac};
            use sha2::Sha256;
            let secret = resolve_sign_secret(ctx);
            let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
                .map_err(|e| anyhow::anyhow!("HMAC key error: {}", e))?;
            mac.update(concat_str.as_bytes());
            let result = mac.finalize();
            BASE64_STANDARD.encode(result.into_bytes())
        }
        other => {
            return Err(anyhow::anyhow!(
                "unsupported signing algorithm: '{}' (supported: md5, sha256, sha1, hmac_sha256, hmac_sha1, hmac_md5, base64_hmac_sha256, bearer, azure_subscription_key, kakao_api_key, custom_header, jwt_bearer, rsa_sha256, tc3_hmac_sha256, aws_sigv4, volcengine_hmac_sha256, alibaba_v1, iflytek_v1, niutrans_v2)",
                other
            ));
        }
    };
    ctx.insert("computed.sign".to_string(), sign_value);

    Ok(SignResult::ContextOnly(HashMap::new()))
}

pub(super) fn prime_sign_context(
    sign_config: Option<&serde_json::Value>,
    ctx: &mut HashMap<String, String>,
) -> anyhow::Result<()> {
    let Some(config) = sign_config else {
        return Ok(());
    };

    let algorithm = config
        .get("algorithm")
        .and_then(|v| v.as_str())
        .unwrap_or("none");

    match algorithm {
        "md5" | "sha256" | "sha1" | "hmac_sha256" | "hmac_sha1" | "hmac_md5"
        | "base64_hmac_sha256" => {
            prime_category_a_sign_context(config, ctx)?;
        }
        "tc3_hmac_sha256"
        | "aws_sigv4"
        | "volcengine_hmac_sha256"
        | "alibaba_v1"
        | "iflytek_v1" => {
            prime_canonical_request_sign_context(ctx)?;
        }
        "niutrans_v2" => {
            if !ctx.contains_key("computed.timestamp_ms") {
                ctx.insert(
                    "computed.timestamp_ms".to_string(),
                    current_unix_timestamp_millis()?,
                );
            }
        }
        _ => {}
    }

    Ok(())
}

fn current_unix_timestamp_millis() -> anyhow::Result<String> {
    Ok(current_unix_duration()?.as_millis().to_string())
}

fn compute_extra_sign_value(
    type_str: &str,
    ctx: &HashMap<String, String>,
) -> anyhow::Result<Option<String>> {
    match type_str {
        "unix_timestamp" => Ok(Some(current_unix_duration()?.as_secs().to_string())),
        "unix_timestamp_ms" => Ok(Some(current_unix_timestamp_millis()?)),
        "youdao_truncate" => Ok({
            let text = ctx.get("input.text").cloned().unwrap_or_default();
            let chars: Vec<char> = text.chars().collect();
            if chars.len() > 20 {
                let first10: String = chars[..10].iter().collect();
                let last10: String = chars[chars.len() - 10..].iter().collect();
                Some(format!("{}{}{}", first10, chars.len(), last10))
            } else {
                Some(text)
            }
        }),
        _ => Ok(None),
    }
}

fn prime_category_a_sign_context(
    config: &serde_json::Value,
    ctx: &mut HashMap<String, String>,
) -> anyhow::Result<()> {
    if !ctx.contains_key("computed.salt") {
        let salt_type = config
            .get("salt_type")
            .and_then(|v| v.as_str())
            .unwrap_or("none");
        match salt_type {
            "random_int" => {
                let salt: u64 = rand::random();
                ctx.insert("computed.salt".to_string(), salt.to_string());
            }
            "uuid" => {
                ctx.insert(
                    "computed.salt".to_string(),
                    uuid::Uuid::new_v4().to_string(),
                );
            }
            "unix_timestamp" => {
                let ts = current_unix_duration()?.as_secs();
                ctx.insert("computed.salt".to_string(), ts.to_string());
            }
            _ => {}
        }
    }

    if let Some(extra) = config.get("extra_computed").and_then(|v| v.as_object()) {
        for (name, type_val) in extra {
            let computed_key = format!("computed.{}", name);
            if ctx.contains_key(&computed_key) {
                continue;
            }
            let type_str = type_val.as_str().unwrap_or("");
            if let Some(value) = compute_extra_sign_value(type_str, ctx)? {
                ctx.insert(computed_key, value);
            }
        }
    }

    Ok(())
}

fn prime_canonical_request_sign_context(ctx: &mut HashMap<String, String>) -> anyhow::Result<()> {
    let now = current_unix_duration()?.as_secs();

    ctx.entry("computed.timestamp".to_string())
        .or_insert_with(|| now.to_string());
    ctx.entry("computed.date".to_string())
        .or_insert_with(|| format_unix_date(now));
    ctx.entry("computed.timestamp_iso8601".to_string())
        .or_insert_with(|| format_iso8601(now));
    ctx.entry("computed.nonce".to_string())
        .or_insert_with(|| uuid::Uuid::new_v4().to_string());

    Ok(())
}

fn process_niutrans_v2_sign(
    ctx: &mut HashMap<String, String>,
    request_url: &str,
) -> anyhow::Result<SignResult> {
    if !ctx.contains_key("computed.timestamp_ms") {
        ctx.insert(
            "computed.timestamp_ms".to_string(),
            current_unix_timestamp_millis()?,
        );
    }

    let mut params: Vec<(String, String)> = ctx
        .iter()
        .filter_map(|(key, value)| {
            let body_key = key.strip_prefix("request.body.")?;
            if matches!(body_key, "file" | "authStr" | "apikey") || value.trim().is_empty() {
                return None;
            }
            Some((body_key.to_string(), value.clone()))
        })
        .collect();

    if params.is_empty() {
        let parsed_url = url::Url::parse(request_url)
            .with_context(|| format!("invalid NiuTrans request URL: {request_url}"))?;
        for (key, value) in parsed_url.query_pairs() {
            let key = key.into_owned();
            let value = value.into_owned();
            if matches!(key.as_str(), "authStr" | "apikey") || value.trim().is_empty() {
                continue;
            }
            params.push((key, value));
        }
    }

    if let Some(api_key) = ctx
        .get("auth.api_key")
        .cloned()
        .filter(|value| !value.trim().is_empty())
    {
        params.push(("apikey".to_string(), api_key));
    }

    params.sort_by(|a, b| a.0.cmp(&b.0));
    let canonical = params
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("&");

    let sign_value = format!("{:x}", md5::compute(canonical.as_bytes()));
    ctx.insert("computed.sign".to_string(), sign_value);

    Ok(SignResult::ContextOnly(HashMap::new()))
}

fn resolve_sign_secret(ctx: &HashMap<String, String>) -> String {
    ctx.get("auth.secret_key")
        .or_else(|| ctx.get("auth.app_secret"))
        .cloned()
        .unwrap_or_default()
}

fn encode_hash_output(bytes: &[u8], encoding: &str) -> String {
    match encoding {
        "base64" => BASE64_STANDARD.encode(bytes),
        "base64url" => {
            use base64::engine::general_purpose::URL_SAFE_NO_PAD;
            URL_SAFE_NO_PAD.encode(bytes)
        }
        _ => to_hex(bytes), // "hex" or default
    }
}

fn process_canonical_request_sign(
    algorithm: &str,
    config: &serde_json::Value,
    ctx: &mut HashMap<String, String>,
    http_method: &str,
    request_url: &str,
) -> anyhow::Result<SignResult> {
    use sha2::{Digest, Sha256};

    prime_canonical_request_sign_context(ctx)?;

    let timestamp = ctx.get("computed.timestamp").cloned().unwrap_or_default();
    let date = ctx.get("computed.date").cloned().unwrap_or_default();
    let time_hhmmss = timestamp
        .parse::<u64>()
        .map(format_unix_time)
        .unwrap_or_default();

    // Derive HTTP method and URL path from actual request (not hardcoded)
    let method_upper = http_method.to_uppercase();
    let parsed_url = url::Url::parse(request_url).ok();
    let url_path = parsed_url
        .as_ref()
        .map(|u| u.path().to_string())
        .unwrap_or_else(|| "/".to_string());
    let request_host = ctx
        .get("auth.host")
        .cloned()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            parsed_url.as_ref().map(|u| match u.port() {
                Some(port) => format!("{}:{}", u.host_str().unwrap_or_default(), port),
                None => u.host_str().unwrap_or_default().to_string(),
            })
        })
        .unwrap_or_default();

    let secret = resolve_sign_secret(ctx);
    let service = config
        .get("service")
        .and_then(|v| v.as_str())
        .unwrap_or("translate");
    let region = config
        .get("region")
        .and_then(|v| v.as_str())
        .unwrap_or("us-east-1");

    // Build payload hash from concat or body context
    let payload = if algorithm == "tc3_hmac_sha256" {
        if let Some(body_json) = ctx.get("request.body_json") {
            if !body_json.is_empty() {
                body_json.clone()
            } else if let Some(concat) = config.get("concat").and_then(|v| v.as_array()) {
                concat
                    .iter()
                    .filter_map(|v| v.as_str())
                    .map(|key| ctx.get(key).cloned().unwrap_or_default())
                    .collect::<Vec<_>>()
                    .join("")
            } else {
                String::new()
            }
        } else if let Some(concat) = config.get("concat").and_then(|v| v.as_array()) {
            concat
                .iter()
                .filter_map(|v| v.as_str())
                .map(|key| ctx.get(key).cloned().unwrap_or_default())
                .collect::<Vec<_>>()
                .join("")
        } else {
            String::new()
        }
    } else if matches!(algorithm, "aws_sigv4" | "volcengine_hmac_sha256") {
        ctx.get("request.body_json").cloned().unwrap_or_default()
    } else if let Some(concat) = config.get("concat").and_then(|v| v.as_array()) {
        concat
            .iter()
            .filter_map(|v| v.as_str())
            .map(|key| ctx.get(key).cloned().unwrap_or_default())
            .collect::<Vec<_>>()
            .join("")
    } else {
        String::new()
    };
    let payload_hash = to_hex(&Sha256::digest(payload.as_bytes()));

    let mut extra_computed = HashMap::new();
    let mut extra_headers: Vec<(String, String)> = Vec::new();

    match algorithm {
        "tc3_hmac_sha256" => {
            // Tencent Cloud TC3-HMAC-SHA256
            let canonical_request = format!(
                "{}\n{}\n\ncontent-type:application/json\nhost:{}\n\ncontent-type;host\n{}",
                method_upper, url_path, request_host, payload_hash
            );
            let credential_scope = format!("{}/{}/tc3_request", date, service);
            let string_to_sign = format!(
                "TC3-HMAC-SHA256\n{}\n{}\n{}",
                timestamp,
                credential_scope,
                to_hex(&Sha256::digest(canonical_request.as_bytes()))
            );
            // Derive signing key
            let secret_date_key =
                hmac_sha256_bytes(format!("TC3{}", secret).as_bytes(), date.as_bytes());
            let secret_service_key = hmac_sha256_bytes(&secret_date_key, service.as_bytes());
            let signing_key = hmac_sha256_bytes(&secret_service_key, b"tc3_request");
            let signature = to_hex(&hmac_sha256_bytes(&signing_key, string_to_sign.as_bytes()));

            let authorization = format!(
                "TC3-HMAC-SHA256 Credential={}/{}, SignedHeaders=content-type;host, Signature={}",
                ctx.get("auth.secret_id")
                    .or_else(|| ctx.get("auth.api_key"))
                    .cloned()
                    .unwrap_or_default(),
                credential_scope,
                signature
            );
            extra_computed.insert("computed.sign".to_string(), signature);
            extra_headers.push(("X-TC-Timestamp".to_string(), timestamp));
            extra_headers.push(("Authorization".to_string(), authorization));
        }
        "aws_sigv4" | "volcengine_hmac_sha256" => {
            // AWS Signature V4 (also used by Volcengine with same logic)
            let canonical_request = format!(
                "{}\n{}\n\ncontent-type:application/json\nhost:{}\n\ncontent-type;host\n{}",
                method_upper, url_path, request_host, payload_hash
            );
            let amz_date = format!("{}T{}Z", date, time_hhmmss);
            let credential_scope = format!("{}/{}/{}/aws4_request", date, region, service);
            let algo_name = if algorithm == "volcengine_hmac_sha256" {
                "HMAC-SHA256"
            } else {
                "AWS4-HMAC-SHA256"
            };
            let string_to_sign = format!(
                "{}\n{}\n{}\n{}",
                algo_name,
                amz_date,
                credential_scope,
                to_hex(&Sha256::digest(canonical_request.as_bytes()))
            );
            let date_key = hmac_sha256_bytes(format!("AWS4{}", secret).as_bytes(), date.as_bytes());
            let region_key = hmac_sha256_bytes(&date_key, region.as_bytes());
            let service_key = hmac_sha256_bytes(&region_key, service.as_bytes());
            let signing_key = hmac_sha256_bytes(&service_key, b"aws4_request");
            let signature = to_hex(&hmac_sha256_bytes(&signing_key, string_to_sign.as_bytes()));

            let access_key = ctx
                .get("auth.access_key")
                .or_else(|| ctx.get("auth.api_key"))
                .cloned()
                .unwrap_or_default();
            let authorization = format!(
                "{} Credential={}/{}, SignedHeaders=content-type;host, Signature={}",
                algo_name, access_key, credential_scope, signature
            );
            extra_computed.insert("computed.sign".to_string(), signature);
            extra_headers.push(("X-Amz-Date".to_string(), amz_date));
            extra_headers.push(("Authorization".to_string(), authorization));
        }
        "alibaba_v1" => {
            // Alibaba Cloud: sort params + HMAC-SHA1 + Base64
            use hmac::{Hmac, Mac};
            let mut params: Vec<(String, String)> = Vec::new();
            if let Some(concat) = config.get("concat").and_then(|v| v.as_array()) {
                for item in concat {
                    if let Some(key) = item.as_str() {
                        // String format (backward-compatible): key is both ctx lookup and param name
                        let val = ctx.get(key).cloned().unwrap_or_default();
                        params.push((key.to_string(), val));
                    } else if let Some(obj) = item.as_object() {
                        // Object format:
                        // - {ctx_key: "auth.api_key", param_name: "AccessKeyId"}
                        // - {value: "TranslateGeneral", param_name: "Action"}
                        let ctx_key = obj.get("ctx_key").and_then(|v| v.as_str()).unwrap_or("");
                        let param_name = obj
                            .get("param_name")
                            .and_then(|v| v.as_str())
                            .unwrap_or(ctx_key);
                        let omit_if_empty = obj
                            .get("omit_if_empty")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        if param_name.trim().is_empty() {
                            continue;
                        }
                        let val = if let Some(value) = obj.get("value") {
                            if let Some(s) = value.as_str() {
                                render_template_string(s, ctx)
                            } else if value.is_number() || value.is_boolean() {
                                value.to_string()
                            } else {
                                String::new()
                            }
                        } else {
                            ctx.get(ctx_key).cloned().unwrap_or_default()
                        };
                        if omit_if_empty && val.trim().is_empty() {
                            continue;
                        }
                        params.push((param_name.to_string(), val));
                    }
                }
            }
            params.sort_by(|a, b| a.0.cmp(&b.0));
            let canonical = params
                .iter()
                .map(|(k, v)| format!("{}={}", k, v))
                .collect::<Vec<_>>()
                .join("&");
            let string_to_sign = format!("POST&%2F&{}", url_encode(&canonical));

            let sign_key = format!("{}&", secret);
            let mut mac = Hmac::<sha1::Sha1>::new_from_slice(sign_key.as_bytes())
                .map_err(|e| anyhow::anyhow!("HMAC-SHA1 key error: {}", e))?;
            mac.update(string_to_sign.as_bytes());
            let signature = BASE64_STANDARD.encode(mac.finalize().into_bytes());

            extra_computed.insert("computed.sign".to_string(), signature);
        }
        "iflytek_v1" => {
            // iFlytek HTTP Signature: HMAC-SHA256 + Digest header
            use sha2::{Digest as Sha2Digest, Sha256 as Sha256Digest};
            let host = ctx.get("auth.host").cloned().unwrap_or_default();
            let api_key = ctx
                .get("auth.api_key")
                .or_else(|| ctx.get("auth.secret_id"))
                .cloned()
                .unwrap_or_default();
            let method = config
                .get("method")
                .and_then(|v| v.as_str())
                .unwrap_or("POST");
            let path = config
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("/v2/its");

            let rfc1123_date = ctx
                .get("computed.timestamp")
                .and_then(|value| value.parse::<u64>().ok())
                .map(format_rfc1123)
                .unwrap_or(format_rfc1123(current_unix_duration()?.as_secs()));
            let body_hash = Sha256Digest::digest(payload.as_bytes());
            let body_digest = format!("SHA-256={}", BASE64_STANDARD.encode(body_hash));

            let signing_string = format!(
                "host: {}\ndate: {}\n{} {} HTTP/1.1\ndigest: {}",
                host, rfc1123_date, method, path, body_digest
            );

            let signature_bytes = hmac_sha256_bytes(secret.as_bytes(), signing_string.as_bytes());
            let signature = BASE64_STANDARD.encode(&signature_bytes);

            let authorization = format!(
                "api_key=\"{}\", algorithm=\"hmac-sha256\", headers=\"host date request-line digest\", signature=\"{}\"",
                api_key, signature
            );

            extra_computed.insert("computed.sign".to_string(), signature);
            extra_computed.insert("computed.date_rfc1123".to_string(), rfc1123_date.clone());
            extra_computed.insert("computed.digest".to_string(), body_digest.clone());

            extra_headers.push(("Host".to_string(), host));
            extra_headers.push(("Date".to_string(), rfc1123_date));
            extra_headers.push(("Digest".to_string(), body_digest));
            extra_headers.push(("Authorization".to_string(), authorization));
        }
        _ => {}
    }

    for (k, v) in &extra_computed {
        ctx.insert(k.clone(), v.clone());
    }

    if extra_headers.is_empty() {
        Ok(SignResult::ContextOnly(extra_computed))
    } else {
        Ok(SignResult::WithHeaders(extra_computed, extra_headers))
    }
}

fn hmac_sha256_bytes(key: &[u8], data: &[u8]) -> Vec<u8> {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn format_unix_time(unix_secs: u64) -> String {
    // Convert unix timestamp to HHMMSS (UTC)
    let day_secs = unix_secs % 86400;
    let hour = day_secs / 3600;
    let minute = (day_secs % 3600) / 60;
    let second = day_secs % 60;
    format!("{:02}{:02}{:02}", hour, minute, second)
}

fn format_unix_date(unix_secs: u64) -> String {
    // Convert unix timestamp to YYYYMMDD
    // Days since epoch calculation
    let secs = unix_secs as i64;
    let days = secs / 86400;
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
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

fn format_iso8601(unix_secs: u64) -> String {
    let secs = unix_secs as i64;
    let total_days = secs / 86400;
    let day_secs = (secs % 86400) as u64;
    let hour = day_secs / 3600;
    let minute = (day_secs % 3600) / 60;
    let second = day_secs % 60;

    let z = total_days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y, m, d, hour, minute, second
    )
}

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

fn format_rfc1123(unix_secs: u64) -> String {
    let secs = unix_secs as i64;
    let total_days = secs / 86400;
    let day_secs = (secs % 86400) as u64;
    let hour = day_secs / 3600;
    let minute = (day_secs % 3600) / 60;
    let second = day_secs % 60;

    let dow = ((total_days % 7 + 4) % 7 + 7) % 7;
    let dow_name = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][dow as usize];

    let z = total_days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    let month_name = [
        "", "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ][m as usize];

    format!(
        "{}, {:02} {} {:04} {:02}:{:02}:{:02} GMT",
        dow_name, d, month_name, y, hour, minute, second
    )
}

fn process_custom_header_sign(
    config: &serde_json::Value,
    ctx: &HashMap<String, String>,
) -> anyhow::Result<SignResult> {
    let mut headers: Vec<(String, String)> = Vec::new();
    let mut authorization: Option<String> = None;
    let build_auth_header_value = |raw_value: String, prefix: Option<&str>| -> Option<String> {
        let trimmed = raw_value.trim();
        if trimmed.is_empty() {
            return None;
        }
        match prefix.map(|p| p.trim()).filter(|p| !p.is_empty()) {
            Some(p) => Some(format!("{p} {trimmed}")),
            None => Some(trimmed.to_string()),
        }
    };

    // Authorization header candidates (first non-empty wins), e.g. oauth -> key fallback.
    if let Some(candidates) = config.get("auth_candidates").and_then(|v| v.as_array()) {
        for cand in candidates {
            let value_key = cand
                .get("value_key")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .trim();
            if value_key.is_empty() {
                continue;
            }
            let raw_value = ctx.get(value_key).cloned().unwrap_or_default();
            let prefix = cand
                .get("prefix")
                .and_then(|v| v.as_str())
                .or_else(|| cand.get("auth_prefix").and_then(|v| v.as_str()));
            if let Some(value) = build_auth_header_value(raw_value, prefix) {
                authorization = Some(value);
                break;
            }
        }
    }

    // Backward-compatible single Authorization header with optional prefix.
    if authorization.is_none() {
        if let Some(auth_value_key) = config.get("auth_value_key").and_then(|v| v.as_str()) {
            let auth_value = ctx.get(auth_value_key).cloned().unwrap_or_default();
            let prefix = config.get("auth_prefix").and_then(|v| v.as_str());
            authorization = build_auth_header_value(auth_value, prefix);
        }
    }

    // Custom headers array
    if let Some(header_arr) = config.get("headers").and_then(|v| v.as_array()) {
        for item in header_arr {
            let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if name.is_empty() {
                continue;
            }
            if let Some(value_key) = item.get("value_key").and_then(|v| v.as_str()) {
                let value = ctx.get(value_key).cloned().unwrap_or_default();
                if !value.trim().is_empty() {
                    headers.push((name.to_string(), value));
                }
            } else if let Some(value) = item.get("value").and_then(|v| v.as_str()) {
                if !value.trim().is_empty() {
                    headers.push((name.to_string(), value.to_string()));
                }
            } else if let Some(template) = item.get("value_template").and_then(|v| v.as_str()) {
                let resolved = resolve_header_template(template, ctx);
                if !resolved.trim().is_empty() {
                    headers.push((name.to_string(), resolved));
                }
            }
        }
    }

    match authorization {
        Some(auth) if headers.is_empty() => Ok(SignResult::AuthorizationHeader(auth)),
        Some(auth) => {
            headers.push(("Authorization".to_string(), auth));
            Ok(SignResult::WithHeaders(HashMap::new(), headers))
        }
        None if !headers.is_empty() => Ok(SignResult::WithHeaders(HashMap::new(), headers)),
        None => Ok(SignResult::ContextOnly(HashMap::new())),
    }
}

fn resolve_header_template(template: &str, ctx: &HashMap<String, String>) -> String {
    // Delegate to render_template_string which uses {{key}} double-curly syntax,
    // unifying the template resolution across all component config paths.
    render_template_string(template, ctx)
}

fn process_jwt_bearer_sign(
    config: &serde_json::Value,
    ctx: &mut HashMap<String, String>,
) -> anyhow::Result<SignResult> {
    let now = current_unix_duration()?.as_secs();

    let iss = config
        .get("issuer")
        .and_then(|v| v.as_str())
        .or_else(|| ctx.get("auth.client_id").map(|s| s.as_str()))
        .unwrap_or_default();
    let sub = config
        .get("subject")
        .and_then(|v| v.as_str())
        .or_else(|| ctx.get("auth.client_id").map(|s| s.as_str()))
        .unwrap_or_default();
    let aud = config
        .get("audience")
        .and_then(|v| v.as_str())
        .or_else(|| ctx.get("auth.token_url").map(|s| s.as_str()))
        .unwrap_or_default();
    let exp_secs = config
        .get("expires_in")
        .and_then(|v| v.as_u64())
        .unwrap_or(3600);

    let private_key_pem = ctx.get("auth.private_key").cloned().unwrap_or_default();

    let claims = serde_json::json!({
        "iss": iss,
        "sub": sub,
        "aud": aud,
        "iat": now,
        "exp": now + exp_secs,
    });

    let header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    let key = jsonwebtoken::EncodingKey::from_rsa_pem(private_key_pem.as_bytes())
        .map_err(|e| anyhow::anyhow!("JWT RSA key error: {}", e))?;
    let token = jsonwebtoken::encode(&header, &claims, &key)
        .map_err(|e| anyhow::anyhow!("JWT encode error: {}", e))?;

    ctx.insert("computed.jwt_token".to_string(), token.clone());
    Ok(SignResult::AuthorizationHeader(format!("Bearer {}", token)))
}

fn process_rsa_sha256_sign(
    config: &serde_json::Value,
    ctx: &mut HashMap<String, String>,
) -> anyhow::Result<SignResult> {
    use rsa::pkcs1v15::SigningKey;
    use rsa::pkcs8::DecodePrivateKey;
    use rsa::signature::{SignatureEncoding, Signer};

    let private_key_pem = ctx.get("auth.private_key").cloned().unwrap_or_default();

    let payload = if let Some(concat) = config.get("concat").and_then(|v| v.as_array()) {
        concat
            .iter()
            .filter_map(|v| v.as_str())
            .map(|key| ctx.get(key).cloned().unwrap_or_default())
            .collect::<Vec<_>>()
            .join("")
    } else {
        String::new()
    };

    let private_key = rsa::RsaPrivateKey::from_pkcs8_pem(&private_key_pem)
        .map_err(|e| anyhow::anyhow!("RSA private key error: {}", e))?;
    let signing_key = SigningKey::<sha2::Sha256>::new_unprefixed(private_key);
    let signature = signing_key.sign(payload.as_bytes());
    let sig_b64 = BASE64_STANDARD.encode(signature.to_bytes());

    ctx.insert("computed.sign".to_string(), sig_b64.clone());
    let mut extra = HashMap::new();
    extra.insert("computed.sign".to_string(), sig_b64);
    Ok(SignResult::ContextOnly(extra))
}
