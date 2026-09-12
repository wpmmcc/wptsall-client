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

pub(super) fn build_content_claim_items(
    content_items: &[ContentItem],
    is_term_claim: bool,
) -> Vec<Value> {
    content_items
        .iter()
        .map(|item| {
            if is_term_claim {
                json!({
                    "object_id": item.object_id,
                    "taxonomy": item.subtype
                })
            } else {
                json!({
                    "object_id": item.object_id,
                    "post_type": item.subtype
                })
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

#[cfg(test)]
mod tests {
    // catalog: WEBUI-MOD-task-engine-discoverer-fetch-rs
    // oracle: L1
    use super::*;
    use serde_json::json;

    #[test]
    fn rule_data_type_normalization_is_strict() {
        assert_eq!(normalize_rule_data_type("Post"), Some("post"));
        assert_eq!(normalize_rule_data_type(" post_type "), Some("post"));
        assert_eq!(normalize_rule_data_type("Term"), Some("term"));
        assert_eq!(normalize_rule_data_type("Taxonomy"), Some("term"));
        assert_eq!(normalize_rule_data_type("option"), Some("option"));
        assert_eq!(normalize_rule_data_type("media"), None);
        assert_eq!(normalize_rule_data_type(""), None);
    }

    #[test]
    fn object_type_key_defaults_to_post_type() {
        assert_eq!(normalize_object_type_key("term"), "taxonomy");
        assert_eq!(normalize_object_type_key("Taxonomy"), "taxonomy");
        assert_eq!(normalize_object_type_key("post"), "post_type");
        assert_eq!(normalize_object_type_key("option"), "option");
        assert_eq!(normalize_object_type_key("language_pack"), "language_pack");
        assert_eq!(normalize_object_type_key("unknown"), "post_type");
        assert_eq!(normalize_object_type_key(""), "post_type");
    }

    fn relation_with_models(
        post_types: Vec<String>,
        taxonomies: Vec<String>,
    ) -> DiscoveredRelation {
        serde_json::from_value(json!({
            "id": 1,
            "source_lang": "en_US",
            "target_site_id": "v_1",
            "target_site_type": "virtual",
            "target_lang": "fr_FR",
            "sync_mode": "auto",
            "models": [ {
                "model_id": 7,
                "post_types": post_types,
                "taxonomies": taxonomies
            } ]
        }))
        .unwrap()
    }

    fn rule_with_type(data_type: &str) -> DiscoveredRule {
        serde_json::from_value(json!({
            "id": 1,
            "model_id": 7,
            "data_type": data_type,
            "object_name": "x"
        }))
        .unwrap()
    }

    #[test]
    fn content_data_types_union_models_and_rules_with_post_default() {
        let models_only = relation_with_models(vec!["post".into()], vec![]);
        assert_eq!(discover_content_data_types(&models_only, &[]), vec!["post"]);

        let taxonomy_only = relation_with_models(vec![], vec!["category".into()]);
        assert_eq!(
            discover_content_data_types(&taxonomy_only, &[]),
            vec!["term"]
        );

        let both = relation_with_models(vec!["post".into()], vec!["category".into()]);
        assert_eq!(discover_content_data_types(&both, &[]), vec!["post", "term"]);

        // A rule widens the union with the option data type.
        let with_option_rule = relation_with_models(vec![], vec![]);
        assert_eq!(
            discover_content_data_types(&with_option_rule, &[rule_with_type("option")]),
            vec!["post", "option"],
            "empty models default to post, and the rule adds option"
        );

        // Unknown rule types contribute nothing.
        let with_unknown_rule = relation_with_models(vec!["post".into()], vec![]);
        assert_eq!(
            discover_content_data_types(&with_unknown_rule, &[rule_with_type("media")]),
            vec!["post"]
        );
    }

    #[test]
    fn tighten_total_pages_lowers_but_never_raises() {
        let pages = AtomicI64::new(10);
        tighten_total_pages(&pages, 95, 10);
        assert_eq!(pages.load(Ordering::Acquire), 10, "95/10 needs 10 pages");

        tighten_total_pages(&pages, 41, 10);
        assert_eq!(pages.load(Ordering::Acquire), 5, "41/10 tightens to 5");

        tighten_total_pages(&pages, 5, 10);
        assert_eq!(
            pages.load(Ordering::Acquire),
            1,
            "fewer remote items must clamp the loop to one page"
        );

        // Only a race that lowers the count further wins; a stale higher
        // value never raises it back.
        tighten_total_pages(&pages, 100, 10);
        assert_eq!(pages.load(Ordering::Acquire), 1, "never raises");

        let degenerate = AtomicI64::new(4);
        tighten_total_pages(&degenerate, 0, 0);
        assert_eq!(
            degenerate.load(Ordering::Acquire),
            1,
            "per_page<=0 falls back to a single page"
        );
    }

    fn content_item(object_id: i64, subtype: &str) -> ContentItem {
        serde_json::from_value(json!({
            "object_type": "post_type",
            "subtype": subtype,
            "object_id": object_id,
            "complete_data": {}
        }))
        .unwrap()
    }

    #[test]
    fn claim_items_use_taxonomy_or_post_type_key_per_claim_kind() {
        let items = vec![content_item(11, "post"), content_item(12, "page")];
        let post_keys = build_content_claim_items(&items, false);
        assert_eq!(post_keys[0]["post_type"], "post");
        assert_eq!(post_keys[1]["post_type"], "page");
        assert!(post_keys[0].get("taxonomy").is_none());

        let term_keys = build_content_claim_items(&items, true);
        assert_eq!(term_keys[0]["taxonomy"], "post");
        assert!(term_keys[0].get("post_type").is_none());
    }

    #[test]
    fn retain_claimed_items_cross_fills_subtype_and_is_case_insensitive() {
        let mut items = vec![
            content_item(11, "post"),
            content_item(12, "Page"),
            content_item(13, "custom"),
        ];
        // Claim shape as the WP claim endpoint returns it: post claims may
        // carry taxonomy and vice versa; whichever field is filled wins,
        // the other backs it up.
        let claimed: Vec<ClaimedContentItem> =
            serde_json::from_value(json!([
                { "object_id": 11, "post_type": "post" },
                { "object_id": 12, "taxonomy": "page" },
                { "object_id": 0,  "post_type": "post" },
                { "object_id": 13, "post_type": "", "taxonomy": " " }
            ]))
            .unwrap();
        let claimed_count = retain_claimed_content_items(&mut items, claimed, false);
        assert_eq!(
            claimed_count, 2,
            "object_id<=0 and empty subtype are dropped from the claimed set"
        );
        let retained_ids: Vec<i64> = items.iter().map(|i| i.object_id).collect();
        assert_eq!(retained_ids, vec![11, 12]);
    }

    #[test]
    fn language_pack_claim_round_trip_uses_entry_id_with_object_id_fallback() {
        let lp = |entry_id: i64| -> LanguagePackItem {
            serde_json::from_value(json!({
                "object_id": 900 + entry_id,
                "complete_data": { "entry_id": entry_id, "msgid": "m" }
            }))
            .unwrap()
        };
        let items = vec![lp(1), lp(2), lp(3)];
        let keys = build_language_pack_claim_items(&items);
        assert_eq!(keys[0]["entry_id"], 1);

        let mut items = items;
        // A claim without entry_id falls back to object_id AS the entry id
        // (the language-pack claim endpoint addresses entries by id in both
        // fields).
        let claimed: Vec<ClaimedContentItem> = serde_json::from_value(json!([
            { "entry_id": 0, "object_id": 2 },
            { "entry_id": 3 }
        ]))
        .unwrap();
        let count = retain_claimed_language_pack_items(&mut items, claimed);
        assert_eq!(count, 2);
        let entry_ids: Vec<i64> = items.iter().map(|i| i.complete_data.entry_id).collect();
        assert_eq!(entry_ids, vec![2, 3]);
    }
}
