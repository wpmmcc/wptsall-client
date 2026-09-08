use crate::auth::UpstreamApiError;
use serde_json::{json, Value};
use tokio::net::TcpStream;

use super::http::write_http_response;

pub(super) async fn write_session_required(socket: &mut TcpStream) -> anyhow::Result<()> {
    let payload = json!({
        "success": false,
        "error": { "code": "SESSION_REQUIRED", "message": "Please login first" }
    });
    write_http_response(
        socket,
        "401 Unauthorized",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(super) async fn write_error_response_with_status(
    socket: &mut TcpStream,
    status: &str,
    code: &str,
    message: &str,
) -> anyhow::Result<()> {
    let payload = json!({
        "success": false,
        "error": { "code": code, "message": message }
    });
    write_http_response(
        socket,
        status,
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(super) async fn write_error_response(
    socket: &mut TcpStream,
    code: &str,
    message: &str,
) -> anyhow::Result<()> {
    write_error_response_with_status(socket, "400 Bad Request", code, message).await
}

pub(super) async fn write_not_found_response(
    socket: &mut TcpStream,
    code: &str,
    message: &str,
) -> anyhow::Result<()> {
    write_error_response_with_status(socket, "404 Not Found", code, message).await
}

pub(super) async fn write_conflict_response(
    socket: &mut TcpStream,
    code: &str,
    message: &str,
) -> anyhow::Result<()> {
    write_error_response_with_status(socket, "409 Conflict", code, message).await
}

fn find_upstream_api_error(err: &anyhow::Error) -> Option<&UpstreamApiError> {
    err.chain()
        .find_map(|source| source.downcast_ref::<UpstreamApiError>())
}

fn extract_status_line_from_error_text(err_text: &str) -> Option<String> {
    for needle in ["status=", "status "] {
        let Some(pos) = err_text.find(needle) else {
            continue;
        };
        let rest = &err_text[pos + needle.len()..];
        let candidate = rest
            .split([',', ')'])
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty())?;
        if candidate
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_digit())
        {
            return Some(candidate.to_string());
        }
    }
    None
}

fn extract_json_object_after<'a>(haystack: &'a str, needle: &str) -> Option<&'a str> {
    let start = haystack.find(needle)? + needle.len();
    let json_start = haystack[start..].find('{')? + start;
    let mut depth = 0usize;
    let mut started = false;
    for (idx, ch) in haystack[json_start..].char_indices() {
        match ch {
            '{' => {
                depth = depth.saturating_add(1);
                started = true;
            }
            '}' => {
                depth = depth.saturating_sub(1);
                if started && depth == 0 {
                    let end = json_start + idx + ch.len_utf8();
                    return Some(&haystack[json_start..end]);
                }
            }
            _ => {}
        }
    }
    None
}

pub(super) fn find_passthrough_http_error(err: &anyhow::Error) -> Option<(String, String, String)> {
    if let Some(upstream) = find_upstream_api_error(err) {
        return Some((
            upstream.status.clone(),
            upstream.code.clone(),
            upstream.message.clone(),
        ));
    }

    let err_text = format!("{:#}", err);
    let status = extract_status_line_from_error_text(&err_text)?;
    let body = extract_json_object_after(&err_text, "body=")?;
    let parsed_body: Value = serde_json::from_str(body).ok()?;
    let code = parsed_body.get("code")?.as_str()?.trim();
    let message = parsed_body.get("message")?.as_str()?.trim();
    if code.is_empty() || message.is_empty() {
        return None;
    }
    Some((status, code.to_string(), message.to_string()))
}

pub(super) async fn maybe_write_upstream_api_error(
    socket: &mut TcpStream,
    err: &anyhow::Error,
) -> Option<anyhow::Result<()>> {
    let (status, code, message) = find_passthrough_http_error(err)?;
    Some(write_error_response_with_status(socket, &status, &code, &message).await)
}

pub(super) fn review_failed_item_payload(id: i64, err: &anyhow::Error) -> Value {
    if let Some((status, code, message)) = find_passthrough_http_error(err) {
        json!({
            "id": id,
            "status": status,
            "code": code,
            "message": message
        })
    } else {
        json!({
            "id": id,
            "error": format!("{:#}", err)
        })
    }
}
