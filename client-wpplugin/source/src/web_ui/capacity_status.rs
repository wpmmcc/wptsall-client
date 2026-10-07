//! A loopback status/control surface when no database growth can be admitted.
use super::*;

pub(super) async fn run(
    bind: &str,
    shutdown: CancellationToken,
    lease: Arc<crate::db::runtime::RuntimeLease>,
    start: std::time::Instant,
) -> anyhow::Result<()> {
    let requested: std::net::SocketAddr = bind
        .parse()
        .context("invalid capacity-status bind address")?;
    // Without a readable settings database, do not assume external access.
    let bind = std::net::SocketAddr::new(
        if requested.ip().is_loopback() {
            requested.ip()
        } else {
            IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
        },
        requested.port(),
    );
    let listener = tokio::net::TcpListener::bind(bind).await?;
    let access = AccessControl::new(false, &[]);
    let mut connections = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            Some(_) = connections.join_next(), if !connections.is_empty() => {},
            accepted = listener.accept() => {
                let (socket, peer) = accepted?;
                if !peer.ip().is_loopback() { continue; }
                let access = access.clone();
                let lease = Arc::clone(&lease);
                connections.spawn(async move {
                    let _lease = lease;
                    let _ = routes::handle_capacity_only_connection(socket, start, access).await;
                });
            }
        }
    }
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    Ok(())
}
