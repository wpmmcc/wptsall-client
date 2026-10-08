//! Process-global translation-failure backoff registry (P8, R0 client-side half).
//!
//! Two cooldown domains share one registry:
//! - **identity** `(wp_base, relation_id, object_type, object_id)`: gates BOTH
//!   re-execution loops — the outbox fast path (leased lifecycle events acked
//!   `retry`) and the server retry queue — so a failed object is not
//!   re-executed until its cooldown expires.
//! - **component** `(component_id)`: a component whose requests keep failing
//!   (bad credential, dead endpoint) is circuit-broken for ALL objects, so a
//!   large untranslated backlog cannot burn one provider failure per object.
//!
//! Cooldowns grow 30s → 60s → … capped at 30 minutes; the circuit breaker
//! opens on the 3rd consecutive failure. A success in the same domain resets
//! it. Measured context: pre-fix, one empty OAuth pool produced 1466 provider
//! 401s inside a 240s run-once budget; identity backoff alone still allowed
//! ~46 first-attempt failures to sweep a backlog in a 45s phase, which the
//! component breaker stops after 3. The WP-side `available_at` column —
//! already pushed by the retry-ack `backoff_seconds` hint — stays the
//! durable, cross-process authority for outbox rows. Registry state is
//! process-local by design in R0.
//!
//! **Structural tier** (the permanent skip): a failure whose cause cannot be
//! fixed by retrying — e.g. "no component runtime available for
//! business_line=…, formats=…" when the bound components do not cover the
//! content's format — is recorded via `record_structural_failure` and blocks
//! the identity **until configuration changes**, not for a cooldown. Without
//! this tier every run-once window re-claimed and re-failed the same doomed
//! items at full speed (observed: WP lab pinned at 423% CPU, zero results in
//! 10 minutes, 18k accumulated failed items). Configuration changes clear
//! the markers (`clear_structural_all` /
//! `clear_structural_for_relation`), so adding the missing binding makes the
//! items claimable again on the next cycle without a restart.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const BASE_BACKOFF_SECS: u64 = 30;
const MAX_BACKOFF_SECS: u64 = 1800;
const CIRCUIT_FAILURE_THRESHOLD: u32 = 3;
/// Fully-doomed relations trip the relation poison after this many
/// consecutive structural failures WITHOUT any success in between.
const RELATION_STRUCTURAL_THRESHOLD: u32 = 5;
/// FL-7 dead-domain circuit: consecutive transport-level failures of a
/// site's lane entry point (connection refused / timeout — the site is
/// down) before the whole domain is skipped for the cooldown below.
const DOMAIN_TRANSPORT_THRESHOLD: u32 = 3;
/// FL-7/FL-2b domain circuit cooldown. Dead domains (transport) reopen for
/// a fresh probe after this; a permanent lane-endpoint status (404/410)
/// opens the SCAN-side circuit for the same window.
const DOMAIN_CIRCUIT_SECS: u64 = 600;
/// FL-2b outbox re-offer hold for rows acked `completed`: if a site
/// re-offers the same row within this window (mock or plugin build that
/// ignores the ack), the row is skipped instead of re-executed.
pub(crate) const OUTBOX_COMPLETED_HOLD_SECS: u64 = 60;

#[derive(Debug, Clone, Copy, PartialEq)]
struct BackoffState {
    consecutive_failures: u32,
    blocked_until: Option<Instant>,
    /// Permanent, configuration-bound skip (no expiry; cleared by
    /// configuration changes, not by time).
    structural: bool,
}

/// Outcome of a backoff query or failure record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct BackoffDecision {
    /// True while the key must not be re-executed.
    pub(crate) blocked: bool,
    /// Whole seconds until the cooldown expires (0 when not blocked).
    pub(crate) remaining_secs: u64,
    /// Consecutive provider-facing failures recorded for the key.
    pub(crate) consecutive_failures: u32,
    /// True once the failure threshold tripped the long cooldown.
    pub(crate) circuit_open: bool,
    /// True when the key is permanently skipped until configuration changes.
    pub(crate) structural: bool,
}

fn registry() -> &'static Mutex<HashMap<String, BackoffState>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, BackoffState>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn identity_key(wp_base: &str, relation_id: i64, object_type: &str, object_id: i64) -> String {
    format!(
        "id|{}|{}|{}|{}",
        wp_base.trim(),
        relation_id,
        object_type.trim().to_lowercase(),
        object_id
    )
}

fn component_key(component_id: &str) -> String {
    format!("comp|{}", component_id.trim().to_lowercase())
}

fn sync_pair_key(pair_id: &str) -> String {
    format!("sync|{}", pair_id.trim())
}

fn relation_key(wp_base: &str, relation_id: i64) -> String {
    format!("rel|{}|{}", wp_base.trim(), relation_id)
}

fn cooldown_for_failures(failures: u32) -> Duration {
    if failures >= CIRCUIT_FAILURE_THRESHOLD {
        return Duration::from_secs(MAX_BACKOFF_SECS);
    }
    let exp = (failures as u64).saturating_sub(1);
    let secs = BASE_BACKOFF_SECS.saturating_mul(2u64.saturating_pow(exp.min(16) as u32));
    Duration::from_secs(secs.min(MAX_BACKOFF_SECS))
}

fn record_failure_keyed(key: String) -> BackoffDecision {
    let mut registry = registry().lock().unwrap_or_else(|e| e.into_inner());
    let state = registry.entry(key).or_insert(BackoffState {
        consecutive_failures: 0,
        blocked_until: None,
        structural: false,
    });
    state.consecutive_failures = state.consecutive_failures.saturating_add(1);
    if state.structural {
        // Never downgrade a permanent skip to a timed cooldown.
        return BackoffDecision {
            blocked: true,
            remaining_secs: 0,
            consecutive_failures: state.consecutive_failures,
            circuit_open: false,
            structural: true,
        };
    }
    let cooldown = cooldown_for_failures(state.consecutive_failures);
    state.blocked_until = Some(Instant::now() + cooldown);
    BackoffDecision {
        blocked: true,
        remaining_secs: cooldown.as_secs(),
        consecutive_failures: state.consecutive_failures,
        circuit_open: state.consecutive_failures >= CIRCUIT_FAILURE_THRESHOLD,
        structural: false,
    }
}

fn record_structural_failure_keyed(key: String) -> BackoffDecision {
    let mut registry = registry().lock().unwrap_or_else(|e| e.into_inner());
    let state = registry.entry(key).or_insert(BackoffState {
        consecutive_failures: 0,
        blocked_until: None,
        structural: false,
    });
    state.consecutive_failures = state.consecutive_failures.saturating_add(1);
    state.structural = true;
    // Permanent: no blocked_until expiry.
    state.blocked_until = None;
    BackoffDecision {
        blocked: true,
        remaining_secs: 0,
        consecutive_failures: state.consecutive_failures,
        circuit_open: false,
        structural: true,
    }
}

fn record_success_keyed(key: String) {
    let mut registry = registry().lock().unwrap_or_else(|e| e.into_inner());
    registry.remove(&key);
}

fn check_keyed(key: String) -> BackoffDecision {
    let registry = registry().lock().unwrap_or_else(|e| e.into_inner());
    match registry.get(&key) {
        None => BackoffDecision::default(),
        Some(state) => {
            if state.structural {
                return BackoffDecision {
                    blocked: true,
                    remaining_secs: 0,
                    consecutive_failures: state.consecutive_failures,
                    circuit_open: false,
                    structural: true,
                };
            }
            let remaining = state
                .blocked_until
                .map(|until| until.saturating_duration_since(Instant::now()))
                .unwrap_or_default();
            BackoffDecision {
                blocked: !remaining.is_zero(),
                remaining_secs: remaining.as_secs(),
                consecutive_failures: state.consecutive_failures,
                circuit_open: state.consecutive_failures >= CIRCUIT_FAILURE_THRESHOLD
                    && !remaining.is_zero(),
                structural: false,
            }
        }
    }
}

/// Record a provider-facing failure for a content identity.
pub(crate) fn record_failure(
    wp_base: &str,
    relation_id: i64,
    object_type: &str,
    object_id: i64,
) -> BackoffDecision {
    record_failure_keyed(identity_key(wp_base, relation_id, object_type, object_id))
}

/// Record a structural (permanent-until-config-change) failure for a content
/// identity. Call ONLY for failures whose cause retrying cannot fix — see
/// [`is_structural_failure_message`].
pub(crate) fn record_structural_failure(
    wp_base: &str,
    relation_id: i64,
    object_type: &str,
    object_id: i64,
) -> BackoffDecision {
    record_structural_failure_keyed(identity_key(wp_base, relation_id, object_type, object_id))
}

/// Classify an error message as structural (retrying cannot fix it; only a
/// configuration change — binding the missing component — can). Keep in sync
/// with the pipeline error text.
pub(crate) fn is_structural_failure_message(message: &str) -> bool {
    message.contains("no component runtime available")
}

/// Record a structural failure against the RELATION (post/term lane).
///
/// A fully-doomed relation — every attempted item fails structurally,
/// nothing succeeds — trips the relation poison after
/// [`RELATION_STRUCTURAL_THRESHOLD`] consecutive structural failures. The
/// poisoned relation's post/term discovery+claim lane is then skipped
/// entirely (no page fetches, no claim leases, no translate attempts)
/// until a configuration change clears the poison, so a backlog of
/// thousands of doomed items costs a handful of attempts per process
/// lifetime instead of one claim+fail per item per cooldown window.
pub(crate) fn record_structural_relation_failure(
    wp_base: &str,
    relation_id: i64,
) -> BackoffDecision {
    let mut registry = registry().lock().unwrap_or_else(|e| e.into_inner());
    let state = registry
        .entry(relation_key(wp_base, relation_id))
        .or_insert(BackoffState {
            consecutive_failures: 0,
            blocked_until: None,
            structural: false,
        });
    state.consecutive_failures = state.consecutive_failures.saturating_add(1);
    if state.consecutive_failures >= RELATION_STRUCTURAL_THRESHOLD {
        state.structural = true;
        state.blocked_until = None;
    }
    BackoffDecision {
        blocked: state.structural,
        remaining_secs: 0,
        consecutive_failures: state.consecutive_failures,
        circuit_open: false,
        structural: state.structural,
    }
}

/// Record a successful (or no-op-completed) execution for the relation:
/// resets the structural streak and lifts the poison if it had tripped.
/// Mixed relations (some items servable) therefore never stay poisoned;
/// the cost is a bounded re-sweep of at most one attempt per remaining
/// doomed item before the streak trips again.
pub(crate) fn record_relation_success(wp_base: &str, relation_id: i64) {
    record_success_keyed(relation_key(wp_base, relation_id));
}

/// Current poison state of a relation's post/term discovery lane.
pub(crate) fn check_relation(wp_base: &str, relation_id: i64) -> BackoffDecision {
    check_keyed(relation_key(wp_base, relation_id))
}

/// Record a success (or terminal no-op) for a content identity.
pub(crate) fn record_success(wp_base: &str, relation_id: i64, object_type: &str, object_id: i64) {
    record_success_keyed(identity_key(wp_base, relation_id, object_type, object_id));
}

/// Current cooldown state for a content identity, without mutating it.
pub(crate) fn check(
    wp_base: &str,
    relation_id: i64,
    object_type: &str,
    object_id: i64,
) -> BackoffDecision {
    check_keyed(identity_key(wp_base, relation_id, object_type, object_id))
}

/// Record a provider-facing failure for a component (credential/endpoint
/// level). Circuit-breaks the component across ALL objects.
pub(crate) fn record_component_failure(component_id: &str) -> BackoffDecision {
    record_failure_keyed(component_key(component_id))
}

/// Record a successful component request: clears the component cooldown.
pub(crate) fn record_component_success(component_id: &str) {
    record_success_keyed(component_key(component_id));
}

/// Current cooldown state for a component, without mutating it.
pub(crate) fn check_component(component_id: &str) -> BackoffDecision {
    check_keyed(component_key(component_id))
}

// ---------------------------------------------------------------------------
// Sync-pair domain (批 C, tasks/5.3falsh2/12: sync-lane observability —
// a pair whose run ends in TOTAL failure (zero synced packets, ≥1 error:
// dead target site, revoked credentials) is cooled down 30s → 60s → … so
// the worker cadence does not retry a dead endpoint at full speed. A run
// with any successful push proves the lane works — residual per-packet
// errors are packet-level concerns and never trip this domain).
// ---------------------------------------------------------------------------

/// Record a total-failure sync run for the pair; returns the decision the
/// caller should log (cooldown the pair is now serving).
pub(crate) fn record_sync_pair_failure(pair_id: &str) -> BackoffDecision {
    record_failure_keyed(sync_pair_key(pair_id))
}

/// Record a sync run that synced at least one packet (or parked cleanly):
/// resets the pair's cooldown ladder entirely.
pub(crate) fn record_sync_pair_success(pair_id: &str) {
    record_success_keyed(sync_pair_key(pair_id))
}

/// Cooldown state for the pair; `blocked` while a recorded total failure
/// is still cooling down.
pub(crate) fn check_sync_pair(pair_id: &str) -> BackoffDecision {
    check_keyed(sync_pair_key(pair_id))
}

// ---------------------------------------------------------------------------
// FL-7/FL-2b domain circuits + FL-2b outbox re-offer hold
// ---------------------------------------------------------------------------

fn domain_key(wp_base: &str) -> String {
    format!("dom|{}", wp_base.trim())
}

fn domain_scan_key(wp_base: &str) -> String {
    format!("domscan|{}", wp_base.trim())
}

fn outbox_key(wp_base: &str, outbox_id: i64) -> String {
    // Domain-scoped: outbox ids are per-site integers, so different sites
    // routinely reuse the same id. A bare-id key made one site's ack holds
    // skip another site's fresh rows (observed in the SIM lane: every mock
    // site numbers its outbox from 7000, so site A's holds suppressed site
    // B's rows entirely).
    format!("outbox|{}|{}", wp_base.trim(), outbox_id)
}

fn decision_for_state(state: &BackoffState) -> BackoffDecision {
    let remaining = state
        .blocked_until
        .map(|until| until.saturating_duration_since(Instant::now()))
        .unwrap_or_default();
    BackoffDecision {
        blocked: !remaining.is_zero(),
        remaining_secs: remaining.as_secs(),
        consecutive_failures: state.consecutive_failures,
        circuit_open: state.consecutive_failures >= DOMAIN_TRANSPORT_THRESHOLD
            && !remaining.is_zero(),
        structural: false,
    }
}

/// FL-7: record a transport-level failure of the site's lane entry point
/// (site-relations fetch refused/timed out — the site is down). The whole
/// domain is circuit-broken after [`DOMAIN_TRANSPORT_THRESHOLD`]
/// consecutive failures, so run-once iterations stop spending one full
/// failed scan per dead domain and live domains get the budget instead.
/// A success in between resets the streak ([`record_domain_success`]).
pub(crate) fn record_domain_transport_failure(wp_base: &str) -> BackoffDecision {
    let mut registry = registry().lock().unwrap_or_else(|e| e.into_inner());
    let state = registry.entry(domain_key(wp_base)).or_insert(BackoffState {
        consecutive_failures: 0,
        blocked_until: None,
        structural: false,
    });
    state.consecutive_failures = state.consecutive_failures.saturating_add(1);
    if state.consecutive_failures >= DOMAIN_TRANSPORT_THRESHOLD {
        state.blocked_until = Some(Instant::now() + Duration::from_secs(DOMAIN_CIRCUIT_SECS));
    }
    decision_for_state(state)
}

/// FL-7: a successful lane entry (site-relations fetched) resets the
/// transport-failure streak and lifts the dead-domain circuit.
pub(crate) fn record_domain_success(wp_base: &str) {
    record_success_keyed(domain_key(wp_base))
}

/// FL-7: current dead-domain circuit state. While blocked, the worker
/// should skip the site entirely (scan AND outbox lanes).
pub(crate) fn check_domain(wp_base: &str) -> BackoffDecision {
    check_keyed(domain_key(wp_base))
}

/// FL-2b: record a PERMANENT failure of the domain's scan-lane endpoints —
/// the claim/content route answered 404/405/410 (route mismatch: plugin
/// older than the claim contract, or the route was never deployed).
/// Retrying within a run is pointless, so the SCAN lane (page fetch +
/// claim) is circuit-broken for [`DOMAIN_CIRCUIT_SECS`]. The OUTBOX fast
/// path is a different endpoint family and stays active: a claim-route
/// gap must not stop lifecycle events from flowing.
pub(crate) fn record_domain_scan_permanent_failure(wp_base: &str) -> BackoffDecision {
    let mut registry = registry().lock().unwrap_or_else(|e| e.into_inner());
    let state = registry
        .entry(domain_scan_key(wp_base))
        .or_insert(BackoffState {
            consecutive_failures: 0,
            blocked_until: None,
            structural: false,
        });
    state.consecutive_failures = state.consecutive_failures.saturating_add(1);
    state.blocked_until = Some(Instant::now() + Duration::from_secs(DOMAIN_CIRCUIT_SECS));
    BackoffDecision {
        blocked: true,
        remaining_secs: DOMAIN_CIRCUIT_SECS,
        consecutive_failures: state.consecutive_failures,
        circuit_open: true,
        structural: false,
    }
}

/// FL-2b: current scan-lane circuit state. While blocked, the relation
/// scan/claim section is skipped; the outbox lane keeps running.
pub(crate) fn check_domain_scan(wp_base: &str) -> BackoffDecision {
    check_keyed(domain_scan_key(wp_base))
}

/// Classify an error message as a PERMANENT HTTP failure (route missing —
/// retrying cannot fix it within a run). Keep in sync with the transport
/// error text ("wp transport non-2xx (status=NNN)").
pub(crate) fn is_permanent_http_failure_message(message: &str) -> bool {
    message.contains("status=404")
        || message.contains("status=405")
        || message.contains("status=410")
}

/// FL-2b: remember how long the site was told to hold an outbox row — the
/// retry-ack `backoff_seconds` hint, or a settle window for completed
/// acks. A site that re-offers the row inside the hold (mock or plugin
/// build that ignores `available_at`/acks) is skipped instead of
/// re-executed; this is what keeps a run-once loop converging instead of
/// re-processing the same row every iteration to the 180-iteration cap.
/// Keyed by (domain, outbox_id): ids are per-site and collide across
/// sites, so a hold must never leak from one site's rows to another's.
pub(crate) fn note_outbox_hold(wp_base: &str, outbox_id: i64, hold_secs: u64) {
    if hold_secs == 0 {
        return;
    }
    let mut registry = registry().lock().unwrap_or_else(|e| e.into_inner());
    // Bound growth: expired outbox holds are dropped on insert.
    let now = Instant::now();
    registry.retain(|key, state| {
        if key.starts_with("outbox|") {
            state
                .blocked_until
                .map(|until| until > now)
                .unwrap_or(false)
        } else {
            true
        }
    });
    registry.insert(
        outbox_key(wp_base, outbox_id),
        BackoffState {
            consecutive_failures: 0,
            blocked_until: Some(now + Duration::from_secs(hold_secs)),
            structural: false,
        },
    );
}

/// FL-2b: seconds remaining in an outbox row's re-offer hold (0 = none).
pub(crate) fn outbox_hold_remaining(wp_base: &str, outbox_id: i64) -> u64 {
    check_keyed(outbox_key(wp_base, outbox_id)).remaining_secs
}

/// Test hook: clear the registry.
#[cfg(test)]
pub(crate) fn clear_all() {
    registry().lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// The registry is process-global; serialize ALL tests that mutate it OR
/// depend on its state persisting across calls. That includes the
/// discoverer integration tests (outbox holds, domain/scan circuits):
/// a concurrent `clear_all()` from a backoff unit test would wipe a hold
/// armed between two passes of an integration test and flip its
/// convergence assertion (observed as a full-suite-only failure).
#[cfg(test)]
pub(crate) fn test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Clear structural (permanent) skips for one relation on a site: the
/// identity markers AND the relation poison. Timed cooldowns are left
/// alone. Call after a configuration change that affects what can serve
/// this relation (rule/component bindings).
pub(crate) fn clear_structural_for_relation(wp_base: &str, relation_id: i64) {
    let prefix = format!("id|{}|{}|", wp_base.trim(), relation_id);
    let rel_key = relation_key(wp_base, relation_id);
    let mut registry = registry().lock().unwrap_or_else(|e| e.into_inner());
    registry.retain(|key, state| {
        if state.structural && (key.starts_with(&prefix) || key == &rel_key) {
            false
        } else {
            true
        }
    });
}

/// Clear ALL structural (permanent) skips. Timed cooldowns are left alone.
/// Call after a global configuration change (Sites credentials upsert,
/// task-type/component defaults change): over-clearing only costs one
/// re-attempt per previously skipped identity, which the classifier turns
/// back into a skip if the failure is still structural.
pub(crate) fn clear_structural_all() {
    let mut registry = registry().lock().unwrap_or_else(|e| e.into_inner());
    registry.retain(|_, state| !state.structural);
}
#[cfg(test)]
mod tests {
    use super::*;

    // `test_lock` (the process-global registry serialization lock) lives in
    // the parent module as pub(crate) so the discoverer integration tests
    // share the SAME lock.

    const SITE: &str = "https://wp.a";

    #[test]
    fn structural_failure_blocks_permanently_until_config_change() {
        let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
        clear_all();

        let decision = record_structural_failure(SITE, 9, "post", 31);
        assert!(
            decision.blocked && decision.structural && !decision.circuit_open,
            "structural failure blocks immediately with no circuit semantics"
        );

        let checked = check(SITE, 9, "post", 31);
        assert!(
            checked.blocked && checked.structural,
            "structural skip never expires by time"
        );

        // A later transient record_failure on the same identity must not
        // downgrade the permanent skip to a timed cooldown.
        let downgraded = record_failure(SITE, 9, "post", 31);
        assert!(
            downgraded.blocked && downgraded.structural,
            "record_failure preserves the structural tier"
        );

        // Other identities are untouched by a relation-scoped clear.
        record_structural_failure(SITE, 9, "post", 32);
        record_structural_failure(SITE, 10, "post", 33);
        clear_structural_for_relation(SITE, 9);
        assert!(
            !check(SITE, 9, "post", 31).blocked,
            "relation-scoped clear unblocks the relation's structural skips"
        );
        assert!(
            !check(SITE, 9, "post", 32).blocked,
            "relation-scoped clear covers all object ids of the relation"
        );
        assert!(
            check(SITE, 10, "post", 33).blocked,
            "other relations keep their structural skips"
        );

        // A global clear removes every structural skip.
        clear_structural_all();
        assert!(!check(SITE, 10, "post", 33).blocked);

        // Success still clears a structural skip (the item got translated,
        // e.g. after the user bound the missing component).
        record_structural_failure(SITE, 9, "post", 34);
        assert!(check(SITE, 9, "post", 34).blocked);
        record_success(SITE, 9, "post", 34);
        assert!(!check(SITE, 9, "post", 34).blocked);
    }

    #[test]
    fn relation_poison_trips_after_threshold_and_lifts_on_success_or_config() {
        let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
        clear_all();

        for i in 1..RELATION_STRUCTURAL_THRESHOLD {
            let decision = record_structural_relation_failure(SITE, 12);
            assert!(
                !decision.blocked,
                "below threshold attempt {i} does not poison"
            );
            assert!(!check_relation(SITE, 12).blocked);
        }
        let tripped = record_structural_relation_failure(SITE, 12);
        assert!(
            tripped.blocked && tripped.structural,
            "threshold-th structural failure poisons the relation lane"
        );
        assert!(check_relation(SITE, 12).blocked);
        // Identity check for the same relation is independent (item lanes
        // gate on identity keys, the discovery lane on the rel key).
        assert!(!check(SITE, 12, "post", 51).blocked);

        // Config-change clears: global and relation-scoped.
        record_structural_failure(SITE, 13, "post", 61);
        for _ in 0..RELATION_STRUCTURAL_THRESHOLD {
            record_structural_relation_failure(SITE, 13);
        }
        assert!(check_relation(SITE, 13).blocked);
        clear_structural_for_relation(SITE, 12);
        assert!(
            !check_relation(SITE, 12).blocked,
            "scoped clear lifts the poison"
        );
        assert!(
            check_relation(SITE, 13).blocked,
            "other relations untouched"
        );
        record_structural_relation_failure(SITE, 12); // streak restarts
        for _ in 1..RELATION_STRUCTURAL_THRESHOLD {
            record_structural_relation_failure(SITE, 12);
        }
        assert!(check_relation(SITE, 12).blocked);
        clear_structural_all();
        assert!(
            !check_relation(SITE, 12).blocked,
            "global clear lifts the poison"
        );

        // Success resets the streak / lifts the poison (mixed relations
        // never stay starved).
        for _ in 0..RELATION_STRUCTURAL_THRESHOLD {
            record_structural_relation_failure(SITE, 12);
        }
        assert!(check_relation(SITE, 12).blocked);
        record_relation_success(SITE, 12);
        assert!(
            !check_relation(SITE, 12).blocked,
            "success lifts the poison"
        );

        // A success BETWEEN structural failures keeps the relation healthy.
        for _ in 0..(RELATION_STRUCTURAL_THRESHOLD * 3) {
            record_structural_relation_failure(SITE, 14);
            record_relation_success(SITE, 14);
        }
        assert!(
            !check_relation(SITE, 14).blocked,
            "interleaved success prevents poison"
        );
    }

    #[test]
    fn structural_classifier_matches_pipeline_error_only() {
        let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
        assert!(is_structural_failure_message(
            "no component runtime available for business_line=post_content, formats=plain_text,rich_html"
        ));
        assert!(is_structural_failure_message(
            "translate step failed: no component runtime available for business_line=site_ui, formats=plain_text"
        ));
        // Transient failures must NOT be classified structural.
        assert!(!is_structural_failure_message(
            "all translatable fields failed for object 42 (relation_id=9, fields_failed=3)"
        ));
        assert!(!is_structural_failure_message(
            "wp transport non-2xx (status=500)"
        ));
        assert!(!is_structural_failure_message("oauth token refresh failed"));
    }

    #[test]
    fn timed_cooldowns_survive_structural_clears() {
        let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
        clear_all();
        record_failure(SITE, 11, "post", 41);
        record_structural_failure(SITE, 11, "post", 42);
        clear_structural_for_relation(SITE, 11);
        assert!(
            check(SITE, 11, "post", 41).blocked,
            "timed cooldown survives relation-scoped structural clear"
        );
        assert!(!check(SITE, 11, "post", 42).blocked);
    }

    #[test]
    fn backoff_grows_exponentially_then_opens_circuit() {
        let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
        clear_all();
        let first = record_failure(SITE, 7, "post", 11);
        assert_eq!(
            (
                first.remaining_secs,
                first.consecutive_failures,
                first.circuit_open
            ),
            (30, 1, false),
            "first failure cools down 30s without circuit"
        );
        assert!(check(SITE, 7, "post", 11).blocked);

        let second = record_failure(SITE, 7, "post", 11);
        assert_eq!(
            (
                second.remaining_secs,
                second.consecutive_failures,
                second.circuit_open
            ),
            (60, 2, false),
            "second failure doubles the cooldown"
        );

        let third = record_failure(SITE, 7, "post", 11);
        assert_eq!(
            (
                third.remaining_secs,
                third.consecutive_failures,
                third.circuit_open
            ),
            (1800, 3, true),
            "third failure trips the circuit breaker for the max cooldown"
        );

        let fourth = record_failure(SITE, 7, "post", 11);
        assert_eq!((fourth.remaining_secs, fourth.circuit_open), (1800, true));
    }

    #[test]
    fn success_clears_cooldown_and_identities_are_isolated() {
        let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
        clear_all();
        record_failure(SITE, 7, "post", 21);
        assert!(check(SITE, 7, "post", 21).blocked);
        // Other object / relation / site stays independent, and the object
        // type is normalized so both loops hit the same entry.
        assert!(!check(SITE, 8, "post", 21).blocked);
        assert!(!check(SITE, 7, "term", 21).blocked);
        assert!(!check(SITE, 7, "post", 22).blocked);
        assert!(!check("https://wp.b", 7, "post", 21).blocked);
        assert!(
            check(SITE, 7, "Post", 21).blocked,
            "type is case-insensitive"
        );

        record_success(SITE, 7, "post", 21);
        let cleared = check(SITE, 7, "post", 21);
        assert!(!cleared.blocked);
        assert_eq!(cleared.consecutive_failures, 0);
    }

    #[test]
    fn check_does_not_mutate_state() {
        let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
        clear_all();
        record_failure(SITE, 7, "post", 31);
        let before = check(SITE, 7, "post", 31);
        let _ = check(SITE, 7, "post", 31);
        let after = check(SITE, 7, "post", 31);
        assert_eq!(before.consecutive_failures, after.consecutive_failures);
        assert_eq!(before.remaining_secs, after.remaining_secs);
    }

    #[test]
    fn component_domain_is_independent_of_identities_and_circuits() {
        let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
        clear_all();
        // An identity cooldown must not affect the component domain and vice
        // versa; component ids are case-insensitive.
        record_failure(SITE, 7, "post", 41);
        assert!(!check_component("comp-bad").blocked);
        assert!(!check(SITE, 7, "post", 41 - 41).blocked);

        let first = record_component_failure("comp-bad");
        assert_eq!(
            (
                first.remaining_secs,
                first.consecutive_failures,
                first.circuit_open
            ),
            (30, 1, false)
        );
        assert!(
            check_component("Comp-BAD").blocked,
            "component ids are case-insensitive"
        );

        let _ = record_component_failure("comp-bad");
        let third = record_component_failure("comp-bad");
        assert!(
            third.circuit_open,
            "third component failure opens the circuit"
        );

        record_component_success("comp-bad");
        assert!(
            !check_component("comp-bad").blocked,
            "success resets the component circuit"
        );
        // Identity entry from this test is untouched by component success.
        assert!(check(SITE, 7, "post", 41).blocked);
    }

    #[test]
    fn domain_transport_circuit_trips_on_third_consecutive_failure() {
        let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
        clear_all();

        let first = record_domain_transport_failure(SITE);
        assert!(!first.blocked && first.consecutive_failures == 1);
        let second = record_domain_transport_failure(SITE);
        assert!(!second.blocked && second.consecutive_failures == 2);
        let third = record_domain_transport_failure(SITE);
        assert!(
            third.blocked && third.circuit_open,
            "third consecutive transport failure opens the dead-domain circuit"
        );
        assert!(check_domain(SITE).blocked);
        // Other domains are independent.
        assert!(!check_domain("https://wp.b").blocked);
        // A success lifts the circuit.
        record_domain_success(SITE);
        assert!(!check_domain(SITE).blocked);
        // A success between failures keeps the domain alive (blips ≠ dead).
        record_domain_transport_failure(SITE);
        record_domain_transport_failure(SITE);
        record_domain_success(SITE);
        record_domain_transport_failure(SITE);
        assert!(
            !check_domain(SITE).blocked,
            "interleaved success resets the streak"
        );
    }

    #[test]
    fn domain_scan_circuit_trips_immediately_and_keeps_outbox_lane_independent() {
        let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
        clear_all();

        let decision = record_domain_scan_permanent_failure(SITE);
        assert!(decision.blocked && decision.circuit_open);
        assert!(
            check_domain_scan(SITE).blocked,
            "scan circuit opens immediately on a permanent route status"
        );
        // The whole-domain transport circuit is NOT tripped by a scan 404:
        // the outbox lane (different endpoint family) stays active.
        assert!(!check_domain(SITE).blocked);
        // The scan circuit is per-domain.
        assert!(!check_domain_scan("https://wp.b").blocked);
    }

    #[test]
    fn permanent_http_failure_classifier_matches_route_missing_statuses_only() {
        assert!(is_permanent_http_failure_message(
            "wp transport non-2xx (status=404)"
        ));
        assert!(is_permanent_http_failure_message(
            "wp transport non-2xx (status=405)"
        ));
        assert!(is_permanent_http_failure_message(
            "wp transport non-2xx (status=410)"
        ));
        assert!(!is_permanent_http_failure_message(
            "wp transport non-2xx (status=500)"
        ));
        assert!(!is_permanent_http_failure_message(
            "wp transport non-2xx (status=429)"
        ));
        assert!(!is_permanent_http_failure_message(
            "oauth token refresh failed"
        ));
    }

    #[test]
    fn outbox_hold_is_per_row_and_expires_by_time() {
        let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
        clear_all();

        note_outbox_hold("https://hold-a.example", 7, 60);
        assert!(
            outbox_hold_remaining("https://hold-a.example", 7) > 0,
            "fresh hold is active"
        );
        assert_eq!(
            outbox_hold_remaining("https://hold-a.example", 8),
            0,
            "other rows are untouched"
        );
        // Holds are domain-scoped: outbox ids are per-site integers, so a
        // hold on one site's row must never touch another site's row with
        // the same id (SIM-lane regression: every mock site numbers its
        // outbox from 7000 and site A's holds suppressed site B's rows).
        assert_eq!(
            outbox_hold_remaining("https://hold-b.example", 7),
            0,
            "another domain's row with the same id is untouched"
        );
        // Zero-second holds are a no-op.
        note_outbox_hold("https://hold-a.example", 9, 0);
        assert_eq!(outbox_hold_remaining("https://hold-a.example", 9), 0);

        note_outbox_hold("https://hold-a.example", 11, 1);
        std::thread::sleep(Duration::from_millis(1100));
        assert_eq!(
            outbox_hold_remaining("https://hold-a.example", 11),
            0,
            "hold expires by time"
        );
        // The still-active hold from this test is unaffected.
        assert!(outbox_hold_remaining("https://hold-a.example", 7) > 0);
    }

    /// 批 C: sync-pair cooldown domain — a total-failure run cools the
    /// pair down (30s ladder), consecutive failures escalate, any success
    /// resets, and the domain never leaks into identity/component keys.
    #[test]
    fn sync_pair_domain_cools_down_and_resets_on_success() {
        let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
        clear_all();

        assert!(
            !check_sync_pair("pair-a").blocked,
            "fresh pair is never blocked"
        );

        let d1 = record_sync_pair_failure("pair-a");
        assert!(
            d1.blocked && d1.remaining_secs >= BASE_BACKOFF_SECS,
            "first failure: {d1:?}"
        );
        let checked = check_sync_pair("pair-a");
        // as_secs() truncates the live remaining duration — a fresh
        // BASE_BACKOFF_SECS cooldown reads back one second short.
        assert!(
            checked.blocked && checked.remaining_secs + 1 >= BASE_BACKOFF_SECS,
            "{checked:?}"
        );

        // Escalation: consecutive failures double the cooldown.
        let d2 = record_sync_pair_failure("pair-a");
        assert!(
            d2.remaining_secs > d1.remaining_secs,
            "cooldown must escalate: {d1:?} -> {d2:?}"
        );
        assert_eq!(d2.consecutive_failures, 2);

        // The sync domain never touches identity/component keys.
        assert!(!check(SITE, 21, "post", 77).blocked);
        assert!(!check_component("pair-a").blocked);
        // And other pairs are untouched.
        assert!(!check_sync_pair("pair-b").blocked);

        // A success (any packet synced) resets the ladder entirely.
        record_sync_pair_success("pair-a");
        let after = check_sync_pair("pair-a");
        assert!(
            !after.blocked && after.consecutive_failures == 0,
            "{after:?}"
        );

        // Pair ids are trimmed (defensive, same as other domains).
        record_sync_pair_failure("  pair-c  ");
        assert!(check_sync_pair("pair-c").blocked);
    }
}