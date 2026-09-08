use anyhow::Context;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::types::*;

use super::{default_bindings_version, encrypt_for_save, load_encrypted_or_plain};

pub(crate) fn parse_task_type_binding_key(raw: &str) -> Option<String> {
    let lower = raw.trim().to_lowercase();
    if lower.is_empty() {
        return None;
    }
    let normalized = match lower.as_str() {
        "text" | "text_translation" | "field" | "fields" => "text",
        "image" | "images" | "image_translation" => "image",
        "video" | "videos" | "video_translation" => "video",
        "audio" | "audios" | "audio_translation" => "audio",
        "document" | "documents" | "doc" | "file" | "files" | "document_translation" => "document",
        "mixed" => "mixed",
        _ => return None,
    };
    Some(normalized.to_string())
}

pub(crate) fn parse_content_format_binding_key(raw: &str) -> Option<String> {
    let lower = raw.trim().to_lowercase();
    if lower.is_empty() {
        return None;
    }
    let normalized = match lower.as_str() {
        "plain_text" | "plain" | "text" => "plain_text",
        "rich_html" | "html" | "richtext" | "rich_text" => "rich_html",
        "json_structured" | "json" => "json_structured",
        "serialized_php" | "serialized" => "serialized_php",
        "media_ref" | "media" | "mediaref" => "media_ref",
        "slug" => "slug",
        "code" => "code",
        _ => return None,
    };
    Some(normalized.to_string())
}

pub(crate) fn parse_rule_component_slot_binding_key(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some((left, right)) = trimmed.split_once(':') {
        if let Some(fmt) = parse_content_format_binding_key(left) {
            if fmt == "media_ref" {
                if let Some(media_type) = parse_task_type_binding_key(right) {
                    if matches!(
                        media_type.as_str(),
                        "image" | "video" | "audio" | "document"
                    ) {
                        return Some(format!("media_ref:{}", media_type));
                    }
                }
            }
        }
    }

    parse_content_format_binding_key(trimmed)
}

fn parse_rule_component_slot_key(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    parse_rule_component_slot_binding_key(trimmed)
}

fn rule_slot_key_preference(raw_key: &str, normalized_key: &str) -> u8 {
    let trimmed = raw_key.trim();
    if trimmed == normalized_key {
        return 0;
    }
    if trimmed.eq_ignore_ascii_case(normalized_key) {
        return 1;
    }
    2
}

fn normalize_rule_slot_map(slot_map: &HashMap<String, String>) -> HashMap<String, String> {
    let mut candidates: Vec<(String, String, String, u8)> = Vec::new();
    for (raw_key, raw_component_id) in slot_map {
        let component_id = raw_component_id.trim();
        if component_id.is_empty() {
            continue;
        }
        let Some(slot_key) = parse_rule_component_slot_key(raw_key) else {
            continue;
        };
        let pref = rule_slot_key_preference(raw_key, &slot_key);
        candidates.push((raw_key.clone(), slot_key, component_id.to_string(), pref));
    }
    candidates.sort_by(|a, b| a.0.cmp(&b.0));

    let mut merged: HashMap<String, (String, u8)> = HashMap::new();
    for (_, slot_key, component_id, pref) in candidates {
        match merged.get(&slot_key) {
            Some((_, old_pref)) if *old_pref <= pref => {}
            _ => {
                merged.insert(slot_key, (component_id, pref));
            }
        }
    }

    merged
        .into_iter()
        .map(|(slot_key, (component_id, _))| (slot_key, component_id))
        .collect()
}

fn normalize_numeric_scope_key(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let value = trimmed.parse::<u64>().ok()?;
    if value == 0 {
        return None;
    }
    Some(value.to_string())
}

pub(crate) fn normalize_rule_component_bindings_doc(
    doc: &RuleComponentBindingsDoc,
) -> RuleComponentBindingsDoc {
    let mut normalized = RuleComponentBindingsDoc {
        version: if doc.version == 0 {
            default_bindings_version()
        } else {
            doc.version
        },
        global_defaults: normalize_rule_slot_map(&doc.global_defaults),
        relation_bindings: HashMap::new(),
        plugin_bindings: HashMap::new(),
        rule_bindings: HashMap::new(),
        site_bindings: doc.site_bindings.clone(),
        migration_issues: doc.migration_issues.clone(),
    };

    let mut plugin_entries: Vec<(&String, &HashMap<String, String>)> =
        doc.plugin_bindings.iter().collect();
    plugin_entries.sort_by(|a, b| a.0.cmp(b.0));
    for (plugin_slug_raw, slot_map) in plugin_entries {
        let plugin_slug = plugin_slug_raw.trim().to_lowercase();
        if plugin_slug.is_empty() {
            continue;
        }
        let slot_map = normalize_rule_slot_map(slot_map);
        if slot_map.is_empty() {
            continue;
        }
        let entry = normalized.plugin_bindings.entry(plugin_slug).or_default();
        for (slot_key, component_id) in slot_map {
            entry.entry(slot_key).or_insert(component_id);
        }
    }

    let mut relation_entries: Vec<(&String, &HashMap<String, String>)> =
        doc.relation_bindings.iter().collect();
    relation_entries.sort_by(|a, b| a.0.cmp(b.0));
    for (relation_id_raw, slot_map) in relation_entries {
        let Some(relation_id) = normalize_numeric_scope_key(relation_id_raw) else {
            continue;
        };
        let slot_map = normalize_rule_slot_map(slot_map);
        if slot_map.is_empty() {
            continue;
        }
        let entry = normalized.relation_bindings.entry(relation_id).or_default();
        for (slot_key, component_id) in slot_map {
            entry.entry(slot_key).or_insert(component_id);
        }
    }

    let mut rule_entries: Vec<(&String, &HashMap<String, String>)> =
        doc.rule_bindings.iter().collect();
    rule_entries.sort_by(|a, b| a.0.cmp(b.0));
    for (rule_id_raw, slot_map) in rule_entries {
        let Some(rule_id) = normalize_numeric_scope_key(rule_id_raw) else {
            continue;
        };
        let slot_map = normalize_rule_slot_map(slot_map);
        if slot_map.is_empty() {
            continue;
        }
        let entry = normalized.rule_bindings.entry(rule_id).or_default();
        for (slot_key, component_id) in slot_map {
            entry.entry(slot_key).or_insert(component_id);
        }
    }

    normalized
}

fn rule_binding_slot_candidates(task_type: &str, content_format: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push_unique = |slot: String| {
        if !slot.is_empty() && !out.iter().any(|existing| existing == &slot) {
            out.push(slot);
        }
    };

    if let Some(format_raw) = content_format {
        if let Some(normalized_format) = parse_content_format_binding_key(format_raw) {
            if normalized_format == "media_ref" {
                if let Some(media_type) = parse_task_type_binding_key(task_type) {
                    if matches!(
                        media_type.as_str(),
                        "image" | "video" | "audio" | "document"
                    ) {
                        push_unique(format!("media_ref:{}", media_type));
                    }
                }
            }
            push_unique(normalized_format);
        }
    }

    out
}

fn resolve_component_id_from_slot_map<'a>(
    slot_map: &'a HashMap<String, String>,
    task_type: &str,
    content_format: Option<&str>,
) -> Option<&'a str> {
    let candidates = rule_binding_slot_candidates(task_type, content_format);
    if candidates.is_empty() {
        return None;
    }

    for slot in &candidates {
        if let Some(component_id) = slot_map.get(slot) {
            let trimmed = component_id.trim();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }

    for (raw_key, component_id) in slot_map {
        let Some(normalized_slot) = parse_rule_component_slot_key(raw_key) else {
            continue;
        };
        if candidates
            .iter()
            .any(|expected| expected == &normalized_slot)
        {
            let trimmed = component_id.trim();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }

    None
}

pub(crate) fn resolve_component_id_from_slot_map_pub<'a>(
    slot_map: &'a HashMap<String, String>,
    task_type: &str,
    content_format: Option<&str>,
) -> Option<&'a str> {
    resolve_component_id_from_slot_map(slot_map, task_type, content_format)
}

pub(crate) fn parse_business_line_key(raw: &str) -> Option<String> {
    let lower = raw.trim().to_lowercase();
    if lower.is_empty() {
        return None;
    }
    let normalized = match lower.as_str() {
        "post" | "post_type" | "post_content" => "post_content",
        "taxonomy" | "term" | "taxonomy_content" => "taxonomy_content",
        "theme" | "theme_i18n" => "theme_i18n",
        "plugin" | "plugin_i18n" | "language_pack" | "language_pack_i18n" => "plugin_i18n",
        "config" | "config_i18n" => "config_i18n",
        "site" | "site_strings" => "site_strings",
        "menu" | "menu_strings" => "menu_strings",
        "widget" | "widget_strings" => "widget_strings",
        "custom_model" | "custom" | "model" => "custom_model",
        _ => return None,
    };
    Some(normalized.to_string())
}

pub(crate) fn parse_business_line_task_type_binding_key(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut parts = trimmed.split(':');
    let business_line = parse_business_line_key(parts.next().unwrap_or_default())?;
    let task_type = parse_task_type_binding_key(parts.next().unwrap_or_default())?;
    if parts.next().is_some() {
        return None;
    }
    Some(format!("{}:{}", business_line, task_type))
}

pub(crate) fn load_task_type_component_bindings(
    path: &str,
) -> anyhow::Result<TaskTypeComponentBindingsDoc> {
    let file_path = Path::new(path);
    if let Some(parent) = file_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create bindings dir failed: {}", parent.display()))?;
        }
    }

    if !file_path.exists() {
        let doc = TaskTypeComponentBindingsDoc {
            version: default_bindings_version(),
            task_types: HashMap::new(),
            business_line_task_types: HashMap::new(),
        };
        save_task_type_component_bindings(path, &doc)?;
        return Ok(doc);
    }

    let decrypted_raw = load_encrypted_or_plain(file_path)?;
    if decrypted_raw.trim().is_empty() {
        return Ok(TaskTypeComponentBindingsDoc {
            version: default_bindings_version(),
            task_types: HashMap::new(),
            business_line_task_types: HashMap::new(),
        });
    }

    let mut doc: TaskTypeComponentBindingsDoc =
        serde_json::from_str(&decrypted_raw).with_context(|| {
            format!(
                "parse task type component bindings failed: {}",
                file_path.display()
            )
        })?;
    if doc.version == 0 {
        doc.version = default_bindings_version();
    }
    doc.task_types.retain(|key, entry| {
        let parsed = parse_task_type_binding_key(key);
        parsed.is_some() && !entry.component_id.trim().is_empty()
    });
    doc.business_line_task_types.retain(|key, entry| {
        let parsed = parse_business_line_task_type_binding_key(key);
        parsed.is_some() && !entry.component_id.trim().is_empty()
    });
    Ok(doc)
}

pub(crate) fn save_task_type_component_bindings(
    path: &str,
    doc: &TaskTypeComponentBindingsDoc,
) -> anyhow::Result<()> {
    let file_path = Path::new(path);
    if let Some(parent) = file_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create bindings dir failed: {}", parent.display()))?;
        }
    }

    let mut normalized = doc.clone();
    let entries: Vec<(String, TaskTypeComponentBindingEntry)> = normalized
        .task_types
        .drain()
        .filter_map(|(key, entry)| parse_task_type_binding_key(&key).map(|k| (k, entry)))
        .collect();
    for (key, entry) in entries {
        let component_id = entry.component_id.trim();
        if component_id.is_empty() {
            continue;
        }
        normalized.task_types.insert(
            key,
            TaskTypeComponentBindingEntry {
                component_id: component_id.to_string(),
            },
        );
    }
    let business_entries: Vec<(String, TaskTypeComponentBindingEntry)> = normalized
        .business_line_task_types
        .drain()
        .filter_map(|(key, entry)| {
            parse_business_line_task_type_binding_key(&key).map(|k| (k, entry))
        })
        .collect();
    for (key, entry) in business_entries {
        let component_id = entry.component_id.trim();
        if component_id.is_empty() {
            continue;
        }
        normalized.business_line_task_types.insert(
            key,
            TaskTypeComponentBindingEntry {
                component_id: component_id.to_string(),
            },
        );
    }

    let encoded = serde_json::to_string_pretty(&normalized)
        .with_context(|| "encode task type component bindings json failed".to_string())?;
    let output = encrypt_for_save(&encoded)?;
    fs::write(file_path, output).with_context(|| {
        format!(
            "write task type component bindings file failed: {}",
            file_path.display()
        )
    })?;
    Ok(())
}

pub(crate) fn task_type_component_binding_status_items(
    doc: &TaskTypeComponentBindingsDoc,
) -> Vec<TaskTypeComponentBindingStatusItem> {
    let mut items: Vec<TaskTypeComponentBindingStatusItem> = Vec::new();

    for (task_type, entry) in &doc.task_types {
        let Some(normalized_type) = parse_task_type_binding_key(task_type) else {
            continue;
        };
        let component_id = entry.component_id.trim();
        if component_id.is_empty() {
            continue;
        }
        items.push(TaskTypeComponentBindingStatusItem {
            business_line: String::new(),
            task_type: normalized_type,
            component_id: component_id.to_string(),
        });
    }

    for (scoped_key, entry) in &doc.business_line_task_types {
        let Some(parsed_key) = parse_business_line_task_type_binding_key(scoped_key) else {
            continue;
        };
        let mut parts = parsed_key.split(':');
        let business_line = parts.next().unwrap_or_default().to_string();
        let task_type = parts.next().unwrap_or_default().to_string();
        if business_line.is_empty() || task_type.is_empty() {
            continue;
        }
        let component_id = entry.component_id.trim();
        if component_id.is_empty() {
            continue;
        }
        items.push(TaskTypeComponentBindingStatusItem {
            business_line,
            task_type,
            component_id: component_id.to_string(),
        });
    }

    items.sort_by(|a, b| {
        a.business_line
            .cmp(&b.business_line)
            .then_with(|| a.task_type.cmp(&b.task_type))
    });
    items
}

pub(crate) fn rule_component_binding_status_items(
    doc: &RuleComponentBindingsDoc,
) -> Vec<RuleComponentBindingStatusItem> {
    let normalized_doc = normalize_rule_component_bindings_doc(doc);
    let mut items: Vec<RuleComponentBindingStatusItem> = Vec::new();

    for (slot_key, component_id) in &normalized_doc.global_defaults {
        items.push(RuleComponentBindingStatusItem {
            scope: "global".to_string(),
            scope_key: String::new(),
            slot_key: slot_key.to_string(),
            component_id: component_id.to_string(),
        });
    }

    for (plugin_slug, slot_map) in &normalized_doc.plugin_bindings {
        for (slot_key_raw, component_id_raw) in slot_map {
            items.push(RuleComponentBindingStatusItem {
                scope: "plugin".to_string(),
                scope_key: plugin_slug.clone(),
                slot_key: slot_key_raw.to_string(),
                component_id: component_id_raw.to_string(),
            });
        }
    }

    for (relation_id, slot_map) in &normalized_doc.relation_bindings {
        for (slot_key_raw, component_id_raw) in slot_map {
            items.push(RuleComponentBindingStatusItem {
                scope: "relation".to_string(),
                scope_key: relation_id.to_string(),
                slot_key: slot_key_raw.to_string(),
                component_id: component_id_raw.to_string(),
            });
        }
    }

    for (rule_id, slot_map) in &normalized_doc.rule_bindings {
        for (slot_key_raw, component_id_raw) in slot_map {
            items.push(RuleComponentBindingStatusItem {
                scope: "rule".to_string(),
                scope_key: rule_id.to_string(),
                slot_key: slot_key_raw.to_string(),
                component_id: component_id_raw.to_string(),
            });
        }
    }

    items.sort_by(|a, b| {
        a.scope
            .cmp(&b.scope)
            .then_with(|| a.scope_key.cmp(&b.scope_key))
            .then_with(|| a.slot_key.cmp(&b.slot_key))
    });

    items
}

pub(crate) fn load_rule_component_bindings(path: &str) -> anyhow::Result<RuleComponentBindingsDoc> {
    let file_path = Path::new(path);
    if let Some(parent) = file_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create bindings dir failed: {}", parent.display()))?;
        }
    }

    if !file_path.exists() {
        let doc = RuleComponentBindingsDoc {
            version: default_bindings_version(),
            global_defaults: HashMap::new(),
            relation_bindings: HashMap::new(),
            plugin_bindings: HashMap::new(),
            rule_bindings: HashMap::new(),
            site_bindings: HashMap::new(),
            migration_issues: Vec::new(),
        };
        save_rule_component_bindings(path, &doc)?;
        return Ok(doc);
    }

    let decrypted_raw = load_encrypted_or_plain(file_path)?;
    if decrypted_raw.trim().is_empty() {
        return Ok(RuleComponentBindingsDoc {
            version: default_bindings_version(),
            global_defaults: HashMap::new(),
            relation_bindings: HashMap::new(),
            plugin_bindings: HashMap::new(),
            rule_bindings: HashMap::new(),
            site_bindings: HashMap::new(),
            migration_issues: Vec::new(),
        });
    }

    let mut doc: RuleComponentBindingsDoc =
        serde_json::from_str(&decrypted_raw).with_context(|| {
            format!(
                "parse rule component bindings failed: {}",
                file_path.display()
            )
        })?;
    if doc.version == 0 {
        doc.version = default_bindings_version();
    }
    Ok(normalize_rule_component_bindings_doc(&doc))
}

pub(crate) fn save_rule_component_bindings(
    path: &str,
    doc: &RuleComponentBindingsDoc,
) -> anyhow::Result<()> {
    let file_path = Path::new(path);
    if let Some(parent) = file_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create bindings dir failed: {}", parent.display()))?;
        }
    }

    let normalized = normalize_rule_component_bindings_doc(doc);
    let encoded = serde_json::to_string_pretty(&normalized)
        .with_context(|| "encode rule component bindings json failed".to_string())?;
    let output = encrypt_for_save(&encoded)?;
    fs::write(file_path, output).with_context(|| {
        format!(
            "write rule component bindings file failed: {}",
            file_path.display()
        )
    })?;
    Ok(())
}

pub(crate) fn resolve_component_id_from_rule_bindings<'a>(
    doc: &'a RuleComponentBindingsDoc,
    rule_id: Option<u64>,
    relation_id: Option<u64>,
    plugin_slug: Option<&str>,
    task_type: &str,
    content_format: Option<&str>,
) -> Option<&'a str> {
    use crate::bindings::binding_v2::{
        resolve_rule_bindings_with_context, BindingResolveContext, BINDINGS_DOC_VERSION_V2,
    };
    let result = resolve_rule_bindings_with_context(
        doc,
        &BindingResolveContext {
            site_key: None,
            relation_semantic_key: None,
            rule_semantic_key: None,
            rule_id,
            relation_id,
            plugin_slug,
            task_type,
            content_format,
            // v1 keeps numeric maps; v2 requires explicit site/semantic context for numeric hints
            allow_legacy_numeric: doc.version < BINDINGS_DOC_VERSION_V2,
        },
    );
    result.component_id
}
