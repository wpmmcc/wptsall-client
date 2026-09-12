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

#[cfg(test)]
mod tests {
    // catalog: WEBUI-MOD-task-engine-discoverer-task-scope-rs
    // oracle: L1
    // The happy path (editable overrides applied to a scoped registry) is
    // exercised in discoverer/tests.rs; these tests pin the guard rails and
    // the relation-override semantics.
    use super::*;
    use serde_json::json;

    fn base_relation() -> DiscoveredRelation {
        serde_json::from_value(json!({
            "id": 1,
            "source_lang": "en_US",
            "target_site_id": "v_1",
            "target_site_type": "virtual",
            "target_lang": "fr_FR",
            "sync_mode": "auto"
        }))
        .unwrap()
    }

    #[test]
    fn relation_overrides_apply_only_when_present() {
        let relation = base_relation();
        let both = DiscoveryTaskParams {
            effective_source_lang: Some("de_DE".into()),
            effective_target_lang: Some("ja_JP".into()),
            ..Default::default()
        };
        let effective = apply_discovery_task_relation_overrides(&relation, &both);
        assert_eq!(effective.source_lang, "de_DE");
        assert_eq!(effective.target_lang, "ja_JP");

        let none = DiscoveryTaskParams::default();
        let untouched = apply_discovery_task_relation_overrides(&relation, &none);
        assert_eq!(untouched.source_lang, "en_US");
        assert_eq!(untouched.target_lang, "fr_FR");
    }

    #[test]
    fn task_scoped_registry_rejects_overrides_without_selected_component() {
        let err = build_task_scoped_runtime_registry(
            None,
            None,
            Some(&json!({ "temperature": 0.5 })),
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("requires selected_component_id"),
            "err={err}"
        );

        // Without overrides and without a selection the scoped registry is
        // simply absent (main execution runs unscoped).
        let absent = build_task_scoped_runtime_registry(None, None, None).unwrap();
        assert!(absent.is_none());
    }

    #[test]
    fn task_scoped_registry_requires_a_registry_and_a_known_component() {
        let err = build_task_scoped_runtime_registry(None, Some("comp-a"), None).unwrap_err();
        assert!(
            err.to_string().contains("registry unavailable"),
            "err={err}"
        );

        let empty_registry = ComponentRuntimeRegistry {
            runtimes: HashMap::new(),
            ordered_ids: Vec::new(),
        };
        let err = build_task_scoped_runtime_registry(
            Some(&empty_registry),
            Some("comp-a"),
            None,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("not available in runtime registry"),
            "err={err}"
        );

        // Blank/whitespace selections behave like no selection.
        let absent = build_task_scoped_runtime_registry(
            Some(&empty_registry),
            Some("  "),
            None,
        )
        .unwrap();
        assert!(absent.is_none());
    }
}
