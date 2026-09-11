//! Local provider catalog, catalog installation, and portable integration packs.
//!
//! The catalog is deliberately a local cache/bundled document.  It is never
//! fetched from the website control plane.  Public catalog entries contain
//! templates only; credentials are accepted only from the local credential
//! stores and are never part of a catalog entry.

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use anyhow::{anyhow, Context};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use hkdf::Hkdf;
use rand::RngCore;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use crate::bindings::{
    load_proxy_profiles, load_vendor_keys, load_vendor_oauth, numeric_scope_keys,
    pack_has_cross_site_numeric_risk, sanitize_rule_bindings_for_public_pack, save_proxy_profiles,
    save_vendor_keys, save_vendor_oauth, strip_numeric_maps_on_import,
};
use crate::logging::unix_ts;
use crate::types::*;

use super::components::{
    load_local_components_runtime_doc, local_component_capability_id,
    save_component_bindings_runtime_doc, save_local_components_runtime_doc,
    save_rule_component_bindings_runtime_doc, save_task_type_component_bindings_runtime_doc,
};
use super::errors::{
    write_conflict_response, write_error_response, write_error_response_with_status,
};
use super::http::{parse_query_string, write_http_response};
use super::{proxy_profiles_path, vendor_keys_path, vendor_oauth_path};

const PROVIDER_CATALOG_SCHEMA: &str = "wptsall-provider-catalog-manifest.v1";
const INTEGRATION_PACK_SCHEMA: &str = "wptsall-integration-pack.v1";
const PROVIDER_CATALOG_SIGNATURE_SCOPE: &str = "provider-catalog-entries-json";
const PRIVATE_BACKUP_SCHEMA: &str = "wptsall-private-backup.v1";
const PRIVATE_BACKUP_KDF_INFO: &[u8] = b"wptsall-integration-pack-private-v1";

fn validate_backup_passphrase(passphrase: Option<&str>) -> anyhow::Result<&str> {
    let passphrase = passphrase
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("private backup passphrase is required"))?;
    if passphrase.chars().count() < 8 {
        anyhow::bail!("private backup passphrase must contain at least 8 characters");
    }
    if passphrase.len() > 4096 {
        anyhow::bail!("private backup passphrase is too long");
    }
    Ok(passphrase)
}

fn encrypt_private_backup(plaintext: &[u8], passphrase: &str) -> anyhow::Result<Vec<u8>> {
    let mut salt = [0u8; 16];
    let mut nonce = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut salt);
    rand::thread_rng().fill_bytes(&mut nonce);

    let hkdf = Hkdf::<Sha256>::new(Some(&salt), passphrase.as_bytes());
    let mut key = [0u8; 32];
    hkdf.expand(PRIVATE_BACKUP_KDF_INFO, &mut key)
        .map_err(|_| anyhow!("derive private backup key failed"))?;
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|_| anyhow!("initialize private backup cipher failed"))?;
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext)
        .map_err(|_| anyhow!("encrypt private backup failed"))?;

    serde_json::to_vec(&json!({
        "schema": PRIVATE_BACKUP_SCHEMA,
        "algorithm": "AES-256-GCM",
        "kdf": "HKDF-SHA256",
        "kdf_info": String::from_utf8_lossy(PRIVATE_BACKUP_KDF_INFO),
        "salt_base64": BASE64_STANDARD.encode(salt),
        "nonce_base64": BASE64_STANDARD.encode(nonce),
        "ciphertext_base64": BASE64_STANDARD.encode(ciphertext),
    }))
    .context("encode private backup envelope failed")
}

fn decrypt_private_backup(data: &[u8], passphrase: Option<&str>) -> anyhow::Result<String> {
    let passphrase = validate_backup_passphrase(passphrase)?;
    let envelope: Value =
        serde_json::from_slice(data).context("parse private backup envelope failed")?;
    if envelope.get("schema").and_then(Value::as_str) != Some(PRIVATE_BACKUP_SCHEMA) {
        anyhow::bail!("unsupported private backup schema");
    }
    if envelope.get("algorithm").and_then(Value::as_str) != Some("AES-256-GCM")
        || envelope.get("kdf").and_then(Value::as_str) != Some("HKDF-SHA256")
    {
        anyhow::bail!("unsupported private backup encryption algorithm");
    }
    let salt = BASE64_STANDARD
        .decode(
            envelope
                .get("salt_base64")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("private backup salt is missing"))?,
        )
        .context("decode private backup salt failed")?;
    let nonce = BASE64_STANDARD
        .decode(
            envelope
                .get("nonce_base64")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("private backup nonce is missing"))?,
        )
        .context("decode private backup nonce failed")?;
    let ciphertext = BASE64_STANDARD
        .decode(
            envelope
                .get("ciphertext_base64")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("private backup ciphertext is missing"))?,
        )
        .context("decode private backup ciphertext failed")?;
    if salt.len() != 16 || nonce.len() != 12 || ciphertext.len() < 16 {
        anyhow::bail!("private backup envelope has invalid lengths");
    }

    let hkdf = Hkdf::<Sha256>::new(Some(&salt), passphrase.as_bytes());
    let mut key = [0u8; 32];
    hkdf.expand(PRIVATE_BACKUP_KDF_INFO, &mut key)
        .map_err(|_| anyhow!("derive private backup key failed"))?;
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|_| anyhow!("initialize private backup cipher failed"))?;
    let plaintext = cipher
        .decrypt(Nonce::from_slice(&nonce), ciphertext.as_ref())
        .map_err(|_| anyhow!("private backup passphrase is incorrect or backup is corrupt"))?;
    String::from_utf8(plaintext).context("decrypted private backup is not valid UTF-8")
}

fn provider_catalog_path() -> String {
    crate::config::provider_catalog_file()
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

/// Shared system prompt for OpenAI-compatible chat-completions templates.
const OPENAI_COMPAT_TRANSLATOR_PROMPT: &str =
    "You are a professional translator. Translate the following text from {{input.source_lang}} to {{input.target_lang}}. Output only the translation.";

/// Build one OpenAI-compatible chat-completions catalog entry. These vendors
/// share the same chat-completions request/response contract and differ only
/// in base URL, default model, and (for Azure) the auth header name.
fn openai_compatible_entry(
    entry_id: &str,
    vendor_id: &str,
    template_id: &str,
    name: &str,
    base_url: &str,
    default_model: &str,
    auth_header: &str,
) -> Value {
    let body = json!({
        "model": default_model,
        "messages": [
            { "role": "system", "content": OPENAI_COMPAT_TRANSLATOR_PROMPT },
            { "role": "user", "content": "{{input.text}}" }
        ],
        "temperature": 0.1
    });
    let mut headers = serde_json::Map::new();
    headers.insert("Content-Type".to_string(), json!("application/json"));
    headers.insert(auth_header.to_string(), json!("Bearer {{auth.api_key}}"));
    let request = json!({
        "method": "POST",
        "url": base_url,
        "headers": headers,
        "body": body,
    });
    json!({
        "id": entry_id,
        "kind": "provider-template-pack",
        "vendor_id": vendor_id,
        "family": "openai_compatible",
        "source": "builtin",
        "templates": [{
            "id": template_id,
            "name": name,
            "version": "1.0.0",
            "type": "text",
            "auth": { "fields": [{ "name": "api_key", "required": true }] },
            "request": request,
            "response": { "translated_text_path": "choices.0.message.content" },
            "constraints": {
                "split_strategy": "paragraph",
                "supported_content_formats": ["plain_text", "rich_html", "json_structured", "serialized_php"]
            },
            "editable_params": [
                { "path": "request.url", "scope": "config", "type": "string", "required": true },
                { "path": "request.body.model", "scope": "config", "type": "string", "required": true }
            ]
        }]
    })
}

/// Build one HTTP-MT catalog entry from a ready-made template JSON object.
/// Used for vendors with bespoke request/response schemas and signatures.
fn http_mt_entry(entry_id: &str, vendor_id: &str, template: Value) -> Value {
    json!({
        "id": entry_id,
        "kind": "provider-template-pack",
        "vendor_id": vendor_id,
        "family": "http_mt",
        "source": "builtin",
        "templates": [template],
    })
}

/// Annotate each builtin template with `evidence_tier` when missing.
/// OpenAI-compatible + mock-api-covered HTTP MT → mock-verified; else schema-only.
fn annotate_builtin_evidence_tiers(entries: &mut [Value]) {
    const HTTP_MT_MOCK_VERIFIED: &[&str] = &[
        "baidu",
        "youdao",
        "tencent",
        "amazon_translate",
        "alibaba",
        "iflytek",
        "niutrans",
        "azure_cognitive",
        "microsoft_translator",
        "kakao",
        "volcengine",
        "hmac_generic",
        "oauth_mt",
        "jwt_mt",
    ];
    for entry in entries.iter_mut() {
        let family = entry
            .get("family")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let vendor = entry
            .get("vendor_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let tier = if family == "openai_compatible" || HTTP_MT_MOCK_VERIFIED.contains(&vendor.as_str())
        {
            "mock-verified"
        } else {
            "schema-only"
        };
        if let Some(templates) = entry.get_mut("templates").and_then(Value::as_array_mut) {
            for template in templates {
                if let Some(obj) = template.as_object_mut() {
                    obj.entry("evidence_tier")
                        .or_insert_with(|| json!(tier));
                }
            }
        }
    }
}

fn builtin_catalog() -> Value {
    // These are templates, not credentials.  Each is intentionally disabled
    // after installation until the user configures a local key.
    //
    // Entries are accumulated in a Vec and wrapped at the end so the `json!`
    // macro never reaches its recursion limit (a single deeply-nested
    // literal for 20+ entries did).
    let mut entries: Vec<Value> = Vec::new();

    // ---- OpenAI-compatible family (shared chat-completions contract) ----
    // Keep entry_id/template_id as "openai-compatible" / "openai-compatible-chat-completions-v1"
    // for back-compat with existing installs and the offline catalog test.
    entries.push(openai_compatible_entry(
        "openai-compatible", "openai", "openai-compatible-chat-completions-v1", "OpenAI Chat Completions",
        "https://api.openai.com/v1/chat/completions", "gpt-4o-mini", "Authorization",
    ));
    entries.push(json!({
        "id": "azure-openai",
        "kind": "provider-template-pack",
        "vendor_id": "azure_openai",
        "family": "openai_compatible",
        "source": "builtin",
        "templates": [{
            "id": "azure-openai-chat-completions-v1",
            "name": "Azure OpenAI Chat Completions",
            "version": "1.0.0",
            "type": "text",
            "auth": { "fields": [{ "name": "api_key", "required": true }] },
            "request": {
                "method": "POST",
                "url": "https://YOUR_RESOURCE.openai.azure.com/openai/deployments/YOUR_DEPLOYMENT/chat/completions?api-version=2024-06-01",
                "headers": {
                    "Content-Type": "application/json",
                    "api-key": "{{auth.api_key}}"
                },
                "body": {
                    "messages": [
                        { "role": "system", "content": OPENAI_COMPAT_TRANSLATOR_PROMPT },
                        { "role": "user", "content": "{{input.text}}" }
                    ],
                    "temperature": 0.1
                }
            },
            "response": { "translated_text_path": "choices.0.message.content" },
            "constraints": {
                "split_strategy": "paragraph",
                "supported_content_formats": ["plain_text", "rich_html", "json_structured", "serialized_php"]
            },
            "editable_params": [
                { "path": "request.url", "scope": "config", "type": "string", "required": true }
            ]
        }]
    }));
    entries.push(openai_compatible_entry(
        "deepseek", "deepseek", "deepseek-chat-completions-v1", "DeepSeek Chat Completions",
        "https://api.deepseek.com/v1/chat/completions", "deepseek-chat", "Authorization",
    ));
    entries.push(openai_compatible_entry(
        "groq", "groq", "groq-chat-completions-v1", "Groq Chat Completions",
        "https://api.groq.com/openai/v1/chat/completions", "llama-3.3-70b-versatile", "Authorization",
    ));
    entries.push(openai_compatible_entry(
        "together", "together", "together-chat-completions-v1", "Together AI Chat Completions",
        "https://api.together.xyz/v1/chat/completions", "meta-llama/Llama-3.3-70B-Instruct-Turbo", "Authorization",
    ));
    entries.push(openai_compatible_entry(
        "mistral", "mistral", "mistral-chat-completions-v1", "Mistral AI Chat Completions",
        "https://api.mistral.ai/v1/chat/completions", "mistral-large-latest", "Authorization",
    ));
    entries.push(openai_compatible_entry(
        "perplexity", "perplexity", "perplexity-chat-completions-v1", "Perplexity Chat Completions",
        "https://api.perplexity.ai/chat/completions", "sonar-pro", "Authorization",
    ));
    entries.push(openai_compatible_entry(
        "moonshot", "moonshot", "moonshot-chat-completions-v1", "Moonshot AI (Kimi) Chat Completions",
        "https://api.moonshot.cn/v1/chat/completions", "moonshot-v1-8k", "Authorization",
    ));
    entries.push(openai_compatible_entry(
        "qwen-dashscope", "qwen", "qwen-dashscope-chat-completions-v1", "Qwen (DashScope OpenAI-compatible) Chat Completions",
        "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions", "qwen-max", "Authorization",
    ));
    entries.push(openai_compatible_entry(
        "openrouter", "openrouter", "openrouter-chat-completions-v1", "OpenRouter Chat Completions",
        "https://openrouter.ai/api/v1/chat/completions", "openai/gpt-4o-mini", "Authorization",
    ));

    // ---- HTTP MT family (bespoke schemas + signatures) ----
    entries.push(http_mt_entry("google-translate", "google_translate", json!({
        "id": "google-translate-v2",
        "name": "Google Cloud Translation (Basic)",
        "version": "1.0.0",
        "type": "text",
        "auth": { "fields": [{ "name": "api_key", "required": true }] },
        "request": {
            "method": "POST",
            "url": "https://translation.googleapis.com/language/translate/v2",
            "headers": { "Content-Type": "application/json" },
            "body": {
                "q": "{{input.text}}",
                "source": "{{input.source_lang}}",
                "target": "{{input.target_lang}}",
                "key": "{{auth.api_key}}"
            }
        },
        "response": { "translated_text_path": "data.translations.0.translatedText" },
        "constraints": { "split_strategy": "paragraph", "supported_content_formats": ["plain_text", "rich_html"] },
        "editable_params": [
            { "path": "request.url", "scope": "config", "type": "string", "required": true },
            { "path": "response.translated_text_path", "scope": "config", "type": "string", "required": false }
        ]
    })));
    entries.push(http_mt_entry("deepl", "deepl", json!({
        "id": "deepl-v2",
        "name": "DeepL API v2",
        "version": "1.0.0",
        "type": "text",
        "auth": { "fields": [{ "name": "api_key", "required": true }] },
        "request": {
            "method": "POST",
            "url": "https://api-free.deepl.com/v2/translate",
            "headers": { "Authorization": "DeepL-Auth-Key {{auth.api_key}}" },
            "body": {
                "text": ["{{input.text}}"],
                "source_lang": "{{input.source_lang}}",
                "target_lang": "{{input.target_lang}}"
            }
        },
        "response": { "translated_text_path": "translations.0.text" },
        "constraints": { "split_strategy": "paragraph", "supported_content_formats": ["plain_text", "rich_html"] },
        "editable_params": [
            { "path": "request.url", "scope": "config", "type": "string", "required": true },
            { "path": "response.translated_text_path", "scope": "config", "type": "string", "required": false }
        ]
    })));
    entries.push(http_mt_entry("microsoft-translator", "microsoft_translator", json!({
        "id": "microsoft-translator-v3",
        "name": "Microsoft Translator v3",
        "version": "1.0.0",
        "type": "text",
        "auth": { "fields": [{ "name": "api_key", "required": true }] },
        "request": {
            "method": "POST",
            "url": "https://api.cognitive.microsofttranslator.com/translate?api-version=3.0&from={{input.source_lang}}&to={{input.target_lang}}",
            "headers": { "Content-Type": "application/json", "Ocp-Apim-Subscription-Key": "{{auth.api_key}}" },
            "body": [{ "text": "{{input.text}}" }]
        },
        "response": { "translated_text_path": "0.translations.0.text" },
        "constraints": { "split_strategy": "paragraph", "supported_content_formats": ["plain_text", "rich_html"] },
        "editable_params": [{ "path": "request.url", "scope": "config", "type": "string", "required": true }]
    })));
    entries.push(http_mt_entry("amazon-translate", "amazon_translate", json!({
        "id": "amazon-translate-v1",
        "name": "Amazon Translate",
        "version": "1.0.0",
        "type": "text",
        // Runner injects Authorization + X-Amz-Date; auth field names match aws_sigv4.
        "auth": { "fields": [
            { "name": "access_key", "required": true },
            { "name": "secret_key", "required": true }
        ] },
        "sign": {
            "algorithm": "aws_sigv4",
            "service": "translate",
            "region": "us-east-1"
        },
        "request": {
            "method": "POST",
            "url": "https://translate.us-east-1.amazonaws.com",
            "headers": {
                "Content-Type": "application/x-amz-json-1.1",
                "X-Amz-Target": "TranslateService.TranslateText"
            },
            "body": {
                "Text": "{{input.text}}",
                "SourceLanguageCode": "{{input.source_lang}}",
                "TargetLanguageCode": "{{input.target_lang}}"
            }
        },
        "response": { "translated_text_path": "TranslatedText" },
        "constraints": { "split_strategy": "paragraph", "supported_content_formats": ["plain_text", "rich_html"] },
        "editable_params": [
            { "path": "request.url", "scope": "config", "type": "string", "required": true },
            { "path": "response.translated_text_path", "scope": "config", "type": "string", "required": true }
        ]
    })));
    entries.push(http_mt_entry("baidu", "baidu", json!({
        "id": "baidu-translate-v1",
        "name": "Baidu Translate (Open API)",
        "version": "1.0.0",
        "type": "text",
        // Match mock-sign-md5 / Baidu Open API: MD5(appid + q + salt + secret).
        // Must declare body_type=form + object body; a raw form string with the
        // default body_type=json is JSON-encoded and breaks MD5 verification.
        "auth": { "fields": [
            { "name": "app_id", "required": true },
            { "name": "api_key", "required": true }
        ] },
        "sign": {
            "algorithm": "md5",
            "salt_type": "random_int",
            "concat": ["auth.app_id", "input.text", "computed.salt", "auth.api_key"]
        },
        "request": {
            "method": "POST",
            "url": "https://fanyi-api.baidu.com/api/trans/vip/translate",
            "headers": { "Content-Type": "application/x-www-form-urlencoded" },
            "body_type": "form",
            "body": {
                "q": "{{input.text}}",
                "from": "{{input.source_lang}}",
                "to": "{{input.target_lang}}",
                "appid": "{{auth.app_id}}",
                "salt": "{{computed.salt}}",
                "sign": "{{computed.sign}}"
            }
        },
        "response": { "translated_text_path": "trans_result.0.dst" },
        "constraints": { "split_strategy": "paragraph", "supported_content_formats": ["plain_text", "rich_html"] },
        "editable_params": [{ "path": "request.url", "scope": "config", "type": "string", "required": true }]
    })));
    entries.push(http_mt_entry("youdao", "youdao", json!({
        "id": "youdao-translate-v1",
        "name": "Youdao Translate",
        "version": "1.0.0",
        "type": "text",
        // Match mock-sign-sha256: SHA256(appKey + input_truncated + salt + curtime + appSecret).
        "auth": { "fields": [
            { "name": "app_key", "required": true },
            { "name": "app_secret", "required": true }
        ] },
        "sign": {
            "algorithm": "sha256",
            "salt_type": "uuid",
            "extra_computed": {
                "curtime": "unix_timestamp",
                "input_truncated": "youdao_truncate"
            },
            "concat": [
                "auth.app_key",
                "computed.input_truncated",
                "computed.salt",
                "computed.curtime",
                "auth.app_secret"
            ]
        },
        "request": {
            "method": "POST",
            "url": "https://openapi.youdao.com/api",
            "headers": { "Content-Type": "application/x-www-form-urlencoded" },
            "body_type": "form",
            "body": {
                "q": "{{input.text}}",
                "from": "{{input.source_lang}}",
                "to": "{{input.target_lang}}",
                "appKey": "{{auth.app_key}}",
                "salt": "{{computed.salt}}",
                "sign": "{{computed.sign}}",
                "signType": "v3",
                "curtime": "{{computed.curtime}}"
            }
        },
        "response": { "translated_text_path": "translation.0" },
        "constraints": { "split_strategy": "paragraph", "supported_content_formats": ["plain_text", "rich_html"] },
        "editable_params": [
            { "path": "request.url", "scope": "config", "type": "string", "required": true },
            { "path": "response.translated_text_path", "scope": "config", "type": "string", "required": true }
        ]
    })));
    entries.push(http_mt_entry("tencent", "tencent", json!({
        "id": "tencent-cloud-translate-v3",
        "name": "Tencent Cloud Translate (TC3)",
        "version": "1.0.0",
        "type": "text",
        // Runner injects Authorization + X-TC-Timestamp; do not use {{sign.*}} placeholders.
        "auth": { "fields": [
            { "name": "secret_id", "required": true },
            { "name": "secret_key", "required": true }
        ] },
        "sign": {
            "algorithm": "tc3_hmac_sha256",
            "service": "tmt"
        },
        "request": {
            "method": "POST",
            "url": "https://tmt.tencentcloudapi.com",
            "headers": {
                "Content-Type": "application/json",
                "X-TC-Action": "TextTranslate",
                "X-TC-Version": "2018-03-21",
                "X-TC-Region": "ap-guangzhou"
            },
            "body": {
                "SourceText": "{{input.text}}",
                "Source": "{{input.source_lang}}",
                "Target": "{{input.target_lang}}"
            }
        },
        "response": { "translated_text_path": "Response.TargetText" },
        "constraints": { "split_strategy": "paragraph", "supported_content_formats": ["plain_text", "rich_html"] },
        "editable_params": [
            { "path": "request.url", "scope": "config", "type": "string", "required": true },
            { "path": "response.translated_text_path", "scope": "config", "type": "string", "required": true }
        ]
    })));
    entries.push(http_mt_entry("alibaba", "alibaba", json!({
        "id": "alibaba-translate-v1",
        "name": "Alibaba Cloud Translate",
        "version": "1.0.0",
        "type": "text",
        // Match mock-sign-alibaba / alibaba_v1: HMAC-SHA1 + Base64 → computed.sign.
        "auth": { "fields": [
            { "name": "access_key", "required": true },
            { "name": "secret_key", "required": true }
        ] },
        "sign": {
            "algorithm": "alibaba_v1",
            "concat": [
                { "param_name": "AccessKeyId", "ctx_key": "auth.access_key" },
                { "param_name": "Action", "value": "TranslateGeneral" },
                { "param_name": "FormatType", "value": "text" },
                { "param_name": "SourceLanguage", "ctx_key": "input.source_lang" },
                { "param_name": "SourceText", "ctx_key": "input.text" },
                { "param_name": "TargetLanguage", "ctx_key": "input.target_lang" }
            ]
        },
        "request": {
            "method": "POST",
            "url": "https://mt.cn-hangzhou.aliyuncs.com/api/translate/web",
            "headers": { "Content-Type": "application/x-www-form-urlencoded" },
            "body_type": "form",
            "body": {
                "Action": "TranslateGeneral",
                "FormatType": "text",
                "SourceLanguage": "{{input.source_lang}}",
                "TargetLanguage": "{{input.target_lang}}",
                "SourceText": "{{input.text}}",
                "AccessKeyId": "{{auth.access_key}}",
                "Signature": "{{computed.sign}}"
            }
        },
        "response": { "translated_text_path": "Data.TranslatedText" },
        "constraints": { "split_strategy": "paragraph", "supported_content_formats": ["plain_text", "rich_html"] },
        "editable_params": [
            { "path": "request.url", "scope": "config", "type": "string", "required": true },
            { "path": "response.translated_text_path", "scope": "config", "type": "string", "required": true }
        ]
    })));
    entries.push(http_mt_entry("iflytek", "iflytek", json!({
        "id": "iflytek-translate-v1",
        "name": "iFlytek Translate",
        "version": "1.0.0",
        "type": "text",
        // Mock /api/iflytek/translate: MD5(api_secret + CurTime + X-Param); empty X-Param OK.
        "auth": { "fields": [
            { "name": "app_id", "required": true },
            { "name": "api_secret", "required": true }
        ] },
        "sign": {
            "algorithm": "md5",
            "salt_type": "unix_timestamp",
            "concat": ["auth.api_secret", "computed.salt"]
        },
        "request": {
            "method": "POST",
            "url": "https://itrans.xf-yun.com/v1/its",
            "headers": {
                "Content-Type": "application/json",
                "X-Appid": "{{auth.app_id}}",
                "X-CurTime": "{{computed.salt}}",
                "X-CheckSum": "{{computed.sign}}"
            },
            "body": {
                "from": "{{input.source_lang}}",
                "to": "{{input.target_lang}}",
                "text": "{{input.text}}"
            }
        },
        "response": { "translated_text_path": "data.trans_result.dst" },
        "constraints": { "split_strategy": "paragraph", "supported_content_formats": ["plain_text", "rich_html"] },
        "editable_params": [
            { "path": "request.url", "scope": "config", "type": "string", "required": true },
            { "path": "response.translated_text_path", "scope": "config", "type": "string", "required": true }
        ]
    })));
    entries.push(http_mt_entry("niutrans", "niutrans", json!({
        "id": "niutrans-translate-v2",
        "name": "NiuTrans Open API v2",
        "version": "1.0.0",
        "type": "text",
        // Mock /api/niutrans/translate: MD5(apikey + q + from + to + apikey).
        "auth": { "fields": [{ "name": "api_key", "required": true }] },
        "sign": {
            "algorithm": "md5",
            "concat": [
                "auth.api_key",
                "input.text",
                "input.source_lang",
                "input.target_lang",
                "auth.api_key"
            ]
        },
        "request": {
            "method": "POST",
            "url": "https://api.niutrans.com/NiuTransServer/translation",
            "headers": { "Content-Type": "application/x-www-form-urlencoded" },
            "body_type": "form",
            "body": {
                "from": "{{input.source_lang}}",
                "to": "{{input.target_lang}}",
                "apikey": "{{auth.api_key}}",
                "q": "{{input.text}}",
                "sign": "{{computed.sign}}"
            }
        },
        "response": { "translated_text_path": "tgt_text" },
        "constraints": { "split_strategy": "paragraph", "supported_content_formats": ["plain_text", "rich_html"] },
        "editable_params": [
            { "path": "request.url", "scope": "config", "type": "string", "required": true },
            { "path": "response.translated_text_path", "scope": "config", "type": "string", "required": true }
        ]
    })));
    entries.push(http_mt_entry("azure-cognitive", "azure_cognitive", json!({
        "id": "azure-cognitive-translate-v3",
        "name": "Azure Cognitive Services Translate",
        "version": "1.0.0",
        "type": "text",
        // Runner injects Ocp-Apim-Subscription-Key from auth.subscription_key|api_key.
        "auth": { "fields": [{ "name": "subscription_key", "required": true }] },
        "sign": { "algorithm": "azure_subscription_key" },
        "request": {
            "method": "POST",
            "url": "https://api.cognitive.microsofttranslator.com/translate?api-version=3.0&from={{input.source_lang}}&to={{input.target_lang}}",
            "headers": { "Content-Type": "application/json" },
            "body": [{ "Text": "{{input.text}}" }]
        },
        "response": { "translated_text_path": "0.Translations.0.Text" },
        "constraints": { "split_strategy": "paragraph", "supported_content_formats": ["plain_text", "rich_html"] },
        "editable_params": [
            { "path": "request.url", "scope": "config", "type": "string", "required": true },
            { "path": "response.translated_text_path", "scope": "config", "type": "string", "required": true }
        ]
    })));
    entries.push(http_mt_entry("kakao", "kakao", json!({
        "id": "kakao-translate-v1",
        "name": "Kakao i Translator",
        "version": "1.0.0",
        "type": "text",
        // Runner injects Authorization: KakaoAK … (mock /api/kakao/translate expects JSON).
        "auth": { "fields": [{ "name": "api_key", "required": true }] },
        "sign": { "algorithm": "kakao_api_key" },
        "request": {
            "method": "POST",
            "url": "https://dapi.kakao.com/v2/translation/translate",
            "headers": { "Content-Type": "application/json" },
            "body": {
                "source_lang": "{{input.source_lang}}",
                "target_lang": "{{input.target_lang}}",
                "query": "{{input.text}}"
            }
        },
        "response": { "translated_text_path": "translated_text.0.0" },
        "constraints": { "split_strategy": "paragraph", "supported_content_formats": ["plain_text", "rich_html"] },
        "editable_params": [
            { "path": "request.url", "scope": "config", "type": "string", "required": true },
            { "path": "response.translated_text_path", "scope": "config", "type": "string", "required": true }
        ]
    })));
    entries.push(http_mt_entry("libretranslate", "libretranslate", json!({
        "id": "libretranslate-v1",
        "name": "LibreTranslate (self-hosted)",
        "version": "1.0.0",
        "type": "text",
        "auth": { "fields": [{ "name": "api_key", "required": false }] },
        "request": {
            "method": "POST",
            "url": "http://localhost:5000/translate",
            "headers": { "Content-Type": "application/json" },
            "body": {
                "q": "{{input.text}}",
                "source": "{{input.source_lang}}",
                "target": "{{input.target_lang}}",
                "api_key": "{{auth.api_key}}"
            }
        },
        "response": { "translated_text_path": "translatedText" },
        "constraints": { "split_strategy": "paragraph", "supported_content_formats": ["plain_text", "rich_html"] },
        "editable_params": [{ "path": "request.url", "scope": "config", "type": "string", "required": true }]
    })));

    // ---- P1-C stage 2: expand to 50 (20 more OpenAI-compatible + 10 HTTP MT) ----
    entries.push(openai_compatible_entry(
        "cohere", "cohere", "cohere-chat-v1", "Cohere Command Chat",
        "https://api.cohere.ai/v1/chat", "command-r-plus", "Authorization",
    ));
    entries.push(openai_compatible_entry(
        "ai21", "ai21", "ai21-chat-v1", "AI21 Jurassic Chat",
        "https://api.ai21.com/v1/chat/completions", "jamba-instruct", "Authorization",
    ));
    entries.push(openai_compatible_entry(
        "novita", "novita", "novita-chat-v1", "Novita AI Chat",
        "https://api.novita.ai/v3/openai/chat/completions", "gpt-4o-mini", "Authorization",
    ));
    entries.push(openai_compatible_entry(
        "deepinfra", "deepinfra", "deepinfra-chat-v1", "DeepInfra Chat",
        "https://api.deepinfra.com/v1/openai/chat/completions", "meta-llama/Llama-3.3-70B-Instruct", "Authorization",
    ));
    entries.push(openai_compatible_entry(
        "siliconflow", "siliconflow", "siliconflow-chat-v1", "SiliconFlow Chat",
        "https://api.siliconflow.cn/v1/chat/completions", "Qwen/Qwen2.5-72B-Instruct", "Authorization",
    ));
    entries.push(openai_compatible_entry(
        "fireworks", "fireworks", "fireworks-chat-v1", "Fireworks AI Chat",
        "https://api.fireworks.ai/inference/v1/chat/completions", "accounts/fireworks/models/llama-v3p3-70b-instruct", "Authorization",
    ));
    entries.push(openai_compatible_entry(
        "replicate", "replicate", "replicate-chat-v1", "Replicate Chat",
        "https://api.replicate.com/v1/chat/completions", "meta/llama-3.3-70b-instruct", "Authorization",
    ));
    entries.push(openai_compatible_entry(
        "anyscale", "anyscale", "anyscale-chat-v1", "Anyscale Chat",
        "https://api.endpoints.anyscale.com/v1/chat/completions", "meta-llama/Llama-3.3-70B-Instruct", "Authorization",
    ));
    entries.push(openai_compatible_entry(
        "upstage", "upstage", "upstage-chat-v1", "Upstage Solar Chat",
        "https://api.upstage.ai/v1/solar/chat/completions", "solar-pro", "Authorization",
    ));
    entries.push(openai_compatible_entry(
        "nvidia-nim", "nvidia", "nvidia-nim-chat-v1", "NVIDIA NIM Chat",
        "https://integrate.api.nvidia.com/v1/chat/completions", "meta/llama-3.3-70b-instruct", "Authorization",
    ));
    entries.push(openai_compatible_entry(
        "cerebras", "cerebras", "cerebras-chat-v1", "Cerebras Chat",
        "https://api.cerebras.ai/v1/chat/completions", "llama-3.3-70b", "Authorization",
    ));
    entries.push(openai_compatible_entry(
        "sambanova", "sambanova", "sambanova-chat-v1", "SambaNova Chat",
        "https://api.sambanova.ai/v1/chat/completions", "Meta-Llama-3.3-70B-Instruct", "Authorization",
    ));
    entries.push(openai_compatible_entry(
        "lepton", "lepton", "lepton-chat-v1", "Lepton AI Chat",
        "https://api.lepton.ai/v1/chat/completions", "meta-llama/Llama-3.3-70B-Instruct", "Authorization",
    ));
    entries.push(openai_compatible_entry(
        "grok", "grok", "grok-chat-v1", "xAI Grok Chat",
        "https://api.x.ai/v1/chat/completions", "grok-3-mini", "Authorization",
    ));
    entries.push(openai_compatible_entry(
        "minimax", "minimax", "minimax-chat-v1", "MiniMax Chat",
        "https://api.minimax.chat/v1/chat/completions", "MiniMax-Text-01", "Authorization",
    ));
    entries.push(openai_compatible_entry(
        "01-ai", "01_ai", "01-ai-chat-v1", "01.AI Yi Chat",
        "https://api.lingyiwanwu.com/v1/chat/completions", "yi-large", "Authorization",
    ));
    entries.push(openai_compatible_entry(
        "zhipu", "zhipu", "zhipu-chat-v1", "Zhipu GLM Chat",
        "https://open.bigmodel.cn/api/paas/v4/chat/completions", "glm-4", "Authorization",
    ));
    entries.push(openai_compatible_entry(
        "baichuan", "baichuan", "baichuan-chat-v1", "Baichuan Chat",
        "https://api.baichuan-ai.com/v1/chat/completions", "Baichuan4", "Authorization",
    ));
    entries.push(openai_compatible_entry(
        "stepfun", "stepfun", "stepfun-chat-v1", "StepFun Chat",
        "https://api.stepfun.com/v1/chat/completions", "step-2-16k", "Authorization",
    ));

    // ---- 10 more HTTP MT providers ----
    entries.push(http_mt_entry("yandex", "yandex", json!({
        "id": "yandex-translate-v2",
        "name": "Yandex Translate",
        "version": "1.0.0",
        "type": "text",
        "auth": { "fields": [{ "name": "api_key", "required": true }] },
        "request": {
            "method": "POST",
            "url": "https://translate.yandex.net/api/v1.5/tr.json/translate",
            "headers": { "Content-Type": "application/json" },
            "body": {
                "text": "{{input.text}}",
                "lang": "{{input.source_lang}}-{{input.target_lang}}",
                "key": "{{auth.api_key}}"
            }
        },
        "response": { "translated_text_path": "text.0" },
        "constraints": { "split_strategy": "paragraph", "supported_content_formats": ["plain_text", "rich_html"] },
        "editable_params": [{ "path": "request.url", "scope": "config", "type": "string", "required": true }]
    })));
    entries.push(http_mt_entry("ibm-watson", "ibm_watson", json!({
        "id": "ibm-watson-translate-v3",
        "name": "IBM Watson Language Translator v3",
        "version": "1.0.0",
        "type": "text",
        "auth": { "fields": [{ "name": "api_key", "required": true }, { "name": "instance_id", "required": true }] },
        "request": {
            "method": "POST",
            "url": "https://api.us-south.language-translator.watson.cloud.ibm.com/instances/{{auth.instance_id}}/v3/translate?version=2018-05-01",
            "headers": {
                "Content-Type": "application/json",
                "Authorization": "Bearer {{auth.api_key}}"
            },
            "body": { "text": ["{{input.text}}"], "source": "{{input.source_lang}}", "target": "{{input.target_lang}}" }
        },
        "response": { "translated_text_path": "translations.0.translation" },
        "constraints": { "split_strategy": "paragraph", "supported_content_formats": ["plain_text", "rich_html"] },
        "editable_params": [{ "path": "request.url", "scope": "config", "type": "string", "required": true }]
    })));
    entries.push(http_mt_entry("papago", "papago", json!({
        "id": "papago-translate-v1",
        "name": "Naver Papago Translate",
        "version": "1.0.0",
        "type": "text",
        "auth": { "fields": [{ "name": "client_id", "required": true }, { "name": "client_secret", "required": true }] },
        "request": {
            "method": "POST",
            "url": "https://openapi.naver.com/v1/papago/n2mt",
            "headers": { "Content-Type": "application/x-www-form-urlencoded", "X-Naver-Client-Id": "{{auth.client_id}}", "X-Naver-Client-Secret": "{{auth.client_secret}}" },
            "body": "source={{input.source_lang}}&target={{input.target_lang}}&text={{input.text}}"
        },
        "response": { "translated_text_path": "message.result.translatedText" },
        "constraints": { "split_strategy": "paragraph", "supported_content_formats": ["plain_text", "rich_html"] },
        "editable_params": [{ "path": "request.url", "scope": "config", "type": "string", "required": true }]
    })));
    entries.push(http_mt_entry("systran", "systran", json!({
        "id": "systran-translate-v1",
        "name": "Systran Translate",
        "version": "1.0.0",
        "type": "text",
        "auth": { "fields": [{ "name": "api_key", "required": true }] },
        "request": {
            "method": "POST",
            "url": "https://api-translate.systran.net/translation/text/translate",
            "headers": { "Authorization": "Bearer {{auth.api_key}}" },
            "body": { "input": ["{{input.text}}"], "source": "{{input.source_lang}}", "target": "{{input.target_lang}}" }
        },
        "response": { "translated_text_path": "outputs.0.output" },
        "constraints": { "split_strategy": "paragraph", "supported_content_formats": ["plain_text", "rich_html"] },
        "editable_params": [{ "path": "request.url", "scope": "config", "type": "string", "required": true }]
    })));
    entries.push(http_mt_entry("smartcat", "smartcat", json!({
        "id": "smartcat-translate-v1",
        "name": "Smartcat Translate",
        "version": "1.0.0",
        "type": "text",
        "auth": { "fields": [{ "name": "api_key", "required": true }] },
        "request": {
            "method": "POST",
            "url": "https://api.smartcat.com/api/v1/document/translate",
            "headers": { "Authorization": "Bearer {{auth.api_key}}", "Content-Type": "application/json" },
            "body": { "text": "{{input.text}}", "sourceLanguage": "{{input.source_lang}}", "targetLanguage": "{{input.target_lang}}" }
        },
        "response": { "translated_text_path": "translatedText" },
        "constraints": { "split_strategy": "paragraph", "supported_content_formats": ["plain_text", "rich_html"] },
        "editable_params": [{ "path": "request.url", "scope": "config", "type": "string", "required": true }]
    })));
    entries.push(http_mt_entry("phrase", "phrase", json!({
        "id": "phrase-translate-v1",
        "name": "Phrase MT",
        "version": "1.0.0",
        "type": "text",
        "auth": { "fields": [{ "name": "api_key", "required": true }] },
        "request": {
            "method": "POST",
            "url": "https://api.phrase.com/v2/projects/mt/translate",
            "headers": { "Authorization": "token {{auth.api_key}}", "Content-Type": "application/json" },
            "body": { "source": "{{input.source_lang}}", "target": "{{input.target_lang}}", "q": "{{input.text}}" }
        },
        "response": { "translated_text_path": "translation" },
        "constraints": { "split_strategy": "paragraph", "supported_content_formats": ["plain_text", "rich_html"] },
        "editable_params": [{ "path": "request.url", "scope": "config", "type": "string", "required": true }]
    })));
    entries.push(http_mt_entry("mymemory", "mymemory", json!({
        "id": "mymemory-translate-v1",
        "name": "MyMemory Translate",
        "version": "1.0.0",
        "type": "text",
        "auth": { "fields": [{ "name": "api_key", "required": false }] },
        "request": {
            "method": "GET",
            "url": "https://api.mymemory.translated.net/get?q={{input.text}}&langpair={{input.source_lang}}|{{input.target_lang}}",
            "headers": {}
        },
        "response": { "translated_text_path": "responseData.translatedText" },
        "constraints": { "split_strategy": "paragraph", "supported_content_formats": ["plain_text"] },
        "editable_params": [{ "path": "request.url", "scope": "config", "type": "string", "required": true }]
    })));
    entries.push(http_mt_entry("reverso", "reverso", json!({
        "id": "reverso-translate-v1",
        "name": "Reverso Translate",
        "version": "1.0.0",
        "type": "text",
        "auth": { "fields": [{ "name": "api_key", "required": true }] },
        "request": {
            "method": "POST",
            "url": "https://api.reverso.net/translate/v1/translation",
            "headers": { "Content-Type": "application/json", "Api-Key": "{{auth.api_key}}" },
            "body": { "from": "{{input.source_lang}}", "to": "{{input.target_lang}}", "input": ["{{input.text}}"] }
        },
        "response": { "translated_text_path": "translation.0" },
        "constraints": { "split_strategy": "paragraph", "supported_content_formats": ["plain_text", "rich_html"] },
        "editable_params": [{ "path": "request.url", "scope": "config", "type": "string", "required": true }]
    })));
    entries.push(http_mt_entry("linguee", "linguee", json!({
        "id": "linguee-translate-v1",
        "name": "Linguee Translate",
        "version": "1.0.0",
        "type": "text",
        "auth": { "fields": [{ "name": "api_key", "required": true }] },
        "request": {
            "method": "GET",
            "url": "https://api.linguee.net/api/v2/translations?query={{input.text}}&src={{input.source_lang}}&dst={{input.target_lang}}",
            "headers": { "Authorization": "Bearer {{auth.api_key}}" }
        },
        "response": { "translated_text_path": "0.0" },
        "constraints": { "split_strategy": "paragraph", "supported_content_formats": ["plain_text"] },
        "editable_params": [{ "path": "request.url", "scope": "config", "type": "string", "required": true }]
    })));

    // ---- P1-C-3: expand to 100+ (OpenAI-compatible gateways + HTTP MT + mock sign families) ----
    for (id, vendor, tid, name, url, model) in [
        ("ollama", "ollama", "ollama-chat-v1", "Ollama (local OpenAI-compatible)", "http://127.0.0.1:11434/v1/chat/completions", "llama3.2"),
        ("lmstudio", "lmstudio", "lmstudio-chat-v1", "LM Studio Chat", "http://127.0.0.1:1234/v1/chat/completions", "local-model"),
        ("vllm", "vllm", "vllm-chat-v1", "vLLM OpenAI-compatible", "http://127.0.0.1:8000/v1/chat/completions", "meta-llama/Llama-3.3-70B-Instruct"),
        ("litellm", "litellm", "litellm-chat-v1", "LiteLLM Proxy", "http://127.0.0.1:4000/v1/chat/completions", "gpt-4o-mini"),
        ("huggingface", "huggingface", "huggingface-chat-v1", "Hugging Face Inference (OpenAI-compatible)", "https://router.huggingface.co/v1/chat/completions", "meta-llama/Llama-3.3-70B-Instruct"),
        ("cloudflare-ai", "cloudflare", "cloudflare-ai-chat-v1", "Cloudflare Workers AI Chat", "https://api.cloudflare.com/client/v4/accounts/YOUR_ACCOUNT/ai/v1/chat/completions", "@cf/meta/llama-3.3-70b-instruct"),
        ("google-gemini-openai", "google_gemini", "google-gemini-openai-v1", "Google Gemini (OpenAI-compatible)", "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions", "gemini-2.0-flash"),
        ("anthropic-openai-compat", "anthropic", "anthropic-openai-compat-v1", "Anthropic via OpenAI-compatible gateway", "https://api.anthropic.com/v1/chat/completions", "claude-sonnet-4-20250514"),
        ("aws-bedrock-openai", "aws_bedrock", "aws-bedrock-openai-v1", "Amazon Bedrock (OpenAI-compatible proxy)", "https://bedrock-runtime.us-east-1.amazonaws.com/openai/v1/chat/completions", "anthropic.claude-3-5-sonnet"),
        ("github-models", "github", "github-models-chat-v1", "GitHub Models Chat", "https://models.inference.ai.azure.com/chat/completions", "gpt-4o-mini"),
        ("azure-ai-inference", "azure_ai", "azure-ai-inference-v1", "Azure AI Inference Chat", "https://YOUR_RESOURCE.services.ai.azure.com/models/chat/completions?api-version=2024-05-01-preview", "gpt-4o-mini"),
        ("volcengine-ark", "volcengine_ark", "volcengine-ark-chat-v1", "Volcengine Ark (Doubao OpenAI-compatible)", "https://ark.cn-beijing.volces.com/api/v3/chat/completions", "doubao-pro-32k"),
        ("baidu-qianfan", "baidu_qianfan", "baidu-qianfan-chat-v1", "Baidu Qianfan (OpenAI-compatible)", "https://qianfan.baidubce.com/v2/chat/completions", "ernie-4.0-8k"),
        ("tencent-hunyuan", "tencent_hunyuan", "tencent-hunyuan-chat-v1", "Tencent Hunyuan (OpenAI-compatible)", "https://hunyuan.tencentcloudapi.com/openai/v1/chat/completions", "hunyuan-turbos"),
        ("xunfei-spark", "xunfei", "xunfei-spark-chat-v1", "iFlytek Spark (OpenAI-compatible)", "https://spark-api-open.xf-yun.com/v1/chat/completions", "generalv3.5"),
        ("modelscope", "modelscope", "modelscope-chat-v1", "ModelScope Chat", "https://api-inference.modelscope.cn/v1/chat/completions", "Qwen/Qwen2.5-72B-Instruct"),
        ("ppio", "ppio", "ppio-chat-v1", "PPIO Chat", "https://api.ppinfra.com/v3/openai/chat/completions", "meta-llama/Llama-3.3-70B-Instruct"),
        ("friendli", "friendli", "friendli-chat-v1", "FriendliAI Chat", "https://api.friendli.ai/serverless/v1/chat/completions", "meta-llama-3.3-70b-instruct"),
        ("hyperbolic", "hyperbolic", "hyperbolic-chat-v1", "Hyperbolic Chat", "https://api.hyperbolic.xyz/v1/chat/completions", "meta-llama/Llama-3.3-70B-Instruct"),
        ("lambdalabs", "lambdalabs", "lambdalabs-chat-v1", "Lambda Labs Chat", "https://api.lambdalabs.com/v1/chat/completions", "llama3.3-70b-instruct-fp8"),
        ("fal-ai", "fal", "fal-ai-chat-v1", "fal.ai Chat", "https://fal.run/fal-ai/any-llm", "meta-llama/llama-3.3-70b-instruct"),
        ("baseten", "baseten", "baseten-chat-v1", "Baseten Chat", "https://bridge.baseten.co/v1/chat/completions", "meta-llama/Llama-3.3-70B-Instruct"),
        ("modal-labs", "modal", "modal-labs-chat-v1", "Modal Labs Chat", "https://YOUR_WORKSPACE--vllm-openai.modal.run/v1/chat/completions", "meta-llama/Llama-3.3-70B-Instruct"),
        ("predibase", "predibase", "predibase-chat-v1", "Predibase Chat", "https://serving.app.predibase.com/v1/chat/completions", "llama-3.3-70b"),
        ("portkey", "portkey", "portkey-chat-v1", "Portkey AI Gateway", "https://api.portkey.ai/v1/chat/completions", "gpt-4o-mini"),
        ("helicone", "helicone", "helicone-chat-v1", "Helicone AI Gateway", "https://gateway.helicone.ai/v1/chat/completions", "gpt-4o-mini"),
        ("openai-compatible-local", "openai_local", "openai-compatible-local-v1", "Generic local OpenAI-compatible endpoint", "http://127.0.0.1:8080/v1/chat/completions", "local-model"),
        ("qwen-intl", "qwen_intl", "qwen-intl-chat-v1", "Qwen International (DashScope)", "https://dashscope-intl.aliyuncs.com/compatible-mode/v1/chat/completions", "qwen-max"),
        ("infini-ai", "infini", "infini-ai-chat-v1", "Infini-AI Chat", "https://cloud.infini-ai.com/maas/v1/chat/completions", "llama-3.3-70b-instruct"),
        ("skywork", "skywork", "skywork-chat-v1", "Skywork Chat", "https://api.skywork.ai/v1/chat/completions", "skywork-13b"),
        ("360-zhinao", "qihoo360", "360-zhinao-chat-v1", "360 Zhinao Chat", "https://api.360.cn/v1/chat/completions", "360gpt-pro"),
        ("sensechat", "sensechat", "sensechat-chat-v1", "SenseChat", "https://api.sensenova.cn/compatible-mode/v1/chat/completions", "SenseChat-5"),
        ("doubao", "doubao", "doubao-chat-v1", "Doubao Chat (OpenAI-compatible)", "https://ark.cn-beijing.volces.com/api/v3/chat/completions", "doubao-1.5-pro-32k"),
        ("ernie-speed", "ernie", "ernie-speed-chat-v1", "ERNIE Speed (OpenAI-compatible)", "https://qianfan.baidubce.com/v2/chat/completions", "ernie-speed-8k"),
        ("watsonx-openai", "ibm_watsonx", "watsonx-openai-v1", "IBM watsonx.ai (OpenAI-compatible)", "https://us-south.ml.cloud.ibm.com/ml/v1/text/chat?version=2024-03-14", "ibm/granite-3-8b-instruct"),
        ("oracle-genai", "oracle", "oracle-genai-chat-v1", "Oracle Generative AI Chat", "https://inference.generativeai.us-chicago-1.oci.oraclecloud.com/20231130/actions/chat", "cohere.command-r-plus"),
        ("deepseek-reasoner", "deepseek_reasoner", "deepseek-reasoner-v1", "DeepSeek Reasoner", "https://api.deepseek.com/v1/chat/completions", "deepseek-reasoner"),
        ("chatgpt-enterprise", "openai_enterprise", "chatgpt-enterprise-v1", "ChatGPT Enterprise / API", "https://api.openai.com/v1/chat/completions", "gpt-4.1-mini"),
        ("groq-compound", "groq_compound", "groq-compound-v1", "Groq Compound Systems", "https://api.groq.com/openai/v1/chat/completions", "compound-beta"),
        ("mistral-codestral", "mistral_codestral", "mistral-codestral-v1", "Mistral Codestral", "https://api.mistral.ai/v1/chat/completions", "codestral-latest"),
    ] {
        entries.push(openai_compatible_entry(id, vendor, tid, name, url, model, "Authorization"));
    }

    // Mock-covered sign families previously missing from builtin catalog.
    entries.push(http_mt_entry("volcengine", "volcengine", json!({
        "id": "volcengine-translate-v1",
        "name": "Volcengine Translate (SigV4 variant)",
        "version": "1.0.0",
        "type": "text",
        // Runner injects Authorization + X-Amz-Date.
        "auth": { "fields": [
            { "name": "access_key", "required": true },
            { "name": "secret_key", "required": true }
        ] },
        "sign": {
            "algorithm": "volcengine_hmac_sha256",
            "service": "translate",
            "region": "cn-north-1"
        },
        "request": {
            "method": "POST",
            "url": "https://translate.volcengineapi.com",
            "headers": { "Content-Type": "application/json" },
            "body": {
                "SourceText": "{{input.text}}",
                "SourceLanguage": "{{input.source_lang}}",
                "TargetLanguage": "{{input.target_lang}}"
            }
        },
        "response": { "translated_text_path": "Translation" },
        "constraints": { "split_strategy": "paragraph", "supported_content_formats": ["plain_text", "rich_html"] },
        "editable_params": [
            { "path": "request.url", "scope": "config", "type": "string", "required": true },
            { "path": "response.translated_text_path", "scope": "config", "type": "string", "required": true }
        ]
    })));
    entries.push(http_mt_entry("hmac-generic", "hmac_generic", json!({
        "id": "hmac-generic-translate-v1",
        "name": "Generic HMAC-SHA256 Translate",
        "version": "1.0.0",
        "type": "text",
        // Match mock-sign-hmac-sha256: HMAC(api_key + text + salt, secret_key).
        "auth": { "fields": [
            { "name": "api_key", "required": true },
            { "name": "secret_key", "required": true }
        ] },
        "sign": {
            "algorithm": "hmac_sha256",
            "salt_type": "unix_timestamp",
            "concat": ["auth.api_key", "input.text", "computed.salt"]
        },
        "request": {
            "method": "POST",
            "url": "https://example.com/api/hmac/translate",
            "headers": { "Content-Type": "application/json" },
            "body": {
                "api_key": "{{auth.api_key}}",
                "text": "{{input.text}}",
                "source_lang": "{{input.source_lang}}",
                "target_lang": "{{input.target_lang}}",
                "timestamp": "{{computed.salt}}",
                "sign": "{{computed.sign}}"
            }
        },
        "response": { "translated_text_path": "translated_text" },
        "constraints": { "split_strategy": "paragraph", "supported_content_formats": ["plain_text", "rich_html"] },
        "editable_params": [
            { "path": "request.url", "scope": "config", "type": "string", "required": true },
            { "path": "response.translated_text_path", "scope": "config", "type": "string", "required": true }
        ]
    })));
    entries.push(http_mt_entry("oauth-mt", "oauth_mt", json!({
        "id": "oauth-mt-translate-v1",
        "name": "OAuth Client-Credentials Translate",
        "version": "1.0.0",
        "type": "text",
        "auth": { "fields": [
            { "name": "client_id", "required": true },
            { "name": "client_secret", "required": true },
            { "name": "access_token", "required": true }
        ] },
        "request": {
            "method": "POST",
            "url": "https://example.com/api/oauth/translate",
            "headers": { "Content-Type": "application/json", "Authorization": "Bearer {{auth.access_token}}" },
            "body": {
                "text": "{{input.text}}",
                "source_lang": "{{input.source_lang}}",
                "target_lang": "{{input.target_lang}}"
            }
        },
        "response": { "translated_text_path": "translated_text" },
        "constraints": { "split_strategy": "paragraph", "supported_content_formats": ["plain_text", "rich_html"] },
        "editable_params": [{ "path": "request.url", "scope": "config", "type": "string", "required": true }]
    })));
    entries.push(http_mt_entry("jwt-mt", "jwt_mt", json!({
        "id": "jwt-mt-translate-v1",
        "name": "JWT Bearer (RSA) Translate",
        "version": "1.0.0",
        "type": "text",
        "auth": { "fields": [{ "name": "jwt_token", "required": true }] },
        "request": {
            "method": "POST",
            "url": "https://example.com/api/jwt/translate",
            "headers": { "Content-Type": "application/json", "Authorization": "Bearer {{auth.jwt_token}}" },
            "body": {
                "text": "{{input.text}}",
                "source_lang": "{{input.source_lang}}",
                "target_lang": "{{input.target_lang}}"
            }
        },
        "response": { "translated_text_path": "translated_text" },
        "constraints": { "split_strategy": "paragraph", "supported_content_formats": ["plain_text", "rich_html"] },
        "editable_params": [{ "path": "request.url", "scope": "config", "type": "string", "required": true }]
    })));
    entries.push(http_mt_entry("custom-http-mt", "custom_http_mt", json!({
        "id": "custom-http-mt-v1",
        "name": "Custom HTTP MT (user-editable)",
        "version": "1.0.0",
        "type": "text",
        "auth": { "fields": [{ "name": "api_key", "required": false }] },
        "request": {
            "method": "POST",
            "url": "https://example.com/translate",
            "headers": { "Content-Type": "application/json", "Authorization": "Bearer {{auth.api_key}}" },
            "body": {
                "q": "{{input.text}}",
                "source": "{{input.source_lang}}",
                "target": "{{input.target_lang}}"
            }
        },
        "response": { "translated_text_path": "translatedText" },
        "constraints": { "split_strategy": "paragraph", "supported_content_formats": ["plain_text", "rich_html"] },
        "editable_params": [
            { "path": "request.method", "scope": "config", "type": "string", "required": true },
            { "path": "request.url", "scope": "config", "type": "string", "required": true },
            { "path": "request.headers.Authorization", "scope": "config", "type": "string", "required": false },
            { "path": "request.headers.Content-Type", "scope": "config", "type": "string", "required": false },
            { "path": "request.body.q", "scope": "config", "type": "string", "required": false },
            { "path": "request.body.source", "scope": "config", "type": "string", "required": false },
            { "path": "request.body.target", "scope": "config", "type": "string", "required": false },
            { "path": "response.translated_text_path", "scope": "config", "type": "string", "required": true },
            { "path": "response.error_path", "scope": "config", "type": "string", "required": false }
        ]
    })));

    for (id, vendor, tid, name, url, path) in [
        ("deepl-pro", "deepl_pro", "deepl-pro-v2", "DeepL API Pro", "https://api.deepl.com/v2/translate", "translations.0.text"),
        ("google-advanced", "google_advanced", "google-advanced-v3", "Google Cloud Translation Advanced", "https://translation.googleapis.com/v3/projects/YOUR_PROJECT/locations/global:translateText", "translations.0.translatedText"),
        ("modernmt", "modernmt", "modernmt-v1", "ModernMT", "https://api.modernmt.com/translate", "data.translation"),
        ("argos-translate", "argos", "argos-translate-v1", "Argos Translate (self-hosted)", "http://127.0.0.1:5001/translate", "translatedText"),
        ("apertium", "apertium", "apertium-v1", "Apertium Translate", "https://www.apertium.org/apy/translate", "responseData.translatedText"),
        ("bergamot", "bergamot", "bergamot-v1", "Bergamot / Firefox Translations", "http://127.0.0.1:8081/translate", "translatedText"),
        ("lingvanex", "lingvanex", "lingvanex-v1", "LingvaNex Translate", "https://api-b2b.backenster.com/b1/api/v3/translate", "result"),
        ("cloudtranslation", "cloudtranslation", "cloudtranslation-v1", "CloudTranslation", "https://api.cloudtranslation.com/v1/translate", "translations.0.text"),
        ("iciba", "iciba", "iciba-v1", "iCIBA Translate", "https://ifanyi.iciba.com/index.php", "content.out"),
        ("sogou", "sogou", "sogou-v1", "Sogou Translate", "https://fanyi.sogou.com/api/transweb/translate", "data.translatedText"),
        ("huawei-nlp", "huawei", "huawei-nlp-v1", "Huawei Cloud NLP Translate", "https://nlp-ext.cn-north-4.myhuaweicloud.com/v1/infers/machine-translation/text-translation", "translations.0.text"),
        ("crowdin-mt", "crowdin", "crowdin-mt-v1", "Crowdin MT", "https://api.crowdin.com/api/v2/machines/translations", "data.0.text"),
        ("lokalise-mt", "lokalise", "lokalise-mt-v1", "Lokalise MT", "https://api.lokalise.com/api2/projects/mt/translate", "translation"),
        ("unbabel", "unbabel", "unbabel-v1", "Unbabel MT", "https://api.unbabel.com/v1/translation", "translated_text"),
        ("translated-matecat", "translated", "translated-matecat-v1", "Translated MateCat MT", "https://api.translated.com/v2/translate", "translation"),
        ("microsoft-custom", "microsoft_custom", "microsoft-custom-v3", "Microsoft Custom Translator", "https://api.cognitive.microsofttranslator.com/translate?api-version=3.0&from={{input.source_lang}}&to={{input.target_lang}}&category=generalnn", "0.translations.0.text"),
    ] {
        let auth_header = if id == "deepl-pro" {
            "DeepL-Auth-Key {{auth.api_key}}"
        } else if id == "microsoft-custom" {
            "{{auth.api_key}}"
        } else {
            "Bearer {{auth.api_key}}"
        };
        let (auth_header_name, auth_header_value) = if id == "microsoft-custom" {
            ("Ocp-Apim-Subscription-Key", auth_header)
        } else if id == "deepl-pro" {
            ("Authorization", auth_header)
        } else {
            ("Authorization", auth_header)
        };
        let mut headers = serde_json::Map::new();
        headers.insert("Content-Type".into(), json!("application/json"));
        headers.insert(auth_header_name.into(), json!(auth_header_value));
        let body = if id == "deepl-pro" {
            json!({
                "text": ["{{input.text}}"],
                "source_lang": "{{input.source_lang}}",
                "target_lang": "{{input.target_lang}}"
            })
        } else if id == "microsoft-custom" {
            json!([{ "Text": "{{input.text}}" }])
        } else if id == "apertium" {
            json!({
                "q": "{{input.text}}",
                "langpair": "{{input.source_lang}}|{{input.target_lang}}"
            })
        } else {
            json!({
                "q": "{{input.text}}",
                "source": "{{input.source_lang}}",
                "target": "{{input.target_lang}}",
                "text": "{{input.text}}"
            })
        };
        entries.push(http_mt_entry(id, vendor, json!({
            "id": tid,
            "name": name,
            "version": "1.0.0",
            "type": "text",
            "auth": { "fields": [{ "name": "api_key", "required": true }] },
            "request": {
                "method": "POST",
                "url": url,
                "headers": headers,
                "body": body
            },
            "response": { "translated_text_path": path },
            "constraints": { "split_strategy": "paragraph", "supported_content_formats": ["plain_text", "rich_html"] },
            "editable_params": [{ "path": "request.url", "scope": "config", "type": "string", "required": true }]
        })));
    }

    annotate_builtin_evidence_tiers(&mut entries);

    json!({
        "schema": PROVIDER_CATALOG_SCHEMA,
        "catalog_version": "builtin-3",
        "created_at": "2026-09-03T00:00:00Z",
        "entries": entries,
    })
}

fn load_catalog_document() -> anyhow::Result<(Value, bool)> {
    load_catalog_document_with_meta().map(|(catalog, builtin, _)| (catalog, builtin))
}

#[derive(Debug, Clone)]
struct CatalogLoadMeta {
    source: String,
    offline: bool,
    state: String,
    catalog_version: Option<String>,
}

fn catalog_cache_paths() -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let legacy = std::path::PathBuf::from(provider_catalog_path());
    let parent = legacy
        .parent()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let stem = legacy
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("provider-catalog")
        .to_string();
    (
        legacy,
        parent.join(format!("{stem}.current.json")),
        parent.join(format!("{stem}.lkg.json")),
        parent.join(format!("{stem}.metadata.json")),
    )
}

fn read_validate_catalog_file(path: &Path, builtin: bool) -> anyhow::Result<Value> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("read provider catalog failed: {}", path.display()))?;
    let catalog: Value = serde_json::from_str(&raw)
        .with_context(|| format!("parse provider catalog JSON failed: {}", path.display()))?;
    validate_catalog_document(&catalog, builtin)?;
    Ok(catalog)
}

fn load_refresh_metadata() -> Value {
    let (_, _, _, meta_path) = catalog_cache_paths();
    if !meta_path.exists() {
        return json!({
            "state": "builtin",
            "offline": true,
        });
    }
    fs::read_to_string(&meta_path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| json!({ "state": "unknown", "offline": true }))
}

fn write_refresh_metadata(meta: &Value) -> anyhow::Result<()> {
    let (_, _, _, meta_path) = catalog_cache_paths();
    if let Some(parent) = meta_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create catalog cache dir failed: {}", parent.display()))?;
    }
    let tmp = meta_path.with_extension("metadata.tmp.json");
    fs::write(&tmp, serde_json::to_vec_pretty(meta)?)
        .with_context(|| format!("write catalog metadata temp failed: {}", tmp.display()))?;
    fs::rename(&tmp, &meta_path).with_context(|| {
        format!(
            "replace catalog metadata failed: {} -> {}",
            tmp.display(),
            meta_path.display()
        )
    })?;
    Ok(())
}

fn load_catalog_document_with_meta() -> anyhow::Result<(Value, bool, CatalogLoadMeta)> {
    let (legacy, current, lkg, _) = catalog_cache_paths();

    if current.exists() {
        match read_validate_catalog_file(&current, false) {
            Ok(catalog) => {
                let version = catalog
                    .get("catalog_version")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                return Ok((
                    catalog,
                    false,
                    CatalogLoadMeta {
                        source: "current_cache".into(),
                        offline: true,
                        state: "current".into(),
                        catalog_version: version,
                    },
                ));
            }
            Err(err) => {
                // Fall through to LKG; keep error for metadata if LKG also fails.
                let _ = err;
            }
        }
    }

    if lkg.exists() {
        match read_validate_catalog_file(&lkg, false) {
            Ok(catalog) => {
                let version = catalog
                    .get("catalog_version")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                return Ok((
                    catalog,
                    false,
                    CatalogLoadMeta {
                        source: "last_known_good".into(),
                        offline: true,
                        state: "last_known_good".into(),
                        catalog_version: version,
                    },
                ));
            }
            Err(_) => {}
        }
    }

    if legacy.exists() {
        let catalog = read_validate_catalog_file(&legacy, false)?;
        let version = catalog
            .get("catalog_version")
            .and_then(Value::as_str)
            .map(str::to_string);
        return Ok((
            catalog,
            false,
            CatalogLoadMeta {
                source: "local_file".into(),
                offline: true,
                state: "local_file".into(),
                catalog_version: version,
            },
        ));
    }

    let catalog = builtin_catalog();
    validate_catalog_document(&catalog, true)?;
    let version = catalog
        .get("catalog_version")
        .and_then(Value::as_str)
        .map(str::to_string);
    Ok((
        catalog,
        true,
        CatalogLoadMeta {
            source: "builtin".into(),
            offline: true,
            state: "builtin".into(),
            catalog_version: version,
        },
    ))
}

fn catalog_source_url() -> Option<String> {
    std::env::var("WPTSALL_PROVIDER_CATALOG_SOURCE_URL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn assert_catalog_source_url_allowed(raw: &str) -> anyhow::Result<()> {
    let parsed = url::Url::parse(raw.trim()).map_err(|_| anyhow!("catalog source URL is invalid"))?;
    if parsed.scheme() == "file" {
        if cfg!(test) || env_flag("WPTSALL_PROVIDER_CATALOG_ALLOW_FILE_SOURCE") {
            return Ok(());
        }
        anyhow::bail!("file:// catalog source is not allowed");
    }
    if !matches!(parsed.scheme(), "http" | "https") {
        anyhow::bail!("catalog source URL scheme is not allowed");
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        anyhow::bail!("catalog source URL must not contain embedded credentials");
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow!("catalog source URL host is missing"))?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    let allow_loopback = env_flag("WPTSALL_PROVIDER_CATALOG_ALLOW_LOOPBACK_SOURCE")
        || (cfg!(test) && parsed.port().is_some() && matches!(host.as_str(), "127.0.0.1" | "localhost" | "::1"));
    if !allow_loopback {
        // Reuse provider host policy for public HTTPS sources.
        if let Err(reason) = catalog_url_check(raw) {
            anyhow::bail!("catalog source URL rejected: {reason}");
        }
        if parsed.scheme() != "https" {
            anyhow::bail!("catalog source URL must use https");
        }
    }
    Ok(())
}

/// Atomically install a validated candidate as current cache, preserving prior current as LKG.
fn install_catalog_candidate(catalog: &Value, source_url: &str) -> anyhow::Result<Value> {
    let (_legacy, current, lkg, _) = catalog_cache_paths();
    if let Some(parent) = current.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create catalog cache dir failed: {}", parent.display()))?;
    }
    let encoded = serde_json::to_vec_pretty(catalog)?;
    let candidate = current.with_extension("candidate.tmp.json");
    fs::write(&candidate, &encoded).with_context(|| {
        format!(
            "write catalog candidate failed: {}",
            candidate.display()
        )
    })?;

    if current.exists() {
        fs::copy(&current, &lkg).with_context(|| {
            format!(
                "preserve LKG failed: {} -> {}",
                current.display(),
                lkg.display()
            )
        })?;
    }

    fs::rename(&candidate, &current).with_context(|| {
        format!(
            "atomic catalog replace failed: {} -> {}",
            candidate.display(),
            current.display()
        )
    })?;

    let meta = json!({
        "state": "current",
        "source_url": source_url,
        "catalog_version": catalog.get("catalog_version").cloned().unwrap_or(Value::Null),
        "last_success_at": unix_ts(),
        "last_error": Value::Null,
        "offline": false,
        "template_count": catalog_template_entries(catalog).map(|v| v.len()).unwrap_or(0),
    });
    write_refresh_metadata(&meta)?;
    Ok(meta)
}

async fn download_catalog_bytes(source_url: &str) -> anyhow::Result<Vec<u8>> {
    assert_catalog_source_url_allowed(source_url)?;
    const MAX_BYTES: usize = 2 * 1024 * 1024;

    if source_url.starts_with("file://") {
        let path = source_url.trim_start_matches("file://");
        let bytes = fs::read(path)
            .with_context(|| format!("read file catalog source failed: {path}"))?;
        if bytes.len() > MAX_BYTES {
            anyhow::bail!("catalog source exceeds max size ({MAX_BYTES} bytes)");
        }
        return Ok(bytes);
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 {
                return attempt.error(anyhow!("too many redirects"));
            }
            let next = attempt.url().as_str();
            if let Err(err) = assert_catalog_source_url_allowed(next) {
                return attempt.error(err);
            }
            attempt.follow()
        }))
        .build()
        .context("build catalog download client failed")?;

    let response = client
        .get(source_url)
        .header(reqwest::header::ACCEPT, "application/json, application/octet-stream")
        .send()
        .await
        .context("catalog source download failed")?;
    if !response.status().is_success() {
        anyhow::bail!("catalog source HTTP {}", response.status());
    }
    if let Some(len) = response.content_length() {
        if len as usize > MAX_BYTES {
            anyhow::bail!("catalog source Content-Length exceeds max size");
        }
    }
    let final_url = response.url().clone();
    assert_catalog_source_url_allowed(final_url.as_str())?;
    let bytes = response
        .bytes()
        .await
        .context("catalog source body read failed")?;
    if bytes.len() > MAX_BYTES {
        anyhow::bail!("catalog source body exceeds max size");
    }
    Ok(bytes.to_vec())
}

async fn refresh_provider_catalog_from_source() -> anyhow::Result<Value> {
    let Some(source_url) = catalog_source_url() else {
        let (catalog, builtin, meta) = load_catalog_document_with_meta()?;
        let count = catalog_template_entries(&catalog)?.len();
        return Ok(json!({
            "catalog_version": catalog.get("catalog_version").cloned().unwrap_or(Value::Null),
            "template_count": count,
            "source": meta.source,
            "verified": builtin || catalog.get("signature").is_some(),
            "offline": true,
            "refresh_metadata": {
                "state": meta.state,
                "offline": true,
                "reason": "no_source_configured",
            },
        }));
    };

    match download_catalog_bytes(&source_url).await {
        Ok(bytes) => {
            let catalog: Value = serde_json::from_slice(&bytes)
                .context("parse remote provider catalog JSON failed")?;
            // Remote catalogs are fail-closed: signature always required.
            if let Err(err) = validate_catalog_document_inner(&catalog, false, true) {
                let (_, _, load_meta) = load_catalog_document_with_meta().unwrap_or((
                    builtin_catalog(),
                    true,
                    CatalogLoadMeta {
                        source: "builtin".into(),
                        offline: true,
                        state: "builtin".into(),
                        catalog_version: None,
                    },
                ));
                let meta = json!({
                    "state": if load_meta.state == "current" { "stale_current" } else { load_meta.state.as_str() },
                    "source_url": source_url,
                    "last_error": format!("{err:#}"),
                    "offline": true,
                    "last_attempt_at": unix_ts(),
                });
                let _ = write_refresh_metadata(&meta);
                return Err(err).context("remote provider catalog validation failed");
            }
            let meta = install_catalog_candidate(&catalog, &source_url)?;
            let count = catalog_template_entries(&catalog)?.len();
            Ok(json!({
                "catalog_version": catalog.get("catalog_version").cloned().unwrap_or(Value::Null),
                "template_count": count,
                "source": "remote",
                "verified": true,
                "offline": false,
                "refresh_metadata": meta,
            }))
        }
        Err(err) => {
            let (catalog, builtin, load_meta) = load_catalog_document_with_meta()?;
            let count = catalog_template_entries(&catalog)?.len();
            let fallback_state = if load_meta.state == "current" {
                "stale_current"
            } else {
                load_meta.state.as_str()
            };
            let meta = json!({
                "state": fallback_state,
                "source_url": source_url,
                "catalog_version": catalog.get("catalog_version").cloned().unwrap_or(Value::Null),
                "last_error": format!("{err:#}"),
                "offline": true,
                "last_attempt_at": unix_ts(),
            });
            let _ = write_refresh_metadata(&meta);
            Ok(json!({
                "catalog_version": catalog.get("catalog_version").cloned().unwrap_or(Value::Null),
                "template_count": count,
                "source": load_meta.source,
                "verified": builtin || catalog.get("signature").is_some(),
                "offline": true,
                "refresh_metadata": meta,
                "fallback": true,
            }))
        }
    }
}

fn hex_sha256(value: &[u8]) -> String {
    Sha256::digest(value)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn catalog_signature_key() -> anyhow::Result<Option<String>> {
    let Some(path) = std::env::var("WPTSALL_PROVIDER_CATALOG_PUBLIC_KEY_FILE")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
    else {
        return Ok(None);
    };
    Ok(Some(fs::read_to_string(&path).with_context(|| {
        format!("read provider catalog public key failed: {path}")
    })?))
}

fn validate_catalog_document(catalog: &Value, builtin: bool) -> anyhow::Result<()> {
    let require_signature = !builtin && !env_flag("WPTSALL_ALLOW_UNSIGNED_CATALOG");
    validate_catalog_document_inner(catalog, builtin, require_signature)
}

fn validate_catalog_document_inner(
    catalog: &Value,
    _builtin: bool,
    require_signature: bool,
) -> anyhow::Result<()> {
    if catalog.get("schema").and_then(Value::as_str) != Some(PROVIDER_CATALOG_SCHEMA) {
        anyhow::bail!("unsupported provider catalog schema");
    }
    let entries = catalog
        .get("entries")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("provider catalog entries must be an array"))?;
    if entries.len() > 1000 {
        anyhow::bail!("provider catalog contains too many entries");
    }

    let entries_json = serde_json::to_vec(entries)?;
    if let Some(expected) = catalog.get("entries_sha256").and_then(Value::as_str) {
        if !expected.eq_ignore_ascii_case(&hex_sha256(&entries_json)) {
            anyhow::bail!("provider catalog entries hash mismatch");
        }
    }

    let signature = catalog
        .get("signature")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty());
    if let Some(signature) = signature {
        if catalog
            .get("signature_scope")
            .and_then(Value::as_str)
            .unwrap_or(PROVIDER_CATALOG_SIGNATURE_SCOPE)
            != PROVIDER_CATALOG_SIGNATURE_SCOPE
        {
            anyhow::bail!("unsupported provider catalog signature scope");
        }
        let public_key = catalog_signature_key()?.ok_or_else(|| {
            anyhow!(
                "provider catalog signature is present but WPTSALL_PROVIDER_CATALOG_PUBLIC_KEY_FILE is not configured"
            )
        })?;
        crate::crypto::verify_component_signature(&entries_json, signature, &public_key)
            .context("provider catalog signature verification failed")?;
    } else if require_signature {
        anyhow::bail!(
            "provider catalog signature is required for this catalog source"
        );
    }

    for entry in entries {
        let entry_id = entry
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| anyhow!("provider catalog entry id is required"))?;
        let templates = entry_templates(entry, entry_id)?;
        if templates.is_empty() {
            anyhow::bail!("provider catalog entry '{entry_id}' has no templates");
        }
        for template in templates {
            validate_public_catalog_template(template, entry_id)?;
        }
    }
    Ok(())
}

fn entry_templates<'a>(entry: &'a Value, entry_id: &str) -> anyhow::Result<Vec<&'a Value>> {
    if let Some(template) = entry.get("template") {
        if !template.is_object() {
            anyhow::bail!("provider catalog entry '{entry_id}' template must be an object");
        }
        return Ok(vec![template]);
    }
    if let Some(templates) = entry.get("templates").and_then(Value::as_array) {
        if templates.iter().any(|template| !template.is_object()) {
            anyhow::bail!("provider catalog entry '{entry_id}' templates must be objects");
        }
        return Ok(templates.iter().collect());
    }
    anyhow::bail!("provider catalog entry '{entry_id}' has no inline template")
}

fn catalog_template_entries(catalog: &Value) -> anyhow::Result<Vec<CatalogTemplateEntry>> {
    let entries = catalog
        .get("entries")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("provider catalog entries must be an array"))?;
    let verified = catalog
        .get("signature")
        .and_then(Value::as_str)
        .is_some_and(|signature| !signature.trim().is_empty())
        || catalog.get("source").and_then(Value::as_str) == Some("builtin")
        || catalog
            .get("catalog_version")
            .and_then(Value::as_str)
            .is_some_and(|v| {
                matches!(v, "builtin-1" | "builtin-2" | "builtin-3")
            });
    let mut output = Vec::new();
    for entry in entries {
        let entry_id = entry
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("provider catalog entry id is required"))?;
        let vendor_id = entry
            .get("vendor_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let family = entry
            .get("family")
            .or_else(|| entry.get("family_id"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let source = entry
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or("local")
            .to_string();
        for template in entry_templates(entry, entry_id)? {
            let template_id = template
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("catalog template id is required"))?;
            output.push(CatalogTemplateEntry {
                entry_id: entry_id.to_string(),
                template_id: template_id.to_string(),
                vendor_id: if vendor_id.is_empty() {
                    template
                        .get("vendor_id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string()
                } else {
                    vendor_id.clone()
                },
                family: family.clone(),
                source: source.clone(),
                verified,
                template: template.clone(),
            });
        }
    }
    Ok(output)
}

#[derive(Debug, Clone)]
struct CatalogTemplateEntry {
    entry_id: String,
    template_id: String,
    vendor_id: String,
    family: String,
    source: String,
    verified: bool,
    template: Value,
}

fn catalog_sensitive_key(key: &str) -> bool {
    let normalized: String = key
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_lowercase())
        .collect();
    normalized == "authvalues"
        || normalized == "clientsecret"
        || normalized == "cachedtoken"
        || normalized == "refreshtoken"
        || normalized == "wpclienttoken"
        || normalized == "routesecret"
        || normalized == "password"
        || normalized == "pairingcode"
}

fn validate_public_catalog_template(template: &Value, entry_id: &str) -> anyhow::Result<()> {
    if let Some(path) = find_catalog_secret_field(template, "template") {
        anyhow::bail!("provider catalog entry '{entry_id}' contains secret field {path}");
    }
    serde_json::from_value::<ComponentTemplate>(template.clone())
        .with_context(|| format!("invalid component template in catalog entry '{entry_id}'"))?;
    Ok(())
}

fn find_catalog_secret_field(value: &Value, path: &str) -> Option<String> {
    match value {
        Value::Object(fields) => {
            for (key, child) in fields {
                let child_path = format!("{path}.{key}");
                if catalog_sensitive_key(key) {
                    if !child.is_null()
                        && (!child.is_string() || !child.as_str().unwrap_or("").is_empty())
                    {
                        return Some(child_path);
                    }
                }
                if let Some(found) = find_catalog_secret_field(child, &child_path) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(items) => items.iter().enumerate().find_map(|(index, child)| {
            find_catalog_secret_field(child, &format!("{path}[{index}]"))
        }),
        _ => None,
    }
}

fn catalog_url_check(raw: &str) -> Result<(), String> {
    let value = raw.trim();
    if value.contains("{{") || value.contains("}}") {
        // Runtime renders config placeholders before the final SSRF check.
        return Ok(());
    }
    let parsed = url::Url::parse(value).map_err(|_| "provider URL is invalid".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("provider URL scheme is not allowed".to_string());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("provider URL must not contain embedded credentials".to_string());
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| "provider URL host is missing".to_string())?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    // Product: any http(s) vendor URL may be installed (including loopback
    // mock-api / local LLM). Only block cloud metadata hostnames.
    if host == "metadata.google.internal"
        || host == "metadata"
        || host.ends_with(".metadata.google.internal")
    {
        return Err("provider host is blocked by egress policy".to_string());
    }
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        if matches!(ip, std::net::IpAddr::V4(v4) if v4.octets() == [169, 254, 169, 254]) {
            return Err("provider host is blocked by egress policy".to_string());
        }
    }
    Ok(())
}

fn collect_urls(value: &Value, path: &str, output: &mut Vec<(String, String)>) {
    match value {
        Value::Object(fields) => {
            for (key, child) in fields {
                let child_path = format!("{path}.{key}");
                if (key.eq_ignore_ascii_case("url")
                    || key.eq_ignore_ascii_case("endpoint")
                    || key.eq_ignore_ascii_case("token_url")
                    || key.eq_ignore_ascii_case("auth_url"))
                    && child.as_str().is_some()
                {
                    output.push((
                        child_path.clone(),
                        child.as_str().unwrap_or_default().to_string(),
                    ));
                }
                collect_urls(child, &child_path, output);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                collect_urls(child, &format!("{path}[{index}]"), output);
            }
        }
        _ => {}
    }
}

fn catalog_url_report(template: &Value) -> Vec<Value> {
    let mut urls = Vec::new();
    collect_urls(template, "template", &mut urls);
    urls.into_iter()
        .map(|(path, url)| match catalog_url_check(&url) {
            Ok(()) => json!({ "path": path, "url": url, "allowed": true }),
            Err(reason) => json!({ "path": path, "url": url, "allowed": false, "reason": reason }),
        })
        .collect()
}

pub(super) async fn handle_provider_catalog_list(
    socket: &mut TcpStream,
    _state: &Arc<Mutex<WebUiState>>,
    query: &str,
) -> anyhow::Result<()> {
    let (catalog, _builtin, load_meta) = match load_catalog_document_with_meta() {
        Ok(value) => value,
        Err(err) => {
            return write_error_response_with_status(
                socket,
                "500 Internal Server Error",
                "PROVIDER_CATALOG_INVALID",
                &format!("{:#}", err),
            )
            .await;
        }
    };
    let params = parse_query_string(query);
    let needle = params.get("q").map(|v| v.to_ascii_lowercase());
    let vendor_filter = params.get("vendor_id").map(|v| v.to_ascii_lowercase());
    let mut items = Vec::new();
    for entry in catalog_template_entries(&catalog)? {
        let template_name = entry
            .template
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(&entry.template_id);
        let kind = entry
            .template
            .get("type")
            .or_else(|| entry.template.get("kind"))
            .and_then(Value::as_str)
            .unwrap_or("text");
        let searchable = format!(
            "{} {} {} {} {}",
            entry.entry_id, entry.template_id, template_name, entry.vendor_id, entry.family
        )
        .to_ascii_lowercase();
        if needle.as_deref().is_some_and(|v| !searchable.contains(v)) {
            continue;
        }
        if vendor_filter
            .as_deref()
            .is_some_and(|v| !entry.vendor_id.to_ascii_lowercase().contains(v))
        {
            continue;
        }
        let formats = entry
            .template
            .get("constraints")
            .and_then(|v| v.get("supported_content_formats"))
            .cloned()
            .unwrap_or_else(|| json!([]));
        items.push(json!({
            "entry_id": entry.entry_id,
            "template_id": entry.template_id,
            "name": template_name,
            "vendor_id": entry.vendor_id,
            "family": entry.family,
            "kind": kind,
            "supported_content_formats": formats,
            "source": entry.source,
            "verified": entry.verified,
            "evidence_tier": entry
                .template
                .get("evidence_tier")
                .and_then(Value::as_str)
                .unwrap_or("schema-only"),
            "requires_local_credentials": entry.template.get("auth").is_some(),
            "editable_params": entry
                .template
                .get("editable_params")
                .cloned()
                .unwrap_or_else(|| json!([])),
            "template": entry.template,
        }));
    }
    let refresh_metadata = load_refresh_metadata();
    let payload = json!({
        "success": true,
        "data": {
            "schema": PROVIDER_CATALOG_SCHEMA,
            "catalog_version": catalog.get("catalog_version").cloned().unwrap_or(Value::Null),
            "items": items,
            "offline": load_meta.offline,
            "cache_source": load_meta.source,
            "cache_state": load_meta.state,
            "refresh_metadata": refresh_metadata,
        }
    });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(super) async fn handle_provider_catalog_refresh(
    socket: &mut TcpStream,
    _state: &Arc<Mutex<WebUiState>>,
) -> anyhow::Result<()> {
    let data = match refresh_provider_catalog_from_source().await {
        Ok(value) => value,
        Err(err) => {
            return write_error_response_with_status(
                socket,
                "422 Unprocessable Entity",
                "PROVIDER_CATALOG_REFRESH_FAILED",
                &format!("{:#}", err),
            )
            .await;
        }
    };
    let payload = json!({
        "success": true,
        "data": data,
    });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(super) async fn handle_install_from_catalog(
    socket: &mut TcpStream,
    _state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
) -> anyhow::Result<()> {
    let req: Value = serde_json::from_slice(body)
        .with_context(|| "invalid POST /api/components/local/install-from-catalog payload")?;
    let requested_id = req
        .get("entry_id")
        .or_else(|| req.get("catalog_id"))
        .or_else(|| req.get("template_id"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if requested_id.is_empty() {
        return write_error_response(socket, "MISSING_CATALOG_ID", "entry_id is required").await;
    }
    let (catalog, _) = match load_catalog_document() {
        Ok(value) => value,
        Err(err) => {
            return write_error_response_with_status(
                socket,
                "422 Unprocessable Entity",
                "PROVIDER_CATALOG_INVALID",
                &format!("{:#}", err),
            )
            .await;
        }
    };
    let entry = catalog_template_entries(&catalog)?
        .into_iter()
        .find(|entry| entry.entry_id == requested_id || entry.template_id == requested_id);
    let Some(entry) = entry else {
        return write_error_response(socket, "CATALOG_ENTRY_NOT_FOUND", "catalog entry not found")
            .await;
    };
    let rejected_urls: Vec<Value> = catalog_url_report(&entry.template)
        .into_iter()
        .filter(|item| item.get("allowed").and_then(Value::as_bool) == Some(false))
        .collect();
    if !rejected_urls.is_empty() {
        return write_error_response_with_status(
            socket,
            "422 Unprocessable Entity",
            "PROVIDER_URL_REJECTED",
            &serde_json::to_string(&rejected_urls)?,
        )
        .await;
    }

    let local_id = req
        .get("local_id")
        .or_else(|| req.get("id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or(&entry.template_id)
        .to_string();
    if local_id.len() > 160 || local_id.contains('/') || local_id.contains('\\') {
        return write_error_response(socket, "INVALID_ID", "local_id is invalid").await;
    }
    let mut doc = load_local_components_runtime_doc();
    if doc.components.contains_key(&local_id)
        && req.get("overwrite").and_then(Value::as_bool) != Some(true)
    {
        return write_conflict_response(
            socket,
            "DUPLICATE_ID",
            "local component already exists; set overwrite=true to replace it",
        )
        .await;
    }
    let template: ComponentTemplate = serde_json::from_value(entry.template.clone())?;
    let now = unix_ts().to_string();
    let name = req
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or(&template.name)
        .to_string();
    let kind = template.kind.clone();
    let was_overwrite = doc.components.contains_key(&local_id);
    doc.components.insert(
        local_id.clone(),
        ComponentInstanceLocal {
            name,
            template_id: template.id.clone(),
            source_template_id: format!("catalog:{}", entry.entry_id),
            source_template_updated_at: None,
            source_template_api_version: Some(template.version.clone()),
            vendor_id: entry.vendor_id.clone(),
            vendor_name: req
                .get("vendor_name")
                .and_then(Value::as_str)
                .unwrap_or(&entry.vendor_id)
                .trim()
                .to_string(),
            kind: kind.clone(),
            remarks: req
                .get("remarks")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string(),
            // Catalogs never carry credentials.  Keep the new component
            // disabled until the user selects a local key/OAuth binding.
            enabled: false,
            created_at: if was_overwrite {
                doc.components
                    .get(&local_id)
                    .map(|component| component.created_at.clone())
                    .unwrap_or_else(|| now.clone())
            } else {
                now.clone()
            },
            updated_at: Some(now),
            component_overrides: None,
            versions: HashMap::new(),
            active_version: None,
            template_json: Some(entry.template.clone()),
        },
    );
    save_local_components_runtime_doc(&doc)?;
    let payload = json!({
        "success": true,
        "data": {
            "id": local_id,
            "template_id": template.id,
            "catalog_entry_id": entry.entry_id,
            "vendor_id": entry.vendor_id,
            "kind": local_component_capability_id(&kind),
            "enabled": false,
            "overwrite": was_overwrite,
            "requires_local_credentials": template.auth.is_some(),
        }
    });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

fn public_pack_sensitive_key(key: &str) -> bool {
    let normalized: String = key
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_lowercase())
        .collect();
    normalized == "auth"
        || normalized == "authvalues"
        || normalized == "credentials"
        || normalized == "clientsecret"
        || normalized == "cachedtoken"
        || normalized == "refreshtoken"
        || normalized == "wpclienttoken"
        || normalized == "routesecret"
        || normalized == "pairingcode"
        || normalized == "password"
        || normalized == "username"
        || normalized == "user"
        || normalized == "login"
        || normalized == "authorization"
        || normalized == "authextraparameters"
        || normalized == "extraparameters"
        || normalized.contains("apikey")
        || normalized.contains("accesstoken")
        || normalized.contains("privatekey")
}

fn redact_public_value(value: &mut Value, path: &str, redacted_fields: &mut Vec<String>) {
    match value {
        Value::Object(fields) => {
            let keys: Vec<String> = fields.keys().cloned().collect();
            for key in keys {
                let child_path = format!("{path}.{key}");
                if public_pack_sensitive_key(&key) {
                    fields.remove(&key);
                    redacted_fields.push(child_path);
                } else if let Some(child) = fields.get_mut(&key) {
                    redact_public_value(child, &child_path, redacted_fields);
                }
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter_mut().enumerate() {
                redact_public_value(child, &format!("{path}[{index}]"), redacted_fields);
            }
        }
        _ => {}
    }
}

fn workflow_snapshot(conn: &rusqlite::Connection) -> Value {
    let policy = crate::task_engine::workflow_policy::load_workflow_policy(conn, false);
    let dsl = crate::task_engine::workflow_dsl::load_workflow_dsl(conn);
    json!({ "policy": policy, "dsl": dsl })
}

async fn build_integration_pack(
    state: &Arc<Mutex<WebUiState>>,
    private: bool,
    pack_id: Option<&str>,
    name: Option<&str>,
) -> anyhow::Result<(Value, Vec<String>)> {
    let (domain_tokens, component_bindings, task_bindings, rule_bindings, device_id, db) = {
        let guard = state.lock().await;
        (
            guard.domain_token_bindings.clone(),
            guard.component_bindings.clone(),
            guard.task_type_component_bindings.clone(),
            guard.rule_component_bindings.clone(),
            guard.device_id.clone(),
            Arc::clone(&guard.db),
        )
    };
    let workflow = {
        let conn = db.lock().await;
        workflow_snapshot(&conn)
    };
    let local_components = load_local_components_runtime_doc();
    let vendor_keys = load_vendor_keys(&vendor_keys_path()).unwrap_or_default();
    let vendor_oauth = load_vendor_oauth(&vendor_oauth_path()).unwrap_or_default();
    let proxy_profiles = load_proxy_profiles(&proxy_profiles_path()).unwrap_or_default();
    let mut redacted_fields = Vec::new();

    let mut site_connections = Vec::new();
    for (api_base_url, binding) in &domain_tokens.domains {
        if private {
            site_connections.push(json!({
                "api_base_url": api_base_url,
                "wp_client_token": binding.wp_client_token,
                "route_secret": binding.route_secret,
                "device_id": device_id,
            }));
        } else {
            site_connections.push(json!({
                "api_base_url": api_base_url,
                "route_secret_set": !binding.route_secret.trim().is_empty(),
                "device_id": "",
                "requires_pairing": binding.wp_client_token.trim().is_empty(),
            }));
            if !binding.wp_client_token.trim().is_empty() {
                redacted_fields.push(format!(
                    "domain_token_bindings.domains.{api_base_url}.wp_client_token"
                ));
            }
            if !binding.route_secret.trim().is_empty() {
                redacted_fields.push(format!(
                    "domain_token_bindings.domains.{api_base_url}.route_secret"
                ));
            }
        }
    }

    let mut local_components_value = serde_json::to_value(&local_components)?;
    let mut component_bindings_value = serde_json::to_value(&component_bindings)?;
    let mut vendor_keys_value = serde_json::to_value(&vendor_keys)?;
    let mut vendor_oauth_value = serde_json::to_value(&vendor_oauth)?;
    let mut proxy_profiles_value = serde_json::to_value(&proxy_profiles)?;
    let mut domain_tokens_value = serde_json::to_value(&domain_tokens)?;
    if !private {
        redact_public_value(
            &mut local_components_value,
            "local_components",
            &mut redacted_fields,
        );
        redact_public_value(
            &mut component_bindings_value,
            "component_bindings",
            &mut redacted_fields,
        );
        redact_public_value(&mut vendor_keys_value, "vendor_keys", &mut redacted_fields);
        redact_public_value(
            &mut vendor_oauth_value,
            "vendor_oauth",
            &mut redacted_fields,
        );
        redact_public_value(
            &mut proxy_profiles_value,
            "proxy_profiles",
            &mut redacted_fields,
        );
        redact_public_value(
            &mut domain_tokens_value,
            "domain_token_bindings",
            &mut redacted_fields,
        );
    }

    let mut required_keys = Vec::new();
    let mut required_oauth = Vec::new();
    for (component_id, binding) in &component_bindings.components {
        for key_id in &binding.key_ids {
            let vendor_id = vendor_keys
                .keys
                .get(key_id)
                .map(|key| key.vendor_id.clone())
                .unwrap_or_default();
            required_keys.push(json!({
                "id": key_id,
                "vendor_id": vendor_id,
                "component_id": component_id,
                "configured": vendor_keys.keys.get(key_id).is_some_and(|key| !key.auth_values.is_empty()),
            }));
        }
        for oauth_id in &binding.oauth_ids {
            let vendor_id = vendor_oauth
                .configs
                .get(oauth_id)
                .map(|config| config.vendor_id.clone())
                .unwrap_or_default();
            required_oauth.push(json!({
                "id": oauth_id,
                "vendor_id": vendor_id,
                "component_id": component_id,
                "configured": vendor_oauth.configs.contains_key(oauth_id),
            }));
        }
    }

    let provider_templates = local_components
        .components
        .iter()
        .filter_map(|(id, component)| {
            component.template_json.as_ref().map(|template| {
                json!({
                    "component_id": id,
                    "template_id": component.template_id,
                    "vendor_id": component.vendor_id,
                    "kind": component.kind,
                    "template": template,
                })
            })
        })
        .collect::<Vec<_>>();
    let mut provider_templates_value = Value::Array(provider_templates);
    if !private {
        redact_public_value(
            &mut provider_templates_value,
            "provider_templates",
            &mut redacted_fields,
        );
    }

    let (rule_bindings_for_pack, pack_binding_warnings) = if private {
        (rule_bindings.clone(), Vec::new())
    } else {
        sanitize_rule_bindings_for_public_pack(&rule_bindings)
    };
    for warning in &pack_binding_warnings {
        redacted_fields.push(format!("rule_component_bindings.{warning}"));
    }

    let pack = json!({
        "schema": INTEGRATION_PACK_SCHEMA,
        "pack_id": pack_id.filter(|v| !v.trim().is_empty()).unwrap_or("local-integration-pack"),
        "name": name.filter(|v| !v.trim().is_empty()).unwrap_or("Local integration pack"),
        "created_at": unix_ts().to_string(),
        "redaction_policy": if private { "private_encrypted" } else { "public_default" },
        "redacted": !private,
        "site_connection": site_connections.first().cloned().unwrap_or(Value::Null),
        "site_connections": site_connections,
        "domain_token_bindings": domain_tokens_value,
        "provider_templates": provider_templates_value,
        "local_components": local_components_value,
        "component_bindings": component_bindings_value,
        "task_type_component_bindings": serde_json::to_value(&task_bindings)?,
        "rule_component_bindings": serde_json::to_value(&rule_bindings_for_pack)?,
        "binding_portability_warnings": pack_binding_warnings,
        "workflow": workflow,
        "vendor_keys": vendor_keys_value,
        "vendor_oauth": vendor_oauth_value,
        "proxy_profiles": proxy_profiles_value,
        "secrets": {
            "mode": if private { "encrypted" } else { "redacted" },
            "required_vendor_keys": required_keys,
            "required_oauth_configs": required_oauth,
        },
        "redacted_fields": redacted_fields,
    });
    let redacted_paths = pack
        .get("redacted_fields")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Ok((pack, redacted_paths))
}

fn pack_from_request(body: &[u8]) -> anyhow::Result<(Value, bool)> {
    let root: Value = serde_json::from_slice(body).context("invalid integration pack JSON")?;
    let root_passphrase = root
        .get("passphrase")
        .and_then(Value::as_str)
        .map(str::to_string);
    let candidate = root
        .get("integration_pack")
        .or_else(|| root.get("pack"))
        .or_else(|| root.get("data").and_then(|v| v.get("integration_pack")))
        .cloned()
        .unwrap_or(root);
    let passphrase = root_passphrase
        .as_deref()
        .or_else(|| candidate.get("passphrase").and_then(Value::as_str));
    let encrypted = candidate
        .get("encrypted")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if encrypted {
        let payload = candidate
            .get("payload_base64")
            .or_else(|| candidate.get("encrypted_payload"))
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("encrypted integration pack payload is missing"))?;
        let bytes = BASE64_STANDARD
            .decode(payload)
            .context("decode encrypted integration pack failed")?;
        let plain = decrypt_private_backup(&bytes, passphrase)
            .context("decrypt integration pack failed")?;
        let pack: Value =
            serde_json::from_str(&plain).context("parse decrypted integration pack failed")?;
        return Ok((pack, true));
    }
    Ok((candidate, false))
}

fn validate_pack_schema(pack: &Value, encrypted: bool) -> anyhow::Result<bool> {
    if pack.get("schema").and_then(Value::as_str) != Some(INTEGRATION_PACK_SCHEMA) {
        anyhow::bail!("unsupported integration pack schema");
    }
    let private = pack
        .get("redaction_policy")
        .and_then(Value::as_str)
        .is_some_and(|v| v == "private" || v == "private_encrypted")
        || pack.get("redacted").and_then(Value::as_bool) == Some(false);
    if private && !encrypted {
        anyhow::bail!("private integration pack must be encrypted");
    }
    if !private && contains_public_pack_secret(pack) {
        anyhow::bail!("public integration pack contains a secret field");
    }
    Ok(!private)
}

fn contains_public_pack_secret(value: &Value) -> bool {
    match value {
        Value::Object(fields) => fields.iter().any(|(key, child)| {
            if public_pack_sensitive_key(key) {
                // `auth` is deliberately not used by public exports, but a
                // provider template's auth schema contains no secret values.
                // Treat an auth_values/client_secret/token field as secret;
                // normal template request placeholders are safe.
                return key != "auth";
            }
            contains_public_pack_secret(child)
        }),
        Value::Array(items) => items.iter().any(contains_public_pack_secret),
        _ => false,
    }
}

fn parse_doc<T>(pack: &Value, key: &str) -> anyhow::Result<Option<T>>
where
    T: DeserializeOwned,
{
    let Some(value) = pack.get(key) else {
        return Ok(None);
    };
    Ok(Some(serde_json::from_value(value.clone()).with_context(
        || format!("invalid integration pack field: {key}"),
    )?))
}

fn validate_local_component_template(
    id: &str,
    component: &ComponentInstanceLocal,
) -> anyhow::Result<()> {
    if let Some(template) = component.template_json.as_ref() {
        serde_json::from_value::<ComponentTemplate>(template.clone())
            .with_context(|| format!("invalid template_json for local component '{id}'"))?;
    }
    Ok(())
}

fn parse_local_components(pack: &Value) -> anyhow::Result<Vec<(String, ComponentInstanceLocal)>> {
    let Some(raw_value) = pack.get("local_components") else {
        return Ok(Vec::new());
    };
    // Native exports use ComponentsLocalDoc (`{version, components}`), while
    // hand-authored packs may use a direct id→component map or an array.
    let value = raw_value
        .get("components")
        .filter(|components| components.is_object())
        .unwrap_or(raw_value);
    let mut output = Vec::new();
    match value {
        Value::Object(items) => {
            for (id, item) in items {
                let component: ComponentInstanceLocal = serde_json::from_value(item.clone())
                    .with_context(|| format!("invalid local component '{id}'"))?;
                validate_local_component_template(id, &component)?;
                output.push((id.clone(), component));
            }
        }
        Value::Array(items) => {
            for item in items {
                let id = item
                    .get("id")
                    .and_then(Value::as_str)
                    .or_else(|| item.get("component_id").and_then(Value::as_str))
                    .ok_or_else(|| anyhow!("local component array item requires id"))?;
                let component_value = item
                    .get("component")
                    .cloned()
                    .unwrap_or_else(|| item.clone());
                let component: ComponentInstanceLocal = serde_json::from_value(component_value)
                    .with_context(|| format!("invalid local component '{id}'"))?;
                validate_local_component_template(id, &component)?;
                output.push((id.to_string(), component));
            }
        }
        _ => anyhow::bail!("local_components must be an object or array"),
    }
    Ok(output)
}

fn component_ids_after_import(
    existing: &ComponentsLocalDoc,
    incoming: &[(String, ComponentInstanceLocal)],
) -> HashSet<String> {
    let mut ids = existing.components.keys().cloned().collect::<HashSet<_>>();
    ids.extend(incoming.iter().map(|(id, _)| id.clone()));
    ids
}

fn binding_conflicts<T>(
    existing: &HashMap<String, T>,
    incoming: &HashMap<String, T>,
) -> Vec<String> {
    incoming
        .keys()
        .filter(|key| existing.contains_key(*key))
        .cloned()
        .collect()
}

async fn integration_preview(
    state: &Arc<Mutex<WebUiState>>,
    pack: &Value,
    encrypted: bool,
) -> anyhow::Result<Value> {
    let redacted = validate_pack_schema(pack, encrypted)?;
    let incoming_components = parse_local_components(pack)?;
    let existing_components = load_local_components_runtime_doc();
    let component_ids = component_ids_after_import(&existing_components, &incoming_components);
    let new_components = incoming_components
        .iter()
        .filter(|(id, _)| !existing_components.components.contains_key(id))
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    let overwritten_components = incoming_components
        .iter()
        .filter(|(id, _)| existing_components.components.contains_key(id))
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();

    let mut provider_urls = Vec::new();
    for (id, component) in &incoming_components {
        if let Some(template) = &component.template_json {
            for item in catalog_url_report(template) {
                let mut item = item;
                if let Some(object) = item.as_object_mut() {
                    object.insert("component_id".to_string(), Value::String(id.clone()));
                }
                provider_urls.push(item);
            }
        }
    }
    let blocked_provider_urls = provider_urls
        .iter()
        .filter(|item| item.get("allowed").and_then(Value::as_bool) == Some(false))
        .count();

    let current = {
        let guard = state.lock().await;
        (
            guard.component_bindings.clone(),
            guard.task_type_component_bindings.clone(),
            guard.rule_component_bindings.clone(),
            guard.domain_token_bindings.clone(),
        )
    };
    let incoming_component_bindings: Option<ComponentBindingsDoc> =
        parse_doc(pack, "component_bindings")?;
    let incoming_task_bindings: Option<TaskTypeComponentBindingsDoc> =
        parse_doc(pack, "task_type_component_bindings")?;
    let incoming_rule_bindings: Option<RuleComponentBindingsDoc> =
        parse_doc(pack, "rule_component_bindings")?;

    let cross_site_numeric_keys = incoming_rule_bindings
        .as_ref()
        .map(|doc| {
            let mut keys = numeric_scope_keys(&doc.relation_bindings)
                .into_iter()
                .map(|k| format!("relation:{k}"))
                .collect::<Vec<_>>();
            keys.extend(
                numeric_scope_keys(&doc.rule_bindings)
                    .into_iter()
                    .map(|k| format!("rule:{k}")),
            );
            keys
        })
        .unwrap_or_default();
    let cross_site_numeric_risk = incoming_rule_bindings
        .as_ref()
        .map(pack_has_cross_site_numeric_risk)
        .unwrap_or(false);

    let component_binding_conflicts = incoming_component_bindings
        .as_ref()
        .map(|doc| binding_conflicts(&current.0.components, &doc.components))
        .unwrap_or_default();
    let task_binding_conflicts = incoming_task_bindings
        .as_ref()
        .map(|doc| {
            let mut conflicts = binding_conflicts(&current.1.task_types, &doc.task_types);
            conflicts.extend(
                binding_conflicts(
                    &current.1.business_line_task_types,
                    &doc.business_line_task_types,
                )
                .into_iter()
                .map(|key| format!("business_line:{key}")),
            );
            conflicts
        })
        .unwrap_or_default();
    let rule_binding_conflicts = incoming_rule_bindings
        .as_ref()
        .map(|doc| {
            let mut conflicts = binding_conflicts(&current.2.global_defaults, &doc.global_defaults);
            conflicts.extend(
                binding_conflicts(&current.2.relation_bindings, &doc.relation_bindings)
                    .into_iter()
                    .map(|key| format!("relation:{key}")),
            );
            conflicts.extend(
                binding_conflicts(&current.2.plugin_bindings, &doc.plugin_bindings)
                    .into_iter()
                    .map(|key| format!("plugin:{key}")),
            );
            conflicts.extend(
                binding_conflicts(&current.2.rule_bindings, &doc.rule_bindings)
                    .into_iter()
                    .map(|key| format!("rule:{key}")),
            );
            conflicts
        })
        .unwrap_or_default();

    let mut missing_component_refs = Vec::new();
    if let Some(doc) = &incoming_component_bindings {
        for id in doc.components.keys() {
            if !component_ids.contains(id) {
                missing_component_refs.push(format!("component_bindings:{id}"));
            }
        }
    }
    if let Some(doc) = &incoming_task_bindings {
        for entry in doc
            .task_types
            .values()
            .chain(doc.business_line_task_types.values())
        {
            if !component_ids.contains(&entry.component_id) {
                missing_component_refs.push(format!("task_type:{}", entry.component_id));
            }
        }
    }
    if let Some(doc) = &incoming_rule_bindings {
        for component_id in doc
            .global_defaults
            .values()
            .chain(doc.relation_bindings.values().flat_map(HashMap::values))
            .chain(doc.plugin_bindings.values().flat_map(HashMap::values))
            .chain(doc.rule_bindings.values().flat_map(HashMap::values))
        {
            if !component_ids.contains(component_id) {
                missing_component_refs.push(format!("rule_binding:{component_id}"));
            }
        }
    }

    let incoming_domains: Option<DomainTokenBindingsDoc> =
        parse_doc(pack, "domain_token_bindings")?;
    let site_count = pack
        .get("site_connections")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or_else(|| usize::from(pack.get("site_connection").is_some_and(|v| !v.is_null())));
    let domains_with_credentials = incoming_domains
        .as_ref()
        .map(|doc| {
            doc.domains
                .values()
                .filter(|entry| !entry.wp_client_token.trim().is_empty())
                .count()
        })
        .unwrap_or(0);

    let workflow_valid = pack
        .get("workflow")
        .map(|workflow| {
            let policy_ok = workflow
                .get("policy")
                .and_then(|value| serde_json::to_string(value).ok())
                .and_then(|raw| {
                    crate::task_engine::workflow_policy::WorkflowPolicy::parse_json(&raw)
                })
                .is_some();
            let dsl_ok = workflow
                .get("dsl")
                .and_then(|value| serde_json::to_string(value).ok())
                .and_then(|raw| crate::task_engine::workflow_dsl::WorkflowDsl::parse_json(&raw))
                .is_some();
            policy_ok && dsl_ok
        })
        .unwrap_or(true);

    Ok(json!({
        "schema": INTEGRATION_PACK_SCHEMA,
        "redacted": redacted,
        "encrypted": encrypted,
        "new_components": new_components,
        "overwrite_components": overwritten_components,
        "component_binding_conflicts": component_binding_conflicts,
        "task_binding_conflicts": task_binding_conflicts,
        "rule_binding_conflicts": rule_binding_conflicts,
        "missing_component_refs": missing_component_refs,
        "provider_urls": provider_urls,
        "blocked_provider_url_count": blocked_provider_urls,
        "site_connections": {
            "count": site_count,
            "ready_to_import": domains_with_credentials,
            "requires_pairing": site_count.saturating_sub(domains_with_credentials),
            "existing_domains": current.3.domains.len(),
        },
        "workflow_valid": workflow_valid,
        "cross_site_numeric_risk": cross_site_numeric_risk,
        "cross_site_numeric_keys": cross_site_numeric_keys,
        "safe_to_import": blocked_provider_urls == 0
            && missing_component_refs.is_empty()
            && workflow_valid
            && !cross_site_numeric_risk,
    }))
}

pub(super) async fn handle_integration_pack_export(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
) -> anyhow::Result<()> {
    let req: Value = if body.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(body).context("invalid integration pack export request")?
    };
    let mode = req
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("public")
        .trim()
        .to_ascii_lowercase();
    let private = mode == "private" || mode == "private_encrypted";
    if private {
        if req.get("confirm").and_then(Value::as_bool) != Some(true) {
            return write_error_response(
                socket,
                "PRIVATE_BACKUP_CONFIRMATION_REQUIRED",
                "private backup requires confirm=true",
            )
            .await;
        }
        if let Err(err) = validate_backup_passphrase(req.get("passphrase").and_then(Value::as_str))
        {
            return write_error_response(
                socket,
                "PRIVATE_BACKUP_PASSPHRASE_REQUIRED",
                &format!("{:#}", err),
            )
            .await;
        }
    } else if mode != "public" {
        return write_error_response(
            socket,
            "INVALID_EXPORT_MODE",
            "mode must be public or private",
        )
        .await;
    }
    let (pack, redacted_fields) = build_integration_pack(
        state,
        private,
        req.get("pack_id").and_then(Value::as_str),
        req.get("name").and_then(Value::as_str),
    )
    .await?;
    if private {
        let passphrase = validate_backup_passphrase(req.get("passphrase").and_then(Value::as_str))?;
        let encoded = serde_json::to_vec(&pack)?;
        let encrypted = encrypt_private_backup(&encoded, passphrase)?;
        let payload = json!({
            "success": true,
            "data": {
                "schema": PRIVATE_BACKUP_SCHEMA,
                "pack_schema": INTEGRATION_PACK_SCHEMA,
                "export_mode": "private_encrypted",
                "encrypted": true,
                "portable": true,
                "algorithm": "AES-256-GCM",
                "kdf": "HKDF-SHA256",
                "payload_base64": BASE64_STANDARD.encode(encrypted),
            }
        });
        return write_http_response(
            socket,
            "200 OK",
            "application/json",
            &serde_json::to_vec(&payload)?,
        )
        .await;
    }
    let payload = json!({
        "success": true,
        "data": {
            "export_mode": "public",
            "encrypted": false,
            "pack": pack,
            "redacted_fields": redacted_fields,
        }
    });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(super) async fn handle_integration_pack_preview(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
) -> anyhow::Result<()> {
    let (pack, encrypted) = match pack_from_request(body) {
        Ok(value) => value,
        Err(err) => {
            return write_error_response(socket, "INVALID_INTEGRATION_PACK", &format!("{:#}", err))
                .await
        }
    };
    match integration_preview(state, &pack, encrypted).await {
        Ok(preview) => {
            let payload = json!({ "success": true, "data": preview });
            write_http_response(
                socket,
                "200 OK",
                "application/json",
                &serde_json::to_vec(&payload)?,
            )
            .await
        }
        Err(err) => {
            write_error_response_with_status(
                socket,
                "422 Unprocessable Entity",
                "INTEGRATION_PACK_PREVIEW_FAILED",
                &format!("{:#}", err),
            )
            .await
        }
    }
}

fn merge_vendor_key(existing: Option<&VendorKey>, mut incoming: VendorKey) -> VendorKey {
    if let Some(existing) = existing {
        if incoming.auth_values.is_empty() {
            incoming.auth_values = existing.auth_values.clone();
        }
    }
    incoming
}

fn merge_oauth_config(existing: Option<&OAuthConfig>, mut incoming: OAuthConfig) -> OAuthConfig {
    if let Some(existing) = existing {
        if incoming.client_secret.is_empty() {
            incoming.client_secret = existing.client_secret.clone();
        }
        if incoming.cached_token.is_none() {
            incoming.cached_token = existing.cached_token.clone();
        }
        if incoming.refresh_token.is_none() {
            incoming.refresh_token = existing.refresh_token.clone();
        }
        if incoming.cached_token_expires_at == 0 {
            incoming.cached_token_expires_at = existing.cached_token_expires_at;
        }
    }
    incoming
}

fn merge_proxy_profile(
    existing: Option<&ProxyProfile>,
    mut incoming: ProxyProfile,
) -> ProxyProfile {
    if let Some(existing) = existing {
        if incoming.username.is_empty() {
            incoming.username = existing.username.clone();
        }
        if incoming.password.is_empty() {
            incoming.password = existing.password.clone();
        }
    }
    incoming
}

fn merge_component_binding(
    existing: Option<&ComponentBindingEntry>,
    mut incoming: ComponentBindingEntry,
) -> ComponentBindingEntry {
    if let Some(existing) = existing {
        if incoming.auth.is_empty() {
            incoming.auth = existing.auth.clone();
        }
    }
    incoming
}

pub(super) async fn handle_integration_pack_import(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
) -> anyhow::Result<()> {
    let req: Value = if body.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(body).context("invalid integration pack import request")?
    };
    let overwrite = req
        .get("overwrite")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let pack_input = if req.get("integration_pack").is_some()
        || req.get("pack").is_some()
        || req.get("encrypted").is_some()
    {
        serde_json::to_vec(&req)?
    } else {
        body.to_vec()
    };
    let (pack, encrypted) = match pack_from_request(&pack_input) {
        Ok(value) => value,
        Err(err) => {
            return write_error_response(socket, "INVALID_INTEGRATION_PACK", &format!("{:#}", err))
                .await
        }
    };
    let preview = match integration_preview(state, &pack, encrypted).await {
        Ok(preview) => preview,
        Err(err) => {
            return write_error_response_with_status(
                socket,
                "422 Unprocessable Entity",
                "INTEGRATION_PACK_IMPORT_REJECTED",
                &format!("{:#}", err),
            )
            .await;
        }
    };
    if preview
        .get("blocked_provider_url_count")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        > 0
        || preview
            .get("missing_component_refs")
            .and_then(Value::as_array)
            .is_some_and(|v| !v.is_empty())
        || preview.get("workflow_valid").and_then(Value::as_bool) == Some(false)
        || preview
            .get("cross_site_numeric_risk")
            .and_then(Value::as_bool)
            == Some(true)
        || preview.get("safe_to_import").and_then(Value::as_bool) == Some(false)
    {
        return write_error_response_with_status(
            socket,
            "422 Unprocessable Entity",
            "INTEGRATION_PACK_IMPORT_REJECTED",
            "pack contains blocked provider URLs, missing component references, invalid workflow, or cross-site numeric binding ids",
        )
        .await;
    }

    let incoming_components = parse_local_components(&pack)?;
    let mut components_doc = load_local_components_runtime_doc();
    let mut imported_components = Vec::new();
    let mut skipped_components = Vec::new();
    for (id, mut component) in incoming_components {
        if components_doc.components.contains_key(&id) && !overwrite {
            skipped_components.push(id);
            continue;
        }
        if pack.get("redacted").and_then(Value::as_bool) == Some(true) {
            component.enabled = false;
        }
        imported_components.push(id.clone());
        components_doc.components.insert(id, component);
    }
    save_local_components_runtime_doc(&components_doc)?;

    let (
        component_bindings_path,
        task_bindings_path,
        rule_bindings_path,
        db,
        mut current_component_bindings,
        mut current_task_bindings,
        mut current_rule_bindings,
        mut current_domain_tokens,
    ) = {
        let guard = state.lock().await;
        (
            guard.component_bindings_path.clone(),
            guard.task_type_component_bindings_path.clone(),
            guard.rule_component_bindings_path.clone(),
            Arc::clone(&guard.db),
            guard.component_bindings.clone(),
            guard.task_type_component_bindings.clone(),
            guard.rule_component_bindings.clone(),
            guard.domain_token_bindings.clone(),
        )
    };
    let component_ids = components_doc
        .components
        .keys()
        .cloned()
        .collect::<HashSet<_>>();

    let mut imported_component_bindings = Vec::new();
    if let Some(incoming) = parse_doc::<ComponentBindingsDoc>(&pack, "component_bindings")? {
        for (id, entry) in incoming.components {
            if !component_ids.contains(&id) {
                continue;
            }
            if current_component_bindings.components.contains_key(&id) && !overwrite {
                continue;
            }
            let existing = current_component_bindings.components.get(&id);
            current_component_bindings
                .components
                .insert(id.clone(), merge_component_binding(existing, entry));
            imported_component_bindings.push(id);
        }
    }

    let mut imported_task_bindings = Vec::new();
    if let Some(incoming) =
        parse_doc::<TaskTypeComponentBindingsDoc>(&pack, "task_type_component_bindings")?
    {
        for (key, entry) in incoming.task_types {
            if !component_ids.contains(&entry.component_id) {
                continue;
            }
            if current_task_bindings.task_types.contains_key(&key) && !overwrite {
                continue;
            }
            current_task_bindings.task_types.insert(key.clone(), entry);
            imported_task_bindings.push(key);
        }
        for (key, entry) in incoming.business_line_task_types {
            if !component_ids.contains(&entry.component_id) {
                continue;
            }
            if current_task_bindings
                .business_line_task_types
                .contains_key(&key)
                && !overwrite
            {
                continue;
            }
            current_task_bindings
                .business_line_task_types
                .insert(key.clone(), entry);
            imported_task_bindings.push(format!("business_line:{key}"));
        }
    }

    let mut imported_rule_bindings = Vec::new();
    let mut skipped_numeric_rule_bindings = Vec::new();
    if let Some(mut incoming) =
        parse_doc::<RuleComponentBindingsDoc>(&pack, "rule_component_bindings")?
    {
        skipped_numeric_rule_bindings = strip_numeric_maps_on_import(&mut incoming);
        let insert_rule_map = |target: &mut HashMap<String, HashMap<String, String>>,
                               source: HashMap<String, HashMap<String, String>>,
                               prefix: &str,
                               imported: &mut Vec<String>| {
            for (scope_key, slots) in source {
                let filtered = slots
                    .into_iter()
                    .filter(|(_, component_id)| component_ids.contains(component_id))
                    .collect::<HashMap<_, _>>();
                if filtered.is_empty() {
                    continue;
                }
                if target.contains_key(&scope_key) && !overwrite {
                    continue;
                }
                target.insert(scope_key.clone(), filtered);
                imported.push(format!("{prefix}:{scope_key}"));
            }
        };
        let global = incoming
            .global_defaults
            .into_iter()
            .filter(|(_, component_id)| component_ids.contains(component_id))
            .collect::<HashMap<_, _>>();
        if overwrite || current_rule_bindings.global_defaults.is_empty() {
            for (slot, component_id) in global {
                if overwrite || !current_rule_bindings.global_defaults.contains_key(&slot) {
                    current_rule_bindings
                        .global_defaults
                        .insert(slot.clone(), component_id);
                    imported_rule_bindings.push(format!("global:{slot}"));
                }
            }
        }
        insert_rule_map(
            &mut current_rule_bindings.relation_bindings,
            incoming.relation_bindings,
            "relation",
            &mut imported_rule_bindings,
        );
        insert_rule_map(
            &mut current_rule_bindings.plugin_bindings,
            incoming.plugin_bindings,
            "plugin",
            &mut imported_rule_bindings,
        );
        insert_rule_map(
            &mut current_rule_bindings.rule_bindings,
            incoming.rule_bindings,
            "rule",
            &mut imported_rule_bindings,
        );
        // Merge portable v2 site_bindings (semantic only).
        for (site_key, site_entry) in incoming.site_bindings {
            let target = current_rule_bindings
                .site_bindings
                .entry(site_key.clone())
                .or_default();
            if target.site_ref.site_origin.is_empty() {
                target.site_ref = site_entry.site_ref;
            }
            for (rel_key, rel) in site_entry.relations {
                let slots: HashMap<_, _> = rel
                    .slots
                    .into_iter()
                    .filter(|(_, id)| component_ids.contains(id))
                    .collect();
                if slots.is_empty() {
                    continue;
                }
                if target.relations.contains_key(&rel_key) && !overwrite {
                    continue;
                }
                target.relations.insert(
                    rel_key.clone(),
                    RelationBindingEntry {
                        semantic_key: rel.semantic_key,
                        relation_ref: rel.relation_ref,
                        legacy_id_hint: None, // never import foreign numeric hints
                        slots,
                    },
                );
                imported_rule_bindings.push(format!("site_relation:{site_key}:{rel_key}"));
            }
            for (rule_key, rule) in site_entry.rules {
                let slots: HashMap<_, _> = rule
                    .slots
                    .into_iter()
                    .filter(|(_, id)| component_ids.contains(id))
                    .collect();
                if slots.is_empty() {
                    continue;
                }
                if target.rules.contains_key(&rule_key) && !overwrite {
                    continue;
                }
                target.rules.insert(
                    rule_key.clone(),
                    RuleBindingEntry {
                        semantic_key: rule.semantic_key,
                        relation_key: rule.relation_key,
                        rule_ref: rule.rule_ref,
                        legacy_id_hint: None,
                        slots,
                    },
                );
                imported_rule_bindings.push(format!("site_rule:{site_key}:{rule_key}"));
            }
        }
        if current_rule_bindings.version < 2 && !current_rule_bindings.site_bindings.is_empty() {
            current_rule_bindings.version = 2;
        }
    }

    save_component_bindings_runtime_doc(&component_bindings_path, &current_component_bindings)?;
    save_task_type_component_bindings_runtime_doc(&task_bindings_path, &current_task_bindings)?;
    save_rule_component_bindings_runtime_doc(&rule_bindings_path, &current_rule_bindings)?;

    let mut imported_domains = Vec::new();
    if encrypted {
        if let Some(incoming) = parse_doc::<DomainTokenBindingsDoc>(&pack, "domain_token_bindings")?
        {
            for (domain, entry) in incoming.domains {
                if entry.wp_client_token.trim().is_empty() {
                    continue;
                }
                if current_domain_tokens.domains.contains_key(&domain) && !overwrite {
                    continue;
                }
                current_domain_tokens.domains.insert(domain.clone(), entry);
                imported_domains.push(domain);
            }
        }
    }

    let mut vendor_keys = load_vendor_keys(&vendor_keys_path()).unwrap_or_default();
    if let Some(incoming) = parse_doc::<VendorKeysDoc>(&pack, "vendor_keys")? {
        for (id, key) in incoming.keys {
            if vendor_keys.keys.contains_key(&id) && !overwrite {
                continue;
            }
            let existing = vendor_keys.keys.get(&id);
            vendor_keys.keys.insert(id, merge_vendor_key(existing, key));
        }
    }
    let mut vendor_oauth = load_vendor_oauth(&vendor_oauth_path()).unwrap_or_default();
    if let Some(incoming) = parse_doc::<VendorOAuthDoc>(&pack, "vendor_oauth")? {
        for (id, config) in incoming.configs {
            if vendor_oauth.configs.contains_key(&id) && !overwrite {
                continue;
            }
            let existing = vendor_oauth.configs.get(&id);
            vendor_oauth
                .configs
                .insert(id, merge_oauth_config(existing, config));
        }
    }
    let mut proxy_profiles = load_proxy_profiles(&proxy_profiles_path()).unwrap_or_default();
    if let Some(incoming) = parse_doc::<ProxyProfilesDoc>(&pack, "proxy_profiles")? {
        for (id, profile) in incoming.profiles {
            if proxy_profiles.profiles.contains_key(&id) && !overwrite {
                continue;
            }
            let existing = proxy_profiles.profiles.get(&id);
            proxy_profiles
                .profiles
                .insert(id, merge_proxy_profile(existing, profile));
        }
    }
    save_vendor_keys(&vendor_keys_path(), &vendor_keys)?;
    save_vendor_oauth(&vendor_oauth_path(), &vendor_oauth)?;
    save_proxy_profiles(&proxy_profiles_path(), &proxy_profiles)?;

    if let Some(workflow) = pack.get("workflow") {
        let db_guard = db.lock().await;
        if let Some(policy) = workflow.get("policy") {
            crate::db::system::set_system_config(
                &db_guard,
                "workflow_policy",
                &serde_json::to_string(policy)?,
            )?;
        }
        if let Some(dsl) = workflow.get("dsl") {
            crate::db::system::set_system_config(
                &db_guard,
                "workflow_dsl",
                &serde_json::to_string(dsl)?,
            )?;
        }
    }
    {
        let db_guard = db.lock().await;
        crate::db::bindings::save_component_bindings_doc(&db_guard, &current_component_bindings)?;
        crate::db::bindings::save_task_type_component_bindings_doc(
            &db_guard,
            &current_task_bindings,
        )?;
        crate::db::bindings::save_rule_component_bindings_doc(&db_guard, &current_rule_bindings)?;
        crate::db::bindings::save_domain_token_bindings_doc(&db_guard, &current_domain_tokens)?;
        crate::db::components::save_local_components_doc(&db_guard, &components_doc)?;
        crate::db::vendor::save_vendor_keys_doc(&db_guard, &vendor_keys)?;
        crate::db::vendor::save_vendor_oauth_doc(&db_guard, &vendor_oauth)?;
        crate::db::proxy::save_proxy_profiles_doc(&db_guard, &proxy_profiles)?;
    }
    {
        let mut guard = state.lock().await;
        guard.component_bindings = current_component_bindings;
        guard.task_type_component_bindings = current_task_bindings;
        guard.rule_component_bindings = current_rule_bindings;
        guard.domain_token_bindings = current_domain_tokens.clone();
        guard.domains =
            crate::web_ui::local_sites_from_domain_token_bindings(&current_domain_tokens);
        guard.last_error.clear();
        guard.last_event = "integration_pack.imported".to_string();
        guard.updated_at = unix_ts();
    }

    let payload = json!({
        "success": true,
        "data": {
            "imported_components": imported_components,
            "skipped_components": skipped_components,
            "imported_component_bindings": imported_component_bindings,
            "imported_task_bindings": imported_task_bindings,
            "imported_rule_bindings": imported_rule_bindings,
            "skipped_numeric_rule_bindings": skipped_numeric_rule_bindings,
            "imported_domains": imported_domains,
            "requires_pairing": if encrypted { 0 } else { preview.get("site_connections").and_then(|v| v.get("requires_pairing")).and_then(Value::as_u64).unwrap_or(0) },
            "overwrite": overwrite,
        }
    });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_catalog_is_valid_and_secret_free() {
        let catalog = builtin_catalog();
        validate_catalog_document(&catalog, true).expect("builtin catalog valid");
        assert!(find_catalog_secret_field(&catalog, "catalog").is_none());
    }

    #[test]
    fn builtin_catalog_entry_count_meets_floor() {
        // P1-C-3: floor raised to 100+ (was 50 in stage 2).
        let catalog = builtin_catalog();
        let entries = catalog
            .get("entries")
            .and_then(Value::as_array)
            .expect("builtin catalog entries array");
        assert!(
            entries.len() >= 100,
            "builtin catalog should have >=100 entries, got {}",
            entries.len()
        );
        // Every entry must have a unique id and at least one template with evidence_tier.
        let mut ids = std::collections::HashSet::new();
        for entry in entries {
            let id = entry.get("id").and_then(Value::as_str).unwrap_or("");
            assert!(!id.is_empty(), "builtin entry has empty id");
            assert!(ids.insert(id), "builtin entry id '{id}' is duplicated");
            let templates = entry_templates(entry, id).expect("builtin entry templates");
            assert!(!templates.is_empty(), "builtin entry '{id}' has no templates");
            for template in templates {
                let tier = template
                    .get("evidence_tier")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                assert!(
                    matches!(tier, "mock-verified" | "live-verified" | "schema-only"),
                    "builtin entry '{id}' template missing/invalid evidence_tier: '{tier}'"
                );
            }
        }
    }

    #[test]
    fn public_redaction_removes_credentials_but_keeps_template_metadata() {
        let mut value = json!({
            "vendor_id": "openai",
            "auth_values": { "api_key": "secret" },
            "template_json": {
                "request": { "headers": { "Authorization": "Bearer {{auth.api_key}}" } }
            },
            "enabled": true
        });
        let mut paths = Vec::new();
        redact_public_value(&mut value, "root", &mut paths);
        assert!(value.get("auth_values").is_none());
        assert!(value["template_json"]["request"]["headers"]
            .get("Authorization")
            .is_none());
        assert!(!paths.is_empty());
    }

    #[test]
    fn provider_url_preflight_allows_any_http_except_metadata() {
        assert!(catalog_url_check("http://127.0.0.1:9090/translate").is_ok());
        assert!(catalog_url_check("http://localhost:5000/translate").is_ok());
        assert!(catalog_url_check("https://api.example.com/v1").is_ok());
        assert!(catalog_url_check("http://metadata.google.internal/a").is_err());
        assert!(catalog_url_check("http://user:pass@example.com/v1").is_err());
    }

    #[test]
    fn encrypted_pack_requires_private_secret_marker() {
        let public = json!({
            "schema": INTEGRATION_PACK_SCHEMA,
            "redaction_policy": "public_default",
            "redacted": true
        });
        assert!(validate_pack_schema(&public, false).unwrap());
        let private = json!({
            "schema": INTEGRATION_PACK_SCHEMA,
            "redaction_policy": "private_encrypted",
            "redacted": false
        });
        assert!(validate_pack_schema(&private, false).is_err());
    }

    fn tiny_unsigned_catalog(version: &str) -> Value {
        json!({
            "schema": PROVIDER_CATALOG_SCHEMA,
            "catalog_version": version,
            "created_at": "2026-09-03T00:00:00Z",
            "entries": [{
                "id": "test-openai",
                "kind": "provider-template-pack",
                "vendor_id": "openai",
                "family": "openai_compatible",
                "source": "remote",
                "templates": [{
                    "id": "test-openai-v1",
                    "name": "Test OpenAI",
                    "version": "1.0.0",
                    "type": "text",
                    "auth": { "fields": [{ "name": "api_key", "required": true }] },
                    "request": {
                        "method": "POST",
                        "url": "https://api.openai.com/v1/chat/completions",
                        "headers": { "Authorization": "Bearer {{auth.api_key}}" },
                        "body": { "model": "gpt-4o-mini", "messages": [] }
                    },
                    "response": { "translated_text_path": "choices.0.message.content" },
                    "constraints": {
                        "split_strategy": "paragraph",
                        "supported_content_formats": ["plain_text"]
                    },
                    "editable_params": [],
                    "evidence_tier": "schema-only"
                }]
            }]
        })
    }

    static CATALOG_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn catalog_cache_atomic_install_preserves_lkg() {
        let _guard = CATALOG_ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().expect("tempdir");
        let base = dir.path().join("provider-catalog.json");
        // SAFETY: test-local env for isolated cache paths.
        unsafe {
            std::env::set_var("WPTSALL_PROVIDER_CATALOG_FILE", &base);
            std::env::set_var("WPTSALL_ALLOW_UNSIGNED_CATALOG", "1");
        }

        let v1 = tiny_unsigned_catalog("cache-v1");
        install_catalog_candidate(&v1, "file://test-v1").expect("install v1");
        let (_, current, lkg, _) = catalog_cache_paths();
        assert!(current.exists());
        assert!(!lkg.exists(), "first install has no prior current to copy");

        let v2 = tiny_unsigned_catalog("cache-v2");
        install_catalog_candidate(&v2, "file://test-v2").expect("install v2");
        assert!(lkg.exists());
        let lkg_doc: Value = serde_json::from_str(&fs::read_to_string(&lkg).unwrap()).unwrap();
        let cur_doc: Value = serde_json::from_str(&fs::read_to_string(&current).unwrap()).unwrap();
        assert_eq!(lkg_doc["catalog_version"], "cache-v1");
        assert_eq!(cur_doc["catalog_version"], "cache-v2");

        let (loaded, builtin, meta) = load_catalog_document_with_meta().expect("load");
        assert!(!builtin);
        assert_eq!(meta.source, "current_cache");
        assert_eq!(loaded["catalog_version"], "cache-v2");

        unsafe {
            std::env::remove_var("WPTSALL_PROVIDER_CATALOG_FILE");
            std::env::remove_var("WPTSALL_ALLOW_UNSIGNED_CATALOG");
        }
    }

    #[test]
    fn catalog_load_falls_back_to_lkg_when_current_corrupt() {
        let _guard = CATALOG_ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().expect("tempdir");
        let base = dir.path().join("provider-catalog.json");
        unsafe {
            std::env::set_var("WPTSALL_PROVIDER_CATALOG_FILE", &base);
            std::env::set_var("WPTSALL_ALLOW_UNSIGNED_CATALOG", "1");
        }

        let v1 = tiny_unsigned_catalog("lkg-keep");
        install_catalog_candidate(&v1, "file://v1").expect("install");
        let v2 = tiny_unsigned_catalog("will-corrupt");
        install_catalog_candidate(&v2, "file://v2").expect("install2");
        let (_, current, _, _) = catalog_cache_paths();
        fs::write(&current, b"{not-json").expect("corrupt current");

        let (loaded, _, meta) = load_catalog_document_with_meta().expect("load lkg");
        assert_eq!(meta.state, "last_known_good");
        assert_eq!(loaded["catalog_version"], "lkg-keep");

        unsafe {
            std::env::remove_var("WPTSALL_PROVIDER_CATALOG_FILE");
            std::env::remove_var("WPTSALL_ALLOW_UNSIGNED_CATALOG");
        }
    }

    #[test]
    fn remote_catalog_refresh_signed_failclosed_and_offline_fallback() {
        let _guard = CATALOG_ENV_LOCK.lock().unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        // 1) unsigned remote → fail-closed
        {
            let dir = tempfile::tempdir().expect("tempdir");
            let base = dir.path().join("provider-catalog.json");
            let source = dir.path().join("remote-unsigned.json");
            fs::write(
                &source,
                serde_json::to_vec_pretty(&tiny_unsigned_catalog("remote-bad")).unwrap(),
            )
            .unwrap();
            unsafe {
                std::env::set_var("WPTSALL_PROVIDER_CATALOG_FILE", &base);
                std::env::set_var("WPTSALL_ALLOW_UNSIGNED_CATALOG", "1");
                std::env::set_var("WPTSALL_PROVIDER_CATALOG_ALLOW_FILE_SOURCE", "1");
            }
            install_catalog_candidate(&tiny_unsigned_catalog("keep-me"), "file://seed").unwrap();
            let (_, current, _, _) = catalog_cache_paths();
            let before = fs::read_to_string(&current).unwrap();
            unsafe {
                std::env::remove_var("WPTSALL_ALLOW_UNSIGNED_CATALOG");
                std::env::set_var(
                    "WPTSALL_PROVIDER_CATALOG_SOURCE_URL",
                    format!("file://{}", source.display()),
                );
            }
            let err = rt
                .block_on(refresh_provider_catalog_from_source())
                .expect_err("unsigned remote must fail");
            let msg = format!("{err:#}");
            assert!(
                msg.contains("signature") || msg.contains("required"),
                "unexpected err: {msg}"
            );
            assert_eq!(before, fs::read_to_string(&current).unwrap());
        }

        // 2) missing source → offline fallback
        {
            let dir = tempfile::tempdir().expect("tempdir");
            let base = dir.path().join("provider-catalog.json");
            unsafe {
                std::env::set_var("WPTSALL_PROVIDER_CATALOG_FILE", &base);
                std::env::set_var("WPTSALL_ALLOW_UNSIGNED_CATALOG", "1");
                std::env::set_var("WPTSALL_PROVIDER_CATALOG_ALLOW_FILE_SOURCE", "1");
                std::env::set_var(
                    "WPTSALL_PROVIDER_CATALOG_SOURCE_URL",
                    format!("file://{}/missing-catalog.json", dir.path().display()),
                );
            }
            install_catalog_candidate(&tiny_unsigned_catalog("offline-keep"), "file://seed")
                .unwrap();
            let result = rt
                .block_on(refresh_provider_catalog_from_source())
                .expect("download fail should soft-fallback");
            assert_eq!(result["offline"], true);
            assert_eq!(result["fallback"], true);
            assert_eq!(result["catalog_version"], "offline-keep");
        }

        // 3) signed file source → updates current
        {
            use base64::Engine;
            use rsa::pkcs1v15::SigningKey;
            use rsa::pkcs8::{EncodePublicKey, LineEnding};
            use rsa::signature::{SignatureEncoding, SignerMut};
            use rsa::{RsaPrivateKey, RsaPublicKey};
            use sha2::Sha256;

            let dir = tempfile::tempdir().expect("tempdir");
            let base = dir.path().join("provider-catalog.json");
            let mut catalog = tiny_unsigned_catalog("remote-signed-1");
            let entries = catalog.get("entries").cloned().unwrap();
            let entries_json = serde_json::to_vec(&entries).unwrap();
            catalog
                .as_object_mut()
                .unwrap()
                .insert("entries_sha256".into(), json!(hex_sha256(&entries_json)));

            let mut rng = rand::thread_rng();
            let private = RsaPrivateKey::new(&mut rng, 2048).expect("rsa");
            let public_pem = RsaPublicKey::from(&private)
                .to_public_key_pem(LineEnding::LF)
                .expect("pem");
            let mut signing_key = SigningKey::<Sha256>::new_unprefixed(private);
            let sig = base64::engine::general_purpose::STANDARD
                .encode(signing_key.sign(&entries_json).to_bytes());
            catalog
                .as_object_mut()
                .unwrap()
                .insert("signature".into(), json!(sig));
            catalog.as_object_mut().unwrap().insert(
                "signature_scope".into(),
                json!(PROVIDER_CATALOG_SIGNATURE_SCOPE),
            );

            let source = dir.path().join("remote-signed.json");
            fs::write(&source, serde_json::to_vec_pretty(&catalog).unwrap()).unwrap();
            let pubkey = dir.path().join("catalog.pub.pem");
            fs::write(&pubkey, public_pem).unwrap();

            unsafe {
                std::env::set_var("WPTSALL_PROVIDER_CATALOG_FILE", &base);
                std::env::set_var("WPTSALL_PROVIDER_CATALOG_PUBLIC_KEY_FILE", &pubkey);
                std::env::set_var("WPTSALL_PROVIDER_CATALOG_ALLOW_FILE_SOURCE", "1");
                std::env::set_var(
                    "WPTSALL_PROVIDER_CATALOG_SOURCE_URL",
                    format!("file://{}", source.display()),
                );
                std::env::remove_var("WPTSALL_ALLOW_UNSIGNED_CATALOG");
            }

            let result = rt
                .block_on(refresh_provider_catalog_from_source())
                .expect("signed refresh");
            assert_eq!(result["offline"], false);
            assert_eq!(result["source"], "remote");
            assert_eq!(result["catalog_version"], "remote-signed-1");
            let (_, current, _, _) = catalog_cache_paths();
            assert!(current.exists());
        }

        unsafe {
            std::env::remove_var("WPTSALL_PROVIDER_CATALOG_FILE");
            std::env::remove_var("WPTSALL_PROVIDER_CATALOG_PUBLIC_KEY_FILE");
            std::env::remove_var("WPTSALL_PROVIDER_CATALOG_SOURCE_URL");
            std::env::remove_var("WPTSALL_PROVIDER_CATALOG_ALLOW_FILE_SOURCE");
            std::env::remove_var("WPTSALL_ALLOW_UNSIGNED_CATALOG");
        }
    }
}
