//! Anti-debug, self-integrity, and injection detection.

use anyhow::{anyhow, Result};
use sha2::{Digest, Sha256};
use std::path::Path;

/// S8 (07 audit, batch G): anti-tamper used to fail closed ONLY under the
/// opt-in `strict-antitamper` build feature — the default build silently
/// discarded every check result (not even a warning). Default is now
/// ENFORCE: detected indicators fail the bootstrap. The documented downgrade
/// is `WPTSALL_ANTITAMPER_WARN_ONLY=1` (warn + continue) for environments
/// that legitimately set LD_PRELOAD-class tooling; the `strict-antitamper`
/// feature remains declared but is a no-op now (kept so existing build
/// matrices/invocations keep working).
pub fn antitamper_warn_only() -> bool {
    client_runtime_core::env_helpers::env_bool("WPTSALL_ANTITAMPER_WARN_ONLY", false)
}

/// S8 decision core: pure map from (violation, warn_only) to the bootstrap
/// outcome — enforce (Err) by default, warn (log + Ok) only via the env
/// downgrade.
fn enforce_or_warn(violation: &str, warn_only: bool) -> Result<()> {
    if warn_only {
        eprintln!("[security] antitamper warning (warn-only mode): {violation}");
        return Ok(());
    }
    Err(anyhow!("{violation}"))
}

/// Run all anti-tamper checks. Fails closed by default (S8, batch G);
/// `WPTSALL_ANTITAMPER_WARN_ONLY=1` downgrades failures to warnings.
pub fn run_startup_checks(current_binary: Option<&Path>) -> Result<()> {
    let warn_only = antitamper_warn_only();
    if debugger_present()? {
        enforce_or_warn("debugger detected", warn_only)?;
    }

    if let Some(path) = current_binary {
        if !self_integrity_ok(path)? {
            enforce_or_warn("binary integrity check failed", warn_only)?;
        }
    }

    Ok(())
}

/// Detect attached debugger (platform-specific).
pub fn debugger_present() -> Result<bool> {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        for line in status.lines() {
            if line.starts_with("TracerPid:") {
                let pid: u32 = line
                    .split_whitespace()
                    .nth(1)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                return Ok(pid != 0);
            }
        }
        return Ok(false);
    }

    #[cfg(target_os = "macos")]
    {
        if std::env::var("DYLD_INSERT_LIBRARIES").is_ok() {
            return Ok(true);
        }
        return Ok(false);
    }

    #[cfg(target_os = "windows")]
    {
        extern "system" {
            fn IsDebuggerPresent() -> i32;
        }
        unsafe {
            return Ok(IsDebuggerPresent() != 0);
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        Ok(false)
    }
}

/// Compare current binary hash against embedded baseline (if set at build time).
pub fn self_integrity_ok(path: &Path) -> Result<bool> {
    let baseline = option_env!("WPTSALL_BINARY_SHA256");
    let Some(expected) = baseline else {
        // No baseline embedded — skip check (dev builds).
        return Ok(true);
    };
    let data = std::fs::read(path).map_err(|e| anyhow!("read binary: {e}"))?;
    let actual = format!("{:x}", Sha256::digest(&data));
    Ok(actual == expected)
}

/// Detect suspicious environment variables used for injection.
pub fn injection_indicators_present() -> bool {
    const SUSPICIOUS: &[&str] = &["LD_PRELOAD", "DYLD_INSERT_LIBRARIES", "FRIDA", "_FRIDA"];
    for key in SUSPICIOUS {
        if std::env::var(key).is_ok() {
            return true;
        }
    }
    false
}
