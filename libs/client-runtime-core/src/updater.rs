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

/// Self-replace the running binary with platform-appropriate service management.
///
/// Spawns a fire-and-forget helper process that:
/// 1. Waits briefly for the current process to return the HTTP response
/// 2. Stops the service (systemd / launchd / none on Windows)
/// 3. Moves the new binary over the old one
/// 4. Marks it executable (Unix only)
/// 5. Starts the service again
///
/// The function returns immediately after spawning the helper — the HTTP
/// handler should return 200 right away.
pub fn perform_self_replace(
    service_name: &str,
    current_binary: &Path,
    new_binary: &Path,
) -> Result<()> {
    let cur = current_binary.display();
    let new = new_binary.display();

    #[cfg(target_os = "linux")]
    {
        // Rename-over a running ELF is OK (old inode stays mapped). systemd unit
        // is optional — install-webui.sh creates `wptsall-client.service`.
        let script = format!(
            "sleep 0.5; \
             systemctl --user stop {svc} >/dev/null 2>&1 || true; \
             mv -f {new} {cur}; \
             chmod +x {cur}; \
             systemctl --user start {svc} >/dev/null 2>&1 || true",
            svc = service_name,
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
        // launchctl label matches the plist filename (without .plist).
        // Step: stop agent → wait for process to exit → replace binary → start agent.
        let script = format!(
            "sleep 0.5 && \
             launchctl bootout gui/$(id -u)/{svc} 2>/dev/null; \
             sleep 1 && \
             mv -f {new} {cur} && \
             chmod +x {cur} && \
             launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/{svc}.plist",
            svc = service_name,
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
        // Windows allows replacing a running .exe (unlike Unix where the file is
        // locked by the kernel). We rename the old exe aside, move the new one in,
        // then start a detached process that cleans up and restarts.
        let cur_str = current_binary.to_string_lossy();
        let new_str = new_binary.to_string_lossy();
        let old_name = format!("{}.old", cur_str);

        // PowerShell script: replace binary, then restart the service process.
        let ps = format!(
            "Start-Sleep -Milliseconds 500; \
             Move-Item -Force '{old_name}' '{cur_str}' -ErrorAction SilentlyContinue; \
             Move-Item -Force '{new_str}' '{cur_str}'; \
             Start-Process -FilePath '{cur_str}' -WindowStyle Hidden",
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
        let _ = (service_name, cur, new);
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
    if dest_ui.exists() {
        std::fs::remove_dir_all(&dest_ui)
            .with_context(|| format!("remove {}", dest_ui.display()))?;
    }
    std::fs::create_dir_all(dest_ui.parent().unwrap_or(install_root))?;
    copy_dir_recursive(&src_ui, &dest_ui)?;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sha256sums_standard() {
        let content = "\
abc123def456  wptsall-client-2.1.0-x86_64-unknown-linux-gnu\n\
789abc123def  SHA256SUMS\n";
        assert_eq!(
            parse_sha256sums(content, "wptsall-client-2.1.0-x86_64-unknown-linux-gnu"),
            Some("abc123def456".to_string())
        );
        assert_eq!(
            parse_sha256sums(content, "SHA256SUMS"),
            Some("789abc123def".to_string())
        );
        assert_eq!(parse_sha256sums(content, "nonexistent"), None);
    }

    #[test]
    fn parse_sha256sums_star_mode() {
        let content = "abc123 *wptsall-client-2.1.0-x86_64-unknown-linux-gnu\n";
        assert_eq!(
            parse_sha256sums(content, "wptsall-client-2.1.0-x86_64-unknown-linux-gnu"),
            Some("abc123".to_string())
        );
    }

    #[test]
    fn resolve_url_replaces_both_placeholders() {
        let template = "https://example.com/releases/download/client-wpplugin-v{version}/wptsall-client-{version}-{target}";
        let url = resolve_url(template, "2.1.1");
        assert_eq!(
            url,
            format!(
                "https://example.com/releases/download/client-wpplugin-v2.1.1/wptsall-client-2.1.1-{}",
                current_target_triple()
            )
        );
    }

    #[test]
    fn resolve_url_replaces_platform() {
        let template = "https://example.com/kits-webui-v{version}/kit-webui-{platform}.tar.gz";
        let url = resolve_url(template, "2.1.0");
        assert_eq!(
            url,
            format!(
                "https://example.com/kits-webui-v2.1.0/kit-webui-{}.tar.gz",
                current_platform()
            )
        );
    }

    #[test]
    fn apply_ui_bundle_replaces_ui_tree() {
        let root =
            std::env::temp_dir().join(format!("wptsall-test-ui-root-{}", uuid::Uuid::new_v4()));
        let stage = root.join("stage");
        let ui_src = stage.join("ui/webui");
        std::fs::create_dir_all(&ui_src).unwrap();
        std::fs::write(ui_src.join("index.html"), b"<html>new</html>").unwrap();
        std::fs::write(stage.join("VERSION-WEBUI"), b"9.9.9").unwrap();
        let tar = root.join("webui-ui-9.9.9.tar.gz");
        let status = std::process::Command::new("tar")
            .args(["-czf"])
            .arg(&tar)
            .arg("-C")
            .arg(&stage)
            .arg(".")
            .status()
            .unwrap();
        assert!(status.success());

        let install = root.join("install");
        std::fs::create_dir_all(install.join("ui/webui")).unwrap();
        std::fs::write(install.join("ui/webui/index.html"), b"<html>old</html>").unwrap();

        apply_ui_bundle(&tar, &install, "webui").unwrap();
        assert_eq!(
            std::fs::read_to_string(install.join("ui/webui/index.html")).unwrap(),
            "<html>new</html>"
        );
        assert_eq!(
            std::fs::read_to_string(install.join("VERSION-WEBUI")).unwrap(),
            "9.9.9"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn extract_update_binary_passthrough_raw() {
        let dir = std::env::temp_dir().join("wptsall-test-extract-raw");
        std::fs::create_dir_all(&dir).unwrap();
        let file_path = dir.join("raw-bin");
        std::fs::write(&file_path, b"not-gzip").unwrap();
        let out = extract_update_binary(&file_path).unwrap();
        assert_eq!(out, file_path);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn extract_update_binary_from_kit_tarball() {
        let dir =
            std::env::temp_dir().join(format!("wptsall-test-extract-kit-{}", uuid::Uuid::new_v4()));
        let stage = dir.join("wptsall-client-webui/bin");
        std::fs::create_dir_all(&stage).unwrap();
        let inner = stage.join("wptsall-client");
        std::fs::write(&inner, b"fake-webui-binary").unwrap();
        let tar_path = dir.join("kit-webui-linux-x86_64.tar.gz");
        let status = std::process::Command::new("tar")
            .args(["-czf"])
            .arg(&tar_path)
            .arg("-C")
            .arg(&dir)
            .arg("wptsall-client-webui")
            .status()
            .unwrap();
        assert!(status.success());
        let extracted = extract_update_binary(&tar_path).unwrap();
        let bytes = std::fs::read(&extracted).unwrap();
        assert_eq!(bytes, b"fake-webui-binary");
        let _ = std::fs::remove_file(&extracted);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_url_empty_template() {
        assert_eq!(resolve_url("", "1.0.0"), "");
    }

    #[tokio::test]
    async fn verify_checksum_correct() {
        let dir = std::env::temp_dir().join("wptsall-test-checksum-correct");
        std::fs::create_dir_all(&dir).unwrap();
        let file_path = dir.join("test-file");
        let data = b"hello world";
        std::fs::write(&file_path, data).unwrap();

        let mut hasher = Sha256::new();
        hasher.update(data);
        let expected = format!("{:x}", hasher.finalize());

        assert!(verify_checksum(&file_path, &expected).await.is_ok());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn verify_checksum_mismatch() {
        let dir = std::env::temp_dir().join("wptsall-test-checksum-mismatch");
        std::fs::create_dir_all(&dir).unwrap();
        let file_path = dir.join("test-file");
        std::fs::write(&file_path, b"hello world").unwrap();

        let result = verify_checksum(&file_path, "deadbeef").await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("checksum mismatch"));

        std::fs::remove_dir_all(&dir).ok();
    }

    // -----------------------------------------------------------------------
    // D-1 OTA rollback semantics (plan §7 / roadmap D-1; NEW-T-08). The
    // secure pipeline (manifest sig → kit sig → sha256) fails closed BEFORE
    // any disk mutation — already covered by run-ota-secure-suite.sh. These
    // two tests pin the APPLY stage: fail-closed pre-flight (hard assert)
    // and the missing mid-apply rollback (known_red pin).
    // -----------------------------------------------------------------------

    fn make_ui_install_root(tag: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("wptsall-test-ota-{tag}-{}", uuid::Uuid::new_v4()));
        let ui = root.join("install/ui/webui");
        std::fs::create_dir_all(&ui).unwrap();
        std::fs::write(ui.join("index.html"), b"<html>old</html>").unwrap();
        std::fs::write(root.join("install/VERSION-WEBUI"), b"2.1.0").unwrap();
        root
    }

    #[test]
    fn apply_ui_bundle_corrupt_or_malformed_leaves_existing_ui_intact() {
        // D-1 hard assert: a corrupt (non-gzip) or malformed (no ui/{subdir})
        // bundle is rejected BEFORE the existing UI tree is touched.
        let root = make_ui_install_root("premag");
        let install = root.join("install");
        let index = install.join("ui/webui/index.html");

        // Case A: corrupt download — garbage bytes, tar must fail.
        let corrupt = root.join("corrupt.tar.gz");
        std::fs::write(&corrupt, b"this is not a gzip archive at all").unwrap();
        let err = apply_ui_bundle(&corrupt, &install, "webui").unwrap_err();
        assert!(
            err.to_string().contains("tar failed"),
            "corrupt archive must fail unpack, got: {err}"
        );
        assert_eq!(std::fs::read(&index).unwrap(), b"<html>old</html>");
        assert_eq!(
            std::fs::read(install.join("VERSION-WEBUI")).unwrap(),
            b"2.1.0"
        );

        // Case B: well-formed tar.gz that simply has no ui/webui member.
        let wrong = root.join("wrong-layout.tar.gz");
        let stage = root.join("wrong-stage/ui/other");
        std::fs::create_dir_all(&stage).unwrap();
        std::fs::write(stage.join("index.html"), b"<html>new</html>").unwrap();
        let status = std::process::Command::new("tar")
            .args(["-czf"])
            .arg(&wrong)
            .arg("-C")
            .arg(&root.join("wrong-stage"))
            .arg("ui")
            .status()
            .unwrap();
        assert!(status.success());
        let err = apply_ui_bundle(&wrong, &install, "webui").unwrap_err();
        assert!(
            err.to_string().contains("UI bundle missing ui/webui"),
            "malformed layout must be rejected pre-flight, got: {err}"
        );
        assert_eq!(std::fs::read(&index).unwrap(), b"<html>old</html>");
        assert_eq!(
            std::fs::read(install.join("VERSION-WEBUI")).unwrap(),
            b"2.1.0"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn apply_ui_bundle_mid_copy_failure_has_no_rollback_pin() {
        // D-1 known_red pin: apply_ui_bundle removes the existing ui/{subdir}
        // BEFORE copying the new tree, with no backup/restore step. A failure
        // during the copy phase therefore leaves the install root with NO UI
        // at all. Deterministic injection: a dangling symlink inside the
        // bundle makes std::fs::copy fail mid-apply (the same class of
        // failure as disk-full / permission errors on any real bundle).
        //
        // When the product lane adds rollback (e.g. move dest aside first and
        // restore on copy failure, or unpack to a staging dir and swap),
        // INVERT this pin: assert the old tree is restored byte-identically.
        let root = make_ui_install_root("norollback");
        let install = root.join("install");
        let old_index = install.join("ui/webui/index.html");
        assert!(old_index.is_file());

        let stage = root.join("stage/ui/webui");
        std::fs::create_dir_all(&stage).unwrap();
        std::os::unix::fs::symlink("no-such-target-xyz", stage.join("broken")).unwrap();
        let bundle = root.join("bad-midcopy.tar.gz");
        let status = std::process::Command::new("tar")
            .args(["-czf"])
            .arg(&bundle)
            .arg("-C")
            .arg(&root.join("stage"))
            .arg("ui")
            .status()
            .unwrap();
        assert!(status.success());

        let err = apply_ui_bundle(&bundle, &install, "webui");
        assert!(err.is_err(), "dangling symlink must fail the copy phase");

        // Pin: the old UI tree is already gone — no rollback happened.
        assert!(
            !old_index.exists(),
            "PIN: old UI must still be present after a mid-apply failure once rollback exists"
        );
        assert!(
            !install.join("ui/webui.bak").exists() && !install.join("ui/webui.old").exists(),
            "PIN: no backup artifact is kept today"
        );

        std::fs::remove_dir_all(&root).ok();
    }
}
