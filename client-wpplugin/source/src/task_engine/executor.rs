// Utility functions retained from the old task executor.
// The task-driven execution path (process_single_task, etc.) was removed in v1.2.0
// because WP Plugin deprecated the /tasks endpoint. These normalizers are still
// used by component_rt/selector.rs and component_rt/runner.rs.

pub(crate) fn normalize_task_content_type(raw: &str) -> String {
    let lower = raw.trim().to_lowercase();
    match lower.as_str() {
        "text" | "text_translation" | "field" | "fields" => "text".to_string(),
        "image" | "images" | "image_translation" => "image".to_string(),
        "video" | "videos" | "video_translation" => "video".to_string(),
        "audio" | "audios" | "audio_translation" => "audio".to_string(),
        "document" | "documents" | "doc" | "file" | "files" | "document_translation" => {
            "document".to_string()
        }
        "mixed" | "mixed_translation" | "multimodal" | "multimodal_translation" => {
            "mixed".to_string()
        }
        _ => "text".to_string(),
    }
}

#[allow(dead_code)]
pub(crate) fn normalize_business_line(raw: &str) -> String {
    let lower = raw.trim().to_lowercase();
    match lower.as_str() {
        "post" | "post_type" | "post_content" => "post_content".to_string(),
        "taxonomy" | "term" | "taxonomy_content" => "taxonomy_content".to_string(),
        "theme" | "theme_i18n" => "theme_i18n".to_string(),
        "plugin" | "plugin_i18n" | "language_pack" | "language_pack_i18n" => {
            "plugin_i18n".to_string()
        }
        "config" | "config_i18n" => "config_i18n".to_string(),
        "site" | "site_strings" => "site_strings".to_string(),
        "menu" | "menu_strings" => "menu_strings".to_string(),
        "widget" | "widget_strings" => "widget_strings".to_string(),
        "custom_model" | "custom" | "model" => "custom_model".to_string(),
        _ => "custom_model".to_string(),
    }
}

pub(crate) fn normalize_patch_field_key(raw: &str) -> String {
    let mut out = String::new();
    for ch in raw.trim().to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch);
        } else if matches!(ch, ' ' | '.' | '/') {
            out.push('_');
        }
    }
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    out.trim_matches('_').to_string()
}
