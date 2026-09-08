use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context;
use reqwest::Client;
use serde_json::json;
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinSet;

use crate::auth::wp_get_json_with_transport_and_secret;
use crate::component_rt::selector::task_priority;
use crate::logging::{log_event, snippet};
use crate::types::TaskQueueStateStore;
use crate::task_engine::executor::process_single_task;
use crate::types::*;

pub(crate) async fn process_domain_tasks(
    client: &Client,
    wp_base: &str,
    token: &str,
    log_file: &str,
    task_queue_state: &Arc<Mutex<TaskQueueStateStore>>,
    component_registry: Option<&ComponentRuntimeRegistry>,
    component_id_override: &str,
    component_prefer_ids: &[String],
    task_type_component_bindings: Option<&TaskTypeComponentBindingsDoc>,
    rule_component_bindings: Option<&RuleComponentBindingsDoc>,
    worker_config: &WorkerConfig,
    route_secret: Option<&str>,
) -> anyhow::Result<DomainRunReport> {
    let ping_url = format!("{}/ping", wp_base);
    match wp_get_json_with_transport_and_secret::<ApiResponse<WpClientPingData>>(
        client,
        &ping_url,
        token,
        &worker_config.worker_id,
        route_secret,
    )
    .await
    {
        Ok(ping_resp) => {
            let execution_mode = ping_resp.data.sync_execution_mode.trim().to_lowercase();
            let is_local_mode =
                execution_mode == "local" || ping_resp.data.local_executor_enabled;
            if is_local_mode {
                let _ = log_event(
                    log_file,
                    "warning",
                    "domain.skipped_local_execution_mode",
                    json!({
                        "api_base_url": wp_base,
                        "sync_execution_mode": if execution_mode.is_empty() { "local" } else { &execution_mode },
                        "local_executor_enabled": ping_resp.data.local_executor_enabled
                    }),
                );
                return Ok(DomainRunReport {
                    api_base_url: wp_base.to_string(),
                    ..DomainRunReport::default()
                });
            }
        }
        Err(err) => {
            let err_text = err.to_string();
            if err_text.contains("status=404") {
                let _ = log_event(
                    log_file,
                    "warning",
                    "domain.ping_unavailable",
                    json!({
                        "api_base_url": wp_base,
                        "error": snippet(&err_text)
                    }),
                );
            } else {
                return Err(err);
            }
        }
    }

    // Discovery mode: use content discovery flow instead of task-pull flow
    if worker_config.discovery_mode {
        return crate::task_engine::discoverer::discover_and_translate(
            client,
            wp_base,
            token,
            log_file,
            component_registry,
            component_id_override,
            component_prefer_ids,
            task_type_component_bindings,
            rule_component_bindings,
            worker_config,
            route_secret,
        )
        .await;
    }

    let task_statuses = if worker_config.task_pull_statuses.is_empty() {
        vec!["pending".to_string(), "retry".to_string()]
    } else {
        worker_config.task_pull_statuses.clone()
    };
    let mut tasks_map: HashMap<i64, ClientTask> = HashMap::new();
    let recovered_tasks = {
        let state = task_queue_state.lock().await;
        state.list_tasks_for_domain(wp_base)
    };
    if !recovered_tasks.is_empty() {
        let _ = log_event(
            log_file,
            "info",
            "tasks.recovered",
            json!({
                "api_base_url": wp_base,
                "count": recovered_tasks.len(),
                "task_ids": recovered_tasks.iter().map(|t| t.task_id).collect::<Vec<i64>>()
            }),
        );
        for task in recovered_tasks {
            tasks_map.insert(task.task_id, task);
        }
    }

    for status in &task_statuses {
        let tasks_url = format!("{}/tasks?status={}&limit=10", wp_base, status);
        let tasks_resp: ApiResponse<TasksData> = wp_get_json_with_transport_and_secret(
            client,
            &tasks_url,
            token,
            &worker_config.worker_id,
            route_secret,
        )
        .await?;
        let _ = log_event(
            log_file,
            "info",
            "tasks.pulled",
            json!({
                "api_base_url": wp_base,
                "status": status,
                "count": tasks_resp.data.items.len()
            }),
        );

        if !tasks_resp.success {
            eprintln!("Tasks request failed for {} status={}", wp_base, status);
            continue;
        }
        for item in tasks_resp.data.items {
            tasks_map.insert(item.task_id, item);
        }
    }

    let mut tasks: Vec<ClientTask> = tasks_map.into_values().collect();
    let pulled_count = tasks.len();
    if tasks.is_empty() {
        return Ok(DomainRunReport {
            api_base_url: wp_base.to_string(),
            pulled: pulled_count,
            ..DomainRunReport::default()
        });
    }

    tasks.sort_by(|a, b| {
        task_priority(b)
            .cmp(&task_priority(a))
            .then_with(|| a.task_id.cmp(&b.task_id))
    });
    let ordered_task_ids: Vec<i64> = tasks.iter().map(|t| t.task_id).collect();
    let (persisted_entries, total_queue_entries) = {
        let mut state = task_queue_state.lock().await;
        let changed = state.upsert_tasks_for_domain(wp_base, &tasks)?;
        (changed, state.entry_count())
    };
    if persisted_entries > 0 {
        let _ = log_event(
            log_file,
            "info",
            "tasks.queue_persisted",
            json!({
                "api_base_url": wp_base,
                "persisted_entries": persisted_entries,
                "queue_entries_total": total_queue_entries
            }),
        );
    }

    let _ = log_event(
        log_file,
        "info",
        "tasks.queue_start",
        json!({
            "api_base_url": wp_base,
            "count": tasks.len(),
            "task_concurrency": worker_config.task_concurrency,
            "ordered_task_ids": ordered_task_ids
        }),
    );

    let semaphore = Arc::new(Semaphore::new(worker_config.task_concurrency.max(1)));
    let mut workers = JoinSet::new();

    for task in tasks {
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .with_context(|| "task queue semaphore closed".to_string())?;
        let client = client.clone();
        let wp_base = wp_base.to_string();
        let token = token.to_string();
        let log_file = log_file.to_string();
        let component_registry = component_registry.cloned();
        let component_id_override = component_id_override.to_string();
        let component_prefer_ids = component_prefer_ids.to_vec();
        let task_type_component_bindings = task_type_component_bindings.cloned();
        let rule_component_bindings = rule_component_bindings.cloned();
        let worker_config = worker_config.clone();
        let task_queue_state = Arc::clone(task_queue_state);
        let route_secret_owned = route_secret.map(|s| s.to_string());

        workers.spawn(async move {
            let _permit = permit;
            process_single_task(
                &client,
                &wp_base,
                &token,
                &log_file,
                task_queue_state,
                task,
                component_registry.as_ref(),
                &component_id_override,
                &component_prefer_ids,
                task_type_component_bindings.as_ref(),
                rule_component_bindings.as_ref(),
                &worker_config,
                route_secret_owned.as_deref(),
            )
            .await
        });
    }

    let mut reports: Vec<TaskRunReport> = Vec::new();
    while let Some(joined) = workers.join_next().await {
        match joined {
            Ok(Ok(report)) => {
                reports.push(report);
            }
            Ok(Err(err)) => {
                let _ = log_event(
                    log_file,
                    "error",
                    "task.worker_failed",
                    json!({
                        "api_base_url": wp_base,
                        "error": err.to_string()
                    }),
                );
            }
            Err(err) => {
                let _ = log_event(
                    log_file,
                    "error",
                    "task.worker_join_failed",
                    json!({
                        "api_base_url": wp_base,
                        "error": err.to_string()
                    }),
                );
            }
        }
    }

    let completed = reports
        .iter()
        .filter(|r| r.final_status == "completed")
        .count();
    let retried = reports.iter().filter(|r| r.final_status == "retry").count();
    let failed = reports
        .iter()
        .filter(|r| r.final_status == "failed")
        .count();
    let skipped = reports
        .iter()
        .filter(|r| r.final_status == "status_update_failed")
        .count();
    let total_elapsed_ms: u64 = reports.iter().map(|r| r.elapsed_ms).sum();
    let avg_elapsed_ms = if reports.is_empty() {
        0
    } else {
        total_elapsed_ms / u64::try_from(reports.len()).unwrap_or(1)
    };

    let _ = log_event(
        log_file,
        "info",
        "tasks.queue_done",
        json!({
            "api_base_url": wp_base,
            "processed": reports.len(),
            "completed": completed,
            "retried": retried,
            "failed": failed,
            "status_update_failed": skipped,
            "avg_elapsed_ms": avg_elapsed_ms,
            "task_ids": reports.iter().map(|r| r.task_id).collect::<Vec<i64>>()
        }),
    );

    Ok(DomainRunReport {
        api_base_url: wp_base.to_string(),
        pulled: pulled_count,
        processed: reports.len(),
        completed,
        retried,
        failed,
        status_update_failed: skipped,
        avg_elapsed_ms,
    })
}
