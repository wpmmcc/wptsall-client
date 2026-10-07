use anyhow::Context;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use super::errors::write_conflict_response;
use super::http::write_http_response;
use crate::component_rt::non_text::{recovery, ChunkedUploadConfig};
use crate::types::WebUiState;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReconcileRequest {
    operation_id: String,
    attachment_id: Option<i64>,
}

pub(super) async fn list(socket: &mut TcpStream) -> anyhow::Result<()> {
    match recovery::operations() {
        Ok(rows) => {
            let rows: Vec<_> = rows.into_iter().map(|row| json!({
                "operation_id":row.operation_id,
                "site":reqwest::Url::parse(&row.wp_base).ok().map(|u|u.origin().ascii_serialization()),
                "source_id":row.source_id,"relation_id":row.relation_id,
                "task_id":row.task_id,"state":row.state,"attachment_id":row.attachment_id,
            })).collect();
            write_http_response(
                socket,
                "200 OK",
                "application/json",
                &serde_json::to_vec(&json!({"success":true,"data":{"items":rows}}))?,
            )
            .await
        }
        Err(_) => {
            write_conflict_response(
                socket,
                "MEDIA_RECOVERY_UNREADABLE",
                "Retained media checkpoints cannot be read. No upload was retried.",
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
        let req: ReconcileRequest = serde_json::from_slice(body)?;
        anyhow::ensure!(
            uuid::Uuid::parse_str(&req.operation_id).is_ok()
                && req.attachment_id.is_none_or(|id| id > 0),
            "invalid media reconciliation request"
        );
        let row = recovery::operations()?
            .into_iter()
            .find(|row| row.operation_id == req.operation_id)
            .context("media operation not found")?;
        let (client, config) = {
            let state = state.lock().await;
            anyhow::ensure!(state.device_id == row.device_id, "media device changed");
            let binding = state
                .domain_token_bindings
                .domains
                .iter()
                .find_map(|(site, binding)| {
                    let base = crate::bindings::build_wp_base_url(site, &binding.route_secret)?;
                    (base == row.wp_base && !binding.wp_client_token.is_empty()).then_some(binding)
                })
                .context("current site binding does not match the retained operation")?;
            (
                state.http_client.clone(),
                ChunkedUploadConfig {
                    wp_base: row.wp_base,
                    token: binding.wp_client_token.clone(),
                    worker_id: state.device_id.clone(),
                    device_id: state.device_id.clone(),
                    route_secret: Some(binding.route_secret.clone()),
                    chunk_size: 0,
                },
            )
        };
        recovery::reconcile(&client, &config, &req.operation_id, req.attachment_id).await
    }
    .await;
    match result {
        Ok(result) => write_http_response(socket,"200 OK","application/json",
            &serde_json::to_vec(&json!({"success":true,"data":{"attachment_id":result.attachment_id}}))?).await,
        Err(_) => write_conflict_response(socket,"MEDIA_REVIEW_REQUIRED",
            "The attachment could not be verified for this operation. Evidence is retained; no import was retried.").await,
    }
}
