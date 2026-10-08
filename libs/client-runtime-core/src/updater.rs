//! Shared self-update primitives for WPTSALL clients.
//!
//! Provides download, checksum verification, and self-replacement logic
//! used by all three client binaries (`client-wpplugin`, `cloud-api-hub`,
//! `github-deployer`). Each client wires these into its own HTTP handler
//! (`POST /api/perform-update`).

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// Download a file from `url` into a temporary file under `/tmp/`.
///
/// Returns the path to the downloaded temporary file. The caller is
/// responsible for cleaning it up (or letting the self-replace script do so
/// via `mv`).
pub async fn download_update(http: &reqwest::Client, url: &str) -> Result<PathBuf> {
    let tmp_dir = std::env::temp_dir();
    let file_name = format!("wptsall-update-{}", uuid::Uuid::new_v4());
    let tmp_path = tmp_dir.join(&file_name);

    let resp = http
        .get(url)
        .send()
        .await
        .with_context(|| format!("download failed: {url}"))?
        .error_for_status()
        .with_context(|| format!("download HTTP error: {url}"))?;

    let bytes = resp.bytes().await.context("failed to read download body")?;

    tokio::fs::write(&tmp_path, &bytes)
        .await
        .with_context(|| format!("failed to write {}", tmp_path.display()))?;

    Ok(tmp_path)
}

/// Verify the SHA-256 checksum of a file.
pub async fn verify_checksum(path: &Path, expected_hex: &str) -> Result<()> {
    let data = tokio::fs::read(path)
        .await
        .with_context(|| format!("cannot read {}", path.display()))?;

    let mut hasher = Sha256::new();
    hasher.update(&data);
    let actual = format!("{:x}", hasher.finalize());

    if actual != expected_hex {
        anyhow::bail!(
            "checksum mismatch for {}: expected {}, got {}",
            path.display(),
            expected_hex,
            actual
        );
    }
    Ok(())
}

/// Parse a SHA256SUMS-style file and extract the hex digest for `filename`.
///
/// Each line in `content` is expected to be `<hex_digest>  <filename>`.
/// Returns `None` if the filename is not found.
pub fn parse_sha256sums(content: &str, filename: &str) -> Option<String> {
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Format: "<hash>  <filename>" (two spaces) or "<hash> *<filename>"
        let parts: Vec<&str> = line.splitn(2, |c: char| c == ' ' || c == '\t').collect();
        if parts.len() == 2 {
            let hash = parts[0].trim();
            let name = parts[1].trim_start_matches(|c: char| c == ' ' || c == '*' || c == '\t');
            if name == filename {
                return Some(hash.to_string());
            }
        }
    }
    None
}

/// Fail-closed pre-flight: the self-replace helper is fire-and-forget with
/// its output discarded, so a permission failure inside it (root-owned
/// install dir, read-only mount, wrong user) would silently no-op while the
/// HTTP response already said "restarting". Verify up front that the
/// directory holding the current binary is writable by the effective user —
/// `rename(2)` needs directory write permission, not file write permission —
/// by creating and removing a probe file.
///
/// The returned error message starts with `install dir not writable:` so
/// wire layers can surface a distinct error code.
fn ensure_replace_dir_writable(current_binary: &Path) -> Result<()> {
    let Some(dir) = current_binary.parent() else {
        anyhow::bail!("install dir not writable: current binary {} has no parent directory", current_binary.display());
    };
    let probe = dir.join(format!(".wptsall-replace-probe-{}", uuid::Uuid::new_v4()));
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            Ok(())
        }
        Err(e) => anyhow::bail!(
            "install dir not writable: {} — cannot self-replace the running binary \
             (rename needs directory write permission; fix ownership/permissions or \
             reinstall to a user-writable prefix): {e}",
            dir.display()
        ),
    }
}

/// Whether an in-place binary swap must be refused for this macOS install.
///
/// OTA-02 defense-in-depth (2026-09-24): replacing the executable inside a
/// signed `.app` bundle invalidates the bundle signature and puts the app on
/// the wrong side of Gatekeeper on its next launch. The supported in-repo
/// desktop shape is UNSIGNED (signature check fails / unavailable), so the
/// swap is signature-neutral and proceeds; a signed + notarized distribution
/// must fail closed instead — updates for that shape go through
/// distribution-side signed installers (Sparkle-style), not self-replace.
/// An in-place `codesign -s -` re-sign remains rejected: ad-hoc re-signing
/// silently DOWNGRADES a Developer ID signature, which is worse than the
/// invalidation it papers over (batch H adjudication).
///
/// `signature_valid` is `None` when the signature state is unknown (no
/// codesign available / unsupported platform); unknown is treated as
/// unsigned — the swap proceeds, matching today's behavior.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn should_block_macos_swap(app_bundle: bool, signature_valid: Option<bool>) -> bool {
    app_bundle && signature_valid == Some(true)
}

/// Verify the current binary's code signature via `codesign --verify`.
///
/// Returns `None` when the state cannot be determined (tool missing or the
/// binary is not inside a bundle context codesign can verify standalone).
/// Only compiled on macOS; other platforms get `None` via the cfg fallback.
#[cfg(target_os = "macos")]
fn macos_signature_valid(current_binary: &Path) -> Option<bool> {
    let status = std::process::Command::new("/usr/bin/codesign")
        .arg("--verify")
        .arg(current_binary)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    Some(status.success())
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[cfg(not(target_os = "macos"))]
fn macos_signature_valid(_current_binary: &Path) -> Option<bool> {
    // Decision core is exercised cross-platform in tests; the probe itself
    // only exists on Darwin.
    None
}

/// Self-replace the running binary with platform-appropriate restart handling.
///
/// `is_desktop` selects the restart mode:
/// - WebUI service (`false`): the binary runs under systemd (Linux) or
///   launchd (macOS) as `service_name`, so the helper stops the service,
///   swaps the binary, and starts the service again. On Windows the restarted
///   process is launched hidden and inherits this process's environment
///   (including `WPTSALL_WEB_UI=1`).
/// - Desktop GUI (`true`): there is no service. After the swap the helper
///   relaunches the binary directly — visible window on Windows, `open` on
///   the .app bundle on macOS, detached `nohup` on Linux. `-WindowStyle
///   Hidden` must NOT be used for a GUI app (BUG-UPD-02: invisible ghost).
///
/// Platform swap mechanics:
/// - Unix: the new binary is first staged NEXT TO the current one, the
///   current one is unlinked, and the staged file is renamed into place.
///   Staging in the same directory guarantees a pure rename (same
///   filesystem); unlinking a running executable is allowed on both Linux
///   and Darwin (the process keeps its vnode), while `mv` straight over
///   the running binary fails: Darwin's rename(2) onto an executing file
///   returns ETXTBSY, and a cross-device `mv` (e.g. /tmp on tmpfs, install
///   under /home) degrades to a write into the running file, which both
///   kernels refuse.
/// - Windows: a running .exe cannot be overwritten (ERROR_SHARING_VIOLATION,
///   BUG-UPD-01) but it CAN be renamed. The helper renames the running exe to
///   `<exe>.old`, moves the new one into place with a retry loop (AV scanners
///   can hold files briefly), restarts, then deletes the `.old` copy. If the
///   move keeps failing it rolls the old binary back. Output goes to
///   `wptsall-update.log` next to the binary because the helper's
///   stdout/stderr are discarded.
///
/// The function returns immediately after spawning the helper — the HTTP
/// handler should return 200 right away (and exit the process; see the
/// webui/desktop callers). Permission viability is checked BEFORE spawning
/// (see [`ensure_replace_dir_writable`]) so an unwritable install directory
/// fails closed instead of silently keeping the old binary.
/// S9 (07 audit, batch G) hygiene: the self-replace scripts are assembled
/// with `format!` into `sh -c` / PowerShell bodies. Paths are quoted, but a
/// path embedding the quote character itself would break out of its quoting
/// context. These escapers neutralize that residue (inputs are exe/temp
/// paths — no remote attack surface per 03 §2-S9; this is hardening hygiene).
fn sh_dq_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' | '"' | '$' | '`' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

// Windows-only escaper: on non-Windows targets the sole caller (the
// self-replace PowerShell branch) is cfg'd out, which would otherwise
// trip dead_code there.
#[cfg(target_os = "windows")]
fn ps_sq_escape(s: &str) -> String {
    s.replace('\'', "''")
}

/// PowerShell snippet that runs `body` only when the CURRENT process tree is
/// NOT SCM-hosted — i.e. the parent process is not services.exe.
///
/// OTA-03 defense (2026-09-24): the only supported Windows WebUI deployment
/// is the Startup-folder shortcut (a normal session process), so the
/// Start-Process child relaunch is correct for it. If the client is ever
/// wrapped by the Service Control Manager (NSSM / `sc create`), relaunching
/// a session child from session 0 both orphans it and bypasses the wrapper's
/// restart policy — the guard makes the helper log and skip instead, so the
/// SCM wrapper's own recovery applies. Pure string builder so the contract
/// is unit-testable on every platform.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn ps_restart_unless_scm_parent(body: &str, log_var: &str) -> String {
    format!(
        "$pp = (Get-CimInstance Win32_Process -Filter \"ProcessId=$PID\").ParentProcessId; \
         $pn = if ($pp) {{ (Get-CimInstance Win32_Process -Filter \"ProcessId=$pp\").Name }} else {{ '' }}; \
         if ($pn -eq 'services.exe') {{ \
             'scm parent detected: skipping child relaunch (SCM restart policy applies)' | Out-File -FilePath {log} -Append; \
         }} else {{ {body} }}"
        ,
        body = body,
        log = log_var,
    )
}

pub fn perform_self_replace(
    service_name: &str,
    current_binary: &Path,
    new_binary: &Path,
    is_desktop: bool,
) -> Result<()> {
    ensure_replace_dir_writable(current_binary)?;

    #[cfg(target_os = "linux")]
    {
        // Rename-over a running ELF is OK on a single filesystem (old inode
        // stays mapped), but a cross-device `mv` (tmpfs /tmp, install under
        // /home) degrades to a write into the running file — ETXTBSY. The
        // stage-aside sequence below keeps every step a same-directory
        // rename/unlink, which the kernel allows even while the old binary
        // is executing. The systemd unit is optional — install-webui.sh
        // creates `wptsall-client.service`; the desktop has no unit, so
        // stop/start are harmless no-ops there and the restart is a
        // detached relaunch.
        let cur = sh_dq_escape(&current_binary.display().to_string());
        let new = sh_dq_escape(&new_binary.display().to_string());
        let cur_dir = sh_dq_escape(
            &current_binary
                .parent()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| ".".to_string()),
        );
        // Restart: the systemd user unit when it exists (service installs),
        // otherwise a nohup relaunch with the helper's inherited
        // environment — which is the client's own environment
        // (WPTSALL_WEB_UI=1, bind/port, DB paths), so a service-less
        // WebUI (manual run, bare runner) comes back on the new version.
        let restart = if is_desktop {
            format!("(nohup \"{cur}\" >/dev/null 2>&1 &) >/dev/null 2>&1")
        } else {
            format!(
                "systemctl --user start {svc} >/dev/null 2>&1 || (nohup \"{cur}\" >/dev/null 2>&1 &) >/dev/null 2>&1",
                svc = service_name
            )
        };
        let script = format!(
            "exec >>\"{cur_dir}/wptsall-update.log\" 2>&1; \
             sleep 0.5; \
             systemctl --user stop {svc} >/dev/null 2>&1 || true; \
             incoming=\"{cur_dir}/wptsall-incoming.$$\"; \
             mv -f \"{new}\" \"$incoming\" && \
             rm -f \"{cur}\" && \
             mv -f \"$incoming\" \"{cur}\" && \
             chmod +x \"{cur}\" && \
             {restart}",
            svc = service_name,
            cur_dir = cur_dir,
            restart = restart,
        );
        std::process::Command::new("/usr/bin/env")
            .args(["sh", "-c", &script])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("failed to spawn self-replace helper")?;
    }

    #[cfg(target_os = "macos")]
    {
        // launchctl label matches the plist filename (without .plist); only
        // the WebUI install has one. The desktop restarts via `open` on the
        // .app bundle (three levels up from the executable inside
        // Contents/MacOS) when the binary actually lives in one, falling back
        // to a detached relaunch for raw binaries. Note: replacing a binary
        // inside a signed .app invalidates the bundle signature — distribute
        // desktop updates through signed installers when that matters.
        // BUG-OTA-02 adjudication (12号批 H, 2026-09-23): in-repo desktop
        // builds are unsigned, so the swap is signature-neutral today; for
        // a signed + notarized distribution the fix is distribution-side
        // (Sparkle-style signed installers). An in-place `codesign -s -`
        // re-sign was considered and rejected: ad-hoc re-signing silently
        // DOWNGRADES a Developer ID signature, which is worse than the
        // invalidation it papers over.
        //
        // Darwin refuses rename(2) onto a running executable with ETXTBSY
        // ("Text file busy"), and the webui process stays alive whenever no
        // LaunchAgent exists (see restart_mechanism_available), so a plain
        // `mv new cur` silently fails there. Sparkle-style sequence instead:
        // stage the incoming binary next to the current one (same directory
        // ⇒ same filesystem ⇒ pure renames), unlink the current one (allowed
        // even while executing — the process keeps its vnode), then rename
        // the staged file into place.
        let cur = sh_dq_escape(&current_binary.display().to_string());
        let new = sh_dq_escape(&new_binary.display().to_string());
        let mut app_dir = current_binary.to_path_buf();
        for _ in 0..3 {
            app_dir.pop();
        }
        let app_bundle = app_dir.extension().map(|ext| ext == "app").unwrap_or(false)
            && app_dir.is_dir();
        // OTA-02 fail-closed guard: a signed bundle must not be corrupted by
        // an in-place swap (see should_block_macos_swap). Unsigned in-repo
        // builds pass through unchanged.
        if should_block_macos_swap(app_bundle, macos_signature_valid(current_binary)) {
            anyhow::bail!(
                "refusing self-replace: {} is inside a signature-valid .app bundle — \
                 an in-place swap would invalidate its signature; distribute this \
                 update as a signed installer instead",
                current_binary.display()
            );
        }
        let cur_dir = sh_dq_escape(
            &current_binary
                .parent()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| ".".to_string()),
        );
        let restart = if is_desktop {
            if app_bundle {
                format!("open \"{}\"", sh_dq_escape(&app_dir.display().to_string()))
            } else {
                format!("(nohup \"{cur}\" >/dev/null 2>&1 &)")
            }
        } else {
            // LaunchAgent first (service installs), otherwise a nohup
            // relaunch with the inherited client environment — same
            // reasoning as the Linux branch.
            format!(
                "launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/{svc}.plist 2>/dev/null || (nohup \"{cur}\" >/dev/null 2>&1 &)",
                svc = service_name
            )
        };
        let script = format!(
            "exec >>\"{cur_dir}/wptsall-update.log\" 2>&1; \
             sleep 0.5 && \
             launchctl bootout gui/$(id -u)/{svc} 2>/dev/null; \
             sleep 1 && \
             incoming=\"{cur_dir}/wptsall-incoming.$$\" && \
             mv -f \"{new}\" \"$incoming\" && \
             rm -f \"{cur}\" && \
             mv -f \"$incoming\" \"{cur}\" && \
             chmod +x \"{cur}\" && \
             {restart}",
            svc = service_name,
            cur_dir = cur_dir,
            restart = restart,
        );
        std::process::Command::new("/usr/bin/env")
            .args(["sh", "-c", &script])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("failed to spawn self-replace helper")?;
    }

    #[cfg(target_os = "windows")]
    {
        // A running .exe is locked for write/delete, but renaming it is
        // allowed. The legacy script moved the new binary straight over the
        // running exe and failed 100% of the time with ERROR_SHARING_VIOLATION
        // (BUG-UPD-01). Sequence here: rename running exe aside → move the
        // new one in (retry loop) → restart → delete the .old copy; rollback
        // restores the old binary if the move keeps failing. Everything is
        // logged to wptsall-update.log next to the binary because the
        // helper's stdout/stderr are discarded.
        //
        // BUG-OTA-03 adjudication (12号批 H, 2026-09-23): the premise
        // "Windows WebUI service mode" does not exist in-repo — the only
        // supported Windows WebUI deployment is the Startup-folder shortcut
        // created by install-webui.ps1 (`--webui`), not an SCM service, so
        // the Start-Process relaunch with the inherited environment IS the
        // correct restart for the supported shape. If SCM-wrapped
        // deployments (NSSM / sc create) ever become supported, the fix is
        // to detect SCM parentage (session 0 / services.exe parent) and
        // skip this child relaunch so the wrapper's own restart policy
        // applies — see the 12号 batch H adjudication table.
        let cur_str = ps_sq_escape(&current_binary.to_string_lossy());
        let new_str = ps_sq_escape(&new_binary.to_string_lossy());
        let old_str = format!("{cur_str}.old");
        let old_leaf = current_binary
            .file_name()
            .map(|leaf| ps_sq_escape(&format!("{}.old", leaf.to_string_lossy())))
            .ok_or_else(|| {
                anyhow::anyhow!("current binary {} has no file name", current_binary.display())
            })?;
        let log_path = current_binary
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("wptsall-update.log")
            .to_string_lossy()
            .into_owned();
        let log_path = ps_sq_escape(&log_path);

        // GUI restart must be visible (BUG-UPD-02); the WebUI service restart
        // stays hidden and inherits this process's environment (which carries
        // WPTSALL_WEB_UI=1 from the service/shortcut that started us).
        // OTA-03: both restarts are guarded against SCM parentage — see
        // ps_restart_unless_scm_parent.
        let restart = if is_desktop {
            format!("Start-Process -FilePath '{cur_str}'")
        } else {
            format!("Start-Process -FilePath '{cur_str}' -WindowStyle Hidden")
        };
        let restart = ps_restart_unless_scm_parent(&restart, "$log");

        let ps = format!(
            "$ErrorActionPreference = 'Continue'; \
             $log = '{log}'; \
             try {{ \
                 Start-Sleep -Milliseconds 500; \
                 if (Test-Path -LiteralPath '{old_str}') {{ \
                     Remove-Item -LiteralPath '{old_str}' -Force -ErrorAction SilentlyContinue; \
                 }}; \
                 Rename-Item -LiteralPath '{cur_str}' -NewName '{old_leaf}' -ErrorAction Stop; \
                 $moved = $false; \
                 for ($i = 0; $i -lt 10 -and -not $moved; $i++) {{ \
                     try {{ \
                         Move-Item -LiteralPath '{new_str}' -Destination '{cur_str}' -ErrorAction Stop; \
                         $moved = $true; \
                     }} catch {{ Start-Sleep -Milliseconds 500 }} \
                 }}; \
                 if (-not $moved) {{ throw 'move of new binary failed after retries' }}; \
                 {restart}; \
                 Start-Sleep -Seconds 2; \
                 Remove-Item -LiteralPath '{old_str}' -Force -ErrorAction SilentlyContinue; \
                 'self-replace ok' | Out-File -FilePath $log -Append; \
             }} catch {{ \
                 Move-Item -LiteralPath '{old_str}' -Destination '{cur_str}' -Force -ErrorAction SilentlyContinue; \
                 ('self-replace failed: ' + ($_ | Out-String)) | Out-File -FilePath $log -Append; \
                 {restart}; \
             }}",
            log = log_path,
            restart = restart,
        );
        std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &ps])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("failed to spawn self-replace helper")?;
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = (service_name, is_desktop);
        anyhow::bail!("self-replace is not supported on this platform");
    }

    Ok(())
}

/// Determine the compile-time target triple of the running binary.
///
/// Uses `cfg!()` macros so the result is baked in at compile time.
/// Covers Linux, macOS, and Windows on x86_64 and aarch64.
pub fn current_target_triple() -> &'static str {
    if cfg!(target_arch = "x86_64") && cfg!(target_os = "linux") && cfg!(target_env = "gnu") {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(target_arch = "aarch64") && cfg!(target_os = "linux") {
        "aarch64-unknown-linux-gnu"
    } else if cfg!(target_arch = "x86_64") && cfg!(target_os = "macos") {
        "x86_64-apple-darwin"
    } else if cfg!(target_arch = "aarch64") && cfg!(target_os = "macos") {
        "aarch64-apple-darwin"
    } else if cfg!(target_arch = "x86_64") && cfg!(target_os = "windows") {
        "x86_64-pc-windows-msvc"
    } else if cfg!(target_arch = "aarch64") && cfg!(target_os = "windows") {
        "aarch64-pc-windows-msvc"
    } else {
        "unknown"
    }
}

/// Kit / installer platform key used in filenames: `linux-x86_64`, `darwin-aarch64`, …
pub fn current_platform() -> &'static str {
    if cfg!(target_os = "linux") && cfg!(target_arch = "x86_64") {
        "linux-x86_64"
    } else if cfg!(target_os = "linux") && cfg!(target_arch = "aarch64") {
        "linux-aarch64"
    } else if cfg!(target_os = "macos") && cfg!(target_arch = "x86_64") {
        "darwin-x86_64"
    } else if cfg!(target_os = "macos") && cfg!(target_arch = "aarch64") {
        "darwin-aarch64"
    } else if cfg!(target_os = "windows") && cfg!(target_arch = "x86_64") {
        "windows-x86_64"
    } else if cfg!(target_os = "windows") && cfg!(target_arch = "aarch64") {
        "windows-aarch64"
    } else {
        "unknown"
    }
}

/// systemd unit (Linux) / launchd label (macOS) created by `install-webui.sh`.
pub fn webui_service_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "cc.wpmm.wptsall-client"
    } else {
        "wptsall-client"
    }
}

/// Resolve a URL template: `{version}`, `{platform}` (linux-x86_64), `{target}` (cargo triple).
pub fn resolve_url(template: &str, version: &str) -> String {
    template
        .replace("{version}", version)
        .replace("{platform}", current_platform())
        .replace("{target}", current_target_triple())
}

fn looks_like_gzip(path: &Path) -> bool {
    let Ok(data) = std::fs::File::open(path).and_then(|mut f| {
        use std::io::Read;
        let mut buf = [0u8; 2];
        f.read_exact(&mut buf)?;
        Ok(buf)
    }) else {
        return false;
    };
    data == [0x1f, 0x8b]
}

/// Resolve install root for kit layout (`bin/` + `ui/...`).
/// Prefers `WPTSALL_INSTALL_ROOT`, else parent of the directory containing `current_exe`.
pub fn install_root_from_exe(current_exe: &Path) -> PathBuf {
    if let Ok(root) = std::env::var("WPTSALL_INSTALL_ROOT") {
        let p = PathBuf::from(root);
        if !p.as_os_str().is_empty() {
            return p;
        }
    }
    current_exe
        .parent()
        .and_then(|bin| bin.parent())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Apply a platform-independent UI bundle tarball onto `install_root`.
///
/// Expected archive layout (from `make-ui-bundle.sh`):
///   ui/{webui|desktop}/...
///   VERSION-WEBUI
///
/// Replaces `install_root/ui/{subdir}/` and writes `VERSION-WEBUI`. No process restart.
pub fn apply_ui_bundle(archive: &Path, install_root: &Path, ui_subdir: &str) -> Result<()> {
    let unpack_dir = std::env::temp_dir().join(format!("wptsall-ui-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&unpack_dir)
        .with_context(|| format!("mkdir {}", unpack_dir.display()))?;

    let status = std::process::Command::new("tar")
        .args(["-xzf"])
        .arg(archive)
        .arg("-C")
        .arg(&unpack_dir)
        .status()
        .context("failed to spawn tar for UI bundle")?;
    if !status.success() {
        anyhow::bail!("tar failed unpacking UI bundle {}", archive.display());
    }

    let src_ui = unpack_dir.join("ui").join(ui_subdir);
    if !src_ui.is_dir() {
        let _ = std::fs::remove_dir_all(&unpack_dir);
        anyhow::bail!(
            "UI bundle missing ui/{} in {}",
            ui_subdir,
            archive.display()
        );
    }

    let dest_ui = install_root.join("ui").join(ui_subdir);
    // S10 (07 audit, batch G): the old flow removed dest_ui BEFORE copying,
    // so a mid-copy failure (disk full / permission / dangling symlink)
    // left the install root with NO UI. Stage-then-swap instead: copy into a
    // sibling staging dir, move the old tree aside, rename the staging dir
    // into place (same-parent rename = atomic-ish on POSIX), and restore the
    // old tree if the final rename fails.
    let staging = install_root
        .join("ui")
        .join(format!(".{}.staging-{}", ui_subdir, uuid::Uuid::new_v4()));
    let _ = std::fs::remove_dir_all(&staging);
    if let Err(e) = copy_dir_recursive(&src_ui, &staging) {
        let _ = std::fs::remove_dir_all(&staging);
        let _ = std::fs::remove_dir_all(&unpack_dir);
        return Err(e);
    }
    let backup = install_root
        .join("ui")
        .join(format!(".{}.old-{}", ui_subdir, uuid::Uuid::new_v4()));
    let had_old = dest_ui.exists();
    if had_old {
        std::fs::rename(&dest_ui, &backup)
            .with_context(|| format!("move {} aside", dest_ui.display()))?;
    }
    if let Err(e) = std::fs::rename(&staging, &dest_ui) {
        // Roll back: the old tree (if any) goes back, staging is cleaned.
        if had_old {
            let _ = std::fs::rename(&backup, &dest_ui);
        }
        let _ = std::fs::remove_dir_all(&staging);
        let _ = std::fs::remove_dir_all(&unpack_dir);
        return Err(e).context(format!("swap staged UI into {}", dest_ui.display()));
    }
    // New tree is live; the old one is now garbage.
    if had_old {
        let _ = std::fs::remove_dir_all(&backup);
    }

    let ver_src = unpack_dir.join("VERSION-WEBUI");
    if ver_src.is_file() {
        std::fs::copy(&ver_src, install_root.join("VERSION-WEBUI"))
            .with_context(|| format!("write VERSION-WEBUI under {}", install_root.display()))?;
    }

    let _ = std::fs::remove_dir_all(&unpack_dir);
    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for ent in std::fs::read_dir(src)? {
        let ent = ent?;
        let from = ent.path();
        let to = dst.join(ent.file_name());
        if from.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&from, &to)
                .with_context(|| format!("copy {} -> {}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

/// If `path` is a signed kit `.tar.gz`, extract `bin/wptsall-client[.exe]` to a temp file.
/// Raw binaries are returned unchanged.
pub fn extract_update_binary(path: &Path) -> Result<PathBuf> {
    if !looks_like_gzip(path) {
        return Ok(path.to_path_buf());
    }

    let unpack_dir = std::env::temp_dir().join(format!("wptsall-kit-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&unpack_dir)
        .with_context(|| format!("mkdir {}", unpack_dir.display()))?;

    let status = std::process::Command::new("tar")
        .args(["-xzf"])
        .arg(path)
        .arg("-C")
        .arg(&unpack_dir)
        .status()
        .context("failed to spawn tar to unpack update kit")?;
    if !status.success() {
        anyhow::bail!("tar failed unpacking {}", path.display());
    }

    let mut found: Option<PathBuf> = None;
    fn walk(dir: &Path, out: &mut Option<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for ent in rd.flatten() {
            let p = ent.path();
            if p.is_dir() {
                walk(&p, out);
                continue;
            }
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name == "wptsall-client" || name == "wptsall-client.exe" {
                *out = Some(p);
            }
        }
    }
    walk(&unpack_dir, &mut found);

    let bin = found.ok_or_else(|| {
        anyhow::anyhow!("update kit {} has no bin/wptsall-client", path.display())
    })?;

    let dest = std::env::temp_dir().join(format!("wptsall-update-bin-{}", uuid::Uuid::new_v4()));
    std::fs::copy(&bin, &dest)
        .with_context(|| format!("copy {} -> {}", bin.display(), dest.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&dest)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&dest, perms)?;
    }
    let _ = std::fs::remove_dir_all(&unpack_dir);
    Ok(dest)
}
