//! P1-D portable binding v2: semantic keys, migration, pack numeric-id guards.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use sha2::{Digest, Sha256};

use crate::bindings::domain_tokens::normalize_domain_base;
use crate::types::*;

use super::task_rules::{
    normalize_rule_component_bindings_doc, parse_rule_component_slot_binding_key,
    resolve_component_id_from_slot_map_pub,
};

pub(crate) const BINDINGS_DOC_VERSION_V2: u32 = 2;

#[derive(Debug, Clone, Default)]
pub(crate) struct DiscoveredRelationIdentity {
    pub(crate) legacy_id: u64,
    pub(crate) relation_ref: RelationRef,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct DiscoveredRuleIdentity {
    pub(crate) legacy_id: u64,
    pub(crate) relation_legacy_id: Option<u64>,
    pub(crate) rule_ref: RuleRef,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SiteDiscoveryIndex {
    pub(crate) site_origin: String,
    pub(crate) relations: Vec<DiscoveredRelationIdentity>,
    pub(crate) rules: Vec<DiscoveredRuleIdentity>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct BindingDiscoveryIndex {
    pub(crate) sites: Vec<SiteDiscoveryIndex>,
}

#[derive(Debug, Clone)]
pub(crate) struct BindingResolveContext<'a> {
    pub(crate) site_key: Option<&'a str>,
    pub(crate) relation_semantic_key: Option<&'a str>,
    pub(crate) rule_semantic_key: Option<&'a str>,
    pub(crate) rule_id: Option<u64>,
    pub(crate) relation_id: Option<u64>,
    pub(crate) plugin_slug: Option<&'a str>,
    pub(crate) task_type: &'a str,
    pub(crate) content_format: Option<&'a str>,
    /// Only for local v1 docs / explicit site-scoped legacy fallback. Never for pack import.
    pub(crate) allow_legacy_numeric: bool,
}

impl<'a> Default for BindingResolveContext<'a> {
    fn default() -> Self {
        Self {
            site_key: None,
            relation_semantic_key: None,
            rule_semantic_key: None,
            rule_id: None,
            relation_id: None,
            plugin_slug: None,
            task_type: "",
            content_format: None,
            allow_legacy_numeric: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BindingResolveOutcome {
    ResolvedRule,
    ResolvedPlugin,
    ResolvedRelation,
    ResolvedGlobal,
    LegacyNumericFallback,
    Unresolved,
}

#[derive(Debug, Clone)]
pub(crate) struct BindingResolveResult<'a> {
    pub(crate) outcome: BindingResolveOutcome,
    pub(crate) component_id: Option<&'a str>,
    pub(crate) semantic_key: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub(crate) struct MigrationReport {
    pub(crate) backup_path: String,
    pub(crate) migrated_relations: usize,
    pub(crate) migrated_rules: usize,
    pub(crate) issue_count: usize,
}

fn hex_sha256_bytes(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn canonical_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .into_iter()
                .map(|k| format!("\"{}\":{}", escape_json_string(k), canonical_json(&map[k])))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        serde_json::Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(canonical_json).collect();
            format!("[{}]", parts.join(","))
        }
        other => other.to_string(),
    }
}

fn escape_json_string(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            _ => out.push(ch),
        }
    }
    out
}

fn normalize_label(raw: &str) -> String {
    raw.trim().to_ascii_lowercase()
}

pub(crate) fn site_key_from_origin(site_origin: &str) -> String {
    let origin = normalize_domain_base(site_origin);
    format!("site:{}", hex_sha256_bytes(origin.as_bytes()))
}

pub(crate) fn relation_semantic_key(relation: &RelationRef) -> String {
    let template = if relation.template.trim().is_empty() {
        "default"
    } else {
        relation.template.as_str()
    };
    let payload = serde_json::json!({
        "source_lang": normalize_label(&relation.source_lang),
        "target_lang": normalize_label(&relation.target_lang),
        "target_site_type": normalize_label(&relation.target_site_type),
        "template": normalize_label(template),
    });
    format!(
        "relation:{}",
        hex_sha256_bytes(canonical_json(&payload).as_bytes())
    )
}

pub(crate) fn rule_semantic_key(relation_key: &str, rule: &RuleRef) -> String {
    let routing = if rule.routing_profile.trim().is_empty() {
        "default"
    } else {
        rule.routing_profile.as_str()
    };
    let payload = serde_json::json!({
        "relation_key": relation_key.trim(),
        "plugin_slug": normalize_label(&rule.plugin_slug),
        "data_type": normalize_label(&rule.data_type),
        "object_name": normalize_label(&rule.object_name),
        "name": normalize_label(&rule.name),
        "source_group": normalize_label(&rule.source_group),
        "routing_profile": normalize_label(routing),
        "delivery_target": normalize_label(&rule.delivery_target),
        "content_format": normalize_label(&rule.content_format),
    });
    format!(
        "rule:{}",
        hex_sha256_bytes(canonical_json(&payload).as_bytes())
    )
}

pub(crate) fn component_semantic_key(component: &ComponentSemanticRef) -> String {
    let payload = serde_json::json!({
        "vendor_id": normalize_label(&component.vendor_id),
        "template_id": normalize_label(&component.template_id),
        "kind": normalize_label(&component.kind),
        "version": normalize_label(&component.version),
    });
    format!(
        "component:{}",
        hex_sha256_bytes(canonical_json(&payload).as_bytes())
    )
}

fn normalize_slot_map(slot_map: &HashMap<String, String>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for (raw_key, raw_component_id) in slot_map {
        let component_id = raw_component_id.trim();
        if component_id.is_empty() {
            continue;
        }
        let Some(slot_key) = parse_rule_component_slot_binding_key(raw_key) else {
            continue;
        };
        out.entry(slot_key)
            .or_insert_with(|| component_id.to_string());
    }
    out
}

/// Detect portable-pack risk: relation/rule maps keyed only by numeric WP ids.
pub(crate) fn numeric_scope_keys(map: &HashMap<String, HashMap<String, String>>) -> Vec<String> {
    let mut keys: Vec<String> = map
        .keys()
        .filter(|k| {
            let trimmed = k.trim();
            !trimmed.is_empty()
                && trimmed.parse::<u64>().ok().filter(|v| *v > 0).is_some()
                && !trimmed.contains(':')
        })
        .cloned()
        .collect();
    keys.sort();
    keys
}

pub(crate) fn pack_has_cross_site_numeric_risk(doc: &RuleComponentBindingsDoc) -> bool {
    !numeric_scope_keys(&doc.relation_bindings).is_empty()
        || !numeric_scope_keys(&doc.rule_bindings).is_empty()
}

/// Public packs must not carry numeric relation/rule maps (cross-site silent bind risk).
pub(crate) fn sanitize_rule_bindings_for_public_pack(
    doc: &RuleComponentBindingsDoc,
) -> (RuleComponentBindingsDoc, Vec<String>) {
    let mut warnings = Vec::new();
    let mut out = doc.clone();
    let numeric_relations = numeric_scope_keys(&out.relation_bindings);
    let numeric_rules = numeric_scope_keys(&out.rule_bindings);
    if !numeric_relations.is_empty() {
        warnings.push(format!(
            "stripped_numeric_relation_bindings:{}",
            numeric_relations.join(",")
        ));
    }
    if !numeric_rules.is_empty() {
        warnings.push(format!(
            "stripped_numeric_rule_bindings:{}",
            numeric_rules.join(",")
        ));
    }
    // Public packs keep semantic site_bindings only; never rehydrate numeric maps.
    out.relation_bindings.clear();
    out.rule_bindings.clear();
    if out.version < BINDINGS_DOC_VERSION_V2 && !out.site_bindings.is_empty() {
        out.version = BINDINGS_DOC_VERSION_V2;
    }
    (out, warnings)
}

/// Import must never apply numeric relation/rule maps from a pack.
pub(crate) fn strip_numeric_maps_on_import(doc: &mut RuleComponentBindingsDoc) -> Vec<String> {
    let mut skipped = Vec::new();
    for key in numeric_scope_keys(&doc.relation_bindings) {
        skipped.push(format!("relation:{key}"));
        doc.relation_bindings.remove(&key);
    }
    for key in numeric_scope_keys(&doc.rule_bindings) {
        skipped.push(format!("rule:{key}"));
        doc.rule_bindings.remove(&key);
    }
    skipped
}

fn find_unique_relation<'a>(
    index: &'a BindingDiscoveryIndex,
    legacy_id: u64,
) -> Result<(&'a SiteDiscoveryIndex, &'a DiscoveredRelationIdentity), &'static str> {
    let mut matches: Vec<(&SiteDiscoveryIndex, &DiscoveredRelationIdentity)> = Vec::new();
    for site in &index.sites {
        for relation in &site.relations {
            if relation.legacy_id == legacy_id {
                matches.push((site, relation));
            }
        }
    }
    match matches.len() {
        0 => Err("unresolved_relation"),
        1 => Ok(matches[0]),
        _ => Err("ambiguous_relation"),
    }
}

fn find_unique_rule<'a>(
    index: &'a BindingDiscoveryIndex,
    legacy_id: u64,
) -> Result<(&'a SiteDiscoveryIndex, &'a DiscoveredRuleIdentity), &'static str> {
    let mut matches: Vec<(&SiteDiscoveryIndex, &DiscoveredRuleIdentity)> = Vec::new();
    for site in &index.sites {
        for rule in &site.rules {
            if rule.legacy_id == legacy_id {
                matches.push((site, rule));
            }
        }
    }
    match matches.len() {
        0 => Err("unresolved_rule"),
        1 => Ok(matches[0]),
        _ => Err("ambiguous_rule"),
    }
}

fn ensure_site_entry<'a>(
    site_bindings: &'a mut HashMap<String, SiteBindingEntry>,
    site: &SiteDiscoveryIndex,
) -> &'a mut SiteBindingEntry {
    let origin = normalize_domain_base(&site.site_origin);
    let key = site_key_from_origin(&origin);
    site_bindings.entry(key).or_insert_with(|| SiteBindingEntry {
        site_ref: SiteRef {
            site_origin: origin,
        },
        relations: HashMap::new(),
        rules: HashMap::new(),
    })
}

/// Migrate a v1 doc to v2 using discovery. Writes `*.v1.bak.*.json` next to `path` when provided.
pub(crate) fn migrate_rule_bindings_v1_to_v2(
    path: Option<&Path>,
    doc: &RuleComponentBindingsDoc,
    discovery: &BindingDiscoveryIndex,
) -> anyhow::Result<(RuleComponentBindingsDoc, MigrationReport)> {
    if doc.version >= BINDINGS_DOC_VERSION_V2
        && doc.relation_bindings.is_empty()
        && doc.rule_bindings.is_empty()
    {
        return Ok((
            doc.clone(),
            MigrationReport {
                backup_path: String::new(),
                migrated_relations: 0,
                migrated_rules: 0,
                issue_count: doc.migration_issues.len(),
            },
        ));
    }

    let backup_path = if let Some(path) = path {
        if path.exists() {
            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let backup = path.with_extension(format!("v1.bak.{ts}.json"));
            fs::copy(path, &backup).with_context(|| {
                format!(
                    "backup v1 rule bindings failed: {} -> {}",
                    path.display(),
                    backup.display()
                )
            })?;
            backup.display().to_string()
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    let normalized = normalize_rule_component_bindings_doc(doc);
    let mut out = RuleComponentBindingsDoc {
        version: BINDINGS_DOC_VERSION_V2,
        global_defaults: normalized.global_defaults.clone(),
        plugin_bindings: normalized.plugin_bindings.clone(),
        relation_bindings: HashMap::new(),
        rule_bindings: HashMap::new(),
        site_bindings: normalized.site_bindings.clone(),
        migration_issues: normalized.migration_issues.clone(),
    };

    let mut migrated_relations = 0usize;
    let mut migrated_rules = 0usize;

    for (raw_key, slots) in &doc.relation_bindings {
        let trimmed = raw_key.trim();
        let Some(legacy_id) = trimmed.parse::<u64>().ok().filter(|v| *v > 0) else {
            out.migration_issues.push(BindingMigrationIssue {
                code: "invalid_relation_key".into(),
                scope: "relation".into(),
                scope_key: raw_key.clone(),
                detail: "non-numeric or non-positive relation key".into(),
            });
            continue;
        };
        let slot_map = normalize_slot_map(slots);
        if slot_map.is_empty() {
            out.migration_issues.push(BindingMigrationIssue {
                code: "empty_slots".into(),
                scope: "relation".into(),
                scope_key: legacy_id.to_string(),
                detail: "no valid slots/component ids".into(),
            });
            continue;
        }
        match find_unique_relation(discovery, legacy_id) {
            Ok((site, relation)) => {
                let semantic_key = relation_semantic_key(&relation.relation_ref);
                let site_entry = ensure_site_entry(&mut out.site_bindings, site);
                site_entry.relations.insert(
                    semantic_key.clone(),
                    RelationBindingEntry {
                        semantic_key,
                        relation_ref: relation.relation_ref.clone(),
                        legacy_id_hint: Some(legacy_id),
                        slots: slot_map,
                    },
                );
                migrated_relations += 1;
            }
            Err(code) => {
                out.migration_issues.push(BindingMigrationIssue {
                    code: code.into(),
                    scope: "relation".into(),
                    scope_key: legacy_id.to_string(),
                    detail: "numeric relation binding quarantined until unique discovery match"
                        .into(),
                });
            }
        }
    }

    for (raw_key, slots) in &doc.rule_bindings {
        let trimmed = raw_key.trim();
        let Some(legacy_id) = trimmed.parse::<u64>().ok().filter(|v| *v > 0) else {
            out.migration_issues.push(BindingMigrationIssue {
                code: "invalid_rule_key".into(),
                scope: "rule".into(),
                scope_key: raw_key.clone(),
                detail: "non-numeric or non-positive rule key".into(),
            });
            continue;
        };
        let slot_map = normalize_slot_map(slots);
        if slot_map.is_empty() {
            out.migration_issues.push(BindingMigrationIssue {
                code: "empty_slots".into(),
                scope: "rule".into(),
                scope_key: legacy_id.to_string(),
                detail: "no valid slots/component ids".into(),
            });
            continue;
        }
        match find_unique_rule(discovery, legacy_id) {
            Ok((site, rule)) => {
                let relation_key = if let Some(rel_id) = rule.relation_legacy_id {
                    match find_unique_relation(discovery, rel_id) {
                        Ok((_, relation)) => relation_semantic_key(&relation.relation_ref),
                        Err(_) => String::new(),
                    }
                } else {
                    String::new()
                };
                let semantic_key = rule_semantic_key(&relation_key, &rule.rule_ref);
                let site_entry = ensure_site_entry(&mut out.site_bindings, site);
                site_entry.rules.insert(
                    semantic_key.clone(),
                    RuleBindingEntry {
                        semantic_key,
                        relation_key,
                        rule_ref: rule.rule_ref.clone(),
                        legacy_id_hint: Some(legacy_id),
                        slots: slot_map,
                    },
                );
                migrated_rules += 1;
            }
            Err(code) => {
                out.migration_issues.push(BindingMigrationIssue {
                    code: code.into(),
                    scope: "rule".into(),
                    scope_key: legacy_id.to_string(),
                    detail: "numeric rule binding quarantined until unique discovery match".into(),
                });
            }
        }
    }

    if let Some(path) = path {
        let encoded =
            serde_json::to_string_pretty(&out).context("encode migrated rule bindings failed")?;
        let tmp = path.with_extension("v2.tmp.json");
        fs::write(&tmp, &encoded)
            .with_context(|| format!("write migrated bindings temp failed: {}", tmp.display()))?;
        fs::rename(&tmp, path).with_context(|| {
            format!(
                "atomic replace migrated bindings failed: {} -> {}",
                tmp.display(),
                path.display()
            )
        })?;
    }

    let issue_count = out.migration_issues.len();
    Ok((
        out,
        MigrationReport {
            backup_path,
            migrated_relations,
            migrated_rules,
            issue_count,
        },
    ))
}

fn resolve_slot_map<'a>(
    slot_map: &'a HashMap<String, String>,
    task_type: &str,
    content_format: Option<&str>,
) -> Option<&'a str> {
    resolve_component_id_from_slot_map_pub(slot_map, task_type, content_format)
}

pub(crate) fn resolve_rule_bindings_with_context<'a>(
    doc: &'a RuleComponentBindingsDoc,
    ctx: &BindingResolveContext<'_>,
) -> BindingResolveResult<'a> {
    // 1) semantic / legacy rule
    if let (Some(site_key), Some(rule_key)) = (ctx.site_key, ctx.rule_semantic_key) {
        if let Some(site) = doc.site_bindings.get(site_key) {
            if let Some(rule) = site.rules.get(rule_key) {
                if let Some(component_id) =
                    resolve_slot_map(&rule.slots, ctx.task_type, ctx.content_format)
                {
                    return BindingResolveResult {
                        outcome: BindingResolveOutcome::ResolvedRule,
                        component_id: Some(component_id),
                        semantic_key: Some(rule.semantic_key.as_str()),
                    };
                }
            }
        }
    }
    if ctx.allow_legacy_numeric {
        if let Some(rid) = ctx.rule_id {
            let rid_str = rid.to_string();
            if let Some(rule_map) = doc.rule_bindings.get(&rid_str) {
                if let Some(component_id) =
                    resolve_slot_map(rule_map, ctx.task_type, ctx.content_format)
                {
                    return BindingResolveResult {
                        outcome: BindingResolveOutcome::LegacyNumericFallback,
                        component_id: Some(component_id),
                        semantic_key: None,
                    };
                }
            }
            if let Some(site_key) = ctx.site_key {
                if let Some(site) = doc.site_bindings.get(site_key) {
                    for rule in site.rules.values() {
                        if rule.legacy_id_hint == Some(rid) {
                            if let Some(component_id) =
                                resolve_slot_map(&rule.slots, ctx.task_type, ctx.content_format)
                            {
                                return BindingResolveResult {
                                    outcome: BindingResolveOutcome::LegacyNumericFallback,
                                    component_id: Some(component_id),
                                    semantic_key: Some(rule.semantic_key.as_str()),
                                };
                            }
                        }
                    }
                }
            }
        }
    }

    // 2) plugin
    if let Some(slug) = ctx.plugin_slug {
        let slug_lower = slug.trim().to_lowercase();
        if let Some(plugin_map) = doc.plugin_bindings.get(&slug_lower) {
            if let Some(component_id) =
                resolve_slot_map(plugin_map, ctx.task_type, ctx.content_format)
            {
                return BindingResolveResult {
                    outcome: BindingResolveOutcome::ResolvedPlugin,
                    component_id: Some(component_id),
                    semantic_key: None,
                };
            }
        }
    }

    // 3) semantic / legacy relation
    if let (Some(site_key), Some(relation_key)) = (ctx.site_key, ctx.relation_semantic_key) {
        if let Some(site) = doc.site_bindings.get(site_key) {
            if let Some(relation) = site.relations.get(relation_key) {
                if let Some(component_id) =
                    resolve_slot_map(&relation.slots, ctx.task_type, ctx.content_format)
                {
                    return BindingResolveResult {
                        outcome: BindingResolveOutcome::ResolvedRelation,
                        component_id: Some(component_id),
                        semantic_key: Some(relation.semantic_key.as_str()),
                    };
                }
            }
        }
    }
    if ctx.allow_legacy_numeric {
        if let Some(rid) = ctx.relation_id {
            let rid_str = rid.to_string();
            if let Some(relation_map) = doc.relation_bindings.get(&rid_str) {
                if let Some(component_id) =
                    resolve_slot_map(relation_map, ctx.task_type, ctx.content_format)
                {
                    return BindingResolveResult {
                        outcome: BindingResolveOutcome::LegacyNumericFallback,
                        component_id: Some(component_id),
                        semantic_key: None,
                    };
                }
            }
            if let Some(site_key) = ctx.site_key {
                if let Some(site) = doc.site_bindings.get(site_key) {
                    for relation in site.relations.values() {
                        if relation.legacy_id_hint == Some(rid) {
                            if let Some(component_id) =
                                resolve_slot_map(&relation.slots, ctx.task_type, ctx.content_format)
                            {
                                return BindingResolveResult {
                                    outcome: BindingResolveOutcome::LegacyNumericFallback,
                                    component_id: Some(component_id),
                                    semantic_key: Some(relation.semantic_key.as_str()),
                                };
                            }
                        }
                    }
                }
            }
        }
    }

    // 4) global
    if let Some(component_id) =
        resolve_slot_map(&doc.global_defaults, ctx.task_type, ctx.content_format)
    {
        return BindingResolveResult {
            outcome: BindingResolveOutcome::ResolvedGlobal,
            component_id: Some(component_id),
            semantic_key: None,
        };
    }

    BindingResolveResult {
        outcome: BindingResolveOutcome::Unresolved,
        component_id: None,
        semantic_key: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_relation() -> RelationRef {
        RelationRef {
            source_lang: "en_US".into(),
            target_lang: "zh_CN".into(),
            target_site_type: "virtual".into(),
            template: "default".into(),
        }
    }

    #[test]
    fn relation_semantic_key_is_stable() {
        let a = relation_semantic_key(&sample_relation());
        let mut flipped = sample_relation();
        flipped.source_lang = "EN_us".into();
        let b = relation_semantic_key(&flipped);
        assert_eq!(a, b);
        assert!(a.starts_with("relation:"));
    }

    #[test]
    fn component_semantic_key_includes_family_fields() {
        let key = component_semantic_key(&ComponentSemanticRef {
            vendor_id: "OpenAI".into(),
            template_id: "openai-compatible-chat-completions-v1".into(),
            kind: "text".into(),
            version: "1.0.0".into(),
        });
        assert!(key.starts_with("component:"));
    }

    #[test]
    fn migrate_unique_discovery_moves_numeric_into_site_bindings() {
        let mut doc = RuleComponentBindingsDoc::default();
        doc.version = 1;
        doc.relation_bindings.insert(
            "12".into(),
            HashMap::from([("plain_text".into(), "comp-rel".into())]),
        );
        doc.rule_bindings.insert(
            "128".into(),
            HashMap::from([("plain_text".into(), "comp-rule".into())]),
        );
        doc.global_defaults
            .insert("plain_text".into(), "comp-global".into());

        let discovery = BindingDiscoveryIndex {
            sites: vec![SiteDiscoveryIndex {
                site_origin: "https://shop.example".into(),
                relations: vec![DiscoveredRelationIdentity {
                    legacy_id: 12,
                    relation_ref: sample_relation(),
                }],
                rules: vec![DiscoveredRuleIdentity {
                    legacy_id: 128,
                    relation_legacy_id: Some(12),
                    rule_ref: RuleRef {
                        plugin_slug: "woocommerce".into(),
                        name: "Product".into(),
                        data_type: "post_type".into(),
                        object_name: "product".into(),
                        source_group: "content_objects".into(),
                        routing_profile: "default".into(),
                        delivery_target: "post".into(),
                        content_format: "plain_text".into(),
                    },
                }],
            }],
        };

        let (migrated, report) =
            migrate_rule_bindings_v1_to_v2(None, &doc, &discovery).expect("migrate");
        assert_eq!(migrated.version, 2);
        assert!(migrated.relation_bindings.is_empty());
        assert!(migrated.rule_bindings.is_empty());
        assert_eq!(report.migrated_relations, 1);
        assert_eq!(report.migrated_rules, 1);
        assert_eq!(report.issue_count, 0);
        assert_eq!(
            migrated.global_defaults.get("plain_text").map(String::as_str),
            Some("comp-global")
        );
        assert!(!migrated.site_bindings.is_empty());
    }

    #[test]
    fn migrate_without_discovery_quarantines_numeric_entries() {
        let mut doc = RuleComponentBindingsDoc::default();
        doc.version = 1;
        doc.relation_bindings.insert(
            "12".into(),
            HashMap::from([("plain_text".into(), "comp-rel".into())]),
        );
        let (migrated, report) =
            migrate_rule_bindings_v1_to_v2(None, &doc, &BindingDiscoveryIndex::default())
                .expect("migrate");
        assert_eq!(migrated.version, 2);
        assert_eq!(report.migrated_relations, 0);
        assert_eq!(report.issue_count, 1);
        assert_eq!(migrated.migration_issues[0].code, "unresolved_relation");
    }

    #[test]
    fn public_pack_sanitizer_strips_numeric_maps() {
        let mut doc = RuleComponentBindingsDoc::default();
        doc.relation_bindings.insert(
            "12".into(),
            HashMap::from([("plain_text".into(), "comp".into())]),
        );
        doc.plugin_bindings.insert(
            "woocommerce".into(),
            HashMap::from([("plain_text".into(), "comp-woo".into())]),
        );
        let (sanitized, warnings) = sanitize_rule_bindings_for_public_pack(&doc);
        assert!(sanitized.relation_bindings.is_empty());
        assert!(sanitized.rule_bindings.is_empty());
        assert!(sanitized.plugin_bindings.contains_key("woocommerce"));
        assert!(!warnings.is_empty());
        assert!(pack_has_cross_site_numeric_risk(&doc));
        assert!(!pack_has_cross_site_numeric_risk(&sanitized));
    }

    #[test]
    fn resolve_prefers_semantic_rule_over_plugin() {
        let relation = sample_relation();
        let rel_key = relation_semantic_key(&relation);
        let rule_ref = RuleRef {
            plugin_slug: "woocommerce".into(),
            name: "Product".into(),
            data_type: "post_type".into(),
            object_name: "product".into(),
            ..RuleRef::default()
        };
        let rule_key = rule_semantic_key(&rel_key, &rule_ref);
        let site_origin = "https://shop.example";
        let site_key = site_key_from_origin(site_origin);

        let mut doc = RuleComponentBindingsDoc {
            version: 2,
            ..Default::default()
        };
        doc.plugin_bindings.insert(
            "woocommerce".into(),
            HashMap::from([("plain_text".into(), "comp-plugin".into())]),
        );
        let mut site = SiteBindingEntry {
            site_ref: SiteRef {
                site_origin: site_origin.into(),
            },
            ..Default::default()
        };
        site.rules.insert(
            rule_key.clone(),
            RuleBindingEntry {
                semantic_key: rule_key.clone(),
                relation_key: rel_key,
                rule_ref,
                legacy_id_hint: Some(128),
                slots: HashMap::from([("plain_text".into(), "comp-rule".into())]),
            },
        );
        doc.site_bindings.insert(site_key.clone(), site);

        let result = resolve_rule_bindings_with_context(
            &doc,
            &BindingResolveContext {
                site_key: Some(&site_key),
                rule_semantic_key: Some(&rule_key),
                plugin_slug: Some("woocommerce"),
                task_type: "text",
                content_format: Some("plain_text"),
                allow_legacy_numeric: false,
                ..Default::default()
            },
        );
        assert_eq!(result.outcome, BindingResolveOutcome::ResolvedRule);
        assert_eq!(result.component_id, Some("comp-rule"));
    }

    #[test]
    fn resolve_does_not_use_foreign_site_legacy_hint() {
        let site_a = site_key_from_origin("https://a.example");
        let site_b = site_key_from_origin("https://b.example");
        let mut doc = RuleComponentBindingsDoc {
            version: 2,
            ..Default::default()
        };
        let mut site = SiteBindingEntry::default();
        site.relations.insert(
            "relation:deadbeef".into(),
            RelationBindingEntry {
                semantic_key: "relation:deadbeef".into(),
                relation_ref: sample_relation(),
                legacy_id_hint: Some(12),
                slots: HashMap::from([("plain_text".into(), "comp-a".into())]),
            },
        );
        doc.site_bindings.insert(site_a, site);

        let result = resolve_rule_bindings_with_context(
            &doc,
            &BindingResolveContext {
                site_key: Some(&site_b),
                relation_id: Some(12),
                task_type: "text",
                content_format: Some("plain_text"),
                allow_legacy_numeric: true,
                ..Default::default()
            },
        );
        assert_eq!(result.outcome, BindingResolveOutcome::Unresolved);
        assert!(result.component_id.is_none());
    }

    #[test]
    fn strip_numeric_on_import_removes_cross_site_ids() {
        let mut doc = RuleComponentBindingsDoc::default();
        doc.relation_bindings.insert(
            "99".into(),
            HashMap::from([("plain_text".into(), "x".into())]),
        );
        doc.plugin_bindings.insert(
            "acf".into(),
            HashMap::from([("plain_text".into(), "y".into())]),
        );
        let skipped = strip_numeric_maps_on_import(&mut doc);
        assert_eq!(skipped, vec!["relation:99".to_string()]);
        assert!(doc.relation_bindings.is_empty());
        assert!(doc.plugin_bindings.contains_key("acf"));
    }
}
