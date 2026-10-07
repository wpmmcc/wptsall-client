//! Debug-only bypass valves (S11, 07 audit, batch G).
//!
//! `tests/docs/guide/16-bypass-flags-inventory.md` already classifies
//! `WPTSALL_SKIP_SECURITY` / `WPTSALL_ALLOW_UNSIGNED_MANIFEST` as
//! **release-forbidden (debug-only)** — but the code honored them in any
//! build profile. This module aligns code with that contract: a valve is
//! honored only in debug builds; a release build ignores it with a loud log
//! so security stays active.

/// True only when the named valve is set AND this is a debug build.
/// A release build logs-and-ignores the attempt.
pub fn debug_only_valve(name: &str) -> bool {
    let set = client_runtime_core::env_helpers::env_bool(name, false);
    if !set {
        return false;
    }
    if cfg!(debug_assertions) {
        eprintln!("[security] {name}=1 honored (debug build)");
        return true;
    }
    eprintln!(
        "[security] {name}=1 IGNORED in release build — security stays active (S11, guide 16)"
    );
    false
}
