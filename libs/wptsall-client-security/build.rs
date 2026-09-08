fn main() {
    let manifest_dir = std::path::PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by cargo"),
    );
    let repo_pubkey = manifest_dir
        .join("../../install-client/security/keys/wptsall-minisign.pub")
        .canonicalize()
        .ok();

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
        .unwrap_or_else(|| "RWR7lrdabZEEywfWEfrRJXIyP5h+LHEabOA8JFiNJ3vGLpNtppyabHfP".to_string());

    println!("cargo:rustc-env=WPTSALL_EMBEDDED_MINISIGN_PUBKEY={key}");
    println!("cargo:rerun-if-env-changed=WPTSALL_MINISIGN_PUBKEY");
    if let Some(path) = repo_pubkey {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}
