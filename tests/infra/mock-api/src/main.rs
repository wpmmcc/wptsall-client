//! Standalone mock translation API — for isolated testing only.
//!
//! Official mock component templates target this service on http://127.0.0.1:9090.

use mock_translate_api::{build_router, config::Config, is_strict_bearer_enabled, AppState};

#[tokio::main]
async fn main() {
    let config = Config::from_env();
    let host = std::env::var("MOCK_TRANSLATE_HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
    let addr = format!("{}:{}", host, config.port);
    let strict_bearer = is_strict_bearer_enabled();

    let media_dir =
        std::env::var("MOCK_MEDIA_DIR").unwrap_or_else(|_| "media/translated".to_string());

    let state = AppState::new();
    let app = build_router(state, &media_dir);

    println!("Mock Translate API listening on {addr}");
    println!("Serving translated media from: {media_dir}");
    println!("Strict Bearer verification: {strict_bearer}");
    println!("Endpoints:");
    if strict_bearer {
        println!("  POST /api/v1/translate/text      (Bearer must match MOCK_TRANSLATE_API_KEY)");
    } else {
        println!("  POST /api/v1/translate/text      (Bearer token, any non-empty)");
    }
    println!("  POST /v1/chat/completions        (OpenAI-compatible)");
    println!("  POST /api/baidu/translate         (MD5 sign)");
    println!("  POST /api/youdao/translate        (SHA256 sign)");
    println!("  POST /api/hmac/translate          (HMAC-SHA256 sign)");
    println!("  POST /api/tencent/translate       (TC3-HMAC-SHA256)");
    println!("  POST /api/aws/translate           (AWS SigV4)");
    println!("  POST /api/volcengine/translate    (Volcengine HMAC-SHA256)");
    println!("  POST /api/alibaba/translate       (Alibaba V1)");
    println!("  POST /api/azure/translate         (Subscription Key)");
    println!("  POST /api/kakao/translate         (KakaoAK)");
    println!("  POST /api/oauth/token             (OAuth client_credentials)");
    println!("  POST /api/oauth/translate         (OAuth Bearer JWT)");
    println!("  POST /api/jwt/translate           (JWT Bearer RSA-SHA256)");
    println!("  GET  /api/v1/languages            (language catalog)");
    println!("  GET  /api/v1/models               (model catalog)");
    println!("  GET  /api/v1/stats                (per-key stats)");
    println!("  POST /api/v1/stats/reset          (reset stats)");
    println!("  GET|HEAD /api/v1/test-file/:size_kb (virtual test file)");
    println!("  GET  /api/v1/mock-auth-profiles   (multi-auth profiles)");
    println!("  GET  /api/v1/lab-provider-cases   (112+ UI fill: auth/url/params)");
    println!("  catalog parity paths              (yandex/papago/smartcat/phrase/…)");

    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
