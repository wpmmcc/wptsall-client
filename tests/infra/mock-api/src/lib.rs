//! Standalone mock translation API library (CI / Lab / e2e).
//!
//! Binary entry is `main.rs`; integration tests use [`build_router`].

pub mod auth_profiles;
pub mod catalog_parity;
pub mod config;
pub mod handlers;
pub mod lab_cases;
pub mod languages;
pub mod mock_failing;
pub mod official;
pub mod profile_handlers;
pub mod types;
pub mod verify;

use std::collections::HashMap;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::time::Instant;

use axum::{
    extract::{Request, State},
    http::{HeaderValue, StatusCode},
    middleware::{self, Next},
    response::Response,
    routing::{get, post},
    Router,
};
use tokio::sync::Mutex;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;

use config::Config;
use official::OfficialJob;
use types::KeyStats;

/// Shared application state: per-key stats and global request counter.
#[derive(Clone)]
pub struct AppState {
    pub key_stats: Arc<Mutex<HashMap<String, KeyStats>>>,
    pub total_requests: Arc<AtomicUsize>,
    pub official_jobs: Arc<Mutex<HashMap<String, OfficialJob>>>,
    pub fault_injection: Arc<Mutex<mock_failing::FaultInjectionState>>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            key_stats: Arc::new(Mutex::new(HashMap::new())),
            total_requests: Arc::new(AtomicUsize::new(0)),
            official_jobs: Arc::new(Mutex::new(HashMap::new())),
            fault_injection: Arc::new(Mutex::new(mock_failing::FaultInjectionState::default())),
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

fn strict_bearer_enabled() -> bool {
    std::env::var("MOCK_TRANSLATE_STRICT_BEARER")
        .map(|v| {
            let normalized = v.trim().to_ascii_lowercase();
            matches!(normalized.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(false)
}

async fn auth(req: Request, next: Next) -> Result<Response, StatusCode> {
    let path = req.uri().path();
    if path == "/api/v1/health"
        || path == "/api/v1/stats"
        || path == "/api/v1/stats/reset"
        || path == "/api/v1/languages"
        || path == "/api/v1/models"
        || path == "/v1/models"
        || path == "/api/v1/mock-auth-profiles"
        || path == "/api/v1/lab-provider-cases"
        || path.starts_with("/api/v1/fault-injection")
        || path.starts_with("/api/v1/test-file/")
    {
        return Ok(next.run(req).await);
    }

    let strict = strict_bearer_enabled();
    let expected_key = Config::from_env().api_key;
    let authorized = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.strip_prefix("Bearer ")
                .map(|token| {
                    if token.is_empty() {
                        false
                    } else if strict {
                        token == expected_key
                    } else {
                        true
                    }
                })
                .unwrap_or(false)
        })
        .unwrap_or(false);

    if authorized {
        Ok(next.run(req).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MockLogLevel {
    Debug,
    Info,
    Warn,
    Error,
    Off,
}

fn get_log_level() -> MockLogLevel {
    if let Ok(lvl) = std::env::var("MOCK_LOG_LEVEL") {
        match lvl.trim().to_ascii_lowercase().as_str() {
            "debug" | "trace" | "verbose" => return MockLogLevel::Debug,
            "info" => return MockLogLevel::Info,
            "warn" | "warning" => return MockLogLevel::Warn,
            "error" => return MockLogLevel::Error,
            "off" | "none" | "0" => return MockLogLevel::Off,
            _ => {}
        }
    }
    if let Ok(v) = std::env::var("MOCK_LOG_ALL_REQUESTS") {
        let normalized = v.trim().to_ascii_lowercase();
        if matches!(normalized.as_str(), "0" | "false" | "no" | "off") {
            return MockLogLevel::Warn;
        }
    }
    MockLogLevel::Info
}

fn should_log_health() -> bool {
    std::env::var("MOCK_LOG_HEALTH")
        .map(|v| {
            let n = v.trim().to_ascii_lowercase();
            matches!(n.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(false)
}

fn summarize_auth(headers: &axum::http::HeaderMap, query_str: Option<&str>) -> String {
    if let Some(auth) = headers.get("authorization").and_then(|v| v.to_str().ok()) {
        let auth_str = auth.trim();
        if auth_str.eq_ignore_ascii_case("bearer") {
            return "Bearer(EMPTY)".to_string();
        }
        if let Some(rest) = auth_str.strip_prefix("Bearer ").or_else(|| auth_str.strip_prefix("bearer ")) {
            let token = rest.trim();
            if token.is_empty() {
                return "Bearer(EMPTY)".to_string();
            }
            if token.starts_with("eyJ") {
                return format!("Bearer(JWT len={})", token.len());
            }
            let preview: String = token.chars().take(8).collect();
            return format!("Bearer({}.. len={})", preview, token.len());
        }
        if auth_str.starts_with("AWS4-HMAC-SHA256") {
            return "AWS4-SigV4".to_string();
        }
        if auth_str.starts_with("TC3-HMAC-SHA256") {
            return "TC3-HMAC".to_string();
        }
        let preview: String = auth_str.chars().take(12).collect();
        return format!("auth:{}..", preview);
    }
    if let Some(v) = headers.get("ocp-apim-subscription-key").and_then(|v| v.to_str().ok()) {
        let preview: String = v.chars().take(6).collect();
        return format!("Ocp-Key({}.. len={})", preview, v.len());
    }
    if let Some(v) = headers.get("x-goog-api-key").and_then(|v| v.to_str().ok()) {
        let preview: String = v.chars().take(6).collect();
        return format!("Goog-Key({}.. len={})", preview, v.len());
    }
    if let Some(v) = headers.get("deepl-auth-key").and_then(|v| v.to_str().ok()) {
        let preview: String = v.chars().take(6).collect();
        return format!("DeepL-Key({}.. len={})", preview, v.len());
    }
    if let Some(v) = headers.get("x-api-key").and_then(|v| v.to_str().ok()) {
        let preview: String = v.chars().take(6).collect();
        return format!("X-Api-Key({}.. len={})", preview, v.len());
    }
    if let Some(q) = query_str {
        if q.contains("appid=") {
            return "query(appid)".to_string();
        }
        if q.contains("appKey=") {
            return "query(appKey)".to_string();
        }
        if q.contains("key=") {
            return "query(key)".to_string();
        }
        if q.contains("auth_key=") {
            return "query(auth_key)".to_string();
        }
    }
    "none".to_string()
}

async fn request_logger(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let path = uri.path().to_string();
    let query_str = uri.query().map(|q| q.to_string());
    // GAP-06 收尾 (批 J): client run-scoped trace. The discovery/sync
    // lanes stamp X-WPTSALL-Trace-Id on every vendor-bound request while a
    // run is active; echo it on the response and carry it in the log line
    // so client / WP / mock timelines correlate on one run id.
    let run_trace_id = req
        .headers()
        .get("x-wptsall-trace-id")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty() && v.len() <= 128);
    let auth_summary = summarize_auth(req.headers(), query_str.as_deref());
    let extra_action = if let Some(a) = req.headers().get("x-tc-action").and_then(|v| v.to_str().ok()) {
        format!(" tc-action={a}")
    } else if let Some(t) = req.headers().get("x-amz-target").and_then(|v| v.to_str().ok()) {
        format!(" amz-target={t}")
    } else {
        String::new()
    };

    let log_level = get_log_level();
    let started_at = Instant::now();
    let mut response = next.run(req).await;
    let elapsed_ms = started_at.elapsed().as_millis() as u64;
    let status = response.status().as_u16();

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);

    // Attach diagnostic headers for clients/runners
    if let Ok(v) = HeaderValue::from_str(&elapsed_ms.to_string()) {
        response.headers_mut().insert("x-mock-latency-ms", v);
    }
    if let Ok(v) = HeaderValue::from_str(&now_ms.to_string()) {
        response.headers_mut().insert("x-mock-timestamp-ms", v);
    }
    // GAP-06 收尾: echo the run trace back so HTTP-level observers see the
    // same id the client logged for the run (ungated — correlation must not
    // depend on the log level).
    if let Some(trace) = &run_trace_id {
        if let Ok(v) = HeaderValue::from_str(trace) {
            response.headers_mut().insert("x-wptsall-trace-id", v);
        }
    }

    let is_health = path == "/api/v1/health";
    if is_health && !should_log_health() {
        return response;
    }

    let should_log = match log_level {
        MockLogLevel::Debug | MockLogLevel::Info => true,
        MockLogLevel::Warn => status >= 400 || elapsed_ms >= 500 || response.headers().contains_key("x-mock-auth-error"),
        MockLogLevel::Error => status >= 500,
        MockLogLevel::Off => false,
    };

    if should_log {
        let secs = now_ms / 1000;
        let rem_ms = now_ms % 1000;
        let h = (secs / 3600) % 24;
        let m = (secs / 60) % 60;
        let s = secs % 60;

        let full_uri = if let Some(q) = &query_str {
            format!("{}?{}", path, q)
        } else {
            path.clone()
        };

        let mut diag = Vec::new();
        if let Some(trace) = &run_trace_id {
            diag.push(format!("trace: {}", trace));
        }
        if auth_summary != "none" {
            diag.push(format!("auth: {}", auth_summary));
        } else {
            diag.push("auth: none".to_string());
        }
        if !extra_action.is_empty() {
            diag.push(extra_action.trim().to_string());
        }
        if let Some(v) = response.headers().get("x-mock-vendor").and_then(|v| v.to_str().ok()) {
            diag.push(format!("vendor: {}", v));
        }
        if let Some(err) = response.headers().get("x-mock-auth-error").and_then(|v| v.to_str().ok()) {
            diag.push(format!("AUTH_ERR: {}", err));
        }

        let diag_str = if diag.is_empty() {
            String::new()
        } else {
            format!(" [{}]", diag.join(", "))
        };

        println!(
            "[mock {:02}:{:02}:{:02}.{:03} | {}ms] {} {} -> {} ({} ms){}",
            h, m, s, rem_ms, now_ms, method, full_uri, status, elapsed_ms, diag_str
        );
    }

    response
}

/// Stamp `X-Mock-Mode: synthetic` on every sign-route response so consumers
/// can distinguish the synthetic `/api/<vendor>/translate` family (unified
/// verification shapes) from official vendor-native response shapes served
/// by the fallback router.
async fn stamp_synthetic_mode(req: Request, next: Next) -> Response {
    let mut response = next.run(req).await;
    response
        .headers_mut()
        .insert("x-mock-mode", HeaderValue::from_static("synthetic"));
    response
}

async fn mock_failing_middleware(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    let path = req.uri().path().to_string();
    let skip = path == "/api/v1/health"
        || path == "/api/v1/stats"
        || path == "/api/v1/stats/reset"
        || path == "/api/v1/languages"
        || path == "/api/v1/models"
        || path == "/v1/models"
        || path.starts_with("/api/v1/fault-injection")
        || path.starts_with("/api/v1/test-file/")
        || path.starts_with("/media/");
    if skip {
        return next.run(req).await;
    }
    if let Some(fault) = mock_failing::maybe_inject_fault(req.headers()).await {
        return fault;
    }
    if let Some(fault) = mock_failing::maybe_inject_state_fault(&state).await {
        return fault;
    }
    next.run(req).await
}

/// Build the full mock router (bearer + signed + official fallback).
pub fn build_router(state: AppState, media_dir: &str) -> Router {
    let bearer_routes = Router::new()
        .route("/api/v1/health", get(handlers::health))
        .route("/api/v1/languages", get(handlers::list_languages))
        .route("/api/v1/models", get(handlers::list_models))
        .route("/v1/models", get(handlers::list_models))
        .route(
            "/api/v1/mock-auth-profiles",
            get(profile_handlers::list_profiles),
        )
        .route(
            "/api/v1/lab-provider-cases",
            get(lab_cases::list_lab_provider_cases),
        )
        .route(
            "/api/v1/fault-injection",
            get(mock_failing::get_fault_injection).post(mock_failing::set_fault_injection),
        )
        .route(
            "/api/v1/fault-injection/reset",
            post(mock_failing::reset_fault_injection),
        )
        .route(
            "/api/v1/translate/text",
            post(handlers::translate_text_handler),
        )
        .route("/api/v1/translate/image", post(handlers::translate_image))
        .route("/api/v1/translate/audio", post(handlers::translate_audio))
        .route("/api/v1/translate/video", post(handlers::translate_video))
        .route(
            "/api/v1/translate/document",
            post(handlers::translate_document),
        )
        .route("/api/v1/translate/batch", post(handlers::translate_batch))
        .route(
            "/v1/chat/completions",
            post(handlers::openai_chat_completions),
        )
        .route("/api/v1/stats", get(handlers::handle_stats))
        .route("/api/v1/stats/reset", post(handlers::handle_stats_reset))
        .route(
            "/api/v1/test-file/:size_kb",
            get(handlers::handle_test_file).head(handlers::handle_test_file),
        )
        .layer(middleware::from_fn(auth))
        .with_state(state.clone());

    let sign_routes = Router::new()
        .route("/api/baidu/translate", post(handlers::baidu_translate))
        .route("/api/youdao/translate", post(handlers::youdao_translate))
        .route("/api/hmac/translate", post(handlers::hmac_translate))
        .route("/api/tencent/translate", post(handlers::tencent_translate))
        .route("/api/aws/translate", post(handlers::aws_translate))
        .route(
            "/api/volcengine/translate",
            post(handlers::volcengine_translate),
        )
        .route("/api/alibaba/translate", post(handlers::alibaba_translate))
        .route("/api/azure/translate", post(handlers::azure_translate))
        .route("/api/kakao/translate", post(handlers::kakao_translate))
        .route("/api/iflytek/translate", post(handlers::iflytek_translate))
        .route(
            "/api/niutrans/translate",
            post(handlers::niutrans_translate),
        )
        .route("/api/oauth/token", post(handlers::oauth_token))
        .route("/api/oauth/authorize", get(handlers::oauth_authorize))
        .route("/api/oauth/translate", post(handlers::oauth_translate))
        .route("/api/jwt/translate", post(handlers::jwt_translate))
        .layer(middleware::from_fn(stamp_synthetic_mode))
        .with_state(state.clone());

    let custom_routes = Router::new()
        .route("/oauth/authorize", get(handlers::oauth_authorize))
        .route("/oauth/token", post(handlers::oauth_token))
        .route("/mock/custom/*path", post(handlers::mock_custom_target))
        .with_state(state.clone());

    let media_service = ServeDir::new(media_dir);

    Router::new()
        .merge(bearer_routes)
        .merge(sign_routes)
        .merge(custom_routes)
        .nest_service("/media", media_service)
        .fallback(official::handle_official)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            mock_failing_middleware,
        ))
        .layer(middleware::from_fn(request_logger))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

/// Whether strict bearer mode is enabled (exported for main banner).
pub fn is_strict_bearer_enabled() -> bool {
    strict_bearer_enabled()
}
