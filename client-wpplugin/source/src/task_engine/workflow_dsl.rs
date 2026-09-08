//! Configurable operator workflow DSL (P2 foundation).
//!
//! Default pipeline: `translate` → `media` → optional `human_review` (from workflow_policy) → `sync`.
//! Custom DSL stored in system_config `workflow_dsl` overrides step order/types.

use serde::{Deserialize, Serialize};

pub(crate) const WORKFLOW_DSL_SCHEMA_VERSION: &str = "workflow-dsl-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PostTranslateAction {
    PendingReview,
    Sync,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct WorkflowDsl {
    #[serde(default = "default_dsl_schema_version")]
    pub(crate) schema_version: String,
    #[serde(default = "default_workflow_steps")]
    pub(crate) steps: Vec<WorkflowStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct WorkflowStep {
    pub(crate) id: String,
    #[serde(rename = "type")]
    pub(crate) step_type: String,
    #[serde(default)]
    pub(crate) when: Option<String>,
}

fn default_dsl_schema_version() -> String {
    WORKFLOW_DSL_SCHEMA_VERSION.to_string()
}

fn default_workflow_steps() -> Vec<WorkflowStep> {
    vec![
        WorkflowStep {
            id: "translate".into(),
            step_type: "builtin".into(),
            when: None,
        },
        WorkflowStep {
            id: "media".into(),
            step_type: "media".into(),
            when: None,
        },
        WorkflowStep {
            id: "human_review".into(),
            step_type: "human_review".into(),
            when: Some("policy".into()),
        },
        WorkflowStep {
            id: "sync".into(),
            step_type: "builtin".into(),
            when: None,
        },
    ]
}

impl Default for WorkflowDsl {
    fn default() -> Self {
        Self {
            schema_version: WORKFLOW_DSL_SCHEMA_VERSION.to_string(),
            steps: default_workflow_steps(),
        }
    }
}

impl WorkflowDsl {
    pub(crate) fn parse_json(raw: &str) -> Option<Self> {
        let dsl: WorkflowDsl = serde_json::from_str(raw).ok()?;
        if dsl.schema_version != WORKFLOW_DSL_SCHEMA_VERSION {
            return None;
        }
        if dsl.steps.is_empty() {
            return None;
        }
        Some(dsl)
    }
}

pub(crate) fn load_workflow_dsl(conn: &rusqlite::Connection) -> WorkflowDsl {
    if let Some(raw) = crate::db::system::get_system_config(conn, "workflow_dsl") {
        if let Some(dsl) = WorkflowDsl::parse_json(&raw) {
            return dsl;
        }
    }
    WorkflowDsl::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task_engine::workflow_interpreter::resolve_post_translate_action;
    use crate::task_engine::workflow_policy::WorkflowPolicy;

    #[test]
    fn default_dsl_includes_review_and_media() {
        let dsl = WorkflowDsl::default();
        assert!(dsl.steps.iter().any(|s| s.id == "human_review"));
        assert!(dsl.steps.iter().any(|s| s.id == "media"));
    }

    #[test]
    fn review_when_policy_says_review() {
        let dsl = WorkflowDsl::default();
        let policy = WorkflowPolicy::from_review_mode_flag(true);
        assert_eq!(
            resolve_post_translate_action(&dsl, &policy, None, None, None),
            PostTranslateAction::PendingReview
        );
    }

    #[test]
    fn sync_when_no_review_step() {
        let dsl = WorkflowDsl {
            schema_version: WORKFLOW_DSL_SCHEMA_VERSION.to_string(),
            steps: vec![WorkflowStep {
                id: "sync".into(),
                step_type: "builtin".into(),
                when: None,
            }],
        };
        let policy = WorkflowPolicy::from_review_mode_flag(true);
        assert_eq!(
            resolve_post_translate_action(&dsl, &policy, None, None, None),
            PostTranslateAction::Sync
        );
    }
}
