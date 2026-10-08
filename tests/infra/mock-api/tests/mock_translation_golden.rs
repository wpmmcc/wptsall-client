//! R9: the actual axum router, not a duplicate translation implementation.
use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use mock_translate_api::{build_router, AppState};
use serde_json::{json, Value};
use std::path::Path;
use tower::ServiceExt;

#[tokio::test]
async fn actual_router_obeys_shared_translation_vectors() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../catalog/mock-translation-golden.json");
    let data: Value = serde_json::from_str(&std::fs::read_to_string(fixture).unwrap()).unwrap();
    assert_eq!(data["version"], 1);
    let cases = data["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 10, "nonempty, exact shared corpus");
    let app = build_router(AppState::new(), "media/translated");
    for case in cases {
        let expected = case["status"].as_u64().unwrap();
        let mut builder = Request::builder()
            .method("POST")
            .uri("/api/v1/translate/text")
            .header("content-type", "application/json")
            .header("authorization", "Bearer mock-golden-owned-key");
        if expected >= 400 {
            builder = builder.header("x-mock-fail-mode", expected.to_string());
        }
        let response = app
            .clone()
            .oneshot(
                builder
                    .body(Body::from(case["payload"].to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status().as_u16();
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(u64::from(status), expected, "{}", case["id"]);
        if expected == 200 {
            assert_eq!(
                body["translated_text"], case["translated_text"],
                "{}",
                case["id"]
            );
        } else {
            assert!(
                body.get("translated_text").is_none(),
                "fault cannot claim output"
            );
            assert!(body.get("error").is_some(), "fault evidence required");
        }
        println!(
            "MOCK_GOLDEN_CASE {}",
            json!({"id":case["id"],"status":status,"body":body})
        );
    }
}
