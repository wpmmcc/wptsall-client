//! Central legacy-route classification (P0-LF-02).
//!
//! The website/server control plane is opt-in (`WPTSALL_USE_SERVER_CONTROL_
//! PLANE`, see `crate::config::server_control_plane_enabled`). Every route
//! that exists only to serve that legacy plane must be rejected at dispatch
//! time — before a handler can clone the HTTP client, read session state, or
//! build a URL from `server_base` — when the gate is off.
//!
//! This module is the single source of truth for "which routes are legacy".
//! The dispatcher consults it before the route `match`; the route-classification
//! test (`routes/tests/legacy_routes.rs`) fails when a legacy handler becomes
//! reachable without appearing here.

/// Classification result for a `(method, path)` pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LegacyRouteClass {
    /// Legacy website/control-plane route. When the gate is disabled the
    /// dispatcher must return `LEGACY_CONTROL_PLANE_DISABLED` without running
    /// the handler.
    Legacy,
    /// Local route (or a shared route with an explicit in-handler gate).
    /// Never blocked by this classifier.
    Local,
}

/// Error code returned for disabled legacy routes.
pub(crate) const LEGACY_DISABLED_CODE: &str = "LEGACY_CONTROL_PLANE_DISABLED";
/// Error message returned for disabled legacy routes.
pub(crate) const LEGACY_DISABLED_MESSAGE: &str = "Legacy server control plane is disabled";

/// HTTP status used for denied legacy routes. 404 on purpose: a disabled
/// account/control-plane surface should not advertise itself (no redirects,
/// no 403 policy debates — 404 simply does not advertise the feature).
pub(crate) const LEGACY_DISABLED_STATUS: &str = "404 Not Found";

/// Static legacy routes: `(method, path)` pairs that exist only for the
/// website control plane. Keep this table in sync with
/// `tasks/solmax/01-P0-LOCAL-FIRST-CLIENT-CLEANUP.md` (P0-LF-02).
const LEGACY_STATIC_ROUTES: &[(&str, &str)] = &[
    ("POST", "/api/components/refresh"),
    ("POST", "/api/components/template"),
    ("POST", "/api/domains/refresh"),
    ("POST", "/api/logout"),
    ("POST", "/api/oauth/start"),
    ("GET", "/oauth/callback"),
    ("GET", "/api/platform/products"),
    ("GET", "/api/platform/entitlements"),
    ("GET", "/api/vendors"),
    ("GET", "/api/wp-translation-providers"),
    ("GET", "/api/cloud-api-types"),
    ("GET", "/api/components/server-search"),
    ("POST", "/api/components/local/install-from-server"),
];

/// Paths under `/api/v1/` are obsolete frontend spellings of the legacy
/// server-control-plane routes (platform/client/account). The dispatcher has
/// no local `/api/v1/*` route arms, so the whole prefix is rejected with the
/// same disabled code instead of an ambiguous 404-not-found route mismatch.
/// Using the full `/api/v1/` prefix (rather than only `/api/v1/platform/`)
/// closes the P1-9 gap so future v1 spellings cannot silently fall through.
const LEGACY_V1_PREFIXES: &[&str] = &["/api/v1/"];

/// `POST /api/components/local/:id/refresh-snapshot` refreshes a local
/// component from the server. The `:id` segment is dynamic, so the route is
/// matched structurally: 5 segments, `["api", "components", "local", <id>,
/// "refresh-snapshot"]`.
fn is_dynamic_refresh_snapshot(method: &str, path: &str) -> bool {
    if method != "POST" {
        return false;
    }
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    segments.len() == 5
        && segments[0] == "api"
        && segments[1] == "components"
        && segments[2] == "local"
        && !segments[3].is_empty()
        && segments[3] != "install-from-server"
        && segments[4] == "refresh-snapshot"
}

/// Classify a `(method, path)` pair. Paths are compared without query strings
/// (the HTTP reader splits those off before dispatch).
pub(crate) fn classify_legacy_route(method: &str, path: &str) -> LegacyRouteClass {
    let method = method.to_ascii_uppercase();
    if LEGACY_STATIC_ROUTES
        .iter()
        .any(|(m, p)| *m == method && *p == path)
    {
        return LegacyRouteClass::Legacy;
    }
    if LEGACY_V1_PREFIXES
        .iter()
        .any(|prefix| path.starts_with(prefix))
    {
        return LegacyRouteClass::Legacy;
    }
    if is_dynamic_refresh_snapshot(&method, path) {
        return LegacyRouteClass::Legacy;
    }
    LegacyRouteClass::Local
}

/// True when the dispatcher must reject this request before running any
/// handler (legacy route + gate disabled).
pub(crate) fn legacy_route_blocked(method: &str, path: &str) -> bool {
    matches!(
        classify_legacy_route(method, path),
        LegacyRouteClass::Legacy
    ) && !crate::config::server_control_plane_enabled()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_every_table_entry_as_legacy() {
        for (method, path) in LEGACY_STATIC_ROUTES {
            assert_eq!(
                classify_legacy_route(method, path),
                LegacyRouteClass::Legacy,
                "{method} {path} must classify as Legacy"
            );
        }
    }

    #[test]
    fn method_mismatch_is_not_legacy() {
        assert_eq!(
            classify_legacy_route("GET", "/api/components/refresh"),
            LegacyRouteClass::Local
        );
        assert_eq!(
            classify_legacy_route("POST", "/api/platform/products"),
            LegacyRouteClass::Local
        );
    }

    #[test]
    fn vendor_oauth_callback_stays_local() {
        // The vendor OAuth callback is LOCAL functionality and must never be
        // misclassified as the website OAuth callback.
        assert_eq!(
            classify_legacy_route("GET", "/oauth/callback"),
            LegacyRouteClass::Legacy
        );
        assert_eq!(
            classify_legacy_route("GET", "/oauth/vendor/callback"),
            LegacyRouteClass::Local
        );
    }

    #[test]
    fn legacy_v1_prefixes_are_recognized() {
        // /api/v1/platform/* — the original obsolete frontend spellings.
        assert_eq!(
            classify_legacy_route("GET", "/api/v1/platform/products"),
            LegacyRouteClass::Legacy
        );
        assert_eq!(
            classify_legacy_route("GET", "/api/v1/platform/entitlements"),
            LegacyRouteClass::Legacy
        );
        assert_eq!(
            classify_legacy_route("POST", "/api/v1/platform/anything"),
            LegacyRouteClass::Legacy
        );
        // P1-9: /api/v1/client/* and /api/v1/account/* are also legacy
        // server-control-plane spellings and must be classified as Legacy so
        // the dispatcher rejects them before any handler runs.
        assert_eq!(
            classify_legacy_route("GET", "/api/v1/client/locale-preference"),
            LegacyRouteClass::Legacy
        );
        assert_eq!(
            classify_legacy_route("PUT", "/api/v1/client/profile"),
            LegacyRouteClass::Legacy
        );
        assert_eq!(
            classify_legacy_route("GET", "/api/v1/account/preferences"),
            LegacyRouteClass::Legacy
        );
        // Any other /api/v1/<x> spelling is covered by the full prefix.
        assert_eq!(
            classify_legacy_route("POST", "/api/v1/something-new"),
            LegacyRouteClass::Legacy
        );
    }

    #[test]
    fn local_component_routes_stay_local_except_server_ops() {
        assert_eq!(
            classify_legacy_route("GET", "/api/components/local"),
            LegacyRouteClass::Local
        );
        assert_eq!(
            classify_legacy_route("POST", "/api/components/local"),
            LegacyRouteClass::Local
        );
        assert_eq!(
            classify_legacy_route("POST", "/api/components/local/import"),
            LegacyRouteClass::Local
        );
        assert_eq!(
            classify_legacy_route("POST", "/api/components/local/test-file"),
            LegacyRouteClass::Local
        );
        assert_eq!(
            classify_legacy_route("POST", "/api/components/local/install-from-server"),
            LegacyRouteClass::Legacy
        );
    }

    #[test]
    fn dynamic_refresh_snapshot_is_legacy_but_other_ids_are_not() {
        assert_eq!(
            classify_legacy_route("POST", "/api/components/local/abc-123/refresh-snapshot"),
            LegacyRouteClass::Legacy
        );
        // Not the refresh-snapshot action.
        assert_eq!(
            classify_legacy_route("POST", "/api/components/local/abc-123/quick-test"),
            LegacyRouteClass::Local
        );
        assert_eq!(
            classify_legacy_route("GET", "/api/components/local/abc-123/refresh-snapshot"),
            LegacyRouteClass::Local
        );
        // Too few segments.
        assert_eq!(
            classify_legacy_route("POST", "/api/components/local/refresh-snapshot"),
            LegacyRouteClass::Local
        );
    }

    #[test]
    fn local_workload_routes_stay_local() {
        for (method, path) in [
            ("GET", "/api/status"),
            ("GET", "/api/components/capabilities"),
            ("POST", "/api/components/test"),
            ("GET", "/api/components/local"),
            ("GET", "/api/provider-catalog"),
            ("POST", "/api/provider-catalog/refresh"),
            ("GET", "/api/vendor-keys"),
            ("GET", "/api/vendor-oauth"),
            ("GET", "/api/proxy-profiles"),
            ("POST", "/api/site-connections/import"),
            ("POST", "/api/domain-tokens/test"),
            ("POST", "/api/worker/run-once"),
            ("POST", "/api/logs/recent"),
            ("POST", "/api/logs/settings"),
            ("POST", "/api/integrations/pack/import"),
            ("GET", "/api/jobs"),
        ] {
            assert_eq!(
                classify_legacy_route(method, path),
                LegacyRouteClass::Local,
                "{method} {path} must stay local"
            );
        }
    }

    #[test]
    fn query_strings_are_not_part_of_the_path() {
        // The HTTP request reader strips the query string before dispatch; the
        // classifier must never see one. This guards the contract.
        assert_eq!(
            classify_legacy_route("GET", "/api/vendors?x=1"),
            LegacyRouteClass::Local,
            "query strings must be split off before classification"
        );
    }
}
