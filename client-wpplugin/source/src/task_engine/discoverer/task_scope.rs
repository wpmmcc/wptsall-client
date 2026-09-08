use anyhow::anyhow;

use super::*;

pub(super) fn build_task_scoped_runtime_registry(
    registry: Option<&ComponentRuntimeRegistry>,
    selected_component_id: Option<&str>,
    editable_overrides: Option<&Value>,
) -> anyhow::Result<Option<ComponentRuntimeRegistry>> {
    let selected_component_id = selected_component_id
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let has_overrides = editable_overrides
        .and_then(|value| value.as_object())
        .map(|obj| !obj.is_empty())
        .unwrap_or(false);

    if selected_component_id.is_none() {
        if has_overrides {
            return Err(anyhow!(
                "task editable_overrides requires selected_component_id for main execution"
            ));
        }
        return Ok(None);
    }

    let selected_component_id = selected_component_id.unwrap();
    let registry = registry.ok_or_else(|| {
        anyhow!(
            "component runtime registry unavailable for selected task component '{}'",
            selected_component_id
        )
    })?;
    let base_runtime = registry
        .runtimes
        .get(selected_component_id)
        .ok_or_else(|| {
            anyhow!(
                "selected task component '{}' is not available in runtime registry",
                selected_component_id
            )
        })?;

    let mut runtime = base_runtime.clone();
    let task_overrides =
        crate::component_rt::loader::task_editable_overrides_to_component_overrides(
            editable_overrides,
        );
    if task_overrides.constraints_override.is_some()
        || task_overrides.request_overrides.is_some()
        || task_overrides.default_values_override.is_some()
    {
        crate::component_rt::loader::apply_component_instance_overrides_to_template(
            &mut runtime.template,
            Some(&task_overrides),
        );
        if let Some(ref constraints) = task_overrides.constraints_override {
            if let Some(ref supported_formats) = constraints.supported_formats {
                runtime.supported_formats = supported_formats.clone();
            }
            if let Some(ref supported_content_formats) = constraints.supported_content_formats {
                runtime.supported_content_formats = supported_content_formats.clone();
            }
        }
        let (
            runtime_max_concurrent_requests,
            runtime_min_interval_ms,
            runtime_concurrency_sem,
            runtime_last_request_at,
        ) = crate::component_rt::loader::runtime_limits_from_constraints(
            runtime.template.constraints.as_ref(),
        );
        runtime.runtime_max_concurrent_requests = runtime_max_concurrent_requests;
        runtime.runtime_min_interval_ms = runtime_min_interval_ms;
        runtime.runtime_concurrency_sem = runtime_concurrency_sem;
        runtime.runtime_last_request_at = runtime_last_request_at;
    }

    let mut runtimes = HashMap::new();
    runtimes.insert(selected_component_id.to_string(), runtime);
    Ok(Some(ComponentRuntimeRegistry {
        runtimes,
        ordered_ids: vec![selected_component_id.to_string()],
    }))
}

pub(super) fn apply_discovery_task_relation_overrides(
    relation: &DiscoveredRelation,
    task_params: &DiscoveryTaskParams,
) -> DiscoveredRelation {
    let mut effective_relation = relation.clone();
    if let Some(ref source_lang) = task_params.effective_source_lang {
        effective_relation.source_lang = source_lang.clone();
    }
    if let Some(ref target_lang) = task_params.effective_target_lang {
        effective_relation.target_lang = target_lang.clone();
    }
    effective_relation
}
