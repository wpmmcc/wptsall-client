use anyhow::Context;
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

// Release default: logging DISABLED. Runtime entry points (web UI, worker)
// apply the persisted Settings-page state (or the WPTSALL_LOG_ENABLED env
// override) via resolve_log_enabled() before meaningful work begins; the
// static starts false so any pre-config event is never persisted.
static LOG_ENABLED: AtomicBool = AtomicBool::new(false);
static LOG_MIN_LEVEL: AtomicU8 = AtomicU8::new(1); // 1 = info

/// Maximum log file size before rotation (50 MB).
const MAX_LOG_SIZE: u64 = 50 * 1024 * 1024;
/// Number of rotated backup files to keep.
const MAX_LOG_BACKUPS: u32 = 3;
/// Flush the buffer after accumulating this many bytes.
const FLUSH_THRESHOLD: usize = 8 * 1024; // 8 KB

/// Buffered log writer. Keeps a `BufWriter<File>` open across calls to reduce
/// syscall overhead from open+write+close per event.
struct LogWriter {
    writer: BufWriter<std::fs::File>,
    path: String,
    /// Approximate bytes written since last flush (tracks buffer fullness).
    pending_bytes: usize,
    /// Approximate total bytes written to the current file (for rotation check).
    file_bytes: u64,
}

static LOG_WRITER: Mutex<Option<LogWriter>> = Mutex::new(None);

pub(crate) fn level_to_u8(level: &str) -> u8 {
    match level {
        "debug" => 0,
        "warn" | "warning" => 2,
        "error" => 3,
        _ => 1, // info and anything else
    }
}

pub(crate) fn set_log_enabled(v: bool) {
    LOG_ENABLED.store(v, Ordering::Relaxed);
}

/// Resolve the effective log-enabled flag from its two sources.
///
/// Release default is **disabled**:
/// * a missing DB value (fresh install) disables logging;
/// * only an explicit DB value `true` (written by the Settings page) enables it;
/// * any other DB value (including garbage) disables it;
/// * the `WPTSALL_LOG_ENABLED` env var overrides the DB when set
///   (`1`/`true`/`yes`/`on` enable, anything else disables).
pub(crate) fn resolve_log_enabled(db_value: Option<&str>, env_value: Option<&str>) -> bool {
    if let Some(env) = env_value {
        return matches!(env.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on");
    }
    db_value
        .map(|v| v.trim().eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

pub(crate) fn set_log_min_level(l: &str) {
    LOG_MIN_LEVEL.store(level_to_u8(l), Ordering::Relaxed);
}

pub(crate) fn get_log_enabled() -> bool {
    LOG_ENABLED.load(Ordering::Relaxed)
}

pub(crate) fn get_log_min_level_str() -> &'static str {
    match LOG_MIN_LEVEL.load(Ordering::Relaxed) {
        0 => "debug",
        2 => "warn",
        3 => "error",
        _ => "info",
    }
}

pub(crate) fn unix_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[allow(dead_code)]
pub(crate) fn elapsed_ms(start: std::time::Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Open (or reopen) the log file and store a buffered writer in the global slot.
fn open_log_writer(log_file: &str) -> anyhow::Result<()> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file)
        .with_context(|| format!("open log file failed: {}", log_file))?;
    let file_bytes = file.metadata().map(|m| m.len()).unwrap_or(0);
    let writer = BufWriter::new(file);
    let mut guard = LOG_WRITER.lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some(LogWriter {
        writer,
        path: log_file.to_string(),
        pending_bytes: 0,
        file_bytes,
    });
    Ok(())
}

pub(crate) fn init_log_file(log_file: &str) -> anyhow::Result<()> {
    if let Some(parent) = Path::new(log_file).parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create log dir failed: {}", parent.display()))?;
        }
    }

    open_log_writer(log_file)
}

/// Rotate log files: current → .1, .1 → .2, … .N deleted.
fn rotate_log(path: &str) -> anyhow::Result<()> {
    // Rename existing backups N-1 → N, ..., 1 → 2
    for i in (1..MAX_LOG_BACKUPS).rev() {
        let from = format!("{}.{}", path, i);
        let to = format!("{}.{}", path, i + 1);
        if Path::new(&from).exists() {
            let _ = fs::rename(&from, &to);
        }
    }
    // Delete the oldest backup if it would overflow
    let oldest = format!("{}.{}", path, MAX_LOG_BACKUPS);
    if Path::new(&oldest).exists() {
        let _ = fs::remove_file(&oldest);
    }
    // Current → .1
    let backup1 = format!("{}.1", path);
    fs::rename(path, &backup1)
        .with_context(|| format!("rotate log failed: {} → {}", path, backup1))?;
    Ok(())
}

pub(crate) fn log_event(
    log_file: &str,
    level: &str,
    event: &str,
    detail: Value,
) -> anyhow::Result<()> {
    if !LOG_ENABLED.load(Ordering::Relaxed) {
        return Ok(());
    }
    if level_to_u8(level) < LOG_MIN_LEVEL.load(Ordering::Relaxed) {
        return Ok(());
    }
    // Central recursive redaction at the boundary (BUG-LOG-02): whatever a
    // caller passes — nested objects, arrays, or free-form strings carrying
    // bearer tokens / URL credentials — is sanitized before it can reach disk.
    let detail = redact_value_for_log(detail);
    let entry = json!({
        "ts": unix_ts(),
        "level": level,
        "event": event,
        "detail": detail
    });
    let line = format!("{}\n", entry);
    let line_len = line.len();
    let is_error = level_to_u8(level) >= 3;

    let mut guard = LOG_WRITER.lock().unwrap_or_else(|e| e.into_inner());

    // If no writer is open or the path changed, open one.
    let needs_open = match &*guard {
        Some(lw) => lw.path != log_file,
        None => true,
    };
    if needs_open {
        drop(guard);
        // Initialize outside the lock to avoid double-locking.
        let _ = open_log_writer(log_file);
        guard = LOG_WRITER.lock().unwrap_or_else(|e| e.into_inner());
    }

    let lw = match guard.as_mut() {
        Some(lw) => lw,
        None => {
            // Fallback: direct write (should not normally happen).
            drop(guard);
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_file)
                .with_context(|| format!("open log file failed: {}", log_file))?;
            file.write_all(line.as_bytes())
                .with_context(|| format!("write log failed: {}", log_file))?;
            return Ok(());
        }
    };

    // Check rotation before writing.
    if lw.file_bytes >= MAX_LOG_SIZE {
        // Flush + close current writer before rotating.
        let _ = lw.writer.flush();
        let path = lw.path.clone();
        *guard = None;
        drop(guard);

        let _ = rotate_log(&path);
        let _ = open_log_writer(&path);
        guard = LOG_WRITER.lock().unwrap_or_else(|e| e.into_inner());

        if let Some(lw) = guard.as_mut() {
            lw.writer
                .write_all(line.as_bytes())
                .with_context(|| format!("write log failed: {}", log_file))?;
            lw.pending_bytes += line_len;
            lw.file_bytes += line_len as u64;
            if is_error || lw.pending_bytes >= FLUSH_THRESHOLD {
                let _ = lw.writer.flush();
                lw.pending_bytes = 0;
            }
        }
        return Ok(());
    }

    lw.writer
        .write_all(line.as_bytes())
        .with_context(|| format!("write log failed: {}", log_file))?;
    lw.pending_bytes += line_len;
    lw.file_bytes += line_len as u64;

    // Flush on error level or when buffer threshold is reached.
    if is_error || lw.pending_bytes >= FLUSH_THRESHOLD {
        let _ = lw.writer.flush();
        lw.pending_bytes = 0;
    }

    Ok(())
}

/// Flush any buffered log data to disk. Called on graceful shutdown.
pub(crate) fn flush_log() {
    let mut guard = LOG_WRITER.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(lw) = guard.as_mut() {
        let _ = lw.writer.flush();
        lw.pending_bytes = 0;
    }
}

pub(crate) fn maybe_export_log(log_file: &str, export_path: &str) -> anyhow::Result<()> {
    if export_path.trim().is_empty() {
        return Ok(());
    }

    // Flush before exporting to ensure all data is on disk.
    flush_log();

    if let Some(parent) = Path::new(export_path).parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create export dir failed: {}", parent.display()))?;
        }
    }

    fs::copy(log_file, export_path)
        .with_context(|| format!("export log failed: from {} to {}", log_file, export_path))?;
    eprintln!("Log exported to {}", export_path);
    Ok(())
}

pub(crate) fn snippet(s: &str) -> String {
    if s.len() > 220 {
        let end = s.floor_char_boundary(220);
        format!("{}...", &s[..end])
    } else {
        s.to_string()
    }
}

pub(crate) fn session_token_prefix(token: &str) -> String {
    token.chars().take(12).collect::<String>()
}

#[allow(dead_code)]
pub(crate) fn mask_email(email: &str) -> String {
    let mut parts = email.split('@');
    let local = parts.next().unwrap_or("");
    let domain = parts.next().unwrap_or("");
    if local.len() <= 2 {
        return format!("{}***@{}", local, domain);
    }
    let first = &local[0..1];
    let last = &local[local.len() - 1..];
    format!("{}***{}@{}", first, last, domain)
}

// ---------------------------------------------------------------------------
// Central log redaction (BUG-LOG-02, v2.1.3)
// ---------------------------------------------------------------------------
// Mirror of the WP plugin's wptsall_redact_log_value(): every value crossing
// the log_event() boundary is sanitized here, so a caller that passes an
// api_key/secret/token/authorization field — nested at any depth — can never
// persist the raw credential to disk. Free-form strings additionally get
// Bearer-token and URL-userinfo scrubbing.

/// Key names (case-insensitive, substring match) whose values are replaced
/// wholesale with `[REDACTED]`.
fn is_sensitive_log_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    k.contains("token")
        || k.contains("secret")
        || k.contains("password")
        || k.contains("passwd")
        || k.contains("api_key")
        || k.contains("apikey")
        || k.contains("credential")
        || k.contains("authorization")
        || k.contains("private_key")
        || k.contains("bearer")
}

/// Recursively redact a JSON value destined for the log file.
fn redact_value_for_log(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, val)| {
                    if is_sensitive_log_key(&key) {
                        (key, Value::String("[REDACTED]".to_string()))
                    } else {
                        (key, redact_value_for_log(val))
                    }
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(redact_value_for_log).collect()),
        Value::String(s) => Value::String(redact_string_for_log(&s)),
        other => other,
    }
}

/// Redact credential-shaped substrings inside free-form string values:
/// `Bearer <token>` headers, `scheme://user:pass@host` URL userinfo, and
/// `token=…`-style query/assignment fragments with sensitive keys.
fn redact_string_for_log(input: &str) -> String {
    let lower = input.to_ascii_lowercase();
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0usize;

    let bearer = "bearer ";
    let needles: [&str; 8] = [
        "token=", "secret=", "password=", "passwd=", "api_key=", "apikey=", "access_key=",
        "authorization=",
    ];

    while i < bytes.len() {
        let rest_lower = &lower[i..];

        if rest_lower.starts_with(bearer) {
            let after = bearer.len();
            let span = credential_span(&input[i + after..], &['"', '\'', ',', ';']);
            out.push_str("Bearer [REDACTED]");
            i += after + span;
            continue;
        }

        if rest_lower.starts_with("://") {
            let after = 3;
            let userinfo = &input[i + after..];
            let at = userinfo.find('@');
            let slash = userinfo.find('/');
            if matches!(at, Some(a) if slash.map_or(true, |s| a < s)) {
                let a = at.unwrap();
                out.push_str("://[REDACTED]@");
                i += after + a + 1;
                continue;
            }
        }

        if let Some(needle) = needles.iter().find(|n| rest_lower.starts_with(*n)) {
            let after = needle.len();
            // A value may be quoted (`token="abc"`, `token='abc'`): keep the
            // quotes, redact between them.
            let value_start = i + after;
            let quote = if input[value_start..].starts_with('"') {
                '"'
            } else if input[value_start..].starts_with('\'') {
                '\''
            } else {
                '\0'
            };
            let scan_from = if quote != '\0' { value_start + 1 } else { value_start };
            let span = if quote != '\0' {
                credential_span(&input[scan_from..], &[quote, '\n'])
            } else {
                credential_span(&input[scan_from..], &['"', '\'', '&', '}', ' ', '\n'])
            };
            out.push_str(&input[i..i + after]);
            if quote != '\0' {
                out.push(quote);
            }
            out.push_str("[REDACTED]");
            let mut next = scan_from + span;
            if quote != '\0' && input.as_bytes().get(next) == Some(&(quote as u8)) {
                out.push(quote);
                next += 1;
            }
            i = next;
            continue;
        }

        // Copy one full UTF-8 character (scans below stay on char boundaries).
        let ch_len = input[i..].chars().next().map(char::len_utf8).unwrap_or(1);
        out.push_str(&input[i..i + ch_len]);
        i += ch_len;
    }
    out
}

/// Length of the credential run starting at `s`, ending at any of the
/// terminator characters (or end of input).
fn credential_span(s: &str, terminators: &[char]) -> usize {
    s.find(|c: char| terminators.contains(&c) || c.is_whitespace())
        .unwrap_or(s.len())
}

#[cfg(test)]
mod tests;
