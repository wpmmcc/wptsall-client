use super::*;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StorageConfigRequest {
    max_physical_bytes: u64,
    expected_revision: Option<String>,
    confirm_change: bool,
}

pub(super) async fn get(socket: &mut TcpStream) -> anyhow::Result<()> {
    match crate::storage_capacity::snapshot() {
        Ok(data) => {
            write_http_response(
                socket,
                "200 OK",
                "application/json",
                &serde_json::to_vec(&json!({"success":true,"data":data}))?,
            )
            .await
        }
        Err(_) => {
            write_error_response_with_status(
                socket,
                "409 Conflict",
                "STORAGE_CAPACITY_INVALID",
                "The retained storage inventory is unavailable. Existing evidence was retained.",
            )
            .await
        }
    }
}

pub(super) async fn update(socket: &mut TcpStream, body: &[u8]) -> anyhow::Result<()> {
    let request = match serde_json::from_slice::<StorageConfigRequest>(body) {
        Ok(request)
            if request.confirm_change
                && (1..=crate::db::capacity::MAX_CONFIG_LIMIT)
                    .contains(&request.max_physical_bytes) =>
        {
            request
        }
        _ => {
            return write_error_response_with_status(
                socket,
                "422 Unprocessable Entity",
                "STORAGE_CAPACITY_INVALID",
                "Confirm a positive safe-integer byte limit after loading the current policy.",
            )
            .await;
        }
    };
    match crate::storage_capacity::save_policy(
        request.expected_revision.as_deref(),
        request.max_physical_bytes,
    ) {
        Ok(data) => {
            write_http_response(
                socket,
                "200 OK",
                "application/json",
                &serde_json::to_vec(&json!({"success":true,"data":data}))?,
            )
            .await
        }
        Err(_) => write_error_response_with_status(
            socket,
            "409 Conflict",
            "STORAGE_CAPACITY_CHANGED",
            "Storage changed or another writer owns the root. Reload before changing its policy.",
        )
        .await,
    }
}
