//! Execute-time gates for L3 client contracts and content formats.
//!
//! Authority: `libs/wptsall-contracts/` (content_formats.json, versions.json).
//! Unknown schema versions and unsupported formats fail-closed.

use anyhow::{bail, Result};

use crate::types::ComponentRuntime;

/// Must stay aligned with `libs/wptsall-contracts/versions.json`
/// → `axes.component_client_contract.supported`.
pub(crate) const SUPPORTED_CLIENT_CONTRACT_SCHEMA_VERSIONS: &[&str] =
    &["component-client-contract-v1"];

/// Canonical content formats (translatable + non-translatable).
/// Keep in sync with `libs/wptsall-contracts/content_formats.json` → `canonical`.
pub(crate) const CANONICAL_CONTENT_FORMATS: &[&str] = &[
    "plain_text",
    "rich_html",
    "serialized_php",
    "json_structured",
    "slug",
    "code",
    "media_ref",
];

/// Formats that must never be accepted on translation callback / free-text MT.
pub(crate) const NON_TRANSLATABLE_CONTENT_FORMATS: &[&str] = &["code", "media_ref"];

pub(crate) fn normalize_content_format_alias(raw: &str) -> String {
    let n = raw.trim().to_lowercase();
    match n.as_str() {
        "text" | "plain" => "plain_text".to_string(),
        "html" => "rich_html".to_string(),
        "json" => "json_structured".to_string(),
        "serialized" => "serialized_php".to_string(),
        other => other.to_string(),
    }
}

pub(crate) fn is_canonical_content_format(format: &str) -> bool {
    let n = normalize_content_format_alias(format);
    CANONICAL_CONTENT_FORMATS.contains(&n.as_str())
}

/// Validate that a component runtime is safe to execute for the given format.
///
/// - If `client_contract.schema_version` is present on the list-level summary
///   embedded in template metadata via constraints/default_values, we only
///   accept known versions (fail-closed).
/// - If `supported_content_formats` is non-empty, `content_format` must match
///   (after alias normalization). Empty support list = legacy "all text formats".
pub(crate) fn assert_runtime_ready_for_format(
    runtime: &ComponentRuntime,
    content_format: Option<&str>,
) -> Result<()> {
    if let Some(version) = extract_client_contract_schema_version(runtime) {
        if !SUPPORTED_CLIENT_CONTRACT_SCHEMA_VERSIONS.contains(&version.as_str()) {
            bail!(
                "component {} client_contract.schema_version '{}' is not supported (supported={:?}); upgrade client or pin an older template",
                runtime.template.id,
                version,
                SUPPORTED_CLIENT_CONTRACT_SCHEMA_VERSIONS
            );
        }
    }

    let Some(raw) = content_format.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(());
    };
    let normalized = normalize_content_format_alias(raw);
    if !is_canonical_content_format(&normalized) {
        bail!(
            "content_format '{}' is not in the canonical vocabulary (component={})",
            raw,
            runtime.template.id
        );
    }
    if NON_TRANSLATABLE_CONTENT_FORMATS.contains(&normalized.as_str()) {
        bail!(
            "content_format '{}' must not be machine-translated (component={})",
            normalized,
            runtime.template.id
        );
    }

    if !runtime.supported_content_formats.is_empty() {
        let ok = runtime.supported_content_formats.iter().any(|f| {
            normalize_content_format_alias(f) == normalized || f.eq_ignore_ascii_case(&normalized)
        });
        if !ok {
            bail!(
                "component {} does not support content_format '{}' (supported={:?})",
                runtime.template.id,
                normalized,
                runtime.supported_content_formats
            );
        }
    }

    Ok(())
}

fn extract_client_contract_schema_version(runtime: &ComponentRuntime) -> Option<String> {
    if let Some(contract) = runtime.template.client_contract.as_ref() {
        let t = contract.schema_version.trim();
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }
    if let Some(defaults) = runtime.template.default_values.as_ref() {
        if let Some(v) = defaults
            .get("client_contract")
            .and_then(|c| c.get("schema_version"))
            .and_then(|s| s.as_str())
        {
            let t = v.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_maps_html_to_rich_html() {
        assert_eq!(normalize_content_format_alias("html"), "rich_html");
        assert_eq!(normalize_content_format_alias("text"), "plain_text");
    }

    #[test]
    fn code_is_non_translatable() {
        assert!(NON_TRANSLATABLE_CONTENT_FORMATS.contains(&"code"));
    }

    #[test]
    fn supported_contract_versions_include_v1() {
        assert!(SUPPORTED_CLIENT_CONTRACT_SCHEMA_VERSIONS.contains(&"component-client-contract-v1"));
    }
}
