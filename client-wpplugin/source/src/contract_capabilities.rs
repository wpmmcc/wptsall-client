//! Multi-axis contract capabilities advertised on WP requests.
//!
//! Authority: `libs/wptsall-contracts/versions.json`.

/// Compact JSON sent as `X-WPTSALL-Contract-Capabilities` on WP client API calls.
pub(crate) fn client_contract_capabilities_json() -> &'static str {
    r#"{"wp_client_protocol":2,"content_formats":"content-formats-v1","component_client_contract":"component-client-contract-v1","callback":"task-callback-v1","workflow_policy":"workflow-policy-v1","workflow_dsl":"workflow-dsl-v1"}"#
}

pub(crate) const CONTRACT_CAPABILITIES_HEADER: &str = "X-WPTSALL-Contract-Capabilities";
