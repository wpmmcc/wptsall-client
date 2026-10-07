use std::collections::HashSet;
use std::sync::atomic::{AtomicI64, Ordering};

use serde_json::{json, Value};

use super::*;

pub(super) fn normalize_rule_data_type(raw: &str) -> Option<&'static str> {
    match raw.trim().to_lowercase().as_str() {
        "post" | "post_type" => Some("post"),
        "term" | "taxonomy" => Some("term"),
        "option" => Some("option"),
        _ => None,
    }
}

pub(super) fn normalize_object_type_key(raw: &str) -> &'static str {
    match raw.trim().to_lowercase().as_str() {
        "term" | "taxonomy" => "taxonomy",
        "post" | "post_type" => "post_type",
        "option" => "option",
        "language_pack" => "language_pack",
        _ => "post_type",
    }
}

pub(super) fn discover_content_data_types(
    relation: &DiscoveredRelation,
    rules: &[DiscoveredRule],
) -> Vec<&'static str> {
    let mut include_post = relation
        .models
        .iter()
        .any(|model| !model.post_types.is_empty());
    let mut include_term = relation
        .models
        .iter()
        .any(|model| !model.taxonomies.is_empty());
    let mut include_option = false;

    for rule in rules {
        match normalize_rule_data_type(&rule.data_type) {
            Some("post") => include_post = true,
            Some("term") => include_term = true,
            Some("option") => include_option = true,
            _ => {}
        }
    }

    if !include_post && !include_term {
        include_post = true;
    }

    let mut out = Vec::new();
    if include_post {
        out.push("post");
    }
    if include_term {
        out.push("term");
    }
    if include_option {
        out.push("option");
    }
    out
}

pub(super) fn tighten_total_pages(total_pages: &AtomicI64, total: i64, per_page: i64) {
    let max_page = if per_page > 0 {
        (total + per_page - 1) / per_page
    } else {
        1
    };
    let mut old_total_pages = total_pages.load(Ordering::Acquire);
    while old_total_pages > max_page {
        match total_pages.compare_exchange(
            old_total_pages,
            max_page,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => break,
            Err(current) => old_total_pages = current,
        }
    }
}

/// STA-02 (ISS gap doc, batch H 2026-09-22): claim item shape per discovery
/// lane. post → {object_id, post_type}; term → {object_id, taxonomy};
/// option → {object_id, option_name} (WP claim_option_entries reads
/// option_name first, falling back to subtype/post_type).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum ContentClaimShape {
    Post,
    Term,
    Option,
}

/// STA-02 decision core: which discovery data types require a durable
/// WP-side claim before translation. Options historically fell into the
/// else branch ("claim_not_required_for_data_type") — but the WP callback
/// for options REQUIRES an active claim, so option callbacks were rejected
/// with claim_required and dual clients raced on options with no lease at
/// all. The WP claim endpoint (claim_option_entries: relation-scoped
/// Option_Sync_State_Service CAS + owner hash) has been ready all along;
/// this closes the client half. language_pack / site_string keep their own
/// lanes and are intentionally NOT claimed here.
pub(super) fn content_claim_shape(data_type: &str) -> Option<ContentClaimShape> {
    match data_type {
        "post" => Some(ContentClaimShape::Post),
        "term" => Some(ContentClaimShape::Term),
        "option" => Some(ContentClaimShape::Option),
        _ => None,
    }
}

pub(super) fn build_content_claim_items(
    content_items: &[ContentItem],
    shape: ContentClaimShape,
) -> Vec<Value> {
    content_items
        .iter()
        .map(|item| {
            match shape {
                ContentClaimShape::Term => json!({
                    "object_id": item.object_id,
                    "taxonomy": item.subtype
                }),
                // Option names ride the subtype field; WP validates that
                // object_id == crc32("option:" + name) and echoes
                // {object_id, post_type: option_name, option_name}.
                ContentClaimShape::Option => json!({
                    "object_id": item.object_id,
                    "option_name": item.subtype
                }),
                ContentClaimShape::Post => json!({
                    "object_id": item.object_id,
                    "post_type": item.subtype
                }),
            }
        })
        .collect()
}

pub(super) fn retain_claimed_content_items(
    content_items: &mut Vec<ContentItem>,
    claimed_items: Vec<ClaimedContentItem>,
    is_term_claim: bool,
) -> usize {
    let claimed_keys: HashSet<(i64, String)> = claimed_items
        .into_iter()
        .filter_map(|item| {
            if item.object_id <= 0 {
                return None;
            }
            let subtype = if is_term_claim {
                if item.taxonomy.trim().is_empty() {
                    item.post_type
                } else {
                    item.taxonomy
                }
            } else if item.post_type.trim().is_empty() {
                item.taxonomy
            } else {
                item.post_type
            };
            let normalized_subtype = subtype.trim().to_lowercase();
            if normalized_subtype.is_empty() {
                return None;
            }
            Some((item.object_id, normalized_subtype))
        })
        .collect();

    content_items.retain(|item| {
        claimed_keys.contains(&(item.object_id, item.subtype.trim().to_lowercase()))
    });
    claimed_keys.len()
}

pub(super) fn build_language_pack_claim_items(
    language_pack_items: &[LanguagePackItem],
) -> Vec<Value> {
    language_pack_items
        .iter()
        .map(|item| json!({ "entry_id": item.complete_data.entry_id }))
        .collect()
}

pub(super) fn retain_claimed_language_pack_items(
    language_pack_items: &mut Vec<LanguagePackItem>,
    claimed_items: Vec<ClaimedContentItem>,
) -> usize {
    let claimed_entry_ids: HashSet<i64> = claimed_items
        .into_iter()
        .map(|item| {
            if item.entry_id > 0 {
                item.entry_id
            } else {
                item.object_id
            }
        })
        .filter(|entry_id| *entry_id > 0)
        .collect();

    language_pack_items.retain(|item| claimed_entry_ids.contains(&item.complete_data.entry_id));
    claimed_entry_ids.len()
}
