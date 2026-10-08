//! Per-field user action for one rule.
//!
//! `translate` calls the configured component. `copy` ships the source
//! value with no provider call. `skip` stays out of the write patch so an
//! empty decision cannot wipe the existing target.

use serde_json::Value;

use crate::types::DiscoveredRule;

use super::pipeline::extract_translate_fields;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FieldAction {
    Translate,
    Copy,
    Skip,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FieldActionPlan {
    pub(crate) translate: Vec<String>,
    pub(crate) copy: Vec<String>,
    pub(crate) skip: Vec<String>,
}

impl FieldActionPlan {
    pub(crate) fn action_of(&self, field: &str) -> Option<FieldAction> {
        if self.translate.iter().any(|name| name == field) {
            Some(FieldAction::Translate)
        } else if self.copy.iter().any(|name| name == field) {
            Some(FieldAction::Copy)
        } else if self.skip.iter().any(|name| name == field) {
            Some(FieldAction::Skip)
        } else {
            None
        }
    }

    pub(crate) fn provider_call_fields(&self) -> &[String] {
        &self.translate
    }
}

pub(crate) fn plan_field_actions(rule: &DiscoveredRule) -> FieldActionPlan {
    let mut chosen: Vec<(String, FieldAction)> = Vec::new();

    for name in rule
        .translate_fields
        .iter()
        .cloned()
        .chain(extract_translate_fields(&rule.field_capabilities))
    {
        if !chosen.iter().any(|(existing, _)| existing == &name) {
            chosen.push((name, FieldAction::Translate));
        }
    }

    if let Some(object) = rule.field_capabilities.as_object() {
        for (key, value) in object {
            if key == "translate_fields" {
                continue;
            }
            let Some(action) = explicit_action(value) else {
                continue;
            };
            if let Some(slot) = chosen.iter_mut().find(|(name, _)| name == key) {
                slot.1 = action;
            } else {
                chosen.push((key.clone(), action));
            }
        }
    }

    let mut plan = FieldActionPlan::default();
    for (name, action) in chosen {
        match action {
            FieldAction::Translate => plan.translate.push(name),
            FieldAction::Copy => plan.copy.push(name),
            FieldAction::Skip => plan.skip.push(name),
        }
    }
    plan.translate.sort();
    plan.copy.sort();
    plan.skip.sort();
    plan.translate.dedup();
    plan.copy.dedup();
    plan.skip.dedup();
    plan
}

fn explicit_action(value: &Value) -> Option<FieldAction> {
    if let Some(token) = value.as_str() {
        return action_token(token);
    }
    let object = value.as_object()?;
    let token = object
        .get("action")
        .or_else(|| object.get("type"))
        .and_then(Value::as_str)?;
    let action = action_token(token)?;
    if object.get("enabled").and_then(Value::as_bool) == Some(false) {
        return Some(FieldAction::Skip);
    }
    Some(action)
}

fn action_token(token: &str) -> Option<FieldAction> {
    match token {
        "translate" => Some(FieldAction::Translate),
        "copy" | "as_is" => Some(FieldAction::Copy),
        "skip" | "exclude" => Some(FieldAction::Skip),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rule_from(value: serde_json::Value) -> DiscoveredRule {
        serde_json::from_value(value).expect("rule")
    }

    #[test]
    fn legacy_translate_list_stays_provider_only() {
        let rule = rule_from(json!({
            "id": 1,
            "model_id": 1,
            "data_type": "post",
            "object_name": "post",
            "translate_fields": ["post_content", "post_title"]
        }));
        let plan = plan_field_actions(&rule);
        assert_eq!(plan.provider_call_fields(), ["post_content", "post_title"]);
        assert!(plan.copy.is_empty());
        assert!(plan.skip.is_empty());
    }

    #[test]
    fn title_translates_content_copies_excerpt_is_left_untouched() {
        let rule = rule_from(json!({
            "id": 7,
            "model_id": 1,
            "data_type": "post",
            "object_name": "post",
            "translate_fields": ["post_title", "post_content", "post_excerpt"],
            "field_capabilities": {
                "post_title": "translate",
                "post_content": "copy",
                "post_excerpt": "skip"
            }
        }));
        let plan = plan_field_actions(&rule);
        assert_eq!(plan.provider_call_fields(), ["post_title"]);
        assert_eq!(plan.copy, ["post_content"]);
        assert_eq!(plan.skip, ["post_excerpt"]);
        assert_eq!(plan.action_of("post_excerpt"), Some(FieldAction::Skip));
    }

    #[test]
    fn disabled_translate_object_does_not_call_provider() {
        let rule = rule_from(json!({
            "id": 8,
            "model_id": 1,
            "data_type": "post",
            "object_name": "post",
            "field_capabilities": {
                "post_title": { "type": "translate", "enabled": false },
                "post_content": { "action": "as_is" }
            }
        }));
        let plan = plan_field_actions(&rule);
        assert!(plan.provider_call_fields().is_empty());
        assert_eq!(plan.copy, ["post_content"]);
        assert_eq!(plan.skip, ["post_title"]);
    }
}
