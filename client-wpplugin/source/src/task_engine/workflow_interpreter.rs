//! Multi-step workflow interpreter (P2).
//!
//! Interprets `workflow_dsl` step graphs after translation.
//! Default: `translate` → `media` → optional `human_review` → `sync`.

use super::workflow_dsl::{PostTranslateAction, WorkflowDsl};
use super::workflow_policy::{WorkflowMode, WorkflowPolicy};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowPipelinePhase {
    Translate,
    HumanReview,
    Media,
    Sync,
}

/// Ordered phases from DSL (unknown steps are skipped).
pub fn pipeline_phases(dsl: &WorkflowDsl) -> Vec<WorkflowPipelinePhase> {
    let mut phases = Vec::new();
    for step in &dsl.steps {
        let phase = match (step.step_type.as_str(), step.id.as_str()) {
            ("translate", _) | ("builtin", "translate") => Some(WorkflowPipelinePhase::Translate),
            ("human_review", _) | ("review", _) | ("builtin", "human_review" | "review") => {
                Some(WorkflowPipelinePhase::HumanReview)
            }
            ("media", _) | ("builtin", "media") => Some(WorkflowPipelinePhase::Media),
            ("sync", _) | ("builtin", "sync") => Some(WorkflowPipelinePhase::Sync),
            _ => None,
        };
        if let Some(phase) = phase {
            if !phases.contains(&phase) {
                phases.push(phase);
            }
        }
    }
    if phases.is_empty() {
        phases.push(WorkflowPipelinePhase::Sync);
    }
    phases
}

/// Whether the DSL includes a media upload/prepare step.
pub fn should_run_media_step(dsl: &WorkflowDsl) -> bool {
    pipeline_phases(dsl).contains(&WorkflowPipelinePhase::Media)
}

/// After translation completes, decide whether to hold for review or sync.
pub fn resolve_post_translate_action(
    dsl: &WorkflowDsl,
    policy: &WorkflowPolicy,
    domain: Option<&str>,
    content_format: Option<&str>,
    rule_id: Option<&str>,
) -> PostTranslateAction {
    let phases = pipeline_phases(dsl);
    let review_in_pipeline = phases.contains(&WorkflowPipelinePhase::HumanReview);
    if !review_in_pipeline {
        return PostTranslateAction::Sync;
    }
    if policy.resolve(domain, content_format, rule_id) == WorkflowMode::Review {
        PostTranslateAction::PendingReview
    } else {
        PostTranslateAction::Sync
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task_engine::workflow_dsl::{
        WorkflowDsl, WorkflowStep, WORKFLOW_DSL_SCHEMA_VERSION,
    };

    #[test]
    fn custom_pipeline_order_preserved() {
        let dsl = WorkflowDsl {
            schema_version: WORKFLOW_DSL_SCHEMA_VERSION.to_string(),
            steps: vec![
                WorkflowStep {
                    id: "translate".into(),
                    step_type: "translate".into(),
                    when: None,
                },
                WorkflowStep {
                    id: "media".into(),
                    step_type: "media".into(),
                    when: None,
                },
                WorkflowStep {
                    id: "sync".into(),
                    step_type: "sync".into(),
                    when: None,
                },
            ],
        };
        let phases = pipeline_phases(&dsl);
        assert_eq!(
            phases,
            vec![
                WorkflowPipelinePhase::Translate,
                WorkflowPipelinePhase::Media,
                WorkflowPipelinePhase::Sync,
            ]
        );
    }

    #[test]
    fn media_step_optional() {
        let with_media = WorkflowDsl::default();
        assert!(should_run_media_step(&with_media));

        let no_media = WorkflowDsl {
            schema_version: WORKFLOW_DSL_SCHEMA_VERSION.to_string(),
            steps: vec![
                WorkflowStep {
                    id: "translate".into(),
                    step_type: "builtin".into(),
                    when: None,
                },
                WorkflowStep {
                    id: "sync".into(),
                    step_type: "builtin".into(),
                    when: None,
                },
            ],
        };
        assert!(!should_run_media_step(&no_media));
    }
}
