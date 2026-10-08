use anyhow::anyhow;

use super::*;

pub(super) fn build_task_scoped_runtime_registry(
    registry: Option<&ComponentRuntimeRegistry>,
    selected_component_id: Option<&str>,
    editable_overrides: Option<&Value>,
) -> anyhow::Result<Option<ComponentRuntimeRegistry>> {
    let task_overrides =
        crate::component_rt::loader::task_editable_overrides_to_component_overrides(
            editable_overrides,
        )?;
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
    if task_overrides.constraints_override.is_some()
        || task_overrides.request_overrides.is_some()
        || task_overrides.default_values_override.is_some()
        || task_overrides.http_limits_overrides.is_some()
    {
        crate::component_rt::loader::apply_component_instance_overrides_to_template(
            &mut runtime.template,
            Some(&task_overrides),
        )?;
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
        let err =
            build_task_scoped_runtime_registry(None, None, Some(&json!({ "temperature": 0.5 })))
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
        let err = build_task_scoped_runtime_registry(Some(&empty_registry), Some("comp-a"), None)
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("not available in runtime registry"),
            "err={err}"
        );

        // Blank/whitespace selections behave like no selection.
        let absent =
            build_task_scoped_runtime_registry(Some(&empty_registry), Some("  "), None).unwrap();
        assert!(absent.is_none());
    }

    #[test]
    fn task_scoped_bad_limits_stop_before_mutating_base_registry() {
        let runtime = ComponentRuntime {
            template: serde_json::from_value(json!({
                "id":"owned","name":"Owned","version":"1.0.0","type":"text_translation",
                "request":{"method":"POST","url":"http://127.0.0.1:1"},
                "response":{"translated_text_path":"text"},
                "constraints":{"rate_limit_qps":5,"max_file_size_mb":1},
                "editable_params":[{"path":"constraints.*"}]
            }))
            .unwrap(),
            auth_values: HashMap::new(),
            supported_business_lines: Vec::new(),
            language_map: HashMap::new(),
            supported_content_formats: Vec::new(),
            supported_formats: Vec::new(),
            key_pool: None,
            oauth_pool: None,
            oauth_manager: None,
            proxy_profile_id: None,
            runtime_max_concurrent_requests: 0,
            runtime_min_interval_ms: 200,
            runtime_concurrency_sem: None,
            runtime_last_request_at: None,
        };
        let registry = ComponentRuntimeRegistry {
            runtimes: HashMap::from([("owned".into(), runtime)]),
            ordered_ids: vec!["owned".into()],
        };
        for value in [
            json!({"constraints.rate_limit_qps":4_294_967_296u64}),
            json!({"constraints.max_file_size_mb":-1}),
            json!(["owned-private-invalid"]),
        ] {
            let error =
                build_task_scoped_runtime_registry(Some(&registry), Some("owned"), Some(&value))
                    .unwrap_err();
            assert!(error
                .downcast_ref::<crate::component_rt::loader::RuntimeConfigurationFault>()
                .is_some());
            assert_eq!(registry.runtimes["owned"].runtime_min_interval_ms, 200);
            assert_eq!(
                registry.runtimes["owned"]
                    .template
                    .constraints
                    .as_ref()
                    .unwrap()
                    .max_file_size_mb,
                Some(1)
            );
        }
    }

    #[test]
    fn task_scoped_bad_override_shape_without_selection_cannot_use_fallback() {
        for value in [
            json!(["owned-invalid"]),
            json!("owned-invalid"),
            Value::Null,
        ] {
            let result = build_task_scoped_runtime_registry(None, None, Some(&value));
            assert!(
                result.is_err(),
                "explicit damaged overrides must not disappear without selection"
            );
        }
    }

    #[test]
    fn http_editable_task_registry_applies_only_to_selected_clone() {
        let template = serde_json::from_value(json!({
            "id":"owned","name":"Owned","version":"1.0.0","type":"text_translation",
            "request":{"method":"POST","url":"http://127.0.0.1:1",
                "http_limits":{"max_response_bytes":100,"timeout_ms":1000}},
            "response":{"translated_text_path":"text"},
            "editable_params":[{"path":"request.http_limits.*"}]
        }))
        .unwrap();
        let runtime = ComponentRuntime {
            template,
            auth_values: HashMap::new(),
            supported_business_lines: vec![],
            language_map: HashMap::new(),
            supported_content_formats: vec![],
            supported_formats: vec![],
            key_pool: None,
            oauth_pool: None,
            oauth_manager: None,
            proxy_profile_id: None,
            runtime_max_concurrent_requests: 0,
            runtime_min_interval_ms: 0,
            runtime_concurrency_sem: None,
            runtime_last_request_at: None,
        };
        let registry = ComponentRuntimeRegistry {
            runtimes: HashMap::from([("owned".to_string(), runtime)]),
            ordered_ids: vec!["owned".into()],
        };
        let scoped = build_task_scoped_runtime_registry(
            Some(&registry),
            Some("owned"),
            Some(&json!({"request.http_limits.max_response_bytes":11})),
        )
        .unwrap()
        .unwrap();
        let limits = scoped.runtimes["owned"]
            .template
            .request
            .http_limits
            .as_ref()
            .unwrap();
        assert_eq!(limits.max_response_bytes, Some(11));
        assert_eq!(limits.timeout_ms, Some(1000));
        assert_eq!(
            registry.runtimes["owned"]
                .template
                .request
                .http_limits
                .as_ref()
                .unwrap()
                .max_response_bytes,
            Some(100)
        );
        assert!(build_task_scoped_runtime_registry(
            Some(&registry),
            Some("owned"),
            Some(&json!({"request.http_limits.timeout_ms":0}))
        )
        .is_err());
    }
}