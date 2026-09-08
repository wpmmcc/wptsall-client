use std::path::PathBuf;
use std::sync::RwLock;
use std::time::SystemTime;

/// Cache for the served HTML: (content, mtime_at_load).
static CACHE: RwLock<Option<(Vec<u8>, SystemTime)>> = RwLock::new(None);

const EMBEDDED_HTML: &str = include_str!("../../frontend/dist/index.html");

fn candidate_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(p) = std::env::var("WPTSALL_WEB_UI_PATH") {
        let path = PathBuf::from(p);
        if path.is_dir() {
            out.push(path.join("index.html"));
        } else {
            out.push(path);
        }
    }
    if let Ok(root) = std::env::var("WPTSALL_INSTALL_ROOT") {
        out.push(PathBuf::from(root).join("ui/webui/index.html"));
    }
    // Dev / repo checkout
    out.push(PathBuf::from("frontend/dist/index.html"));
    // Relative to binary when started via kit launcher (bin/../ui/webui)
    if let Ok(exe) = std::env::current_exe() {
        if let Some(bin_dir) = exe.parent() {
            out.push(bin_dir.join("../ui/webui/index.html"));
        }
    }
    out
}

fn read_with_mtime(path: &std::path::Path) -> Option<(Vec<u8>, SystemTime)> {
    let metadata = std::fs::metadata(path).ok()?;
    let mtime = metadata.modified().ok()?;
    if let Ok(cache) = CACHE.read() {
        if let Some((cached, cached_mtime)) = cache.as_ref() {
            if *cached_mtime == mtime {
                return Some((cached.clone(), mtime));
            }
        }
    }
    let content = std::fs::read(path).ok()?;
    if content.is_empty() {
        return None;
    }
    if let Ok(mut cache) = CACHE.write() {
        *cache = Some((content.clone(), mtime));
    }
    Some((content, mtime))
}

/// Serve the main SPA HTML.
///
/// Priority: `WPTSALL_WEB_UI_PATH` → `$WPTSALL_INSTALL_ROOT/ui/webui` →
/// CWD `frontend/dist` → beside-binary `../ui/webui` → compile-time embed fallback.
pub(crate) fn web_ui_html() -> Vec<u8> {
    for path in candidate_paths() {
        let canonical = std::fs::canonicalize(&path).unwrap_or(path);
        if let Some((content, _)) = read_with_mtime(&canonical) {
            return content;
        }
    }
    EMBEDDED_HTML.as_bytes().to_vec()
}
