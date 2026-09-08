//! Operator workflow policy: auto sync vs human review.
//!
//! Authority: `libs/wptsall-contracts/workflow_policy.v1.schema.json`.
//! Legacy global `review_mode` maps to `default_mode = review`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(crate) const WORKFLOW_POLICY_SCHEMA_VERSION: &str = "workflow-policy-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkflowMode {
    Auto,
    Review,
}

impl WorkflowMode {
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "review" => Some(Self::Review),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct WorkflowPolicy {
    #[serde(default = "default_schema_version")]
    pub(crate) schema_version: String,
    #[serde(default = "default_mode_auto")]
    pub(crate) default_mode: String,
    #[serde(default)]
    pub(crate) by_domain: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub(crate) by_content_format: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub(crate) by_rule: std::collections::HashMap<String, String>,
}

fn default_schema_version() -> String {
    WORKFLOW_POLICY_SCHEMA_VERSION.to_string()
}

fn default_mode_auto() -> String {
    "auto".to_string()
}

impl WorkflowPolicy {
    pub(crate) fn from_review_mode_flag(review_mode: bool) -> Self {
        Self {
            schema_version: WORKFLOW_POLICY_SCHEMA_VERSION.to_string(),
            default_mode: if review_mode {
                "review".to_string()
            } else {
                "auto".to_string()
            },
            ..Default::default()
        }
    }

    pub(crate) fn parse_json(raw: &str) -> Option<Self> {
        let v: Value = serde_json::from_str(raw).ok()?;
        let policy: WorkflowPolicy = serde_json::from_value(v).ok()?;
        if !policy.schema_version.is_empty()
            && policy.schema_version != WORKFLOW_POLICY_SCHEMA_VERSION
        {
            return None;
        }
        Some(policy)
    }

    /// Resolution order (most specific wins):
    /// 1. by_rule
    /// 2. by_content_format
    /// 3. by_domain
    /// 4. default_mode
    pub(crate) fn resolve(
        &self,
        domain: Option<&str>,
        content_format: Option<&str>,
        rule_id: Option<&str>,
    ) -> WorkflowMode {
        if let Some(rule) = rule_id.map(str::trim).filter(|s| !s.is_empty()) {
            if let Some(mode) = self.by_rule.get(rule).and_then(|m| WorkflowMode::parse(m)) {
                return mode;
            }
        }
        if let Some(fmt) = content_format.map(str::trim).filter(|s| !s.is_empty()) {
            let normalized = crate::component_rt::contract::normalize_content_format_alias(fmt);
            if let Some(mode) = self
                .by_content_format
                .get(&normalized)
                .or_else(|| self.by_content_format.get(fmt))
                .and_then(|m| WorkflowMode::parse(m))
            {
                return mode;
            }
        }
        if let Some(dom) = domain.map(str::trim).filter(|s| !s.is_empty()) {
            if let Some(mode) = self.by_domain.get(dom).and_then(|m| WorkflowMode::parse(m)) {
                return mode;
            }
        }
        WorkflowMode::parse(&self.default_mode).unwrap_or(WorkflowMode::Auto)
    }

    #[allow(dead_code)]
    pub(crate) fn requires_review(
        &self,
        domain: Option<&str>,
        content_format: Option<&str>,
        rule_id: Option<&str>,
    ) -> bool {
        self.resolve(domain, content_format, rule_id) == WorkflowMode::Review
    }
}

/// Load policy from DB `workflow_policy` JSON, falling back to legacy `review_mode`.
pub(crate) fn load_workflow_policy(
    conn: &rusqlite::Connection,
    legacy_review_mode: bool,
) -> WorkflowPolicy {
    if let Some(raw) = crate::db::system::get_system_config(conn, "workflow_policy") {
        if let Some(policy) = WorkflowPolicy::parse_json(&raw) {
            return policy;
        }
    }
    WorkflowPolicy::from_review_mode_flag(legacy_review_mode)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_review_flag_maps_to_default_review() {
        let p = WorkflowPolicy::from_review_mode_flag(true);
        assert!(p.requires_review(None, None, None));
        let p2 = WorkflowPolicy::from_review_mode_flag(false);
        assert!(!p2.requires_review(None, None, None));
    }

    #[test]
    fn format_override_beats_default() {
        let mut p = WorkflowPolicy::from_review_mode_flag(false);
        p.by_content_format
            .insert("json_structured".into(), "review".into());
        assert!(p.requires_review(None, Some("json_structured"), None));
        assert!(!p.requires_review(None, Some("plain_text"), None));
    }

    #[test]
    fn domain_override() {
        let mut p = WorkflowPolicy::from_review_mode_flag(false);
        p.by_domain
            .insert("shop.example.com".into(), "review".into());
        assert!(p.requires_review(Some("shop.example.com"), Some("plain_text"), None));
    }
}
