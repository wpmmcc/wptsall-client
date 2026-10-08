#[test]
fn physical_sqlite_cold_full_root_serves_capacity_without_creating_a_database() {
    if child_probe() {
        return;
    }
    let test = "physical_sqlite_cold_full_root_serves_capacity_without_creating_a_database";
    let root = tempfile::Builder::new()
        .prefix("cli05-runtime-sqlite-")
        .tempdir()
        .unwrap();
    std::fs::write(
        root.path().join(".wptsall-storage-v1.json"),
        br#"{"format":"wptsall-storage-v1","max_physical_bytes":1}"#,
    )
    .unwrap();
    let mut probe = spawn_probe(root.path(), test, "webui", "cold-sqlite");
    let started = probe.wait_result();
    assert_eq!(
        started["event"], "started",
        "full SQLite cannot block capacity status: {started}"
    );
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let base = format!("http://{}", started["addr"].as_str().unwrap());
    let status: serde_json::Value = client
        .get(format!("{base}/api/status"))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(status["success"], true);
    assert_eq!(status["data"]["storage_paused"], true);
    assert_eq!(status["data"]["database_available"], false);
    let health: serde_json::Value = client
        .get(format!("{base}/health"))
        .send()
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(health["data"]["storage_paused"], true);
    let rejected_host = client
        .get(format!("{base}/api/status"))
        .header("Host", "owned-untrusted.invalid")
        .send()
        .unwrap();
    assert_eq!(rejected_host.status(), reqwest::StatusCode::FORBIDDEN);
    let worker = client
        .post(format!("{base}/api/worker/start"))
        .header("Origin", &base)
        .json(&serde_json::json!({}))
        .send()
        .unwrap();
    assert_eq!(worker.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    let jobs = client.get(format!("{base}/api/jobs")).send().unwrap();
    assert_eq!(jobs.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    let body: serde_json::Value = jobs.json().unwrap();
    assert_eq!(body["error"]["code"], "STORAGE_CAPACITY_EXHAUSTED");
    let rejected = client
        .post(format!("{base}/api/storage/capacity"))
        .header("Origin", "https://owned-untrusted.invalid")
        .json(&serde_json::json!({}))
        .send()
        .unwrap();
    assert_eq!(rejected.status(), reqwest::StatusCode::FORBIDDEN);
    let capacity: serde_json::Value = client
        .get(format!("{base}/api/storage/capacity"))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(capacity["data"]["max_physical_bytes"], 1);
    let raised: serde_json::Value = client
        .post(format!("{base}/api/storage/capacity"))
        .header("Origin", &base)
        .json(&serde_json::json!({
            "max_physical_bytes": 32 * 1024 * 1024,
            "expected_revision": capacity["data"]["revision"],
            "confirm_change": true,
        }))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(raised["success"], true);
    assert!(
        !root.path().join("runtime/wptsall.db").exists(),
        "changing policy does not silently start a worker or migrate a database"
    );
    std::fs::write(
        probe.result.with_extension("shutdown"),
        b"stop owned capacity-only runtime",
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = probe.child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(
            Instant::now() < deadline,
            "capacity-only child did not stop"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(probe.result.with_extension("stopped").exists());
}
