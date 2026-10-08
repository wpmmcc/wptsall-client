use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::hmac::sha256_hex;

pub const PACKET_SCHEMA_VERSION: &str = "wpmmcc-sync-v1.0";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OriginContext {
    pub origin_site_uuid: String,
    pub origin_site_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_permalink: Option<String>,
    pub origin_blog_id: u64,
    pub origin_lang: String,
    pub vector_clock: HashMap<String, u64>,
    pub hop_count: u32,
    pub dispatch_timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorHint {
    #[serde(default = "default_author_name")]
    pub display_name: String,
}

fn default_author_name() -> String {
    "Sync Engine".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityPayload {
    pub guid: String,
    pub object_type: String,
    pub subtype: String,
    pub source_id: u64,
    pub slug: String,
    pub status: String,
    #[serde(default)]
    pub author_hint: Option<AuthorHint>,
    /// Core fields as exported by the source site. Values stay `Value`
    /// (not `String`) because the plugin export mixes types — e.g.
    /// `menu_order` is an int while `post_title` is a string.
    pub core_fields: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub taxonomies: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub meta_fields: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub plugin_specific: HashMap<String, serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_fields: Option<Vec<String>>,
    /// Fields the target must keep on an existing post. A missing core key
    /// would otherwise become an empty title, content or excerpt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preserved_fields: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncPacket {
    pub schema_version: String,
    pub packet_id: String,
    pub origin_context: OriginContext,
    pub action: String,    // "upsert" | "delete"
    pub sync_mode: String, // "sync_only" | "sync_and_translate"
    pub target_lang: String,
    pub source_fingerprint: String,
    pub entity: EntityPayload,
    #[serde(default)]
    pub multimodal_manifest: Vec<serde_json::Value>,
}

pub fn compute_content_fingerprint(
    origin_uuid: &str,
    clock: u64,
    core_fields: &HashMap<String, serde_json::Value>,
    meta_fields: &HashMap<String, serde_json::Value>,
) -> String {
    let core_json = serde_json::to_string(core_fields).unwrap_or_default();
    let meta_json = serde_json::to_string(meta_fields).unwrap_or_default();
    let content_hash = sha256_hex(format!("{}{}", core_json, meta_json).as_bytes());
    sha256_hex(format!("{}|{}|{}", origin_uuid, clock, content_hash).as_bytes())
}

/// URL rewriting engine: rewrites all occurrences of source_base_url
/// to target_base_url within HTML content, preventing source site links
/// from leaking into the target synchronized post.
pub fn rewrite_content_urls(html: &str, source_url: &str, target_url: &str) -> String {
    let src_clean = source_url.trim_end_matches('/');
    let tgt_clean = target_url.trim_end_matches('/');
    if src_clean.is_empty() || tgt_clean.is_empty() || src_clean == tgt_clean {
        return html.to_string();
    }
    html.replace(src_clean, tgt_clean)
}

/// Extract media image URLs from HTML markup (`<img ... src="..." ...>`)
pub fn extract_image_urls(html: &str) -> Vec<String> {
    let mut urls = Vec::new();
    let mut remaining = html;
    while let Some(img_idx) = remaining.find("<img") {
        let after_img = &remaining[img_idx..];
        let Some(tag_end) = after_img.find('>') else {
            break;
        };
        let tag = &after_img[..tag_end];
        if let Some(src_idx) = tag.find("src=") {
            let after_src = &tag[src_idx + 4..];
            let quote = after_src.chars().next();
            if let Some(q) = quote {
                if q == '"' || q == '\'' {
                    let val_str = &after_src[1..];
                    if let Some(close_q) = val_str.find(q) {
                        let url = &val_str[..close_q];
                        if !url.is_empty() && !urls.contains(&url.to_string()) {
                            urls.push(url.to_string());
                        }
                    }
                }
            }
        }
        remaining = &after_img[tag_end..];
    }
    urls
}
