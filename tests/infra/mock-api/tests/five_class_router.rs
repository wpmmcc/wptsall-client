//! The real mock router, one request per content class.
//! Text stays text. Image, video, audio, and document each return a file
//! URL and a separate text product. The text product is not the file URL.
use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use mock_translate_api::{build_router, AppState};
use serde_json::Value;
use tower::ServiceExt;

async fn post(app: &axum::Router, path: &str, body: &str) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .header("authorization", "Bearer mock-five-class")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "{path}");
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

#[tokio::test]
async fn five_classes_on_the_mock_router() {
    let app = build_router(AppState::new(), "media/translated");

    let text = post(
        &app,
        "/api/v1/translate/text",
        r#"{"text":"Hello","source_lang":"en","target_lang":"zh","content_format":"plain_text"}"#,
    )
    .await;
    let text_out = text["translated_text"].as_str().unwrap();
    assert!(text_out.contains("【zh】") && text_out.contains("Hello"));
    assert!(text.get("translated_ref").is_none());

    let cases = [
        ("/api/v1/translate/image", "image", "ocr", "https://cdn.example/a.png", "-zh"),
        ("/api/v1/translate/video", "video", "subtitle", "https://cdn.example/clip.mp4", "-zh"),
        ("/api/v1/translate/audio", "audio", "transcript", "https://cdn.example/talk.mp3", "-zh"),
        ("/api/v1/translate/document", "document", "document", "https://cdn.example/spec.pdf", "-zh"),
    ];
    for (path, _class, kind, source, suffix) in cases {
        let body = format!(
            r#"{{"source_ref":"{source}","source_lang":"en","target_lang":"zh"}}"#
        );
        let value = post(&app, path, &body).await;
        let file_url = value["translated_ref"].as_str().unwrap();
        let words = value["translated_text"].as_str().unwrap();
        assert!(file_url.contains(suffix), "{path} {file_url}");
        assert!(words.contains("【zh】") && words.contains(kind), "{path} {words}");
        assert_ne!(words, file_url, "{path} text product must not be the file url");
    }
}
