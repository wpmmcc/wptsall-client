//! Owned real helper calls. No shared WordPress, provider or saved user key.
use super::*;

#[tokio::test]
async fn encrypted_spool_frozen_manual_fixture_owns_its_component_backoff() {
    // A deliberately failed field may arm its own component. It must not
    // arm a different fixture's frozen proxy request by reusing "comp-test".
    owned_frozen_manual_request(None, true).await;
    owned_frozen_manual_request(Some("owned-frozen-proxy".into()), false).await;
}
