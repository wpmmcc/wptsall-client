use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::webui_proxy;

#[derive(Serialize)]
pub struct WorkerStatus {
    pub running: bool,
    pub active_tasks: u32,
    pub completed_total: u64,
    pub uptime_seconds: u64,
}

#[derive(Debug, Deserialize)]
pub struct WorkerConfigRequest {
    #[serde(default = "default_poll_seconds")]
    pub poll_seconds: u64,
    #[serde(default = "default_callback_concurrency")]
    pub callback_concurrency: u64,
    #[serde(default = "default_callback_retry_max")]
    pub callback_retry_max: u64,
    #[serde(default = "default_fetch_timeout_secs")]
    pub fetch_timeout_secs: u64,
    #[serde(default = "default_fetch_retry_max")]
    pub fetch_retry_max: u64,
    #[serde(default)]
    pub relation_max_pending_callbacks: Option<u64>,
    #[serde(default)]
    pub global_callback_concurrency: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct WorkerRunOnceRequest {
    #[serde(default = "default_run_once_iterations")]
    pub max_iterations: u64,
    #[serde(default = "default_run_once_elapsed_secs")]
    pub max_elapsed_secs: u64,
    #[serde(default = "default_run_once_items")]
    pub max_items_per_run: u64,
}

fn default_poll_seconds() -> u64 {
    20
}

fn default_callback_concurrency() -> u64 {
    4
}

fn default_callback_retry_max() -> u64 {
    4
}

fn default_fetch_timeout_secs() -> u64 {
    45
}

fn default_fetch_retry_max() -> u64 {
    6
}

fn default_run_once_iterations() -> u64 {
    4
}

fn default_run_once_elapsed_secs() -> u64 {
    180
}

fn default_run_once_items() -> u64 {
    64
}

fn count_recent_successes(recent_runs: Option<&Value>) -> u64 {
    recent_runs
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    item.get("tasks_succeeded")
                        .or_else(|| item.get("succeeded"))
                        .and_then(Value::as_u64)
                        .unwrap_or(0)
                })
                .sum()
        })
        .unwrap_or(0)
}

pub(crate) fn map_worker_status(data: &Value) -> WorkerStatus {
    let running = data
        .get("worker_loop_running")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let completed_total = data
        .get("worker_last_summary")
        .and_then(|summary| {
            summary
                .get("tasks_succeeded")
                .or_else(|| summary.get("succeeded"))
                .and_then(Value::as_u64)
        })
        .unwrap_or_else(|| count_recent_successes(data.get("worker_recent_runs")));

    WorkerStatus {
        running,
        active_tasks: if running { 1 } else { 0 },
        completed_total,
        uptime_seconds: 0,
    }
}

#[tauri::command]
pub async fn get_worker_status() -> Result<WorkerStatus, String> {
    let data = webui_proxy::get("/api/status").await?;
    Ok(map_worker_status(&data))
}

#[tauri::command]
pub async fn start_worker() -> Result<(), String> {
    webui_proxy::post("/api/worker/start", json!({ "force": true }))
        .await
        .map(|_| ())
}

#[tauri::command]
pub async fn stop_worker() -> Result<(), String> {
    webui_proxy::post("/api/worker/stop", json!({}))
        .await
        .map(|_| ())
}

#[tauri::command]
pub async fn configure_worker(request: WorkerConfigRequest) -> Result<Value, String> {
    let mut body = json!({
        "poll_seconds": request.poll_seconds,
        "callback_concurrency": request.callback_concurrency,
        "callback_retry_max": request.callback_retry_max,
        "fetch_timeout_secs": request.fetch_timeout_secs,
        "fetch_retry_max": request.fetch_retry_max,
    });
    if let Some(value) = request.relation_max_pending_callbacks {
        body["relation_max_pending_callbacks"] = json!(value);
    }
    if let Some(value) = request.global_callback_concurrency {
        body["global_callback_concurrency"] = json!(value);
    }
    webui_proxy::post("/api/worker/config", body).await
}

#[tauri::command]
pub async fn run_worker_once(request: WorkerRunOnceRequest) -> Result<Value, String> {
    webui_proxy::post(
        "/api/worker/run-once",
        json!({
            "max_iterations": request.max_iterations,
            "max_elapsed_secs": request.max_elapsed_secs,
            "max_items_per_run": request.max_items_per_run,
        }),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_webui_worker_status() {
        let status = map_worker_status(&json!({
            "worker_loop_running": true,
            "worker_last_summary": { "tasks_succeeded": 12 }
        }));
        assert!(status.running);
        assert_eq!(status.active_tasks, 1);
        assert_eq!(status.completed_total, 12);
    }
}
