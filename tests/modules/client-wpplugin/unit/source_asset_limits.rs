use super::fetch_source_asset;
use reqwest::Client;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

struct OwnedSource {
    url: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for OwnedSource {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl OwnedSource {
    async fn start(
        content_type: &str,
        announced: Option<usize>,
        chunks: Vec<Vec<u8>>,
        finish: bool,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/source?token=owned-synthetic-source-token",
            listener.local_addr().unwrap()
        );
        let framing = announced
            .map(|size| format!("Content-Length: {size}\r\n"))
            .unwrap_or_else(|| "Transfer-Encoding: chunked\r\n".into());
        let headers = format!("HTTP/1.1 200 OK\r\n{framing}Content-Type: {content_type}\r\nContent-Disposition: attachment; filename=\"owned.asset\"\r\nConnection: close\r\n\r\n");
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            socket.read(&mut request).await.unwrap();
            if socket.write_all(headers.as_bytes()).await.is_err() {
                return;
            }
            for chunk in chunks {
                if announced.is_none()
                    && socket
                        .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                        .await
                        .is_err()
                {
                    return;
                }
                if socket.write_all(&chunk).await.is_err() {
                    return;
                }
                if announced.is_none() && socket.write_all(b"\r\n").await.is_err() {
                    return;
                }
            }
            if finish {
                if announced.is_none() {
                    let _ = socket.write_all(b"0\r\n\r\n").await;
                }
            } else {
                std::future::pending::<()>().await;
            }
            drop(socket);
        });
        Self { url, task }
    }
}

fn client() -> Client {
    Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
}

#[tokio::test]
async fn source_asset_limits_actual_chunk_over_budget_refuses_before_eof() {
    let source = OwnedSource::start(
        "application/pdf",
        None,
        vec![vec![1; 16], vec![2; 17]],
        false,
    )
    .await;
    let observed = tokio::time::timeout(
        Duration::from_secs(2),
        fetch_source_asset(&client(), &source.url, 32),
    )
    .await;
    assert!(
        observed.is_ok(),
        "actual-byte budget must refuse without waiting for EOF"
    );
    let error = observed.unwrap().unwrap_err().to_string();
    assert!(error.contains("source asset too large"), "{error}");
    assert!(!error.contains("owned-synthetic-source-token"), "{error}");
}

#[tokio::test]
async fn source_asset_limits_exact_budget_keeps_bytes_and_metadata() {
    let source = OwnedSource::start(
        "application/pdf; charset=binary",
        None,
        vec![vec![1; 16], vec![2; 16]],
        true,
    )
    .await;
    let asset = fetch_source_asset(&client(), &source.url, 32)
        .await
        .unwrap();
    assert_eq!(asset.bytes, [vec![1; 16], vec![2; 16]].concat());
    assert_eq!(asset.content_type, "application/pdf");
    assert_eq!(asset.filename, "owned.asset");
}

#[tokio::test]
async fn source_asset_limits_declared_over_budget_does_not_wait_for_body() {
    let source = OwnedSource::start("image/png", Some(33), Vec::new(), false).await;
    let observed = tokio::time::timeout(
        Duration::from_secs(2),
        fetch_source_asset(&client(), &source.url, 32),
    )
    .await;
    assert!(
        observed.is_ok(),
        "oversized Content-Length must refuse before reading a body"
    );
    assert!(observed
        .unwrap()
        .unwrap_err()
        .to_string()
        .contains("source asset too large"));
}

#[tokio::test]
async fn source_asset_limits_zero_unlimited_preserves_all_five_content_types() {
    for content_type in [
        "text/plain",
        "image/png",
        "video/webm",
        "audio/wav",
        "application/pdf",
    ] {
        let source =
            OwnedSource::start(content_type, None, vec![vec![1; 33], vec![2; 33]], true).await;
        let asset = fetch_source_asset(&client(), &source.url, 0).await.unwrap();
        assert_eq!(asset.bytes, [vec![1; 33], vec![2; 33]].concat());
        assert_eq!(asset.content_type, content_type);
    }
}

#[tokio::test]
async fn source_asset_limits_truncated_body_never_returns_partial_asset() {
    let source = OwnedSource::start("video/webm", Some(32), vec![vec![7; 16]], true).await;
    let error = fetch_source_asset(&client(), &source.url, 32)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("read source_ref bytes failed"), "{error}");
    assert!(!error.contains("owned-synthetic-source-token"), "{error}");
}

#[tokio::test]
async fn source_asset_limits_large_configured_budget_does_not_preallocate_budget() {
    let source = OwnedSource::start("audio/wav", None, vec![vec![7; 33]], true).await;
    assert_eq!(
        fetch_source_asset(&client(), &source.url, u64::MAX)
            .await
            .unwrap()
            .bytes,
        vec![7; 33]
    );
}

#[tokio::test]
async fn source_asset_limits_data_url_exact_and_zero_semantics_are_unchanged() {
    let source = "data:image/png;base64,AQID";
    assert_eq!(
        fetch_source_asset(&client(), source, 3)
            .await
            .unwrap()
            .bytes,
        vec![1, 2, 3]
    );
    assert!(fetch_source_asset(&client(), source, 2).await.is_err());
    assert_eq!(
        fetch_source_asset(&client(), source, 0)
            .await
            .unwrap()
            .bytes,
        vec![1, 2, 3]
    );
}

#[tokio::test]
async fn source_asset_limits_data_url_padding_mime_and_large_budget_controls() {
    for (encoded, expected) in [
        ("AQ==", vec![1]),
        ("AQI=", vec![1, 2]),
        ("AQID", vec![1, 2, 3]),
    ] {
        let source = format!("data:image/png;charset=utf-8;BaSe64,{encoded}");
        for budget in [expected.len() as u64, u64::MAX, 0] {
            let asset = fetch_source_asset(&client(), &source, budget)
                .await
                .unwrap();
            assert_eq!(asset.bytes, expected);
            assert_eq!(asset.content_type, "image/png");
            assert_eq!(asset.filename, "source_file");
        }
        if expected.len() > 1 {
            assert!(
                fetch_source_asset(&client(), &source, expected.len() as u64 - 1)
                    .await
                    .is_err()
            );
        }
    }
}

#[tokio::test]
async fn source_asset_limits_invalid_data_url_never_exposes_payload() {
    for source in [
        "data:image/png,owned-private-payload",
        "data:image/png;base64,owned-private-payload",
        "data:image/png;base64,",
        "data:image/png;base64,AQ",
        "data:image/png;base64,AQ=A",
    ] {
        let error = fetch_source_asset(&client(), source, 32).await.unwrap_err();
        assert!(!format!("{error:#}").contains("owned-private-payload"));
        assert!(!format!("{error:?}").contains("owned-private-payload"));
    }
}

#[tokio::test]
async fn source_asset_limits_data_url_exact_decode_buffer_covers_quantum_boundaries() {
    use base64::Engine as _;
    let client = client();
    for length in 1..=257 {
        let expected: Vec<u8> = (0..length).map(|index| (index % 256) as u8).collect();
        let source = format!(
            "data:application/pdf;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&expected)
        );
        let asset = fetch_source_asset(&client, &source, length as u64)
            .await
            .unwrap();
        assert_eq!(asset.bytes, expected);
        if length > 1 {
            assert!(fetch_source_asset(&client, &source, length as u64 - 1)
                .await
                .is_err());
        }
    }
}

#[cfg(target_os = "linux")]
fn process_peak_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .unwrap()
        .lines()
        .find_map(|line| {
            line.strip_prefix("VmHWM:")?
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        })
        .expect("owned Linux process must expose its high-water RSS")
}

#[cfg(target_os = "linux")]
fn process_resident_kib() -> (u64, u64) {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let field = |name: &str| {
        status
            .lines()
            .find_map(|line| {
                line.strip_prefix(name)?
                    .split_whitespace()
                    .next()?
                    .parse::<u64>()
                    .ok()
            })
            .expect("owned Linux process must expose resident memory components")
    };
    (field("RssAnon:"), field("RssFile:"))
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn source_asset_limits_data_url_budget_checks_before_decode_allocation() {
    const PROBE: &str = "WPTSALL_OWNED_DATA_URL_ALLOC_PROBE";
    if std::env::var_os(PROBE).as_deref() == Some(std::ffi::OsStr::new("1")) {
        const PREFIX: &str = "data:application/pdf;base64,";
        const ENCODED: usize = 64 * 1024 * 1024;
        let client = client();
        // Fault in the same small error path before measuring the large input.
        // Cold executable pages are not a decoded asset allocation.
        let warm = fetch_source_asset(&client, "data:application/pdf;base64,YWJjYWJj", 1)
            .await
            .unwrap_err();
        assert!(warm.to_string().contains("source asset too large"));
        let mut source = String::with_capacity(PREFIX.len() + ENCODED);
        source.push_str(PREFIX);
        for _ in 0..ENCODED / 4 {
            source.push_str("YWJj");
        }
        let resident_before = process_resident_kib();
        let before = process_peak_kib();
        let error = fetch_source_asset(&client, &source, 32).await.unwrap_err();
        assert!(error.to_string().contains("source asset too large"));
        let after = process_peak_kib();
        let resident_after = process_resident_kib();
        eprintln!("owned-data-url-probe before_kib={before} after_kib={after}");
        eprintln!(
            "owned-data-url-resident before_anon_kib={} after_anon_kib={} before_file_kib={} after_file_kib={}",
            resident_before.0, resident_after.0, resident_before.1, resident_after.1
        );
        assert!(
            after.saturating_sub(before) <= 4096,
            "a 32-byte budget must not allocate a 48MiB decoded buffer"
        );
        return;
    }

    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "component_rt::runner::source_asset::source_asset_limits::source_asset_limits_data_url_budget_checks_before_decode_allocation",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(PROBE, "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let start = std::time::Instant::now();
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() > Duration::from_secs(30) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("owned data URL allocation probe exceeded its deadline");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    eprintln!("{}", String::from_utf8_lossy(&output.stderr));
    assert!(
        output.status.success(),
        "owned allocation probe failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
