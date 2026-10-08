use super::*;
use std::collections::BTreeSet;
use std::process::{Command, Stdio};
use std::time::Duration;

const WORKER: &str =
    "component_rt::non_text::recovery::tests::process::media_recovery_process_worker";

fn worker_command(base: &str, root: &std::path::Path, resume: bool) -> Command {
    // Pin the running Linux image, even when Cargo replaces its pathname.
    let executable = if cfg!(target_os = "linux") {
        std::path::PathBuf::from("/proc/self/exe")
    } else {
        std::env::current_exe().unwrap()
    };
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let stderr = options
        .open(root.join(format!("owned-worker-{}.stderr", uuid::Uuid::new_v4())))
        .unwrap();
    let mut command = Command::new(executable);
    command
        .args(["--exact", WORKER, "--ignored", "--test-threads=1"])
        .env("WPTSALL_OWNED_MEDIA_PROCESS", "owned-media-process-v1")
        .env("WPTSALL_OWNED_MEDIA_PROCESS_BASE", base)
        .env(
            "WPTSALL_OWNED_MEDIA_PROCESS_RESUME",
            if resume { "1" } else { "0" },
        )
        .env("WPTSALL_DATA_DIR", root)
        .env("WPTSALL_WP_TRANSPORT_ENCRYPT", "always")
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr));
    command
}

fn worker_diagnostics(root: &std::path::Path) -> String {
    use std::io::Read;
    let mut output = String::new();
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path
            .extension()
            .is_some_and(|extension| extension == "stderr")
        {
            let mut bytes = Vec::new();
            std::fs::File::open(path)
                .unwrap()
                .take(8192)
                .read_to_end(&mut bytes)
                .unwrap();
            output.push_str(&String::from_utf8_lossy(&bytes));
        }
    }
    output
}

#[tokio::test]
#[ignore = "owned subprocess harness only; never uses a saved site binding"]
async fn media_recovery_process_worker() {
    assert_eq!(
        std::env::var("WPTSALL_OWNED_MEDIA_PROCESS").unwrap(),
        "owned-media-process-v1"
    );
    let base = std::env::var("WPTSALL_OWNED_MEDIA_PROCESS_BASE").unwrap();
    assert_eq!(
        reqwest::Url::parse(&base).unwrap().host_str(),
        Some("127.0.0.1")
    );
    let config = ChunkedUploadConfig {
        wp_base: base,
        token: "owned-recovery-token".into(),
        worker_id: "owned-process-worker".into(),
        device_id: "owned-device".into(),
        route_secret: Some("owned".into()),
        chunk_size: 0,
    };
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let result = if std::env::var("WPTSALL_OWNED_MEDIA_PROCESS_RESUME").unwrap() == "1" {
        resume_scope(&client, &config, "owned-process-scope")
            .await
            .unwrap()
            .unwrap()
    } else {
        let path =
            std::path::Path::new(&std::env::var("WPTSALL_DATA_DIR").unwrap()).join("original.bin");
        upload(
            &client,
            &config,
            path.to_str().unwrap(),
            "owned.bin",
            "application/octet-stream",
            77,
            22,
            33,
            Some("owned-process-scope"),
        )
        .await
        .unwrap()
    };
    assert_eq!(result.attachment_id, 321);
}

async fn process_crash_case(point: &'static str) {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _data = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let path = root.path().join("original.bin");
    let mut file = std::fs::File::create(&path).unwrap();
    for index in 0..10u8 {
        file.write_all(&vec![index + 1; 5 * 1024 * 1024]).unwrap();
    }
    drop(file);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!(
        "http://{}/wp-json/wptsall/v2/owned/client",
        listener.local_addr().unwrap()
    );
    let counts = Arc::new(Mutex::new((0, 0, 0)));
    let observed = counts.clone();
    let accepted = Arc::new(tokio::sync::Notify::new());
    let signal = accepted.clone();
    let clock = std::time::Instant::now();
    let server = tokio::spawn(async move {
        let mut operation = String::new();
        let mut sha = String::new();
        let mut received = BTreeSet::new();
        let mut done = false;
        let mut interrupted = false;
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let read_start = std::time::Instant::now();
            let (headers, bytes) = read_request(&mut socket).await;
            let target = headers
                .lines()
                .next()
                .unwrap()
                .split_whitespace()
                .nth(1)
                .unwrap();
            eprintln!(
                "OWNED_MEDIA_PHASE point={point} elapsed_ms={} read_ms={} phase={}",
                clock.elapsed().as_millis(),
                read_start.elapsed().as_millis(),
                target
                    .rsplit('/')
                    .next()
                    .unwrap()
                    .split('?')
                    .next()
                    .unwrap()
            );
            let mut pause = false;
            let body = if target.ends_with("/init") {
                observed.lock().unwrap().0 += 1;
                let init: Value = serde_json::from_slice(&bytes).unwrap();
                operation = init["operation_id"].as_str().unwrap().into();
                sha = init["content_sha256"].as_str().unwrap().into();
                if point == "init" && !interrupted {
                    pause = true;
                }
                json!({"success":true,"upload_id":operation})
            } else if target.ends_with("/chunk") {
                let index = header(&headers, "X-WPTSALL-Chunk-Index")
                    .parse::<usize>()
                    .unwrap();
                assert_eq!(bytes.len(), 5 * 1024 * 1024);
                assert!(bytes.iter().all(|byte| *byte == index as u8 + 1));
                assert!(
                    received.insert(index),
                    "a committed chunk was resent by the new process"
                );
                observed.lock().unwrap().1 += 1;
                if point == "chunk" && !interrupted {
                    pause = true;
                }
                json!({"success":true})
            } else if target.ends_with("/complete") {
                assert_eq!(received.len(), 10);
                observed.lock().unwrap().2 += 1;
                done = true;
                if point == "complete" && !interrupted {
                    pause = true;
                }
                json!({"success":true,"attachment_id":321,"operation_id":operation,
                    "content_sha256":sha,"source_id":77,"task_id":22,"relation_id":33})
            } else {
                assert!(target.contains("/status?"));
                json!({"success":true,"data":{
                    "operation_id":operation,"upload_id":operation,"content_sha256":sha,
                    "source_id":77,"task_id":22,"relation_id":33,
                    "state":if done {"result_ready"} else {"uploading"},
                    "attachment_id":if done {321} else {0},
                    "received_chunks":received.iter().copied().collect::<Vec<_>>(),"total_chunks":10,
                    "missing_chunks":(0..10).filter(|index|!received.contains(index)).collect::<Vec<_>>(),
                }})
            };
            if pause {
                interrupted = true;
                signal.notify_one();
                // Keep the accepted request open until the parent kills only
                // its owned worker. No response or client completion is possible.
                let mut byte = [0];
                assert_eq!(socket.read(&mut byte).await.unwrap(), 0);
                continue;
            }
            let body = serde_json::to_vec(&body).unwrap();
            let signature = crate::web_ui::test_support::sign_wp_plaintext_response(
                "owned-recovery-token",
                &body,
            );
            let response = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nX-WPTSALL-Response-Signature: {signature}\r\nConnection: close\r\n\r\n", body.len());
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.write_all(&body).await.unwrap();
        }
    });
    let mut first = worker_command(&base, root.path(), false).spawn().unwrap();
    let started = tokio::time::timeout(Duration::from_secs(45), accepted.notified()).await;
    if started.is_err() {
        let exit = first.try_wait().unwrap();
        let _ = first.kill();
        let _ = first.wait();
        panic!(
            "owned media worker did not reach the crash checkpoint; elapsed_ms={} counts={:?} early_exit={exit:?} diagnostics={}",
            clock.elapsed().as_millis(),
            *counts.lock().unwrap(),
            worker_diagnostics(root.path())
        );
    }
    let before = operations().unwrap().pop().unwrap();
    assert_eq!(
        before.state,
        match point {
            "init" => "init_unknown",
            "chunk" => "uploading",
            _ => "complete_unknown",
        }
    );
    if point == "chunk" {
        assert!(before.upload_id.is_some());
    }
    if point == "init" {
        let mut competing = worker_command(&base, root.path(), false).spawn().unwrap();
        let rejected = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if let Some(status) = competing.try_wait().unwrap() {
                    return status;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        if rejected.is_err() {
            let _ = competing.kill();
            let _ = competing.wait();
            let _ = first.kill();
            let _ = first.wait();
            panic!("a competing owned process did not refuse the active operation");
        }
        assert!(
            !rejected.unwrap().success(),
            "a second process acquired the active upload"
        );
    }
    first.kill().unwrap();
    assert!(!first.wait().unwrap().success());
    std::fs::remove_file(&path).unwrap();
    let mut second = worker_command(&base, root.path(), true).spawn().unwrap();
    let restarted = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if let Some(status) = second.try_wait().unwrap() {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    if restarted.is_err() {
        let _ = second.kill();
        let _ = second.wait();
        panic!("owned media resume worker timed out");
    }
    assert!(
        restarted.unwrap().success(),
        "fresh process could not resume retained media"
    );
    let after = operations().unwrap().pop().unwrap();
    assert_eq!(before.operation_id, after.operation_id);
    assert_eq!(after.state, "result_ready");
    assert_eq!(after.attachment_id, Some(321));
    assert_eq!(*counts.lock().unwrap(), (1, 10, 1));
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn media_recovery_fresh_process_lost_init_resumes_without_source() {
    process_crash_case("init").await;
}

#[tokio::test]
async fn media_recovery_fresh_process_mid_chunk_sends_only_missing() {
    process_crash_case("chunk").await;
}

#[tokio::test]
async fn media_recovery_fresh_process_lost_complete_never_reimports() {
    process_crash_case("complete").await;
}
