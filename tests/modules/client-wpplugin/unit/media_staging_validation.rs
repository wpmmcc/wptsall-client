//! Observe real staging passes without bypassing crypto, scopes or network fences.
use super::*;
use std::cell::RefCell;
use std::path::PathBuf;

thread_local! {
    static DIGEST_CALLS: RefCell<Option<Vec<PathBuf>>> = const { RefCell::new(None) };
    static TAMPER_ROOT: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

pub(crate) fn record_digest_file(path: &Path) {
    TAMPER_ROOT.with(|root| {
        let mut root = root.borrow_mut();
        if root.as_ref().is_some_and(|root| path.starts_with(root))
            && path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("wpa1"))
        {
            *root = None;
            let mut bytes = std::fs::read(path).unwrap();
            *bytes.last_mut().unwrap() ^= 0x80;
            std::fs::write(path, bytes).unwrap();
        }
    });
    DIGEST_CALLS.with(|calls| {
        if let Some(calls) = calls.borrow_mut().as_mut() {
            calls.push(path.to_owned());
        }
    });
}

struct DigestObserver;

impl DigestObserver {
    fn start() -> Self {
        DIGEST_CALLS.with(|calls| {
            assert!(calls.borrow().is_none());
            *calls.borrow_mut() = Some(Vec::new());
        });
        Self
    }

    fn calls(&self) -> Vec<PathBuf> {
        DIGEST_CALLS.with(|calls| calls.borrow().as_ref().unwrap().clone())
    }
}

impl Drop for DigestObserver {
    fn drop(&mut self) {
        DIGEST_CALLS.with(|calls| *calls.borrow_mut() = None);
        TAMPER_ROOT.with(|root| *root.borrow_mut() = None);
    }
}

#[tokio::test]
async fn encrypted_spool_new_media_validates_its_staged_asset_once_before_egress() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _data =
        crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().display().to_string());
    let source = root.path().join("owned-original.bin");
    let bytes = b"owned staging pass observation";
    std::fs::write(&source, bytes).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = ChunkedUploadConfig {
        wp_base: format!(
            "http://{}/wp-json/wptsall/v2/owned/client",
            listener.local_addr().unwrap()
        ),
        token: "owned-recovery-token".into(),
        worker_id: "owned-worker".into(),
        device_id: "owned-device".into(),
        route_secret: Some("owned".into()),
        chunk_size: 0,
    };
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buffer = [0; 8192];
        assert!(socket.read(&mut buffer).await.unwrap() > 0);
        socket
            .write_all(
                b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
    });
    let observer = DigestObserver::start();
    let result = upload(
        &reqwest::Client::builder().no_proxy().build().unwrap(),
        &config,
        source.to_str().unwrap(),
        "owned.bin",
        "application/octet-stream",
        77,
        22,
        33,
        Some("owned-single-validation"),
    )
    .await;
    assert!(result.is_err(), "the mock refuses the final upload");
    server.await.unwrap();
    let row = operations().unwrap().pop().unwrap();
    assert_eq!(row.state, "complete_unknown");
    assert_eq!(std::fs::read(&source).unwrap(), bytes);
    assert_ne!(std::fs::read(&row.asset).unwrap(), bytes);
    let calls = observer.calls();
    assert_eq!(
        calls.iter().filter(|path| **path == source).count(),
        1,
        "measure the original once"
    );
    assert_eq!(
        calls
            .iter()
            .filter(|path| **path == PathBuf::from(&row.asset))
            .count(),
        1,
        "authenticate the staged asset once before any egress, not twice"
    );
}

#[tokio::test]
async fn encrypted_spool_new_media_refused_readback_publishes_no_checkpoint_or_egress() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _data =
        crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().display().to_string());
    let source = root.path().join("owned-original.bin");
    let bytes = b"owned refused initial encrypted readback";
    std::fs::write(&source, bytes).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = ChunkedUploadConfig {
        wp_base: format!(
            "http://{}/wp-json/wptsall/v2/owned/client",
            listener.local_addr().unwrap()
        ),
        token: "owned-recovery-token".into(),
        worker_id: "owned-worker".into(),
        device_id: "owned-device".into(),
        route_secret: Some("owned".into()),
        chunk_size: 0,
    };
    let _observer = DigestObserver::start();
    TAMPER_ROOT.with(|tamper| *tamper.borrow_mut() = Some(root.path().to_owned()));
    let result = upload(
        &reqwest::Client::builder().no_proxy().build().unwrap(),
        &config,
        source.to_str().unwrap(),
        "owned.bin",
        "application/octet-stream",
        77,
        22,
        33,
        Some("owned-refused-readback"),
    )
    .await;
    assert!(result.is_err());
    assert!(TAMPER_ROOT.with(|root| root.borrow().is_none()));
    assert!(operations().unwrap().is_empty());
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), listener.accept())
            .await
            .is_err()
    );
    let assets: Vec<_> = std::fs::read_dir(root.path().join("media-recovery/assets"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(assets.len(), 1, "refused evidence remains available");
    assert_ne!(std::fs::read(&assets[0]).unwrap(), bytes);
    assert!(crate::retained_assets::read(&assets[0], 512 * 1024 * 1024).is_err());
    assert_eq!(std::fs::read(source).unwrap(), bytes);
}
