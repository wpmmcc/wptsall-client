//! Resume the frozen candidate before its file exists, using the real route.
use super::*;

#[tokio::test]
async fn encrypted_json_candidate_only_fresh_process_restores_same_bytes_without_provider() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider = format!("http://{}", listener.local_addr().unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            count.fetch_add(1, Ordering::SeqCst);
            let response = b"HTTP/1.1 500 Owned unexpected request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            let _ = socket.write_all(response).await;
        }
    });
    let db = seed(root.path(), &provider).await;
    let absent_source = root.path().join("never-created-source.json");
    assert_eq!(
        db.lock()
            .await
            .execute(
                "UPDATE translation_items SET raw_path=?1 WHERE id=1",
                [absent_source.to_str().unwrap()],
            )
            .unwrap(),
        1,
    );
    let item = crate::db::jobs::get_item_checked(&*db.lock().await, 1)
        .unwrap()
        .unwrap();
    assert!(
        item.raw_path == absent_source.to_string_lossy() && !absent_source.exists(),
        "there is no source file to redispatch"
    );
    let plan = crate::db::review_attempts::saved_plan(&*db.lock().await, 1, REQUEST)
        .unwrap()
        .unwrap();
    let lease = crate::db::unit_lock::UnitLease::item(&db, 1).await.unwrap();
    let scope =
        crate::db::review_attempts::scope(&*db.lock().await, &db, &lease, &item, &plan, REQUEST)
            .unwrap();
    let payload: TranslationCallbackPayload = serde_json::from_value(json!({
        "relation_id":1,"business_line":"post_content","object_type":"post_type","post_type":"post","object_id":item.wp_object_id,
        "translated_fields":{"post_title":"owned candidate result"},"translated_meta":{},"media_mappings":[],
        "client_task_id":format!("manual-{REQUEST}"),"worker_id":"owned","source_lang":"en_US","target_lang":"zh_CN",
        "execution_time_ms":1,"source_revision":"owned-revision"
    })).unwrap();
    let path = root.path().join("candidate-before-file.json");
    let frozen = crate::db::review_attempts::prepare_result(
        &db,
        &lease,
        &scope,
        1,
        &json!({"idempotency_key":format!("manual-{REQUEST}"),"payload":payload}),
        path.to_str().unwrap(),
    )
    .await
    .unwrap();
    assert!(frozen.starts_with(b"WPTC"));
    assert!(
        !path.exists(),
        "the original file-publication window is open"
    );
    drop(scope);
    drop(lease);
    drop(db);
    let mut child = spawn(root.path(), "candidate-recovered");
    wait(&mut child).await;
    let response =
        std::fs::read_to_string(root.path().join("candidate-recovered.response")).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert_eq!(parse_http_json_body(&response)["data"]["replayed"], true);
    assert_eq!(
        parse_http_json_body(&response)["data"]["translated_path"],
        path.to_str().unwrap()
    );
    assert_eq!(std::fs::read(&path).unwrap(), frozen);
    let reopened = crate::db::open_db(root.path().join("owned.sqlite").to_str().unwrap()).unwrap();
    assert_eq!(
        crate::db::review_attempts::replay(&reopened, 1, REQUEST)
            .unwrap()
            .unwrap()
            .translated_path,
        path.to_string_lossy()
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "candidate recovery must precede provider access"
    );
    server.abort();
    let _ = server.await;
}
