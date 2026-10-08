//! Identity Contract v1.1 client side (task C-1).
//!
//! Single source of truth: `tasks/wpmmcc/contract/identity-v1/CONTRACT.md`.
//! This module implements §4 (binding persistence v3 identity fields) and
//! §5 (fail-closed routing gate with the three reserved codes) for the Rust
//! client. §4 field semantics live on `DomainTokenBindingEntry`; this module
//! owns verification, the dispatch gate, and the pairing-form hook.
//!
//! Reserved codes (contract §5, never reuse): `identity_mismatch`,
//! `identity_unknown`, `identity_stale`. Event logs MUST NOT contain
//! credential fields.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::auth::wp_get_json_with_transport_and_secret;
use crate::types::{DomainTokenBindingEntry, IdentityCapabilities, PluginIdentity};

/// Identity verification TTL (contract §5): 24 hours.
pub(crate) const IDENTITY_TTL_SECS: i64 = 24 * 3600;

/// Reserved fail-closed code (contract §5): lane identity ≠ binding identity.
pub(crate) const CODE_IDENTITY_MISMATCH: &str = "identity_mismatch";
/// Reserved fail-closed code (contract §5): response identity outside the
/// §1 enum, or missing from the response.
pub(crate) const CODE_IDENTITY_UNKNOWN: &str = "identity_unknown";
/// Reserved fail-closed code (contract §5): verification TTL expired and
/// re-verification has not (yet) succeeded.
pub(crate) const CODE_IDENTITY_STALE: &str = "identity_stale";

/// Dispatch-gate verdict for one binding entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GateVerdict {
    /// Identity known and verification fresh: dispatch is allowed.
    Fresh(PluginIdentity),
    /// Verification is required before dispatch. `stale = true` means the
    /// previous verification expired (TTL) — on re-verify failure the event
    /// code is `identity_stale`; otherwise it is `identity_unknown`.
    Reverify { stale: bool },
}

/// Current unix time in seconds.
pub(crate) fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Evaluate the §5 dispatch gate for a binding entry at `now_unix`.
///
/// Contract §4 migration semantics: a binding with no identity on record
/// (legacy v2 entry, or an out-of-enum stored value downgraded to `None` by
/// the lenient deserializer) defaults to `wpmmcc_ats` and re-verifies on the
/// next heartbeat. The reserved `identity_unknown` code fires from LIVE
/// verification outcomes (§5: response missing or carrying an out-of-enum
/// identity), never from static state — an old-plugin site freezes with
/// `identity_unknown` until the plugin is upgraded, exactly as §3 specifies.
pub(crate) fn gate(entry: &DomainTokenBindingEntry, now: i64) -> GateVerdict {
    let identity = entry.plugin_identity.unwrap_or(PluginIdentity::WpmmccAts);
    match entry.identity_verified_at.as_deref() {
        None => GateVerdict::Reverify { stale: false },
        Some(verified_at) => match parse_rfc3339_utc_to_unix(verified_at) {
            Some(verified) if now - verified <= IDENTITY_TTL_SECS => GateVerdict::Fresh(identity),
            Some(_) => GateVerdict::Reverify { stale: true },
            None => {
                // Corrupt timestamp cannot prove freshness: re-verify, and
                // treat failure as unknown (not stale) since no valid verify
                // ever landed in this file.
                GateVerdict::Reverify { stale: false }
            }
        },
    }
}

/// Outcome of a live verification ping against a binding's WP endpoint.
#[derive(Debug, Clone)]
pub(crate) enum VerifyOutcome {
    /// Verified identity + capability snapshot. The caller decides whether
    /// this matches the stored binding identity (mismatch handling below).
    Verified(PluginIdentity, IdentityCapabilities),
    /// The endpoint responded but carried no contract-valid identity
    /// (missing field, wrong shape, or out-of-enum value) → `identity_unknown`.
    Unknown,
    /// Transport-level failure (network/HTTP/auth). The binding is skipped
    /// this cycle and retried; NOT a reserved-code event.
    Unreachable,
}

/// Live identity verification ping (contract §3/§4): GET `{wp_base}/ping`.
///
/// `wp_base` is the secret-bearing ATS client base
/// (`.../wp-json/wptsall/v2/{secret}/client`) built by `build_wp_base_url`.
/// For wpmmcc endpoints the caller builds the wpmmcc sync base instead —
/// the response shape (`data.plugin_identity` + capability fields) is the
/// same by contract.
pub(crate) async fn verify_identity(
    client: &reqwest::Client,
    wp_base: &str,
    token: &str,
    worker_id: &str,
    device_id: &str,
    route_secret: Option<&str>,
) -> VerifyOutcome {
    let url = format!("{}/ping", wp_base.trim_end_matches('/'));
    let resp: Value = match wp_get_json_with_transport_and_secret(
        client,
        &url,
        token,
        worker_id,
        device_id,
        route_secret,
    )
    .await
    {
        Ok(resp) => resp,
        Err(_) => return VerifyOutcome::Unreachable,
    };

    // Contract §3: a response without a valid data.plugin_identity is
    // identity_unknown (fail-closed) — old hosts that omit the field freeze
    // the binding until upgraded or re-bound, never guessed.
    let Some(data) = resp.get("data").and_then(Value::as_object) else {
        return VerifyOutcome::Unknown;
    };
    let Some(identity_raw) = data.get("plugin_identity").and_then(Value::as_str) else {
        return VerifyOutcome::Unknown;
    };
    let Some(identity) = PluginIdentity::from_wire_str(identity_raw) else {
        return VerifyOutcome::Unknown;
    };

    // Capability snapshot (contract §4): keep only what the host actually
    // reported — ATS pings omit the protocol fields, and defaults would lie.
    let capabilities = IdentityCapabilities {
        plugin_version: data
            .get("plugin_version")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        protocol_min: data
            .get("protocol_min")
            .and_then(Value::as_i64)
            .and_then(|v| u32::try_from(v).ok()),
        protocol_current: data
            .get("protocol_current")
            .and_then(Value::as_i64)
            .and_then(|v| u32::try_from(v).ok()),
    };
    VerifyOutcome::Verified(identity, capabilities)
}

/// Decide how a verification outcome applies to a binding entry, returning
/// the reserved event code for the reject paths (for the caller to log).
///
/// - Verified with the same identity (or an entry that was never verified,
///   i.e. the v3 migration default): caller refreshes the entry.
/// - Verified with a different identity than stored: `identity_mismatch` —
///   the binding freezes; the operator re-binds deliberately. Never silently
///   flip a stored identity.
/// - Unknown: `identity_unknown` (or `identity_stale` when the previous
///   verification had simply expired).
/// - Unreachable: `None` (no reserved code; skip + retry next cycle).
pub(crate) fn classify_verify_outcome(
    outcome: &VerifyOutcome,
    entry: &DomainTokenBindingEntry,
) -> Result<PluginIdentity, Option<&'static str>> {
    match outcome {
        VerifyOutcome::Verified(identity, _) => {
            if entry.plugin_identity == Some(*identity) || entry.identity_verified_at.is_none() {
                Ok(*identity)
            } else {
                Err(Some(CODE_IDENTITY_MISMATCH))
            }
        }
        VerifyOutcome::Unknown => {
            let stale = entry
                .identity_verified_at
                .as_deref()
                .and_then(parse_rfc3339_utc_to_unix)
                .map(|verified| now_unix() - verified > IDENTITY_TTL_SECS)
                .unwrap_or(false);
            if stale {
                Err(Some(CODE_IDENTITY_STALE))
            } else {
                Err(Some(CODE_IDENTITY_UNKNOWN))
            }
        }
        VerifyOutcome::Unreachable => Err(None),
    }
}

/// Pairing-form validation hook (contract §5 row 4): both endpoints of a
/// wpmmcc cross-site pairing must be verified `wpmmcc` bindings. Rejections
/// carry `identity_mismatch` ("同码透出") with an operator-facing message.
///
/// Contract scaffolding: unit-tested now, wired into the live pairing flow
/// at T-ID-4..7 once the wpmmcc-side pairing endpoint lands.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PairingIdentityRejection {
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

// ---------------------------------------------------------------------------
// FL-3 lane-exclusion once-notices (Wave-2)
// ---------------------------------------------------------------------------

/// Process-global set of identity lane-exclusion keys already reported.
/// Keyed `(context, domain, identity)`: the context distinguishes the
/// task-generation pre-filter from the lane-entry guard, and the identity
/// is part of the key so a deliberate re-bind to a NEW identity reports
/// again.
static IDENTITY_EXCLUSION_NOTICES: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashSet<String>>,
> = std::sync::OnceLock::new();

/// FL-3: should the caller EMIT its identity lane-exclusion event for this
/// (context, domain, identity) triple? True exactly once per triple per
/// process; false on every later occurrence.
///
/// Why: the ATS task-generation collectors and the lane-entry guard used to
/// re-report the same standing exclusion on EVERY run-once iteration — a
/// wpmmcc-bound site parked behind a 180-iteration loop produced 180
/// identical `identity_mismatch` warns (SIM-08/SIM-13 storms). The verdict
/// itself (fail-closed skip) is unchanged on every call; only the EVENT is
/// deduped. Keyed with the domain so one site's exclusion never silences
/// another's, and with the identity so re-binding reports fresh state.
///
/// Unkeyed test isolation note: same contract as the backoff registries —
/// tests use distinct domain strings instead of a reset hook.
pub(crate) fn identity_exclusion_first_notice(
    context: &str,
    domain_key: &str,
    identity: &str,
) -> bool {
    let set = IDENTITY_EXCLUSION_NOTICES
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()));
    let mut guard = match set.lock() {
        Ok(guard) => guard,
        // Poisoned lock: emit (fail-open visibility) rather than silence.
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.insert(format!(
        "{}\u{1f}{}\u{1f}{}",
        context,
        domain_key.trim(),
        identity
    ))
}

/// See [`PairingIdentityRejection`] (contract scaffolding, T-ID-4..7).
#[allow(dead_code)]
pub(crate) fn validate_pairing_ends(
    end_a: Option<PluginIdentity>,
    end_b: Option<PluginIdentity>,
) -> Result<(), PairingIdentityRejection> {
    for (label, identity) in [("本端", end_a), ("对端", end_b)] {
        match identity {
            Some(PluginIdentity::Wpmmcc) => {}
            Some(other) => {
                return Err(PairingIdentityRejection {
                    code: CODE_IDENTITY_MISMATCH,
                    message: format!(
                        "{}运行的是 WPMMCC ATS（{}），跨站配对要求两端均为 WPMMCC 插件",
                        label,
                        other.as_wire_str()
                    ),
                });
            }
            None => {
                return Err(PairingIdentityRejection {
                    code: CODE_IDENTITY_MISMATCH,
                    message: format!(
                        "{}插件身份未验证（identity_unknown），跨站配对要求两端均为已验证的 WPMMCC 插件",
                        label
                    ),
                });
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Minimal UTC RFC3339 helpers (no chrono dependency): the client only ever
// writes "YYYY-MM-DDTHH:MM:SSZ" and must read back exactly that shape (plus
// an explicit +00:00 offset for tolerance). Non-UTC offsets are rejected —
// verified_at is always written by this client in UTC.
// ---------------------------------------------------------------------------

/// Format a unix timestamp as UTC RFC3339 with second precision.
pub(crate) fn format_rfc3339_utc(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let secs_of_day = unix.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year,
        month,
        day,
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60
    )
}

/// Parse an RFC3339 UTC timestamp ("YYYY-MM-DDTHH:MM:SSZ" or with an
/// explicit +00:00 offset) into unix seconds. Returns None for anything
/// else (non-UTC offsets, other separators, out-of-range fields).
pub(crate) fn parse_rfc3339_utc_to_unix(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    if bytes.len() < 20 {
        return None;
    }
    let digit = |i: usize| -> Option<i64> {
        bytes.get(i).and_then(|b| {
            if b.is_ascii_digit() {
                Some((b - b'0') as i64)
            } else {
                None
            }
        })
    };
    let num = |range: (usize, usize)| -> Option<i64> {
        let (start, end) = range;
        let mut v: i64 = 0;
        for i in start..end {
            v = v * 10 + digit(i)?;
        }
        Some(v)
    };
    let sep = |i: usize, expected: u8| bytes.get(i) == Some(&expected);

    let year = num((0, 4))?;
    let month = num((5, 7))?;
    let day = num((8, 10))?;
    if !sep(4, b'-') || !sep(7, b'-') || !sep(10, b'T') {
        return None;
    }
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let hour = num((11, 13))?;
    let minute = num((14, 16))?;
    let second = num((17, 19))?;
    if !sep(13, b':') || !sep(16, b':') {
        return None;
    }
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    match value.get(19..) {
        Some("Z") => {}
        Some("+00:00") => {}
        _ => return None,
    }
    let days = days_from_civil(year, month as u32, day as u32);
    Some(days * 86_400 + hour * 3600 + minute * 60 + second)
}

/// Howard Hinnant's `civil_from_days` (public domain): days since 1970-01-01
/// to (year, month, day) in the proleptic Gregorian calendar.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Howard Hinnant's `days_from_civil` (public domain), inverse of the above.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}
#[cfg(test)]
mod tests {
    use super::*;

    fn entry(
        identity: Option<PluginIdentity>,
        verified_at: Option<&str>,
    ) -> DomainTokenBindingEntry {
        DomainTokenBindingEntry {
            wp_client_token: "tok".to_string(),
            route_secret: "sec".to_string(),
            plugin_identity: identity,
            identity_verified_at: verified_at.map(str::to_string),
            identity_capabilities: None,
        }
    }

    // ---- §1 enum: byte-exact wire strings, strict parsing ----

    #[test]
    fn plugin_identity_wire_strings_are_byte_exact() {
        assert_eq!(PluginIdentity::WpmmccAts.as_wire_str(), "wpmmcc_ats");
        assert_eq!(PluginIdentity::Wpmmcc.as_wire_str(), "wpmmcc");
        assert_eq!(
            PluginIdentity::from_wire_str("wpmmcc_ats"),
            Some(PluginIdentity::WpmmccAts)
        );
        assert_eq!(
            PluginIdentity::from_wire_str("wpmmcc"),
            Some(PluginIdentity::Wpmmcc)
        );
    }

    #[test]
    fn plugin_identity_rejects_out_of_enum_values() {
        // T-ID-6 shape: forged identity values never parse.
        assert_eq!(PluginIdentity::from_wire_str("wpmmcc_pro"), None);
        assert_eq!(PluginIdentity::from_wire_str("WPMMCC_ATS"), None);
        assert_eq!(PluginIdentity::from_wire_str("wpmmcc_ats "), None);
        assert_eq!(PluginIdentity::from_wire_str(" wpmmcc"), None);
        assert_eq!(PluginIdentity::from_wire_str("wpmmcc-ats"), None);
        assert_eq!(PluginIdentity::from_wire_str(""), None);
    }

    // ---- §5 gate: the three fail-closed paths ----

    #[test]
    fn gate_fresh_within_ttl() {
        let now = now_unix();
        let e = entry(
            Some(PluginIdentity::WpmmccAts),
            Some(&format_rfc3339_utc(now - 3600)),
        );
        assert_eq!(gate(&e, now), GateVerdict::Fresh(PluginIdentity::WpmmccAts));
    }

    #[test]
    fn gate_stale_after_ttl() {
        let now = now_unix();
        let e = entry(
            Some(PluginIdentity::WpmmccAts),
            Some(&format_rfc3339_utc(now - IDENTITY_TTL_SECS - 1)),
        );
        assert_eq!(gate(&e, now), GateVerdict::Reverify { stale: true });
    }

    #[test]
    fn gate_pending_needs_reverify_not_blocked() {
        // v3 migration default: identity stored, verified_at null.
        let e = entry(Some(PluginIdentity::WpmmccAts), None);
        assert_eq!(gate(&e, now_unix()), GateVerdict::Reverify { stale: false });
    }

    #[test]
    fn gate_legacy_missing_identity_defaults_to_ats_pending_reverify() {
        // Contract §4: legacy v2 entry (no identity on record) defaults to
        // wpmmcc_ats and re-verifies on the next heartbeat — the reserved
        // identity_unknown code comes from live verification outcomes, not
        // from static state.
        let e = entry(None, None);
        assert_eq!(gate(&e, now_unix()), GateVerdict::Reverify { stale: false });
    }

    // ---- verify outcome classification (three-code paths) ----

    #[test]
    fn classify_verified_matching_identity_is_ok() {
        let e = entry(
            Some(PluginIdentity::WpmmccAts),
            Some("2026-09-20T10:00:00Z"),
        );
        let outcome = VerifyOutcome::Verified(
            PluginIdentity::WpmmccAts,
            IdentityCapabilities {
                plugin_version: "2.1.4".to_string(),
                protocol_min: None,
                protocol_current: None,
            },
        );
        assert!(matches!(
            classify_verify_outcome(&outcome, &e),
            Ok(PluginIdentity::WpmmccAts)
        ));
    }

    #[test]
    fn classify_identity_change_is_mismatch() {
        // T-ID-5 shape: binding says ats, endpoint now says wpmmcc → freeze.
        let e = entry(
            Some(PluginIdentity::WpmmccAts),
            Some("2026-09-20T10:00:00Z"),
        );
        let outcome =
            VerifyOutcome::Verified(PluginIdentity::Wpmmcc, IdentityCapabilities::default());
        assert_eq!(
            classify_verify_outcome(&outcome, &e),
            Err(Some(CODE_IDENTITY_MISMATCH))
        );
    }

    #[test]
    fn classify_first_verify_of_migrated_entry_accepts_any_identity() {
        // T-ID-7 shape: v2→v3 default ats with null verified_at; the live
        // ping value overwrites the migration default.
        let e = entry(Some(PluginIdentity::WpmmccAts), None);
        let outcome =
            VerifyOutcome::Verified(PluginIdentity::Wpmmcc, IdentityCapabilities::default());
        assert!(matches!(
            classify_verify_outcome(&outcome, &e),
            Ok(PluginIdentity::Wpmmcc)
        ));
    }

    #[test]
    fn classify_unknown_response_is_unknown_or_stale() {
        let fresh = entry(
            Some(PluginIdentity::WpmmccAts),
            Some(&format_rfc3339_utc(now_unix())),
        );
        assert_eq!(
            classify_verify_outcome(&VerifyOutcome::Unknown, &fresh),
            Err(Some(CODE_IDENTITY_UNKNOWN))
        );

        let stale = entry(
            Some(PluginIdentity::WpmmccAts),
            Some(&format_rfc3339_utc(now_unix() - IDENTITY_TTL_SECS - 60)),
        );
        assert_eq!(
            classify_verify_outcome(&VerifyOutcome::Unknown, &stale),
            Err(Some(CODE_IDENTITY_STALE))
        );
    }

    #[test]
    fn classify_unreachable_has_no_reserved_code() {
        let e = entry(Some(PluginIdentity::WpmmccAts), None);
        assert_eq!(
            classify_verify_outcome(&VerifyOutcome::Unreachable, &e),
            Err(None)
        );
    }

    // ---- §5 pairing hook (T-ID-5) ----

    #[test]
    fn pairing_requires_both_ends_wpmmcc() {
        assert_eq!(
            validate_pairing_ends(Some(PluginIdentity::Wpmmcc), Some(PluginIdentity::Wpmmcc)),
            Ok(())
        );

        let rejection = validate_pairing_ends(
            Some(PluginIdentity::WpmmccAts),
            Some(PluginIdentity::Wpmmcc),
        )
        .unwrap_err();
        assert_eq!(rejection.code, CODE_IDENTITY_MISMATCH);
        assert!(rejection.message.contains("WPMMCC ATS"));

        let unverified = validate_pairing_ends(None, Some(PluginIdentity::Wpmmcc)).unwrap_err();
        assert_eq!(unverified.code, CODE_IDENTITY_MISMATCH);
        assert!(unverified.message.contains("identity_unknown"));
    }

    // ---- RFC3339 helpers ----

    #[test]
    fn rfc3339_roundtrip_and_contract_examples() {
        for unix in [0i64, 1_758_381_600, 1_700_000_000, -1] {
            let formatted = format_rfc3339_utc(unix);
            assert_eq!(
                parse_rfc3339_utc_to_unix(&formatted),
                Some(unix),
                "{}",
                formatted
            );
        }
        // Contract fixture literal (domain-binding-v3.json).
        assert_eq!(
            parse_rfc3339_utc_to_unix("2026-09-20T10:00:00Z"),
            Some(1_789_898_400)
        );
        assert_eq!(
            parse_rfc3339_utc_to_unix("2026-09-20T10:05:00+00:00"),
            Some(1_789_898_400 + 300)
        );
    }

    #[test]
    fn rfc3339_rejects_non_utc_and_garbage() {
        assert_eq!(parse_rfc3339_utc_to_unix("2026-09-20T10:00:00+08:00"), None);
        assert_eq!(parse_rfc3339_utc_to_unix("2026-09-20 10:00:00Z"), None);
        assert_eq!(parse_rfc3339_utc_to_unix("not-a-time"), None);
        assert_eq!(parse_rfc3339_utc_to_unix(""), None);
    }

    // ---- FL-3 lane-exclusion once-notices ----

    #[test]
    fn exclusion_first_notice_fires_once_per_context_domain_identity() {
        // Distinct keys per test (same discipline as the backoff
        // registries): the once-set is process-global and has no reset
        // hook, so a shared domain string would couple these tests to any
        // other test using it.
        let domain = "https://fl3-once.example";
        assert!(identity_exclusion_first_notice(
            "wpmmcc_ats_lane_entry",
            domain,
            "wpmmcc"
        ));
        // Same triple again: suppressed.
        assert!(!identity_exclusion_first_notice(
            "wpmmcc_ats_lane_entry",
            domain,
            "wpmmcc"
        ));
        // Different context: reports independently.
        assert!(identity_exclusion_first_notice(
            "wpmmcc_ats_task_generation",
            domain,
            "wpmmcc"
        ));
        // Different domain: one site's exclusion never silences another's.
        assert!(identity_exclusion_first_notice(
            "wpmmcc_ats_lane_entry",
            "https://fl3-other.example",
            "wpmmcc"
        ));
        // Re-bind to a NEW identity: fresh state reports again (deliberate
        // re-bind must not stay invisible).
        assert!(identity_exclusion_first_notice(
            "wpmmcc_ats_lane_entry",
            domain,
            "wpmmcc_ats"
        ));
        // Domain keys are trimmed, so stray whitespace cannot split a key.
        assert!(!identity_exclusion_first_notice(
            "wpmmcc_ats_lane_entry",
            &format!("  {}  ", domain),
            "wpmmcc"
        ));
    }
}