use super::*;
use serde_json::json;
use std::collections::HashMap;

fn make_text_runtime(id: &str) -> ComponentRuntime {
    ComponentRuntime {
        template: ComponentTemplate {
            id: id.to_string(),
            name: format!("Text Component {}", id),
            version: "1.0".to_string(),
            kind: "text".to_string(),
            client_contract: None,
            default_values: None,
            auth: None,
            prepare: None,
            request: ComponentRequest {
                method: "POST".to_string(),
                url: "https://api.example.com/translate".to_string(),
                headers: None,
                body: None,
                body_type: None,
                response_type: None,
            },
            response: ComponentResponse {
                translated_text_path: Some("data.text".to_string()),
                error_path: None,
                translated_ref_path: None,
                translated_media_ref_path: None,
                translated_image_ref_path: None,
                translated_video_ref_path: None,
                translated_audio_ref_path: None,
                translated_document_ref_path: None,
            },
            async_poll: None,
            source_upload: None,
            sign: None,
            constraints: None,
            editable_params: vec![],
            translation_modes: vec![],
        },
        auth_values: HashMap::new(),
        supported_business_lines: Vec::new(),
        language_map: HashMap::new(),
        supported_content_formats: Vec::new(),
        supported_formats: Vec::new(),
        key_pool: None,
        oauth_pool: None,
        oauth_manager: None,
        runtime_max_concurrent_requests: 0,
        runtime_min_interval_ms: 0,
        runtime_concurrency_sem: None,
        runtime_last_request_at: None,
        proxy_profile_id: None,
    }
}

fn make_image_runtime(id: &str) -> ComponentRuntime {
    let mut rt = make_text_runtime(id);
    rt.template.kind = "image".to_string();
    rt
}

fn set_artifact_constraints(
    runtime: &mut ComponentRuntime,
    input_artifact_kind: &str,
    output_artifact_kinds: &[&str],
) {
    runtime.template.constraints = Some(ComponentConstraints {
        input_artifact_kind: Some(input_artifact_kind.to_string()),
        output_artifact_kinds: Some(
            output_artifact_kinds
                .iter()
                .map(|value| value.to_string())
                .collect(),
        ),
        ..Default::default()
    });
}

fn make_mixed_runtime(id: &str) -> ComponentRuntime {
    let mut rt = make_text_runtime(id);
    rt.template.kind = "mixed".to_string();
    rt
}

fn make_registry(runtimes: Vec<ComponentRuntime>) -> ComponentRuntimeRegistry {
    let ordered_ids: Vec<String> = runtimes.iter().map(|r| r.template.id.clone()).collect();
    let runtimes_map: HashMap<String, ComponentRuntime> = runtimes
        .into_iter()
        .map(|r| (r.template.id.clone(), r))
        .collect();
    ComponentRuntimeRegistry {
        runtimes: runtimes_map,
        ordered_ids,
    }
}

// -----------------------------------------------------------------------
// task_priority
// -----------------------------------------------------------------------

#[test]
fn task_priority_uses_field_first() {
    let task = ClientTask {
        task_id: 1,
        payload: json!({"priority": 99}),
        source_lang: None,
        target_lang: None,
        priority: Some(json!(5)),
        retry_count: None,
        template: None,
        relation_id: None,
        target_type: None,
        updated_at: None,
    };
    assert_eq!(task_priority(&task), 5);
}

#[test]
fn task_priority_string_high() {
    let task = ClientTask {
        task_id: 1,
        payload: json!({}),
        source_lang: None,
        target_lang: None,
        priority: Some(json!("high")),
        retry_count: None,
        template: None,
        relation_id: None,
        target_type: None,
        updated_at: None,
    };
    assert_eq!(task_priority(&task), 75);
}

#[test]
fn task_priority_falls_back_to_payload() {
    let task = ClientTask {
        task_id: 1,
        payload: json!({"priority": 42}),
        source_lang: None,
        target_lang: None,
        priority: None,
        retry_count: None,
        template: None,
        relation_id: None,
        target_type: None,
        updated_at: None,
    };
    assert_eq!(task_priority(&task), 42);
}

#[test]
fn task_priority_defaults_to_zero() {
    let task = ClientTask {
        task_id: 1,
        payload: json!({}),
        source_lang: None,
        target_lang: None,
        priority: None,
        retry_count: None,
        template: None,
        relation_id: None,
        target_type: None,
        updated_at: None,
    };
    assert_eq!(task_priority(&task), 0);
}

// -----------------------------------------------------------------------
// select_component_runtime_for_task_type - priority order
// -----------------------------------------------------------------------

#[test]
fn select_override_highest_priority() {
    let rt_a = make_text_runtime("comp-a");
    let rt_b = make_text_runtime("comp-b");
    let registry = make_registry(vec![rt_a, rt_b]);

    let selected = select_component_runtime_for_task_type(
        Some(&registry),
        &json!({}),
        "",
        "text",
        "comp-b", // override
        &[],
        None,
    );
    assert_eq!(selected.unwrap().template.id, "comp-b");
}

#[test]
fn select_payload_component_id() {
    let rt_a = make_text_runtime("comp-a");
    let rt_b = make_text_runtime("comp-b");
    let registry = make_registry(vec![rt_a, rt_b]);

    let selected = select_component_runtime_for_task_type(
        Some(&registry),
        &json!({"component_id": "comp-b"}),
        "",
        "text",
        "", // no override
        &[],
        None,
    );
    assert_eq!(selected.unwrap().template.id, "comp-b");
}

#[test]
fn select_prefer_list() {
    let rt_a = make_text_runtime("comp-a");
    let rt_b = make_text_runtime("comp-b");
    let registry = make_registry(vec![rt_a, rt_b]);

    let selected = select_component_runtime_for_task_type(
        Some(&registry),
        &json!({}),
        "",
        "text",
        "",
        &["comp-b".to_string()],
        None,
    );
    assert_eq!(selected.unwrap().template.id, "comp-b");
}

#[test]
fn select_falls_back_to_first_ordered() {
    let rt_a = make_text_runtime("comp-a");
    let rt_b = make_text_runtime("comp-b");
    let registry = make_registry(vec![rt_a, rt_b]);

    let selected = select_component_runtime_for_task_type(
        Some(&registry),
        &json!({}),
        "",
        "text",
        "",
        &[],
        None,
    );
    // Should select the first in ordered_ids
    assert_eq!(selected.unwrap().template.id, "comp-a");
}

#[test]
fn select_none_registry_returns_none() {
    let selected =
        select_component_runtime_for_task_type(None, &json!({}), "", "text", "", &[], None);
    assert!(selected.is_none());
}

#[test]
fn select_type_mismatch_skips_component() {
    let rt_image = make_image_runtime("comp-image");
    let registry = make_registry(vec![rt_image]);

    let selected = select_component_runtime_for_task_type(
        Some(&registry),
        &json!({}),
        "",
        "text", // looking for text, only image available
        "",
        &[],
        None,
    );
    assert!(selected.is_none());
}

#[test]
fn select_mixed_component_matches_nothing() {
    let rt_mixed = make_mixed_runtime("comp-mixed");
    let registry = make_registry(vec![rt_mixed]);

    for task_type in &["text", "image", "video", "audio", "document"] {
        let selected = select_component_runtime_for_task_type(
            Some(&registry),
            &json!({}),
            "",
            task_type,
            "",
            &[],
            None,
        );
        assert!(
            selected.is_none(),
            "mixed should NOT match task_type={}",
            task_type
        );
    }
}

// -----------------------------------------------------------------------
// select with task_type_component_bindings
// -----------------------------------------------------------------------

#[test]
fn select_from_task_type_bindings() {
    let rt_a = make_text_runtime("comp-a");
    let rt_b = make_text_runtime("comp-b");
    let registry = make_registry(vec![rt_a, rt_b]);

    let mut bindings = TaskTypeComponentBindingsDoc::default();
    bindings.task_types.insert(
        "text".to_string(),
        TaskTypeComponentBindingEntry {
            component_id: "comp-b".to_string(),
        },
    );

    let selected = select_component_runtime_for_task_type(
        Some(&registry),
        &json!({}),
        "",
        "text",
        "",
        &[],
        Some(&bindings),
    );
    assert_eq!(selected.unwrap().template.id, "comp-b");
}

// -----------------------------------------------------------------------
// select with business_line filtering
// -----------------------------------------------------------------------

#[test]
fn select_filters_by_business_line_via_prefer_list() {
    let mut rt_post = make_text_runtime("comp-post");
    rt_post.supported_business_lines = vec!["post_content".to_string()];
    let mut rt_theme = make_text_runtime("comp-theme");
    rt_theme.supported_business_lines = vec!["theme_i18n".to_string()];

    let registry = make_registry(vec![rt_post, rt_theme]);

    // Use prefer list to direct selection to comp-theme
    let selected = select_component_runtime_for_task_type(
        Some(&registry),
        &json!({}),
        "theme",
        "text",
        "",
        &["comp-theme".to_string()],
        None,
    );
    assert_eq!(selected.unwrap().template.id, "comp-theme");

    // comp-post should not match theme business line via prefer list
    let selected = select_component_runtime_for_task_type(
        Some(&registry),
        &json!({}),
        "theme",
        "text",
        "",
        &["comp-post".to_string()],
        None,
    );
    // comp-post doesn't support theme, so it should skip it
    assert!(selected.is_none() || selected.unwrap().template.id == "comp-theme");
}

#[test]
fn select_business_line_empty_matches_any() {
    let mut rt = make_text_runtime("comp-restricted");
    rt.supported_business_lines = vec!["post_content".to_string()];

    let registry = make_registry(vec![rt]);

    // Empty business line should match any runtime
    let selected = select_component_runtime_for_task_type(
        Some(&registry),
        &json!({}),
        "",
        "text",
        "",
        &[],
        None,
    );
    assert!(selected.is_some());
}

// -----------------------------------------------------------------------
// collect_required_task_types
// -----------------------------------------------------------------------

#[test]
fn collect_required_types_from_declared() {
    let result = collect_required_task_types("text", &[], &[]);
    assert_eq!(result, vec!["text"]);
}

#[test]
fn collect_required_types_defaults_to_text() {
    let result = collect_required_task_types("", &[], &[]);
    assert_eq!(result, vec!["text"]);
}

#[test]
fn collect_required_types_includes_subtask_types() {
    let subtasks = vec![
        TaskContentSubtaskUnit {
            task_type: "image".to_string(),
            key: "img1".to_string(),
            source_text: String::new(),
            source_payload: json!(null),
            content_format: String::new(),
        },
        TaskContentSubtaskUnit {
            task_type: "video".to_string(),
            key: "vid1".to_string(),
            source_text: String::new(),
            source_payload: json!(null),
            content_format: String::new(),
        },
    ];
    let text_fields = vec![TaskTextFieldUnit {
        key: "title".to_string(),
        source_text: "hi".to_string(),
    }];
    let result = collect_required_task_types("", &text_fields, &subtasks);
    // Should be sorted: text, image, video
    assert_eq!(result, vec!["text", "image", "video"]);
}

#[test]
fn collect_required_types_deduplicates() {
    let subtasks = vec![TaskContentSubtaskUnit {
        task_type: "text".to_string(),
        key: "s1".to_string(),
        source_text: String::new(),
        source_payload: json!(null),
        content_format: String::new(),
    }];
    let text_fields = vec![TaskTextFieldUnit {
        key: "t1".to_string(),
        source_text: "hi".to_string(),
    }];
    let result = collect_required_task_types("text", &text_fields, &subtasks);
    assert_eq!(result, vec!["text"]);
}

// -----------------------------------------------------------------------
// runtime_supports_task_type
// -----------------------------------------------------------------------

#[test]
fn runtime_supports_same_type() {
    let rt = make_text_runtime("comp-1");
    assert!(runtime_supports_task_type(&rt, "text"));
    assert!(!runtime_supports_task_type(&rt, "image"));
}

#[test]
fn runtime_mixed_supports_none() {
    let rt = make_mixed_runtime("comp-m");
    assert!(!runtime_supports_task_type(&rt, "text"));
    assert!(!runtime_supports_task_type(&rt, "image"));
    assert!(!runtime_supports_task_type(&rt, "video"));
    assert!(!runtime_supports_task_type(&rt, "audio"));
    assert!(!runtime_supports_task_type(&rt, "document"));
}

#[test]
fn runtime_supports_business_line_empty_matches_all() {
    let rt = make_text_runtime("comp-1");
    assert!(runtime_supports_business_line(&rt, ""));
    assert!(runtime_supports_business_line(&rt, "post"));
    assert!(runtime_supports_business_line(&rt, "theme"));
}

#[test]
fn runtime_supports_business_line_filtered() {
    let mut rt = make_text_runtime("comp-1");
    rt.supported_business_lines = vec!["post_content".to_string()];
    assert!(runtime_supports_business_line(&rt, "post"));
    assert!(!runtime_supports_business_line(&rt, "theme"));
}

// -----------------------------------------------------------------------
// resolve_component_id_for_task_type from payload
// -----------------------------------------------------------------------

#[test]
fn resolve_component_from_payload_direct_key() {
    let payload = json!({"text_component_id": "comp-direct"});
    let result = resolve_component_id_for_task_type(&payload, "", "text");
    assert_eq!(result, Some("comp-direct"));
}

#[test]
fn resolve_component_from_payload_map() {
    let payload = json!({"component_hints": {"text": "comp-hint"}});
    let result = resolve_component_id_for_task_type(&payload, "", "text");
    assert_eq!(result, Some("comp-hint"));
}

#[test]
fn resolve_component_from_payload_empty_returns_none() {
    let payload = json!({"text_component_id": "  "});
    let result = resolve_component_id_for_task_type(&payload, "", "text");
    assert_eq!(result, None);
}

// -----------------------------------------------------------------------
// resolve_component_id_from_task_type_bindings
// -----------------------------------------------------------------------

#[test]
fn resolve_from_bindings_simple() {
    let mut bindings = TaskTypeComponentBindingsDoc::default();
    bindings.task_types.insert(
        "text".to_string(),
        TaskTypeComponentBindingEntry {
            component_id: "comp-bound".to_string(),
        },
    );
    let result = resolve_component_id_from_task_type_bindings(Some(&bindings), "", "text");
    assert_eq!(result, Some("comp-bound"));
}

#[test]
fn resolve_from_bindings_business_line_scoped() {
    let mut bindings = TaskTypeComponentBindingsDoc::default();
    bindings.business_line_task_types.insert(
        "post_content:text".to_string(),
        TaskTypeComponentBindingEntry {
            component_id: "comp-post-text".to_string(),
        },
    );
    let result = resolve_component_id_from_task_type_bindings(Some(&bindings), "post", "text");
    assert_eq!(result, Some("comp-post-text"));
}

#[test]
fn resolve_from_bindings_none_doc() {
    let result = resolve_component_id_from_task_type_bindings(None, "", "text");
    assert_eq!(result, None);
}

// -----------------------------------------------------------------------
// runtime_supports_content_format
// -----------------------------------------------------------------------

#[test]
fn content_format_empty_matches_all() {
    let rt = make_text_runtime("comp-1");
    assert!(runtime_supports_content_format(&rt, "plain_text"));
    assert!(runtime_supports_content_format(&rt, "rich_html"));
    assert!(runtime_supports_content_format(&rt, "media_ref"));
}

#[test]
fn content_format_specific_matches() {
    let mut rt = make_text_runtime("comp-1");
    rt.supported_content_formats = vec!["plain_text".to_string(), "rich_html".to_string()];
    assert!(runtime_supports_content_format(&rt, "plain_text"));
    assert!(runtime_supports_content_format(&rt, "rich_html"));
    assert!(!runtime_supports_content_format(&rt, "json_structured"));
    assert!(!runtime_supports_content_format(&rt, "media_ref"));
}

#[test]
fn select_with_format_awareness_prefers_compatible_runtime() {
    let mut rt_plain = make_text_runtime("comp-plain");
    rt_plain.supported_content_formats = vec!["plain_text".to_string()];
    let mut rt_rich = make_text_runtime("comp-rich");
    rt_rich.supported_content_formats = vec!["rich_html".to_string()];
    let registry = make_registry(vec![rt_plain, rt_rich]);

    let selected = select_component_with_format_awareness(
        Some(&registry),
        None,
        None,
        None,
        None,
        &json!({}),
        "post",
        "text",
        "rich_html",
        "",
        &[],
        None,
    )
    .expect("expected a compatible component");
    assert_eq!(selected.template.id, "comp-rich");
}

#[test]
fn select_with_format_awareness_override_mismatch_is_skipped() {
    let mut rt_plain = make_text_runtime("comp-plain");
    rt_plain.supported_content_formats = vec!["plain_text".to_string()];
    let mut rt_rich = make_text_runtime("comp-rich");
    rt_rich.supported_content_formats = vec!["rich_html".to_string()];
    let registry = make_registry(vec![rt_plain, rt_rich]);

    let selected = select_component_with_format_awareness(
        Some(&registry),
        None,
        None,
        None,
        None,
        &json!({}),
        "post",
        "text",
        "rich_html",
        "comp-plain",
        &[],
        None,
    )
    .expect("expected fallback to a format-compatible component");
    assert_eq!(selected.template.id, "comp-rich");
}

#[test]
fn select_with_format_awareness_returns_none_when_no_compatible_runtime() {
    let mut rt_plain = make_text_runtime("comp-plain");
    rt_plain.supported_content_formats = vec!["plain_text".to_string()];
    let registry = make_registry(vec![rt_plain]);

    let mut rule_bindings = RuleComponentBindingsDoc::default();
    rule_bindings.rule_bindings.insert(
        "9".to_string(),
        HashMap::from([("text".to_string(), "comp-plain".to_string())]),
    );

    let selected = select_component_with_format_awareness(
        Some(&registry),
        Some(&rule_bindings),
        Some(9),
        None,
        None,
        &json!({}),
        "post",
        "text",
        "rich_html",
        "",
        &[],
        None,
    );
    assert!(
        selected.is_none(),
        "no rich_html-compatible runtime should be returned"
    );
}

#[test]
fn select_with_format_awareness_respects_rich_html_binding_slot() {
    let mut rt_plain = make_text_runtime("comp-plain");
    rt_plain.supported_content_formats = vec!["plain_text".to_string()];
    let mut rt_rich = make_text_runtime("comp-rich");
    rt_rich.supported_content_formats = vec!["rich_html".to_string()];
    let registry = make_registry(vec![rt_plain, rt_rich]);

    let mut rule_bindings = RuleComponentBindingsDoc::default();
    rule_bindings.rule_bindings.insert(
        "9".to_string(),
        HashMap::from([("rich_html".to_string(), "comp-rich".to_string())]),
    );

    let selected = select_component_with_format_awareness(
        Some(&registry),
        Some(&rule_bindings),
        Some(9),
        None,
        None,
        &json!({}),
        "post",
        "text",
        "rich_html",
        "",
        &[],
        None,
    )
    .expect("expected rich_html binding to select comp-rich");
    assert_eq!(selected.template.id, "comp-rich");
}

#[test]
fn select_with_format_awareness_respects_media_subtype_binding_slot() {
    let mut rt_img = make_image_runtime("comp-image");
    rt_img.supported_content_formats = vec!["media_ref".to_string()];
    let mut rt_video = make_image_runtime("comp-video");
    rt_video.template.kind = "video_translation".to_string();
    rt_video.supported_content_formats = vec!["media_ref".to_string()];
    let registry = make_registry(vec![rt_img, rt_video]);

    let mut rule_bindings = RuleComponentBindingsDoc::default();
    rule_bindings.rule_bindings.insert(
        "9".to_string(),
        HashMap::from([("media_ref:video".to_string(), "comp-video".to_string())]),
    );

    let selected = select_component_with_format_awareness(
        Some(&registry),
        Some(&rule_bindings),
        Some(9),
        None,
        None,
        &json!({}),
        "post",
        "video",
        "media_ref",
        "",
        &[],
        None,
    )
    .expect("expected media_ref:video binding to select comp-video");
    assert_eq!(selected.template.id, "comp-video");
}

#[test]
fn select_with_format_awareness_uses_task_type_bindings_fallback() {
    let mut rt_plain = make_text_runtime("comp-plain");
    rt_plain.supported_content_formats = vec!["plain_text".to_string()];
    let registry = make_registry(vec![rt_plain]);

    let mut bindings = TaskTypeComponentBindingsDoc::default();
    bindings.task_types.insert(
        "text".to_string(),
        TaskTypeComponentBindingEntry {
            component_id: "comp-plain".to_string(),
        },
    );

    let selected = select_component_with_format_awareness(
        Some(&registry),
        None,
        None,
        None,
        None,
        &json!({}),
        "post_content",
        "text",
        "plain_text",
        "",
        &[],
        Some(&bindings),
    )
    .expect("expected task-type binding fallback to select comp-plain");
    assert_eq!(selected.template.id, "comp-plain");
}

#[test]
fn select_with_format_awareness_respects_file_extension_hint() {
    let mut rt_jpg = make_image_runtime("comp-jpg");
    rt_jpg.supported_content_formats = vec!["media_ref".to_string()];
    rt_jpg.supported_formats = vec!["jpg".to_string()];

    let mut rt_png = make_image_runtime("comp-png");
    rt_png.supported_content_formats = vec!["media_ref".to_string()];
    rt_png.supported_formats = vec!["png".to_string()];

    let registry = make_registry(vec![rt_jpg, rt_png]);
    let selected = select_component_with_format_awareness(
        Some(&registry),
        None,
        None,
        None,
        None,
        &json!({ "__file_ext": "png" }),
        "post_content",
        "image",
        "media_ref",
        "",
        &[],
        None,
    )
    .expect("expected png-capable runtime to be selected");
    assert_eq!(selected.template.id, "comp-png");
}

#[test]
fn select_with_format_awareness_prefers_exact_artifact_match_over_generic() {
    let mut rt_generic = make_image_runtime("comp-generic-image");
    rt_generic.supported_content_formats = vec!["media_ref".to_string()];

    let mut rt_translated = make_image_runtime("comp-translated-image");
    rt_translated.supported_content_formats = vec!["media_ref".to_string()];
    set_artifact_constraints(&mut rt_translated, "image_file", &["translated_image_file"]);

    let mut rt_ocr = make_image_runtime("comp-ocr-image");
    rt_ocr.supported_content_formats = vec!["media_ref".to_string()];
    set_artifact_constraints(&mut rt_ocr, "image_file", &["translated_text_content"]);

    let registry = make_registry(vec![rt_generic, rt_ocr, rt_translated]);
    let selected = select_component_with_format_awareness(
        Some(&registry),
        None,
        None,
        None,
        None,
        &json!({
            "__input_artifact_kind": "image_file",
            "__expected_output_artifact_kind": "translated_image_file"
        }),
        "post_content",
        "image",
        "media_ref",
        "",
        &[],
        None,
    )
    .expect("expected translated-image runtime");

    assert_eq!(selected.template.id, "comp-translated-image");
}

#[test]
fn select_with_format_awareness_accepts_dubbed_video_as_translated_alias() {
    let mut rt_did = make_image_runtime("comp-did-video");
    rt_did.template.kind = "video_translation".to_string();
    rt_did.supported_content_formats = vec!["media_ref".to_string()];
    set_artifact_constraints(&mut rt_did, "video_file", &["dubbed_video_file"]);
    let registry = make_registry(vec![rt_did]);

    let selected = select_component_with_format_awareness(
        Some(&registry),
        None,
        None,
        None,
        None,
        &json!({
            "__input_artifact_kind": "video_file",
            "__expected_output_artifact_kind": "translated_video_file"
        }),
        "post_content",
        "video",
        "media_ref",
        "",
        &[],
        None,
    )
    .expect("dubbed_video_file should alias translated_video_file");
    assert_eq!(selected.template.id, "comp-did-video");
}

#[test]
fn select_with_format_awareness_honors_explicit_binding_despite_specialty_artifact() {
    let mut rt_ocr = make_image_runtime("comp-ocr-structured");
    rt_ocr.supported_content_formats = vec!["media_ref".to_string()];
    set_artifact_constraints(&mut rt_ocr, "image_file", &["structured_json"]);
    let registry = make_registry(vec![rt_ocr]);

    let mut rule_bindings = RuleComponentBindingsDoc::default();
    rule_bindings.global_defaults.insert(
        "media_ref:image".to_string(),
        "comp-ocr-structured".to_string(),
    );

    let selected = select_component_with_format_awareness(
        Some(&registry),
        Some(&rule_bindings),
        None,
        None,
        None,
        &json!({
            "__input_artifact_kind": "image_file",
            "__expected_output_artifact_kind": "translated_image_file"
        }),
        "post_content",
        "image",
        "media_ref",
        "",
        &[],
        None,
    )
    .expect("explicit binding should win over specialty artifact mismatch");
    assert_eq!(selected.template.id, "comp-ocr-structured");
}

#[test]
fn select_task_type_prefers_exact_i18n_bundle_runtime() {
    let mut rt_generic = make_text_runtime("comp-generic-text");
    rt_generic.supported_business_lines = vec!["plugin_i18n".to_string()];

    let mut rt_i18n = make_text_runtime("comp-i18n-bundle");
    rt_i18n.supported_business_lines = vec!["plugin_i18n".to_string()];
    set_artifact_constraints(&mut rt_i18n, "i18n_bundle", &["translated_i18n_bundle"]);

    let registry = make_registry(vec![rt_generic, rt_i18n]);
    let selected = select_component_runtime_for_task_type(
        Some(&registry),
        &json!({
            "__input_artifact_kind": "i18n_bundle",
            "__expected_output_artifact_kind": "translated_i18n_bundle"
        }),
        "plugin_i18n",
        "text",
        "",
        &[],
        None,
    )
    .expect("expected i18n-bundle runtime");

    assert_eq!(selected.template.id, "comp-i18n-bundle");
}

// -----------------------------------------------------------------------
// runtime_supports_file_format
// -----------------------------------------------------------------------

#[test]
fn file_format_empty_matches_all() {
    let rt = make_text_runtime("comp-1");
    assert!(runtime_supports_file_format(&rt, "jpg"));
    assert!(runtime_supports_file_format(&rt, ".png"));
}

#[test]
fn file_format_specific_matches() {
    let mut rt = make_image_runtime("comp-img");
    rt.supported_formats = vec!["jpg".to_string(), "png".to_string()];
    assert!(runtime_supports_file_format(&rt, "jpg"));
    assert!(runtime_supports_file_format(&rt, ".JPG"));
    assert!(runtime_supports_file_format(&rt, "png"));
    assert!(!runtime_supports_file_format(&rt, "webp"));
}

// -----------------------------------------------------------------------
// build_format_capability_summary
// -----------------------------------------------------------------------

#[test]
fn capability_summary_includes_all_components() {
    let mut rt_text = make_text_runtime("comp-text");
    rt_text.supported_content_formats = vec!["plain_text".to_string(), "rich_html".to_string()];
    let mut rt_img = make_image_runtime("comp-img");
    rt_img.supported_content_formats = vec!["media_ref".to_string()];
    rt_img.supported_formats = vec!["jpg".to_string(), "png".to_string()];
    let registry = make_registry(vec![rt_text, rt_img]);

    let summary = build_format_capability_summary(&registry);
    let components = summary.get("components").unwrap().as_array().unwrap();
    assert_eq!(components.len(), 2);

    let format_map = summary
        .get("format_component_map")
        .unwrap()
        .as_object()
        .unwrap();
    assert!(format_map.contains_key("plain_text"));
    assert!(format_map.contains_key("rich_html"));
    assert!(format_map.contains_key("media_ref:image"));
}
