fn main() {
    let manifest_dir = std::path::PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by cargo"),
    );
    // Monorepo-only lookup: the legacy host production key. The public
    // source repository has no install-client/ tree, so this path misses
    // there and the fallback below applies.
    let repo_pubkey = manifest_dir
        .join("../../install-client/security/keys/wptsall-minisign.pub")
        .canonicalize()
        .ok();

    // Pin precedence:
    //   1. WPTSALL_MINISIGN_PUBKEY env (ephemeral CI keys, host overrides —
    //      e.g. the local OTA secure suite signs its mock manifest with the
    //      monorepo key and exports this to match);
    //   2. the public release channel key (DEFAULT — `wptsall-public-release`,
    //      key id DD1B5C2FCE14D14A). The public repo's `release-publish`
    //      workflow signs every GitHub Release with the matching private key,
    //      held only in the repository's Actions secret
    //      WPTSALL_RELEASE_MINISIGN_KEY. Out-of-the-box builds therefore
    //      verify the official update channel.
    //   3. the monorepo legacy host key file — OPT-IN only, via
    //      WPTSALL_USE_LOCAL_REPO_KEY=1 (host/private-channel builds that
    //      serve a monorepo-signed releases manifest). Without the opt-in a
    //      monorepo build must still trust the public channel: silently
    //      embedding the legacy key shipped binaries that could never
    //      verify the releases they were published with (BUG-OTA-01).
    let public_key = "RWRK0RTOL1wb3YAJ6sywCLsN0kvpOttJ1CYv3AHluZAfYUXzSEm/0cSw".to_string();
    let use_repo_key = std::env::var("WPTSALL_USE_LOCAL_REPO_KEY")
        .map(|v| {
            let normalized = v.trim().to_ascii_lowercase();
            matches!(normalized.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(false);
    let key = std::env::var("WPTSALL_MINISIGN_PUBKEY")
        .ok()
        .or_else(|| {
            if use_repo_key {
                repo_pubkey
                    .as_deref()
                    .and_then(|path| std::fs::read_to_string(path).ok())
                    .and_then(|content| {
                        content
                            .lines()
                            .find(|l| {
                                l.starts_with("RWR") || l.starts_with("RWQ") || l.starts_with("RWT")
                            })
                            .map(|l| l.trim().to_string())
                    })
            } else {
                None
            }
        })
        .unwrap_or(public_key);

    println!("cargo:rustc-env=WPTSALL_EMBEDDED_MINISIGN_PUBKEY={key}");
    println!("cargo:rerun-if-env-changed=WPTSALL_MINISIGN_PUBKEY");
    println!("cargo:rerun-if-env-changed=WPTSALL_USE_LOCAL_REPO_KEY");
    if let Some(path) = repo_pubkey {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}
