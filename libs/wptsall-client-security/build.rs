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
    //   1. WPTSALL_MINISIGN_PUBKEY env (ephemeral CI keys, host overrides);
    //   2. the monorepo key file above (legacy host/private channel —
    //      monorepo builds only);
    //   3. the public release channel key (public repository builds —
    //      `wptsall-public-release`, key id DD1B5C2FCE14D14A). The public
    //      repo's `release-publish` workflow signs every GitHub Release
    //      with the matching private key, held only in the repository's
    //      Actions secret WPTSALL_RELEASE_MINISIGN_KEY. Out-of-the-box
    //      public builds therefore verify the official update channel.
    let key = std::env::var("WPTSALL_MINISIGN_PUBKEY")
        .ok()
        .or_else(|| {
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
        })
        .unwrap_or_else(|| "RWRK0RTOL1wb3YAJ6sywCLsN0kvpOttJ1CYv3AHluZAfYUXzSEm/0cSw".to_string());

    println!("cargo:rustc-env=WPTSALL_EMBEDDED_MINISIGN_PUBKEY={key}");
    println!("cargo:rerun-if-env-changed=WPTSALL_MINISIGN_PUBKEY");
    if let Some(path) = repo_pubkey {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}
