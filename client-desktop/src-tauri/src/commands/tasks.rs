use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::webui_proxy;

#[derive(Serialize)]
pub struct TaskSummary {
    pub id: String,
    pub site_domain: String,
    pub status: String,
    pub progress: f32,
    pub created_at: String,
}

#[derive(Debug, Deserialize)]
pub struct FocusDiscoveryRelationRequest {
    pub relation_id: i64,
    #[serde(default)]
    pub include_resync: bool,
    #[serde(default)]
    pub selected_component_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct FocusDiscoveryRelationResult {
    pub relation_id: i64,
    pub updated: usize,
    pub enabled: usize,
}

fn progress_from_job(job: &Value) -> f32 {
    if let Some(progress) = job.get("progress") {
        if let Some(percent) = progress
            .get("percent")
            .or_else(|| progress.get("percentage"))
            .and_then(Value::as_f64)
        {
            return percent as f32;
        }
    }

    let total = job
        .get("total_items")
        .or_else(|| job.get("items_total"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let done = job
        .get("done_items")
        .or_else(|| job.get("items_done"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    if total <= 0.0 {
        0.0
    } else {
        ((done / total) * 100.0).clamp(0.0, 100.0) as f32
    }
}

pub(crate) fn map_task_summary(job: &Value) -> TaskSummary {
    TaskSummary {
        id: job
            .get("id")
            .or_else(|| job.get("job_id"))
            .and_then(|value| {
                value
                    .as_i64()
                    .map(|n| n.to_string())
                    .or_else(|| value.as_str().map(ToString::to_string))
            })
            .unwrap_or_default(),
        site_domain: job
            .get("domain")
            .or_else(|| job.get("site_domain"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        status: job
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        progress: progress_from_job(job),
        created_at: job
            .get("created_at")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    }
}

#[tauri::command]
pub async fn list_tasks() -> Result<Vec<TaskSummary>, String> {
    let data = webui_proxy::get("/api/jobs").await?;
    Ok(data
        .get("items")
        .and_then(Value::as_array)
        .map(|items| items.iter().map(map_task_summary).collect())
        .unwrap_or_default())
}

#[tauri::command]
pub async fn trigger_discover(site_id: String) -> Result<String, String> {
    if !site_id.trim().is_empty() {
        webui_proxy::post(
            "/api/domain-tokens/test",
            json!({ "api_base_url": site_id.trim() }),
        )
        .await?;
    }
    let data = webui_proxy::post("/api/discovery-tasks/bootstrap", json!({})).await?;
    Ok(data
        .get("task_id")
        .or_else(|| data.get("id"))
        .and_then(|value| {
            value
                .as_str()
                .map(ToString::to_string)
                .or_else(|| value.as_i64().map(|n| n.to_string()))
        })
        .unwrap_or_else(|| "discovery-tasks-bootstrap".to_string()))
}

#[tauri::command]
pub async fn get_task_detail(task_id: String) -> Result<serde_json::Value, String> {
    let id = task_id.trim();
    if id.is_empty() {
        return Err("task_id is required".into());
    }
    webui_proxy::get(&format!("/api/jobs/{id}")).await
}

#[tauri::command]
pub async fn focus_discovery_relation(
    request: FocusDiscoveryRelationRequest,
) -> Result<FocusDiscoveryRelationResult, String> {
    if request.relation_id <= 0 {
        return Err("relation_id must be positive".into());
    }
    let selected_component_id = request
        .selected_component_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let data = webui_proxy::get("/api/discovery-tasks").await?;
    let items = data
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut updated = 0;
    let mut enabled = 0;

    for item in items {
        let id = item.get("id").and_then(Value::as_i64).unwrap_or(0);
        if id <= 0 {
            continue;
        }
        let relation_id = item.get("relation_id").and_then(Value::as_i64).unwrap_or(0);
        let enable = relation_id == request.relation_id;
        if enable {
            enabled += 1;
        }
        webui_proxy::put(
            &format!("/api/discovery-tasks/{id}"),
            json!({
                "concurrency": 1,
                "batch_parallel": 1,
                "per_page": 100,
                "include_resync": request.include_resync,
                "retry_max": 4,
                "timeout_secs": 90,
                "enabled": enable,
                "selected_component_id": if enable { selected_component_id.clone() } else { None },
            }),
        )
        .await?;
        updated += 1;
    }

    Ok(FocusDiscoveryRelationResult {
        relation_id: request.relation_id,
        updated,
        enabled,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_job_summary_progress() {
        let task = map_task_summary(&json!({
            "id": 42,
            "domain": "http://127.0.0.1:9181",
            "status": "completed",
            "total_items": 10,
            "done_items": 7,
            "created_at": "2026-08-28 12:00:00"
        }));
        assert_eq!(task.id, "42");
        assert_eq!(task.status, "completed");
        assert_eq!(task.progress, 70.0);
    }
}
