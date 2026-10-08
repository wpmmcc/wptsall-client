//! Mock-failing profile + catalog endpoints + SSE helpers for T-MOCK.
//!
//! Fault injection is driven by request headers (deterministic for tests):
//! - `X-Mock-Fail-Mode: 429|500|timeout` forces a specific fault
//! - `X-Mock-Fail-Probability: 0.0..=1.0` enables probabilistic faults
//! - `X-Mock-Timeout-Ms` bounds the timeout sleep (default 3500)

use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::AppState;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaultInjectionState {
    pub enabled: bool,
    #[serde(default = "default_mode")]
    pub mode: String, // "429", "500", "timeout", "delay"
    #[serde(default)]
    pub delay_ms: u64,
    #[serde(default = "default_retry_after")]
    pub retry_after_secs: u64,
    #[serde(default)]
    pub remaining_count: Option<usize>,
    #[serde(default = "default_probability")]
    pub probability: f64,
}

fn default_mode() -> String {
    "429".to_string()
}

fn default_retry_after() -> u64 {
    1
}

fn default_probability() -> f64 {
    1.0
}

impl Default for FaultInjectionState {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: "429".to_string(),
            delay_ms: 0,
            retry_after_secs: 1,
            remaining_count: None,
            probability: 1.0,
        }
    }
}

pub async fn get_fault_injection(State(state): State<AppState>) -> Json<FaultInjectionState> {
    let guard = state.fault_injection.lock().await;
    Json(guard.clone())
}

pub async fn set_fault_injection(
    State(state): State<AppState>,
    Json(payload): Json<FaultInjectionState>,
) -> Json<Value> {
    let mut guard = state.fault_injection.lock().await;
    *guard = payload.clone();
    Json(json!({
        "success": true,
        "config": payload
    }))
}

pub async fn reset_fault_injection(State(state): State<AppState>) -> Json<Value> {
    let mut guard = state.fault_injection.lock().await;
    *guard = FaultInjectionState::default();
    Json(json!({
        "success": true
    }))
}

pub async fn maybe_inject_state_fault(state: &AppState) -> Option<Response> {
    let mut guard = state.fault_injection.lock().await;
    if !guard.enabled {
        return None;
    }
    if guard.probability > 0.0 && guard.probability < 1.0 {
        if !rand::thread_rng().gen_bool(guard.probability) {
            return None;
        }
    }

    let mode = guard.mode.clone();
    let delay_ms = guard.delay_ms;
    let retry_after_secs = guard.retry_after_secs;

    if let Some(remaining) = guard.remaining_count.as_mut() {
        if *remaining > 0 {
            *remaining -= 1;
            if *remaining == 0 {
                guard.enabled = false;
            }
        } else {
            guard.enabled = false;
            return None;
        }
    }
    drop(guard);

    match mode.as_str() {
        "429" => {
            let mut resp = (
                StatusCode::TOO_MANY_REQUESTS,
                Json(json!({
                    "error": {
                        "code": "rate_limit_exceeded",
                        "message": "mock-failing: state-injected rate limit (429)",
                        "type": "rate_limit_error"
                    }
                })),
            )
                .into_response();
            resp.headers_mut().insert(
                "retry-after",
                HeaderValue::from_str(&retry_after_secs.to_string())
                    .unwrap_or(HeaderValue::from_static("1")),
            );
            Some(resp)
        }
        "delay" => {
            if delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
            None
        }
        "timeout" => {
            let ms = if delay_ms > 0 { delay_ms } else { 3500 };
            tokio::time::sleep(Duration::from_millis(ms)).await;
            Some(
                (
                    StatusCode::GATEWAY_TIMEOUT,
                    Json(json!({
                        "error": {
                            "code": "timeout",
                            "message": format!("mock-failing: timed out after {ms}ms"),
                            "type": "timeout_error"
                        }
                    })),
                )
                    .into_response(),
            )
        }
        _ => Some(
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": {
                        "code": "internal_error",
                        "message": "mock-failing: state-injected 500",
                        "type": "server_error"
                    }
                })),
            )
                .into_response(),
        ),
    }
}

/// Parse fault controls from headers and optionally return an early response.
pub async fn maybe_inject_fault(headers: &HeaderMap) -> Option<Response> {
    let mode = header_str(headers, "x-mock-fail-mode")
        .map(|v| v.to_ascii_lowercase())
        .filter(|v| matches!(v.as_str(), "429" | "500" | "timeout"));
    let probability = header_str(headers, "x-mock-fail-probability")
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0)
        .clamp(0.0, 1.0);

    let chosen = if let Some(m) = mode {
        // Explicit mode: always fire unless probability is set and RNG misses.
        if probability > 0.0 && probability < 1.0 && !rand::thread_rng().gen_bool(probability) {
            return None;
        }
        m
    } else if probability > 0.0 {
        if !rand::thread_rng().gen_bool(probability) {
            return None;
        }
        match rand::thread_rng().gen_range(0u8..3) {
            0 => "429".to_string(),
            1 => "500".to_string(),
            _ => "timeout".to_string(),
        }
    } else {
        return None;
    };

    Some(fault_response(&chosen, headers).await)
}

async fn fault_response(mode: &str, headers: &HeaderMap) -> Response {
    match mode {
        "429" => {
            let mut resp = (
                StatusCode::TOO_MANY_REQUESTS,
                Json(json!({
                    "error": {
                        "code": "rate_limit_exceeded",
                        "message": "mock-failing: rate limited",
                        "type": "rate_limit_error"
                    }
                })),
            )
                .into_response();
            resp.headers_mut()
                .insert("retry-after", HeaderValue::from_static("1"));
            resp
        }
        "timeout" => {
            let ms = header_str(headers, "x-mock-timeout-ms")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(3500)
                .min(30_000);
            tokio::time::sleep(Duration::from_millis(ms)).await;
            (
                StatusCode::GATEWAY_TIMEOUT,
                Json(json!({
                    "error": {
                        "code": "timeout",
                        "message": format!("mock-failing: timed out after {ms}ms"),
                        "type": "timeout_error"
                    }
                })),
            )
                .into_response()
        }
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "error": {
                    "code": "internal_error",
                    "message": "mock-failing: forced 500",
                    "type": "server_error"
                }
            })),
        )
            .into_response(),
    }
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok()).map(str::trim)
}

pub fn languages_payload() -> Value {
    json!({
        "object": "list",
        "data": [
            {"code": "en", "name": "English"},
            {"code": "zh-CN", "name": "Chinese (Simplified)"},
            {"code": "zh-TW", "name": "Chinese (Traditional)"},
            {"code": "ja", "name": "Japanese"},
            {"code": "ko", "name": "Korean"},
            {"code": "fr", "name": "French"},
            {"code": "de", "name": "German"},
            {"code": "es", "name": "Spanish"}
        ]
    })
}

pub fn models_payload() -> Value {
    json!({
        "object": "list",
        "data": [
            {
                "id": "mock-openai",
                "object": "model",
                "owned_by": "mock-translate-api",
                "created": 1700000000
            },
            {
                "id": "mock-translate-model",
                "object": "model",
                "owned_by": "mock-translate-api",
                "created": 1700000000
            }
        ]
    })
}

/// Build OpenAI-compatible SSE body for `stream=true` chat completions.
pub fn openai_sse_body(model: &str, content: &str) -> String {
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let chunk = json!({
        "id": format!("chatcmpl-mock-{}", created),
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "delta": {"role": "assistant", "content": content},
            "finish_reason": null
        }]
    });
    let done = json!({
        "id": format!("chatcmpl-mock-{}", created),
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "delta": {},
            "finish_reason": "stop"
        }]
    });
    format!("data: {chunk}\n\ndata: {done}\n\ndata: [DONE]\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[tokio::test]
    async fn forced_429_returns_retry_after() {
        let mut headers = HeaderMap::new();
        headers.insert("x-mock-fail-mode", HeaderValue::from_static("429"));
        let resp = maybe_inject_fault(&headers).await.expect("fault");
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            resp.headers().get("retry-after").and_then(|v| v.to_str().ok()),
            Some("1")
        );
    }

    #[tokio::test]
    async fn forced_500_returns_server_error() {
        let mut headers = HeaderMap::new();
        headers.insert("x-mock-fail-mode", HeaderValue::from_static("500"));
        let resp = maybe_inject_fault(&headers).await.expect("fault");
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn no_headers_means_no_fault() {
        let headers = HeaderMap::new();
        assert!(maybe_inject_fault(&headers).await.is_none());
    }

    #[test]
    fn languages_and_models_are_non_empty() {
        assert!(languages_payload()["data"].as_array().unwrap().len() >= 4);
        assert!(models_payload()["data"].as_array().unwrap().len() >= 2);
    }

    #[test]
    fn sse_body_contains_done_marker() {
        let body = openai_sse_body("mock-openai", "hello");
        assert!(body.contains("data: [DONE]"));
        assert!(body.contains("chat.completion.chunk"));
        assert!(body.contains("hello"));
    }

    #[tokio::test]
    async fn state_fault_429_with_remaining_count_and_auto_recovery() {
        let state = AppState::new();
        {
            let mut guard = state.fault_injection.lock().await;
            guard.enabled = true;
            guard.mode = "429".to_string();
            guard.remaining_count = Some(2);
            guard.retry_after_secs = 3;
        }

        // Call 1: 429
        let resp1 = maybe_inject_state_fault(&state).await.expect("fault 1");
        assert_eq!(resp1.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            resp1.headers().get("retry-after").and_then(|v| v.to_str().ok()),
            Some("3")
        );

        // Call 2: 429
        let resp2 = maybe_inject_state_fault(&state).await.expect("fault 2");
        assert_eq!(resp2.status(), StatusCode::TOO_MANY_REQUESTS);

        // Call 3: recovered to None
        let resp3 = maybe_inject_state_fault(&state).await;
        assert!(resp3.is_none(), "Should have auto-recovered after remaining_count reached 0");

        // Verify state is disabled
        let guard = state.fault_injection.lock().await;
        assert!(!guard.enabled);
    }

    #[tokio::test]
    async fn state_fault_delay_execution() {
        let state = AppState::new();
        {
            let mut guard = state.fault_injection.lock().await;
            guard.enabled = true;
            guard.mode = "delay".to_string();
            guard.delay_ms = 50;
            guard.remaining_count = Some(1);
        }

        let start = std::time::Instant::now();
        let resp = maybe_inject_state_fault(&state).await;
        assert!(resp.is_none(), "delay returns None so request proceeds");
        assert!(start.elapsed().as_millis() >= 45);
    }

    #[tokio::test]
    async fn state_fault_timeout() {
        let state = AppState::new();
        {
            let mut guard = state.fault_injection.lock().await;
            guard.enabled = true;
            guard.mode = "timeout".to_string();
            guard.delay_ms = 10;
            guard.remaining_count = Some(1);
        }

        let resp = maybe_inject_state_fault(&state).await.expect("timeout fault");
        assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT);
    }
}
