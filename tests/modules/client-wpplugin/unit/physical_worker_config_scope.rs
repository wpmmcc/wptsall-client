#[tokio::test]
async fn physical_worker_config_live_usage_changes_without_changing_saved_configuration() {
    let _scope = components_env_lock().lock().unwrap();
    let state = build_test_web_ui_state("http://127.0.0.1:1", None);
    let (mut socket, reader) = spawn_config_reader().await;
    handle_worker_config(
        &mut socket,
        &state,
        &serde_json::to_vec(&json!({"poll_seconds":7})).unwrap(),
    )
    .await
    .unwrap();
    drop(socket);
    let response = reader.await.unwrap();
    assert!(response.starts_with("HTTP/1.1 200 "));
    let post = parse_http_json_body(&response);
    assert_eq!(
        post["data"]["storage_capacity"]["physical_retained_bytes"],
        0
    );
    let root = std::path::PathBuf::from(std::env::var("WPTSALL_DATA_DIR").unwrap());
    // A legitimate owned writer changes usage between the two requests.
    crate::bindings::atomic_file::install(&root.join("owned.bin"), &[42u8; 152]).unwrap();
    let (mut socket, reader) = spawn_config_reader().await;
    handle_worker_config_get(&mut socket, &state).await.unwrap();
    drop(socket);
    let get = parse_http_json_body(&reader.await.unwrap());
    assert_eq!(
        get["data"]["storage_capacity"]["physical_retained_bytes"],
        152
    );
    assert_eq!(
        get["data"]["storage_capacity"]["physical_retained_files"],
        1
    );
    let mut old = post["data"].clone();
    let mut new = get["data"].clone();
    for field in ["physical_retained_bytes", "physical_retained_files"] {
        old["storage_capacity"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        new["storage_capacity"]
            .as_object_mut()
            .unwrap()
            .remove(field);
    }
    assert_eq!(old, new, "saved configuration did not change");
    let guard = state.lock().await;
    let conn = guard.db.lock().await;
    assert_eq!(
        crate::db::system::get_system_config(&conn, "worker_poll_seconds"),
        Some("7".to_string())
    );
}
