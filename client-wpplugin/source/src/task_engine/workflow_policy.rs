//! Operator workflow policy: auto sync vs human review.
//!
//! Authority: `libs/wptsall-contracts/workflow_policy.v1.schema.json`.
//! Legacy global `review_mode` maps to `default_mode = review`.

use serde::{Deserialize, Serialize};

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
#[serde(deny_unknown_fields)]
pub(crate) struct WorkflowPolicy {
    pub(crate) schema_version: String,
    pub(crate) default_mode: String,
    #[serde(default)]
    pub(crate) by_domain: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub(crate) by_content_format: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub(crate) by_rule: std::collections::HashMap<String, String>,
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
        serde_json::from_str::<serde_json::Value>(raw)
            .ok()?
            .as_object()?;
        let policy: WorkflowPolicy = serde_json::from_str(raw).ok()?;
        if policy.schema_version != WORKFLOW_POLICY_SCHEMA_VERSION {
            return None;
        }
        if !matches!(policy.default_mode.as_str(), "auto" | "review")
            || policy
                .by_domain
                .values()
                .chain(policy.by_content_format.values())
                .chain(policy.by_rule.values())
                .any(|mode| !matches!(mode.as_str(), "auto" | "review"))
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

/// FL-8 single-source sync: rewrite the stored policy's `default_mode` to
/// match a legacy `review_mode` flag write, preserving the finer-grained
/// by_domain/by_content_format/by_rule overrides. The engine resolves items
/// with the stored policy when present (see `load_workflow_policy`), so a
/// flag write that leaves a stale `default_mode` behind silently overrides
/// the operator. Returns the updated raw JSON, or `None` when no parseable
/// policy is stored (the legacy fallback is already authoritative then).
pub(crate) fn sync_stored_policy_default_mode(
    conn: &rusqlite::Connection,
    review_mode: bool,
) -> anyhow::Result<Option<String>> {
    let Some(raw) = crate::db::system::get_system_config_checked(conn, "workflow_policy")? else {
        return Ok(None);
    };
    let Some(mut policy) = WorkflowPolicy::parse_json(&raw) else {
        return Ok(None);
    };
    policy.default_mode = if review_mode {
        "review".to_string()
    } else {
        "auto".to_string()
    };
    let updated = serde_json::to_string(&policy)?;
    crate::db::system::set_system_config(conn, "workflow_policy", &updated)?;
    Ok(Some(updated))
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

    #[test]
    fn flag_write_resyncs_stored_policy_default_mode() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::create_tables(&conn).unwrap();
        // Settings-UI style save: review default plus a domain override.
        crate::db::system::set_system_config(
            &conn,
            "workflow_policy",
            r#"{"schema_version":"workflow-policy-v1","default_mode":"review","by_domain":{"shop.example.com":"auto"}}"#,
        )
        .unwrap();
        let updated = sync_stored_policy_default_mode(&conn, false)
            .unwrap()
            .expect("stored policy should resync");
        let policy = WorkflowPolicy::parse_json(&updated).unwrap();
        assert_eq!(policy.default_mode, "auto");
        // Finer-grained overrides survive the flag write.
        assert_eq!(
            policy.by_domain.get("shop.example.com").map(String::as_str),
            Some("auto")
        );
        // The loader the engine uses now resolves auto by default (FL-8:
        // previously a stale default_mode kept resolving review).
        assert_eq!(
            load_workflow_policy(&conn, false).resolve(None, None, None),
            WorkflowMode::Auto
        );
    }

    #[test]
    fn flag_write_without_stored_policy_is_noop() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::create_tables(&conn).unwrap();
        assert!(sync_stored_policy_default_mode(&conn, true)
            .unwrap()
            .is_none());
    }

    #[test]
    fn unparseable_stored_policy_is_ignored_by_sync() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::create_tables(&conn).unwrap();
        crate::db::system::set_system_config(&conn, "workflow_policy", "not-json").unwrap();
        // Corrupt rows fall back to the legacy flag at load time, so the
        // sync must not fabricate a policy from them.
        assert!(sync_stored_policy_default_mode(&conn, true)
            .unwrap()
            .is_none());
    }
}