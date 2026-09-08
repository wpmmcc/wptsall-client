use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use crate::types::WebUiState;

use super::errors::{write_error_response, write_session_required};
use super::http::write_http_response;

/// GET /api/platform/products — proxy to server platform product list API.
pub(super) async fn handle_platform_products(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    query_string: &str,
) -> anyhow::Result<()> {
    let (server_base, session_token_opt, client) = {
        let guard = state.lock().await;
        (
            guard.server_base.clone(),
            guard.session_token.clone(),
            guard.http_client.clone(),
        )
    };
    let Some(session_token) = session_token_opt else {
        return write_session_required(socket).await;
    };

    let url = if query_string.is_empty() {
        format!("{}/api/v1/platform/products", server_base)
    } else {
        format!("{}/api/v1/platform/products?{}", server_base, query_string)
    };

    match client
        .get(&url)
        .header("x-client-session", &session_token)
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status();
            let body = resp.bytes().await?;
            let status_line = format!(
                "{} {}",
                status.as_u16(),
                status.canonical_reason().unwrap_or("OK")
            );
            write_http_response(socket, &status_line, "application/json", &body).await
        }
        Err(err) => {
            write_error_response(socket, "PRODUCTS_FETCH_FAILED", &format!("{:#}", err)).await
        }
    }
}

/// GET /api/platform/entitlements — proxy to server platform entitlement API.
pub(super) async fn handle_platform_entitlements(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    query_string: &str,
) -> anyhow::Result<()> {
    let (server_base, session_token_opt, client) = {
        let guard = state.lock().await;
        (
            guard.server_base.clone(),
            guard.session_token.clone(),
            guard.http_client.clone(),
        )
    };
    let Some(session_token) = session_token_opt else {
        return write_session_required(socket).await;
    };

    let url = if query_string.is_empty() {
        format!("{}/api/v1/platform/entitlements", server_base)
    } else {
        format!(
            "{}/api/v1/platform/entitlements?{}",
            server_base, query_string
        )
    };

    match client
        .get(&url)
        .header("x-client-session", &session_token)
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status();
            let body = resp.bytes().await?;
            let status_line = format!(
                "{} {}",
                status.as_u16(),
                status.canonical_reason().unwrap_or("OK")
            );
            write_http_response(socket, &status_line, "application/json", &body).await
        }
        Err(err) => {
            write_error_response(socket, "ENTITLEMENTS_FETCH_FAILED", &format!("{:#}", err)).await
        }
    }
}
