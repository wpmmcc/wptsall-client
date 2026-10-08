//! Which sync fields call a provider, which ship the source, and which
//! stay off the target write.

use super::types::{SyncFieldAction, SyncMode, SyncPair};

pub(crate) const SYNC_TEXT_FIELDS: [&str; 3] = ["post_title", "post_content", "post_excerpt"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SyncFieldKind {
    Translate,
    Copy,
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlannedSyncField {
    pub(crate) field: String,
    pub(crate) action: SyncFieldKind,
    pub(crate) component_id: String,
}

pub(crate) fn plan_sync_fields(pair: &SyncPair) -> Vec<PlannedSyncField> {
    if pair.sync_mode != SyncMode::SyncAndTranslate {
        return Vec::new();
    }
    let fallback = pair
        .translate_component_id
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_string();
    if pair.field_actions.is_empty() {
        return SYNC_TEXT_FIELDS
            .into_iter()
            .map(|field| PlannedSyncField {
                field: field.to_string(),
                action: SyncFieldKind::Translate,
                component_id: fallback.clone(),
            })
            .collect();
    }
    let mut planned: Vec<PlannedSyncField> = Vec::new();
    for action in &pair.field_actions {
        let Some(field) = canonical_field(&action.field) else {
            continue;
        };
        let kind = kind_of(&action.action);
        let component_id = action
            .component_id
            .as_deref()
            .unwrap_or("")
            .trim()
            .to_string();
        let component_id = if component_id.is_empty() {
            fallback.clone()
        } else {
            component_id
        };
        if let Some(existing) = planned.iter_mut().find(|row| row.field == field) {
            existing.action = kind;
            existing.component_id = component_id;
        } else {
            planned.push(PlannedSyncField {
                field,
                action: kind,
                component_id,
            });
        }
    }
    planned
}

pub(crate) fn translate_component_required(pair: &SyncPair) -> bool {
    plan_sync_fields(pair)
        .iter()
        .any(|row| row.action == SyncFieldKind::Translate && row.component_id.is_empty())
}

fn canonical_field(raw: &str) -> Option<String> {
    let name = raw.trim();
    SYNC_TEXT_FIELDS
        .into_iter()
        .find(|field| *field == name)
        .map(str::to_string)
}

fn kind_of(raw: &str) -> SyncFieldKind {
    match raw.trim() {
        "translate" => SyncFieldKind::Translate,
        "copy" | "as_is" => SyncFieldKind::Copy,
        _ => SyncFieldKind::Skip,
    }
}

pub(crate) fn normalize_field_actions(actions: Vec<SyncFieldAction>) -> Vec<SyncFieldAction> {
    let planned_pair = SyncPair {
        sync_mode: SyncMode::SyncAndTranslate,
        field_actions: actions,
        ..empty_pair()
    };
    plan_sync_fields(&planned_pair)
        .into_iter()
        .map(|row| SyncFieldAction {
            field: row.field,
            action: match row.action {
                SyncFieldKind::Translate => "translate".to_string(),
                SyncFieldKind::Copy => "copy".to_string(),
                SyncFieldKind::Skip => "skip".to_string(),
            },
            component_id: if row.component_id.is_empty() {
                None
            } else {
                Some(row.component_id)
            },
        })
        .collect()
}

fn empty_pair() -> SyncPair {
    SyncPair {
        id: String::new(),
        name: String::new(),
        source_domain: String::new(),
        target_domain: String::new(),
        direction: Default::default(),
        sync_mode: SyncMode::SyncOnly,
        source_lang: String::new(),
        target_lang: String::new(),
        conflict_strategy: Default::default(),
        sync_frequency: Default::default(),
        post_types: Vec::new(),
        status: Default::default(),
        last_sync_at: None,
        last_seen_source_id: None,
        last_sync_count: None,
        last_error: None,
        translate_component_id: None,
        field_actions: Vec::new(),
        review_before_push: false,
        created_at: 0,
        updated_at: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pair(value: serde_json::Value) -> SyncPair {
        serde_json::from_value(value).expect("pair")
    }

    #[test]
    fn legacy_pair_translates_the_three_text_fields() {
        let pair = pair(json!({
            "id": "p",
            "name": "n",
            "source_domain": "https://a.example",
            "target_domain": "https://b.example",
            "sync_mode": "sync_and_translate",
            "translate_component_id": "comp-a",
            "created_at": 1,
            "updated_at": 1
        }));
        let plan = plan_sync_fields(&pair);
        assert_eq!(plan.len(), 3);
        assert!(plan.iter().all(|row| row.action == SyncFieldKind::Translate));
        assert!(plan.iter().all(|row| row.component_id == "comp-a"));
        assert!(!translate_component_required(&pair));
        let mut missing = pair.clone();
        missing.translate_component_id = None;
        assert!(translate_component_required(&missing));
    }

    #[test]
    fn title_translates_content_copies_excerpt_is_not_sent() {
        let pair = pair(json!({
            "id": "p",
            "name": "n",
            "source_domain": "https://a.example",
            "target_domain": "https://b.example",
            "sync_mode": "sync_and_translate",
            "translate_component_id": "comp-a",
            "field_actions": [
                {"field": "post_title", "action": "translate", "component_id": "comp-title"},
                {"field": "post_content", "action": "copy"},
                {"field": "post_excerpt", "action": "skip"}
            ],
            "created_at": 1,
            "updated_at": 1
        }));
        let plan = plan_sync_fields(&pair);
        assert_eq!(plan[0].action, SyncFieldKind::Translate);
        assert_eq!(plan[0].component_id, "comp-title");
        assert_eq!(plan[1].field, "post_content");
        assert_eq!(plan[1].action, SyncFieldKind::Copy);
        assert_eq!(plan[2].action, SyncFieldKind::Skip);
        assert!(!translate_component_required(&pair));
    }

    #[test]
    fn copy_and_skip_only_does_not_require_a_component() {
        let pair = pair(json!({
            "id": "p",
            "name": "n",
            "source_domain": "https://a.example",
            "target_domain": "https://b.example",
            "sync_mode": "sync_and_translate",
            "field_actions": [
                {"field": "post_title", "action": "copy"},
                {"field": "post_content", "action": "skip"}
            ],
            "created_at": 1,
            "updated_at": 1
        }));
        assert!(!translate_component_required(&pair));
        assert!(plan_sync_fields(&pair)
            .iter()
            .all(|row| row.action != SyncFieldKind::Translate));
    }
}
