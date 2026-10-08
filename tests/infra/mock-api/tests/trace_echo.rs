//! GAP-06 收尾 (批 J): run-scoped trace consumption — the request logger
//! middleware echoes the client's X-WPTSALL-Trace-Id on every response so
//! client / WP / mock timelines correlate on one run id.

use axum::body::Body;
use axum::http::Request;
use axum::http::StatusCode;
use http_body_util::BodyExt;
use mock_translate_api::{build_router, AppState};
use tower::ServiceExt;

fn app() -> axum::Router {
    build_router(AppState::new(), "media/translated")
}

async fn health_with_trace(trace: Option<&str>) -> (StatusCode, Option<String>) {
    let mut builder = Request::builder().method("GET").uri("/api/v1/health");
    if let Some(trace) = trace {
        builder = builder.header("x-wptsall-trace-id", trace);
    }
    let req = builder.body(Body::empty()).unwrap();
    let response = app().oneshot(req).await.unwrap();
    let status = response.status();
    let trace = response
        .headers()
        .get("x-wptsall-trace-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    // Drain the body so the middleware (and the response headers) complete.
    let _ = response.into_body().collect().await.unwrap();
    (status, trace)
}

#[tokio::test]
async fn echoes_run_trace_id_verbatim_on_health_route() {
    let (status, trace) = health_with_trace(Some("disc-run-mock-probe")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        trace.as_deref(),
        Some("disc-run-mock-probe"),
        "run trace must be echoed back on the response"
    );
}

#[tokio::test]
async fn no_trace_header_when_request_ran_without_one() {
    let (status, trace) = health_with_trace(None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        trace.is_none(),
        "a request without a run trace must not get an echo (or an invented fallback)"
    );
}

#[tokio::test]
async fn overlong_trace_id_is_dropped_not_echoed() {
    let overlong = "x".repeat(129);
    let (status, trace) = health_with_trace(Some(&overlong)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        trace.is_none(),
        "trace ids longer than 128 chars must be dropped (validation parity with WP/client)"
    );
}
