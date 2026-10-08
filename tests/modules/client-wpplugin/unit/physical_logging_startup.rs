#[test]
fn physical_logging_full_root_keeps_owned_recovery_ui_available() {
    if child_probe() {
        return;
    }
    let test = "physical_logging_full_root_keeps_owned_recovery_ui_available";
    let root = interrupted_root(test);
    let before = snapshot(&fixture_connection(root.path()));
    std::fs::write(
        root.path().join(".wptsall-storage-v1.json"),
        br#"{"format":"wptsall-storage-v1","max_physical_bytes":1}"#,
    )
    .unwrap();
    let mut restarted = spawn_probe(root.path(), test, "webui", "quota-restart");
    let started = restarted.wait_result();
    assert_eq!(
        started["event"], "started",
        "a full optional log must not block paid/review recovery: {started}"
    );
    assert!(!root.path().join("quota-restart-runtime.log").exists());
    assert_recovered(root.path(), &before);
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let status = client
        .get(format!(
            "http://{}/api/status",
            started["addr"].as_str().unwrap()
        ))
        .send()
        .unwrap();
    assert!(status.status().is_success());
    let capacity: serde_json::Value = client
        .get(format!(
            "http://{}/api/storage/capacity",
            started["addr"].as_str().unwrap()
        ))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(capacity["data"]["max_physical_bytes"], 1);
    assert!(
        capacity["data"]["physical_retained_bytes"]
            .as_u64()
            .unwrap()
            > 1
    );
    std::fs::write(
        restarted.result.with_extension("shutdown"),
        b"stop owned quota probe",
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = restarted.child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline, "owned quota probe did not stop");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(restarted.result.with_extension("stopped").exists());
}

fn physical_logging_full_root_keeps_owned_worker_recovery(test: &str, mode: &str) {
    if child_probe() {
        return;
    }
    let root = interrupted_root(test);
    if mode == "local" {
        // Existing legacy authority, not permission to create configuration at full quota.
        for name in [
            "WPTSALL_COMPONENT_BINDINGS_FILE",
            "WPTSALL_COMPONENTS_LOCAL_FILE",
        ] {
            std::fs::write(root.path().join(name), br#"{"version":1,"components":{}}"#).unwrap();
        }
    }
    let before = snapshot(&fixture_connection(root.path()));
    std::fs::write(
        root.path().join(".wptsall-storage-v1.json"),
        br#"{"format":"wptsall-storage-v1","max_physical_bytes":1}"#,
    )
    .unwrap();
    let label = format!("quota-{mode}");
    let mut restarted = spawn_probe(root.path(), test, mode, &label);
    let result = restarted.wait_result();
    assert_eq!(result["event"], "returned");
    assert_eq!(
        result["ok"], true,
        "optional logging must not block {mode}: {result}"
    );
    assert_recovered(root.path(), &before);
    assert!(!root.path().join(format!("{label}-runtime.log")).exists());
    if mode == "local" {
        for name in [
            "WPTSALL_COMPONENT_BINDINGS_FILE",
            "WPTSALL_COMPONENTS_LOCAL_FILE",
        ] {
            assert_eq!(
                std::fs::read(root.path().join(name)).unwrap(),
                br#"{"version":1,"components":{}}"#
            );
        }
    }
    assert_owned_quota_worker_finished(&mut restarted, root.path());
}

fn assert_owned_quota_worker_finished(probe: &mut OwnedProbe, root: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = probe.child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline, "owned quota CLI did not finish");
        std::thread::sleep(Duration::from_millis(10));
    }
    let db = root.join("runtime/wptsall.db");
    let _reusable = crate::db::runtime::RuntimeLease::acquire(db.to_str().unwrap())
        .expect("owned CLI must release its runtime lease on return");
}

#[test]
fn physical_logging_full_root_keeps_owned_local_worker_recovery_available() {
    physical_logging_full_root_keeps_owned_worker_recovery(
        "physical_logging_full_root_keeps_owned_local_worker_recovery_available",
        "local",
    );
}

#[test]
fn physical_logging_full_root_keeps_owned_legacy_worker_recovery_available() {
    physical_logging_full_root_keeps_owned_worker_recovery(
        "physical_logging_full_root_keeps_owned_legacy_worker_recovery_available",
        "server",
    );
}

#[test]
fn physical_logging_full_root_missing_configuration_still_refuses_new_authority() {
    if child_probe() {
        return;
    }
    let test = "physical_logging_full_root_missing_configuration_still_refuses_new_authority";
    let root = interrupted_root(test);
    let before = snapshot(&fixture_connection(root.path()));
    std::fs::write(
        root.path().join(".wptsall-storage-v1.json"),
        br#"{"format":"wptsall-storage-v1","max_physical_bytes":1}"#,
    )
    .unwrap();
    let mut restarted = spawn_probe(root.path(), test, "local", "quota-missing-authority");
    let result = restarted.wait_result();
    assert_eq!(result["event"], "returned");
    assert_eq!(result["ok"], false);
    assert!(result["error"]
        .as_str()
        .unwrap()
        .contains("STORAGE_CAPACITY_EXHAUSTED"));
    assert_recovered(root.path(), &before);
    for name in [
        "WPTSALL_COMPONENT_BINDINGS_FILE",
        "WPTSALL_COMPONENTS_LOCAL_FILE",
    ] {
        assert!(!root.path().join(name).exists());
    }
    assert!(!root
        .path()
        .join("quota-missing-authority-runtime.log")
        .exists());
    assert_owned_quota_worker_finished(&mut restarted, root.path());
}
