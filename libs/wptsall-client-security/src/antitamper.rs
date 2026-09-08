//! Anti-debug, self-integrity, and injection detection.

use anyhow::{anyhow, Result};
use sha2::{Digest, Sha256};
use std::path::Path;

/// Run all anti-tamper checks. Fails closed when `strict-antitamper` feature enabled.
pub fn run_startup_checks(current_binary: Option<&Path>) -> Result<()> {
    if debugger_present()? {
        #[cfg(feature = "strict-antitamper")]
        return Err(anyhow!("debugger detected"));
    }

    if let Some(path) = current_binary {
        if !self_integrity_ok(path)? {
            #[cfg(feature = "strict-antitamper")]
            return Err(anyhow!("binary integrity check failed"));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injection_check_runs() {
        let _ = injection_indicators_present();
    }
}
