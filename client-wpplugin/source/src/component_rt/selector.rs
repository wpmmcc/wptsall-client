use serde_json::{json, Value};
use std::collections::HashSet;

use crate::bindings::{
    parse_business_line_key, parse_task_type_binding_key, resolve_component_id_from_rule_bindings,
};
use crate::component_rt::loader::normalize_component_runtime_kind;
use crate::task_engine::executor::{normalize_business_line, normalize_task_content_type};
use crate::types::*;

#[derive(Clone, Copy)]
struct ArtifactSelectionHints<'a> {
    input_artifact_kind: Option<&'a str>,
    expected_output_artifact_kind: Option<&'a str>,
}

pub(crate) fn select_component_runtime_for_task_type<'a>(
    registry: Option<&'a ComponentRuntimeRegistry>,
    task_payload: &Value,
    business_line: &str,
    task_type: &str,
    component_id_override: &str,
    component_prefer_ids: &[String],
    task_type_component_bindings: Option<&TaskTypeComponentBindingsDoc>,
) -> Option<&'a ComponentRuntime> {
    let registry = registry?;
    let artifact_hints = extract_artifact_selection_hints(task_payload);

    if !component_id_override.trim().is_empty() {
        if let Some(runtime) = registry.runtimes.get(component_id_override) {
            if runtime_supports_route_and_artifacts(
                runtime,
                business_line,
                task_type,
                artifact_hints,
            ) {
                return Some(runtime);
            }
        }
    }

    if let Some(type_component_id) =
        resolve_component_id_for_task_type(task_payload, business_line, task_type)
    {
        if let Some(runtime) = registry.runtimes.get(type_component_id.trim()) {
            if runtime_supports_route_and_artifacts(
                runtime,
                business_line,
                task_type,
                artifact_hints,
            ) {
                return Some(runtime);
            }
        }
    }

    if let Some(type_component_id) = resolve_component_id_from_task_type_bindings(
        task_type_component_bindings,
        business_line,
        task_type,
    ) {
        if let Some(runtime) = registry.runtimes.get(type_component_id) {
            if runtime_supports_route_and_artifacts(
                runtime,
                business_line,
                task_type,
                artifact_hints,
            ) {
                return Some(runtime);
            }
        }
    }

    if let Some(id) = task_payload.get("component_id").and_then(|v| v.as_str()) {
        if let Some(runtime) = registry.runtimes.get(id) {
            if runtime_supports_route_and_artifacts(
                runtime,
                business_line,
                task_type,
                artifact_hints,
            ) {
                return Some(runtime);
            }
        }
    }

    for id in component_prefer_ids {
        if let Some(runtime) = registry.runtimes.get(id) {
            if runtime_supports_route_and_artifacts(
                runtime,
                business_line,
                task_type,
                artifact_hints,
            ) {
                return Some(runtime);
            }
        }
    }

    select_best_runtime_by_artifacts(
        registry
            .ordered_ids
            .iter()
            .filter_map(|id| registry.runtimes.get(id))
            .filter(|runtime| runtime_supports_route(runtime, business_line, task_type)),
        artifact_hints,
    )
}

fn runtime_supports_route_and_format(
    runtime: &ComponentRuntime,
    business_line: &str,
    task_type: &str,
    content_format: &str,
    artifact_hints: ArtifactSelectionHints<'_>,
) -> bool {
    runtime_supports_route(runtime, business_line, task_type)
        && runtime_supports_content_format(runtime, content_format)
        && runtime_artifact_match_score(runtime, artifact_hints) > 0
}

fn runtime_supports_route_format_and_file(
    runtime: &ComponentRuntime,
    business_line: &str,
    task_type: &str,
    content_format: &str,
    file_ext_hint: Option<&str>,
    artifact_hints: ArtifactSelectionHints<'_>,
) -> bool {
    if !runtime_supports_route_and_format(
        runtime,
        business_line,
        task_type,
        content_format,
        artifact_hints,
    ) {
        return false;
    }
    if let Some(ext) = file_ext_hint {
        return runtime_supports_file_format(runtime, ext);
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn select_component_runtime_for_task_type_and_format<'a>(
    registry: Option<&'a ComponentRuntimeRegistry>,
    task_payload: &Value,
    business_line: &str,
    task_type: &str,
    content_format: &str,
    component_id_override: &str,
    component_prefer_ids: &[String],
    task_type_component_bindings: Option<&TaskTypeComponentBindingsDoc>,
) -> Option<&'a ComponentRuntime> {
    let registry = registry?;
    let file_ext_hint = task_payload
        .get("__file_ext")
        .or_else(|| task_payload.get("file_ext"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());
    let artifact_hints = extract_artifact_selection_hints(task_payload);

    if !component_id_override.trim().is_empty() {
        if let Some(runtime) = registry.runtimes.get(component_id_override) {
            if runtime_supports_route_format_and_file(
                runtime,
                business_line,
                task_type,
                content_format,
                file_ext_hint,
                artifact_hints,
            ) {
                return Some(runtime);
            }
        }
    }

    if let Some(type_component_id) =
        resolve_component_id_for_task_type(task_payload, business_line, task_type)
    {
        if let Some(runtime) = registry.runtimes.get(type_component_id.trim()) {
            if runtime_supports_route_format_and_file(
                runtime,
                business_line,
                task_type,
                content_format,
                file_ext_hint,
                artifact_hints,
            ) {
                return Some(runtime);
            }
        }
    }

    if let Some(type_component_id) = resolve_component_id_from_task_type_bindings(
        task_type_component_bindings,
        business_line,
        task_type,
    ) {
        if let Some(runtime) = registry.runtimes.get(type_component_id) {
            if runtime_supports_route_format_and_file(
                runtime,
                business_line,
                task_type,
                content_format,
                file_ext_hint,
                artifact_hints,
            ) {
                return Some(runtime);
            }
        }
    }

    if let Some(id) = task_payload.get("component_id").and_then(|v| v.as_str()) {
        if let Some(runtime) = registry.runtimes.get(id) {
            if runtime_supports_route_format_and_file(
                runtime,
                business_line,
                task_type,
                content_format,
                file_ext_hint,
                artifact_hints,
            ) {
                return Some(runtime);
            }
        }
    }

    for id in component_prefer_ids {
        if let Some(runtime) = registry.runtimes.get(id) {
            if runtime_supports_route_format_and_file(
                runtime,
                business_line,
                task_type,
                content_format,
                file_ext_hint,
                artifact_hints,
            ) {
                return Some(runtime);
            }
        }
    }

    select_best_runtime_by_artifacts(
        registry
            .ordered_ids
            .iter()
            .filter_map(|id| registry.runtimes.get(id))
            .filter(|runtime| {
                runtime_supports_route_format_and_file(
                    runtime,
                    business_line,
                    task_type,
                    content_format,
                    file_ext_hint,
                    artifact_hints,
                )
            }),
        artifact_hints,
    )
}

fn extract_artifact_selection_hints(task_payload: &Value) -> ArtifactSelectionHints<'_> {
    ArtifactSelectionHints {
        input_artifact_kind: task_payload_string_hint(
            task_payload,
            &["__input_artifact_kind", "input_artifact_kind"],
        ),
        expected_output_artifact_kind: task_payload_string_hint(
            task_payload,
            &[
                "__expected_output_artifact_kind",
                "expected_output_artifact_kind",
                "output_artifact_kind",
            ],
        ),
    }
}

fn task_payload_string_hint<'a>(task_payload: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .filter_map(|key| task_payload.get(*key))
        .filter_map(Value::as_str)
        .map(str::trim)
        .find(|value| !value.is_empty())
}

fn runtime_supports_route_and_artifacts(
    runtime: &ComponentRuntime,
    business_line: &str,
    task_type: &str,
    artifact_hints: ArtifactSelectionHints<'_>,
) -> bool {
    runtime_supports_route(runtime, business_line, task_type)
        && runtime_artifact_match_score(runtime, artifact_hints) > 0
}

fn artifact_kinds_compatible(declared: &str, expected: &str) -> bool {
    if declared.eq_ignore_ascii_case(expected) {
        return true;
    }
    // Dubbing vendors declare dubbed_* while the discovery pipeline asks for translated_*.
    const ALIASES: &[(&str, &str)] = &[
        ("dubbed_video_file", "translated_video_file"),
        ("dubbed_audio_file", "translated_audio_file"),
    ];
    ALIASES.iter().any(|(left, right)| {
        (declared.eq_ignore_ascii_case(left) && expected.eq_ignore_ascii_case(right))
            || (declared.eq_ignore_ascii_case(right) && expected.eq_ignore_ascii_case(left))
    })
}

fn runtime_artifact_match_score(
    runtime: &ComponentRuntime,
    artifact_hints: ArtifactSelectionHints<'_>,
) -> u8 {
    let constraints = runtime.template.constraints.as_ref();
    let mut score: u8 = 1;

    if let Some(expected_input) = artifact_hints.input_artifact_kind {
        let declared_input = constraints
            .and_then(|c| c.input_artifact_kind.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty());
        match declared_input {
            Some(actual) if artifact_kinds_compatible(actual, expected_input) => score += 2,
            Some(_) => return 0,
            None => score += 1,
        }
    }

    if let Some(expected_output) = artifact_hints.expected_output_artifact_kind {
        let declared_outputs: Vec<&str> = constraints
            .and_then(|c| c.output_artifact_kinds.as_ref())
            .map(|values| {
                values
                    .iter()
                    .map(|value| value.trim())
                    .filter(|value| !value.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        if declared_outputs.is_empty() {
            score += 1;
        } else if declared_outputs
            .iter()
            .any(|value| artifact_kinds_compatible(value, expected_output))
        {
            score += 2;
        } else {
            return 0;
        }
    }

    score
}

/// Explicit rule bindings should still select the bound component even when its
/// declared output artifact kind is a specialty/dubbing variant.
fn runtime_supports_explicit_binding(
    runtime: &ComponentRuntime,
    business_line: &str,
    task_type: &str,
    content_format: &str,
    file_ext_hint: Option<&str>,
) -> bool {
    if !runtime_supports_route(runtime, business_line, task_type) {
        return false;
    }
    if !runtime_supports_content_format(runtime, content_format) {
        return false;
    }
    if let Some(ext) = file_ext_hint {
        return runtime_supports_file_format(runtime, ext);
    }
    true
}

fn select_best_runtime_by_artifacts<'a>(
    candidates: impl Iterator<Item = &'a ComponentRuntime>,
    artifact_hints: ArtifactSelectionHints<'_>,
) -> Option<&'a ComponentRuntime> {
    let mut best_runtime: Option<&'a ComponentRuntime> = None;
    let mut best_score: u8 = 0;

    for runtime in candidates {
        let score = runtime_artifact_match_score(runtime, artifact_hints);
        if score == 0 {
            continue;
        }
        if best_runtime.is_none() || score > best_score {
            best_runtime = Some(runtime);
            best_score = score;
        }
    }

    best_runtime
}

#[allow(dead_code)]
/// Select component runtime using rule→plugin→relation→global priority chain,
/// falling back to the existing `select_component_runtime_for_task_type` logic.
#[allow(clippy::too_many_arguments)]
pub(crate) fn select_component_with_rule_bindings<'a>(
    registry: Option<&'a ComponentRuntimeRegistry>,
    rule_bindings: Option<&RuleComponentBindingsDoc>,
    rule_id: Option<u64>,
    relation_id: Option<u64>,
    plugin_slug: Option<&str>,
    task_payload: &Value,
    business_line: &str,
    task_type: &str,
    component_id_override: &str,
    component_prefer_ids: &[String],
    task_type_component_bindings: Option<&TaskTypeComponentBindingsDoc>,
) -> Option<&'a ComponentRuntime> {
    let registry = registry?;

    // Try rule-component-bindings priority chain first
    if let Some(rule_doc) = rule_bindings {
        if let Some(component_id) = resolve_component_id_from_rule_bindings(
            rule_doc,
            rule_id,
            relation_id,
            plugin_slug,
            task_type,
            None,
        ) {
            if let Some(runtime) = registry.runtimes.get(component_id) {
                if runtime_supports_route(runtime, business_line, task_type) {
                    return Some(runtime);
                }
            }
        }
    }

    // Fall back to existing selection logic
    select_component_runtime_for_task_type(
        Some(registry),
        task_payload,
        business_line,
        task_type,
        component_id_override,
        component_prefer_ids,
        task_type_component_bindings,
    )
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn collect_required_task_types(
    declared_task_type: &str,
    text_field_units: &[TaskTextFieldUnit],
    content_subtask_units: &[TaskContentSubtaskUnit],
) -> Vec<String> {
    fn push_required_task_type(out: &mut Vec<String>, seen: &mut HashSet<String>, raw: &str) {
        let normalized = normalize_task_content_type(raw);
        if seen.insert(normalized.clone()) {
            out.push(normalized);
        }
    }

    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();

    if !declared_task_type.trim().is_empty() {
        push_required_task_type(&mut out, &mut seen, declared_task_type);
    }
    if !text_field_units.is_empty() {
        push_required_task_type(&mut out, &mut seen, "text");
    }
    for unit in content_subtask_units {
        push_required_task_type(&mut out, &mut seen, &unit.task_type);
    }
    if out.is_empty() {
        push_required_task_type(&mut out, &mut seen, "text");
    }

    let rank = |task_type: &str| match task_type {
        "text" => 0,
        "image" => 1,
        "video" => 2,
        "audio" => 3,
        "document" => 4,
        "mixed" => 5,
        _ => 6,
    };
    out.sort_by(|a, b| {
        rank(a.as_str())
            .cmp(&rank(b.as_str()))
            .then_with(|| a.cmp(b))
    });
    out
}

#[allow(dead_code)]
pub(crate) fn build_component_capability_probe(
    component_registry: Option<&ComponentRuntimeRegistry>,
    task_payload: &Value,
    business_line: &str,
    required_task_types: &[String],
    component_id_override: &str,
    component_prefer_ids: &[String],
    task_type_component_bindings: Option<&TaskTypeComponentBindingsDoc>,
) -> Value {
    let mut unsupported_task_types: Vec<String> = Vec::new();
    let mut items: Vec<Value> = Vec::new();

    for task_type in required_task_types {
        let selected = select_component_runtime_for_task_type(
            component_registry,
            task_payload,
            business_line,
            task_type,
            component_id_override,
            component_prefer_ids,
            task_type_component_bindings,
        );

        if let Some(runtime) = selected {
            items.push(json!({
                "task_type": task_type,
                "supported": true,
                "component_id": runtime.template.id,
                "component_type": normalize_component_runtime_kind(&runtime.template.kind).unwrap_or_else(|| "unknown".to_string()),
                "supported_business_lines": runtime.supported_business_lines
            }));
        } else {
            unsupported_task_types.push(task_type.clone());
            items.push(json!({
                "task_type": task_type,
                "supported": false,
                "component_id": "",
                "component_type": "",
                "supported_business_lines": Value::Array(Vec::new())
            }));
        }
    }

    json!({
        "business_line": normalize_business_line(business_line),
        "registry_loaded": component_registry.is_some(),
        "required_task_types": required_task_types,
        "unsupported_task_types": unsupported_task_types,
        "supported": unsupported_task_types.is_empty(),
        "items": items
    })
}

pub(crate) fn resolve_component_id_from_task_type_bindings<'a>(
    bindings_doc: Option<&'a TaskTypeComponentBindingsDoc>,
    business_line: &str,
    task_type: &str,
) -> Option<&'a str> {
    let doc = bindings_doc?;
    if let Some(normalized_business_line) = parse_business_line_key(business_line) {
        let key = format!(
            "{}:{}",
            normalized_business_line,
            parse_task_type_binding_key(task_type)?
        );
        if let Some(entry) = doc.business_line_task_types.get(&key) {
            let component_id = entry.component_id.trim();
            if !component_id.is_empty() {
                return Some(component_id);
            }
        }
    }
    let normalized_type = parse_task_type_binding_key(task_type)?;
    let entry = doc.task_types.get(&normalized_type)?;
    let component_id = entry.component_id.trim();
    if component_id.is_empty() {
        None
    } else {
        Some(component_id)
    }
}

pub(crate) fn resolve_component_id_for_task_type<'a>(
    task_payload: &'a Value,
    business_line: &str,
    task_type: &str,
) -> Option<&'a str> {
    let normalized_type = normalize_task_content_type(task_type);
    let normalized_business_line = parse_business_line_key(business_line).unwrap_or_default();
    let payload_obj = task_payload.as_object()?;

    let direct_key = format!("{}_component_id", normalized_type);
    if let Some(id) = payload_obj.get(&direct_key).and_then(|v| v.as_str()) {
        let trimmed = id.trim();
        if !trimmed.is_empty() {
            return Some(id);
        }
    }
    if !normalized_business_line.is_empty() {
        let direct_line_key = format!(
            "{}_{}_component_id",
            normalized_business_line, normalized_type
        );
        if let Some(id) = payload_obj.get(&direct_line_key).and_then(|v| v.as_str()) {
            let trimmed = id.trim();
            if !trimmed.is_empty() {
                return Some(id);
            }
        }
    }

    for map_key in [
        "component_hints",
        "component_by_type",
        "component_map",
        "component_ids",
        "components",
    ] {
        let Some(map) = payload_obj.get(map_key).and_then(|v| v.as_object()) else {
            continue;
        };
        let mut candidate_keys = vec![normalized_type.as_str(), task_type];
        if !normalized_business_line.is_empty() {
            candidate_keys.insert(0, normalized_business_line.as_str());
            let scoped_normalized = format!("{}:{}", normalized_business_line, normalized_type);
            let scoped_raw = format!("{}:{}", normalized_business_line, task_type);
            if let Some(id) = map.get(scoped_normalized.as_str()).and_then(|v| v.as_str()) {
                let trimmed = id.trim();
                if !trimmed.is_empty() {
                    return Some(id);
                }
            }
            if let Some(id) = map.get(scoped_raw.as_str()).and_then(|v| v.as_str()) {
                let trimmed = id.trim();
                if !trimmed.is_empty() {
                    return Some(id);
                }
            }
        }
        for key in candidate_keys {
            if let Some(id) = map.get(key).and_then(|v| v.as_str()) {
                let trimmed = id.trim();
                if !trimmed.is_empty() {
                    return Some(id);
                }
            }
        }
    }

    None
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn task_priority(task: &ClientTask) -> i32 {
    if let Some(ref p) = task.priority {
        // Handle both numeric and string priority values.
        if let Some(n) = p.as_i64() {
            return i32::try_from(n).unwrap_or(0);
        }
        if let Some(s) = p.as_str() {
            return match s {
                "critical" => 100,
                "high" => 75,
                "normal" => 50,
                "low" => 25,
                _ => s.parse::<i32>().unwrap_or(50),
            };
        }
    }

    task.payload
        .get("priority")
        .and_then(|v| v.as_i64())
        .and_then(|v| i32::try_from(v).ok())
        .unwrap_or(0)
}

/// Check if a component runtime supports the given WP content format.
/// Empty supported_content_formats list = supports all formats (backward compatible).
fn runtime_supports_content_format(runtime: &ComponentRuntime, content_format: &str) -> bool {
    if runtime.supported_content_formats.is_empty() {
        return true;
    }
    runtime
        .supported_content_formats
        .iter()
        .any(|f| f == content_format)
}

/// Check if a component runtime supports the given file extension.
/// Empty supported_formats list = supports all formats (backward compatible).
fn runtime_supports_file_format(runtime: &ComponentRuntime, file_ext: &str) -> bool {
    if runtime.supported_formats.is_empty() {
        return true;
    }
    let ext_lower = file_ext.trim_start_matches('.').to_lowercase();
    runtime
        .supported_formats
        .iter()
        .any(|f| f.to_lowercase() == ext_lower)
}

/// Select component runtime with content-format awareness.
/// Uses the existing rule-binding priority chain but only returns a runtime when
/// both route and content_format are compatible.
#[allow(clippy::too_many_arguments)]
pub(crate) fn select_component_with_format_awareness<'a>(
    registry: Option<&'a ComponentRuntimeRegistry>,
    rule_bindings: Option<&RuleComponentBindingsDoc>,
    rule_id: Option<u64>,
    relation_id: Option<u64>,
    plugin_slug: Option<&str>,
    task_payload: &Value,
    business_line: &str,
    task_type: &str,
    content_format: &str,
    component_id_override: &str,
    component_prefer_ids: &[String],
    task_type_component_bindings: Option<&TaskTypeComponentBindingsDoc>,
) -> Option<&'a ComponentRuntime> {
    let registry = registry?;
    let file_ext_hint = task_payload
        .get("__file_ext")
        .or_else(|| task_payload.get("file_ext"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());

    if let Some(rule_doc) = rule_bindings {
        if let Some(component_id) = resolve_component_id_from_rule_bindings(
            rule_doc,
            rule_id,
            relation_id,
            plugin_slug,
            task_type,
            Some(content_format),
        ) {
            if let Some(runtime) = registry.runtimes.get(component_id) {
                if runtime_supports_explicit_binding(
                    runtime,
                    business_line,
                    task_type,
                    content_format,
                    file_ext_hint,
                ) {
                    return Some(runtime);
                }
            }
        }
    }

    select_component_runtime_for_task_type_and_format(
        Some(registry),
        task_payload,
        business_line,
        task_type,
        content_format,
        component_id_override,
        component_prefer_ids,
        task_type_component_bindings,
    )
}

/// Build a JSON summary of all component capabilities (for the Web UI).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn build_format_capability_summary(registry: &ComponentRuntimeRegistry) -> Value {
    let mut items = Vec::new();
    for id in &registry.ordered_ids {
        if let Some(rt) = registry.runtimes.get(id) {
            items.push(json!({
                "id": rt.template.id,
                "name": rt.template.name,
                "kind": rt.template.kind,
                "supported_business_lines": rt.supported_business_lines,
                "supported_content_formats": rt.supported_content_formats,
                "supported_formats": rt.supported_formats,
                "constraints": {
                    "max_input_chars": rt.template.constraints.as_ref()
                        .and_then(|c| c.max_input_chars),
                    "max_file_size_mb": rt.template.constraints.as_ref()
                        .and_then(|c| c.max_file_size_mb),
                    "split_strategy": rt.template.constraints.as_ref()
                        .and_then(|c| c.split_strategy.clone()),
                    "input_artifact_kind": rt.template.constraints.as_ref()
                        .and_then(|c| c.input_artifact_kind.clone()),
                    "output_artifact_kinds": rt.template.constraints.as_ref()
                        .and_then(|c| c.output_artifact_kinds.clone()),
                }
            }));
        }
    }

    // Build format → component_id mapping
    let mut format_map: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for id in &registry.ordered_ids {
        if let Some(rt) = registry.runtimes.get(id) {
            if rt.supported_content_formats.is_empty() {
                // Supports all text formats
                for fmt in &[
                    "plain_text",
                    "rich_html",
                    "json_structured",
                    "serialized_php",
                ] {
                    format_map
                        .entry(fmt.to_string())
                        .or_default()
                        .push(rt.template.id.clone());
                }
            } else {
                for fmt in &rt.supported_content_formats {
                    if fmt == "media_ref" {
                        // For media_ref, subdivide by component kind
                        let kind_suffix = match rt.template.kind.as_str() {
                            "image_translation" | "image" => "image",
                            "video_translation" | "video" => "video",
                            "audio_translation" | "audio" => "audio",
                            "document_translation" | "document" => "document",
                            _ => "unknown",
                        };
                        format_map
                            .entry(format!("media_ref:{}", kind_suffix))
                            .or_default()
                            .push(rt.template.id.clone());
                    } else {
                        format_map
                            .entry(fmt.clone())
                            .or_default()
                            .push(rt.template.id.clone());
                    }
                }
            }
        }
    }

    json!({
        "components": items,
        "format_component_map": format_map,
    })
}

fn runtime_supports_business_line(runtime: &ComponentRuntime, business_line: &str) -> bool {
    let normalized_business_line = parse_business_line_key(business_line).unwrap_or_default();
    if normalized_business_line.is_empty() {
        return true;
    }
    if runtime.supported_business_lines.is_empty() {
        return true;
    }
    runtime
        .supported_business_lines
        .iter()
        .any(|line| line == &normalized_business_line)
}

fn runtime_supports_task_type(runtime: &ComponentRuntime, task_type: &str) -> bool {
    let requested_type = normalize_task_content_type(task_type);
    let Some(runtime_type) = normalize_component_runtime_kind(&runtime.template.kind) else {
        return false;
    };
    // "mixed" is a task-splitting hint (task may contain multiple subtasks),
    // not a valid component/template runtime kind. Components must be precise.
    if requested_type == "mixed" {
        return false;
    }
    runtime_type == requested_type
}

fn runtime_supports_route(
    runtime: &ComponentRuntime,
    business_line: &str,
    task_type: &str,
) -> bool {
    runtime_supports_business_line(runtime, business_line)
        && runtime_supports_task_type(runtime, task_type)
}

#[cfg(test)]
mod tests;
