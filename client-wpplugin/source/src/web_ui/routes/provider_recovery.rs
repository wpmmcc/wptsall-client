use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use super::errors::write_conflict_response;
use super::http::write_http_response;
use crate::types::WebUiState;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReconcileRequest {
    operation_id: String,
}

pub(super) async fn list(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
) -> anyhow::Result<()> {
    let db = Arc::clone(&state.lock().await.db);
    match crate::db::async_jobs::list_provider_operations(&db).await {
        Ok(items) => {
            write_http_response(
                socket,
                "200 OK",
                "application/json",
                &serde_json::to_vec(&json!({"success":true,"data":{"items":items}}))?,
            )
            .await
        }
        Err(_) => {
            write_conflict_response(
                socket,
                "PROVIDER_RECOVERY_UNREADABLE",
                "Retained provider evidence cannot be read. No submission was retried.",
            )
            .await
        }
    }
}

pub(super) async fn reconcile(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
) -> anyhow::Result<()> {
    let result = async {
        let request: ReconcileRequest = serde_json::from_slice(body)?;
        let db = {
            let state = state.lock().await;
            anyhow::ensure!(
                !state.worker_loop_running,
                "stop the worker before provider review"
            );
            Arc::clone(&state.db)
        };
        let review =
            crate::db::async_jobs::review_provider_operation(&db, &request.operation_id).await?;
        {
            let state = state.lock().await;
            anyhow::ensure!(
                state
                    .domain_token_bindings
                    .domains
                    .iter()
                    .any(|(site, binding)| !binding.wp_client_token.is_empty()
                        && crate::bindings::build_wp_base_url(site, &binding.route_secret)
                            .is_some_and(|base| base == review.domain())),
                "retained provider operation is not bound to the current site"
            );
        }
        // Only the encrypted original contract and credentials are used.
        // New URLs, keys, job IDs and user-supplied proofs are not accepted.
        crate::component_rt::runner::provider_recovery::reconcile(&review).await
    }
    .await;
    match result {
        Ok(_) => write_http_response(socket, "200 OK", "application/json",
            &serde_json::to_vec(&json!({"success":true,"data":{"state":"polling"}}))?).await,
        Err(_) => write_conflict_response(socket, "PROVIDER_REVIEW_REQUIRED",
            "The existing provider job could not be verified. Stop the worker and check the retained evidence and site binding. No submission was retried.").await,
    }
}
