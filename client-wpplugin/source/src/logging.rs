use anyhow::{ensure, Context};
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
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
const MAX_LOG_SNAPSHOT_BYTES: u64 = 2 * MAX_LOG_SIZE;

struct AdmittedLogFile {
    file: std::fs::File,
    path: PathBuf,
}

fn check_log_handle(file: &std::fs::File, path: &Path) -> anyhow::Result<()> {
    let current = fs::symlink_metadata(path)?;
    let opened = file.metadata()?;
    ensure!(
        current.is_file() && current.len() == opened.len(),
        "log file authority changed"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            current.dev() == opened.dev() && current.ino() == opened.ino(),
            "log file authority changed"
        );
    }
    Ok(())
}

impl Write for AdmittedLogFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let result = (|| -> anyhow::Result<usize> {
            let storage = crate::storage_capacity::StorageLease::for_uncredited_write(
                &self.path,
                u64::try_from(bytes.len())?,
            )?;
            check_log_handle(&self.file, &self.path)?;
            let written = self.file.write(bytes)?;
            storage.finish()?;
            Ok(written)
        })();
        result.map_err(|error| io::Error::other(error.to_string()))
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.sync_data()
    }
}

/// Buffered log writer. Keeps a `BufWriter<File>` open across calls to reduce
/// syscall overhead from open+write+close per event.
struct LogWriter {
    writer: BufWriter<AdmittedLogFile>,
    path: String,
    /// Approximate bytes written since last flush (tracks buffer fullness).
    pending_bytes: usize,
    /// Approximate total bytes written to the current file (for rotation check).
    file_bytes: u64,
}

static LOG_WRITER: Mutex<Option<LogWriter>> = Mutex::new(None);

/// Process-wide log file path registered once at web-UI / worker startup so
/// route handlers can emit audit events (log_event_global) without threading
/// a `log_file` parameter through every handler signature. Unit tests never
/// register it, so route-level events stay silent there unless a test opts
/// in explicitly.
static LOG_FILE_PATH: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Register the process-wide log file path (first call wins; later calls are
/// ignored so a nested runtime cannot repoint an established deployment).
pub(crate) fn set_log_file_path(path: String) {
    let _ = LOG_FILE_PATH.set(path);
}

/// Route-level audit event variant of [`log_event`]: resolves the path from
/// the process-wide registry instead of a parameter. No-op when the registry
/// was never populated (unit tests) or when logging is disabled.
pub(crate) fn log_event_global(level: &str, event: &str, detail: Value) {
    if let Some(path) = LOG_FILE_PATH.get() {
        let _ = log_event(path, level, event, detail);
    }
}

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
        return matches!(
            env.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        );
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

/// Millisecond-resolution epoch (audit 3.3: seconds-only ts could not order
/// near-simultaneous events). Emitted alongside the seconds `ts` field so
/// existing readers keep working.
pub(crate) fn unix_ts_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[allow(dead_code)]
pub(crate) fn elapsed_ms(start: std::time::Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Metadata header line for a fresh log file (audit 3.3 / matrix P2: log
/// excerpts must self-identify version/os/arch without server access).
/// Emitted as a regular JSON line with event `log_file_header` so every
/// existing JSON-lines consumer renders it without special casing.
fn log_file_header_line() -> String {
    let entry = json!({
        "ts": unix_ts(),
        "ts_ms": unix_ts_ms(),
        "level": "info",
        "event": "log_file_header",
        "detail": {
            "version": env!("CARGO_PKG_VERSION"),
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "pid": std::process::id(),
        }
    });
    format!("{}\n", entry)
}

fn create_log_writer(log_file: &str) -> anyhow::Result<LogWriter> {
    let path = Path::new(log_file);
    let needs_header = match fs::metadata(path) {
        Ok(metadata) => {
            ensure!(metadata.is_file(), "log destination is not a regular file");
            metadata.len() == 0
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => true,
        Err(error) => return Err(error.into()),
    };
    let header = needs_header.then(log_file_header_line);
    let storage = crate::storage_capacity::StorageLease::for_uncredited_write(
        path,
        header.as_ref().map_or(0, |value| value.len() as u64),
    )?;
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("open log file failed: {}", log_file))?;
    let resolved = fs::canonicalize(path)?;
    check_log_handle(&file, &resolved)?;
    let mut file_bytes = file.metadata()?.len();
    if file_bytes == 0 {
        let header = header.context("log header authority changed")?;
        file.write_all(header.as_bytes())?;
        file.sync_data()?;
        file_bytes = u64::try_from(header.len())?;
    }
    storage.finish()?;
    Ok(LogWriter {
        writer: BufWriter::new(AdmittedLogFile {
            file,
            path: resolved,
        }),
        path: log_file.to_string(),
        pending_bytes: 0,
        file_bytes,
    })
}

fn flush_writer(writer: &mut LogWriter) -> anyhow::Result<()> {
    let result = writer.writer.flush();
    writer.pending_bytes = writer.writer.buffer().len();
    result.context("flush admitted log failed")
}

fn select_log_writer(slot: &mut Option<LogWriter>, path: &str) -> anyhow::Result<()> {
    if slot.as_ref().is_some_and(|writer| writer.path == path) {
        return Ok(());
    }
    if let Some(writer) = slot.as_mut() {
        flush_writer(writer)?;
    }
    let next = create_log_writer(path)?;
    *slot = Some(next);
    Ok(())
}

pub(crate) fn init_log_file(log_file: &str) -> anyhow::Result<()> {
    let mut slot = LOG_WRITER.lock().unwrap_or_else(|error| error.into_inner());
    select_log_writer(&mut slot, log_file)
}

pub(crate) fn init_runtime_log_file(log_file: &str) -> anyhow::Result<()> {
    if !get_log_enabled() {
        return Ok(());
    }
    match init_log_file(log_file) {
        Err(error) if error.is::<crate::storage_capacity::RootCapacityExhausted>() => {
            // Optional logging must not block recovery or the capacity settings.
            // No fallback write, result credit, or sensitive error detail.
            eprintln!("warning: persistent logging is full; recovery remains available");
            Ok(())
        }
        result => result,
    }
}

/// Truncate the log file and reset the buffered writer so the next event
/// starts at offset 0 (route: POST /api/logs/clear). The writer is dropped
/// BEFORE truncation — otherwise its stale file offset would punch a sparse
/// hole into the truncated file on the next write.
pub(crate) fn clear_log_file(log_file: &str) -> anyhow::Result<()> {
    {
        let mut guard = LOG_WRITER.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(writer) = guard.take() {
            // Explicit clear discards pending bytes without a drop-time write.
            let (file, _buffer) = writer.writer.into_parts();
            drop(file);
        }
    }
    if Path::new(log_file).exists() {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(log_file)
            .with_context(|| format!("truncate log failed: {}", log_file))?;
        drop(file);
    }
    Ok(())
}

fn log_snapshot(path: &Path) -> anyhow::Result<Vec<u8>> {
    let resolved = fs::canonicalize(path)?;
    let path = resolved.as_path();
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() && metadata.len() <= MAX_LOG_SNAPSHOT_BYTES,
        "log snapshot is not a bounded regular file"
    );
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_LOG_SNAPSHOT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        u64::try_from(bytes.len())? == metadata.len(),
        "log snapshot authority changed"
    );
    Ok(bytes)
}

fn copy_log_backup_and_truncate(path: &Path, backup: &Path) -> anyhow::Result<()> {
    let bytes = log_snapshot(path)?;
    let previous = match fs::symlink_metadata(backup) {
        Ok(_) => Some(log_snapshot(backup)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    crate::bindings::atomic_file::install_siblings_uncredited(
        path,
        &[(backup, &bytes, previous.as_deref())],
    )?;
    let storage = crate::storage_capacity::StorageLease::for_uncredited_write(path, 0)?;
    ensure!(log_snapshot(path)? == bytes, "retained live log changed");
    ensure!(
        log_snapshot(backup)? == bytes,
        "log backup readback differs"
    );
    let file = OpenOptions::new().write(true).open(path)?;
    check_log_handle(&file, path)?;
    file.set_len(0)?;
    file.sync_data()?;
    storage.finish()?;
    Ok(())
}

/// Rotate only a regular-file family. A failed backup never authorizes truncation.
fn rotate_log(path: &str) -> anyhow::Result<()> {
    let resolved = fs::canonicalize(path)?;
    let path = resolved.to_str().context("log path is not UTF-8")?;
    let current = Path::new(path);
    let storage = crate::storage_capacity::StorageLease::for_uncredited_write(current, 0)?;
    ensure!(fs::symlink_metadata(current)?.is_file(), "invalid live log");
    for slot in 1..=MAX_LOG_BACKUPS {
        match fs::symlink_metadata(format!("{path}.{slot}")) {
            Ok(metadata) => ensure!(metadata.is_file(), "invalid log backup"),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    // Rename existing backups N-1 → N, ..., 1 → 2
    for i in (1..MAX_LOG_BACKUPS).rev() {
        let from = format!("{}.{}", path, i);
        let to = format!("{}.{}", path, i + 1);
        if Path::new(&from).exists() {
            fs::rename(&from, &to).context("shift log backup failed")?;
        }
    }
    // Keep the current inode present, including for retained aliases. The
    // complete admitted backup must publish before any live-byte truncation.
    let backup1 = format!("{}.1", path);
    drop(storage);
    copy_log_backup_and_truncate(current, Path::new(&backup1))?;
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
        "ts_ms": unix_ts_ms(),
        "level": level,
        "event": event,
        "detail": detail
    });
    let line = format!("{}\n", entry);
    let line_len = line.len();
    ensure!(
        u64::try_from(line_len)? <= MAX_LOG_SIZE,
        "log event exceeds its bounded snapshot"
    );
    // Flush immediately for warning-and-above: identity-chain and other
    // security/ops events (identity_mismatch / identity_unknown /
    // identity_stale / identity.verify_failed) are warning level, and
    // external gates assert on them while the client is still running.
    // Waiting for the byte threshold (or graceful shutdown) made those
    // events invisible for minutes to live observers.
    let should_flush = level_to_u8(level) >= 2;

    let mut guard = LOG_WRITER.lock().unwrap_or_else(|e| e.into_inner());

    select_log_writer(&mut guard, log_file)?;
    let lw = guard.as_mut().context("log writer missing")?;

    // Check rotation before writing.
    if lw.file_bytes >= MAX_LOG_SIZE {
        // Flush + close current writer before rotating.
        flush_writer(lw)?;
        let path = lw.writer.get_ref().path.clone();
        *guard = None;
        let actual = path.to_str().context("log path is not UTF-8")?;
        rotate_log(log_file)?;
        let mut next = create_log_writer(actual)?;
        next.path = log_file.to_string();
        *guard = Some(next);
    }
    let lw = guard
        .as_mut()
        .context("log writer missing after rotation")?;
    lw.writer
        .write_all(line.as_bytes())
        .with_context(|| format!("write log failed: {}", log_file))?;
    lw.pending_bytes += line_len;
    lw.file_bytes += line_len as u64;

    // Flush on warning-and-above or when the buffer threshold is reached.
    if should_flush || lw.pending_bytes >= FLUSH_THRESHOLD {
        flush_writer(lw)?;
    }

    Ok(())
}

/// Flush any buffered log data to disk. Called on graceful shutdown.
pub(crate) fn flush_log() {
    let _ = flush_log_checked();
}

fn flush_log_checked() -> anyhow::Result<()> {
    let mut guard = LOG_WRITER.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(lw) = guard.as_mut() {
        flush_writer(lw)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Test support (cfg(test)): serialize global log-state mutation across ALL
// test modules. logging::tests used to own this privately; discoverer
// integration tests assert on warn lines written through the real writer,
// so they must flip LOG_ENABLED under the SAME lock these tests use.
// ---------------------------------------------------------------------------









pub(crate) fn maybe_export_log(log_file: &str, export_path: &str) -> anyhow::Result<()> {
    if export_path.trim().is_empty() {
        return Ok(());
    }

    // Flush before exporting to ensure all data is on disk.
    flush_log_checked()?;

    let snapshot = log_snapshot(Path::new(log_file))?;
    crate::bindings::atomic_file::install_uncredited(Path::new(export_path), &snapshot)
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
    // S7 (07 audit, batch G): was 12 chars — enough of a prefix to aid
    // guessing/confirmation attacks on the session token. 8 chars keep it
    // debuggable (log correlation) while halving the exposed material.
    token.chars().take(8).collect::<String>()
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
    k == "key"
        || k == "signature"
        || k == "access_code"
        || k.contains("token")
        || k.contains("secret")
        || k.contains("password")
        || k.contains("passwd")
        || k.contains("api_key")
        || k.contains("apikey")
        || k.contains("access_key")
        || k.contains("signature")
        || k.contains("access_code")
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
pub(crate) fn redact_string_for_log(input: &str) -> String {
    let lower = input.to_ascii_lowercase();
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0usize;

    let bearer = "bearer ";
    while i < bytes.len() {
        let rest_lower = &lower[i..];

        // Parse an embedded JSON value rather than searching for its first
        // quote. This preserves escaped/Unicode strings and also unwraps JSON
        // strings containing another JSON response before recursive redaction.
        if matches!(bytes[i], b'{' | b'[' | b'"') {
            let mut values = serde_json::Deserializer::from_str(&input[i..]).into_iter::<Value>();
            if let Some(Ok(value)) = values.next() {
                let consumed = values.byte_offset();
                let following = input[i + consumed..].trim_start();
                // A quoted key in a partial JSON/assignment fragment belongs
                // to the key-value scanner below, not the string-value parser.
                if !value.is_string() || !following.starts_with([':', '=']) {
                    let sanitized = redact_value_for_log(value);
                    out.push_str(&sanitized.to_string());
                    i += consumed;
                    continue;
                }
            }
        }

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

        let route_prefix = "/wptsall/v2/";
        if rest_lower.starts_with(route_prefix) {
            let after = i + route_prefix.len();
            let span = credential_span(&input[after..], &['/', '?', '#', '"', '\'', ')']);
            out.push_str(route_prefix);
            out.push_str("[REDACTED]");
            i = after + span;
            continue;
        }

        let peer_prefix = "/wpmmcc/v1/";
        if rest_lower.starts_with(peer_prefix) {
            let after = i + peer_prefix.len();
            let span = credential_span(&input[after..], &['/', '?', '#', '"', '\'', ')']);
            let tail = input[after + span..].strip_prefix("/sync");
            if tail.is_some_and(|tail| {
                tail.is_empty()
                    || tail.chars().next().is_some_and(|ch| {
                        ch.is_whitespace()
                            || matches!(ch, '/' | '?' | '#' | '"' | '\'' | ')' | ',' | ';')
                    })
            }) {
                out.push_str(peer_prefix);
                out.push_str("[REDACTED]");
                i = after + span;
                continue;
            }
        }

        // Recognize query assignments and embedded JSON keys without changing
        // the actual provider request. Percent-encoded key names count too.
        let key_end = input[i..]
            .find(|ch: char| !ch.is_ascii_alphanumeric() && !matches!(ch, '_' | '-' | '.' | '%'))
            .map(|offset| i + offset)
            .unwrap_or(bytes.len());
        let raw_key = &input[i..key_end];
        let decoded_key = url::form_urlencoded::parse(format!("{raw_key}=").as_bytes())
            .next()
            .map(|(key, _)| key.into_owned())
            .unwrap_or_default();
        let mut separator = key_end;
        if matches!(bytes.get(separator), Some(b'"' | b'\'')) {
            separator += 1;
        }
        while bytes
            .get(separator)
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            separator += 1;
        }
        if !raw_key.is_empty()
            && is_sensitive_log_key(&decoded_key)
            && matches!(bytes.get(separator), Some(b'=' | b':'))
        {
            let mut value_start = separator + 1;
            while bytes
                .get(value_start)
                .is_some_and(|byte| byte.is_ascii_whitespace())
            {
                value_start += 1;
            }
            // A value may be quoted (`token="abc"`, `token='abc'`): keep the
            // quotes, redact between them.
            let quote = if input[value_start..].starts_with('"') {
                '"'
            } else if input[value_start..].starts_with('\'') {
                '\''
            } else {
                '\0'
            };
            let scan_from = if quote != '\0' {
                value_start + 1
            } else {
                value_start
            };
            let span = if quote != '\0' {
                let mut escaped = false;
                input[scan_from..]
                    .char_indices()
                    .find_map(|(offset, ch)| {
                        if ch == '\n' || (ch == quote && !escaped) {
                            return Some(offset);
                        }
                        escaped = ch == '\\' && !escaped;
                        None
                    })
                    .unwrap_or(input.len() - scan_from)
            } else {
                credential_span(&input[scan_from..], &['"', '\'', '&', '}', ' ', '\n'])
            };
            out.push_str(&input[i..value_start]);
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

        if key_end > i {
            out.push_str(&input[i..key_end]);
            i = key_end;
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
