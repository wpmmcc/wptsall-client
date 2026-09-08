use anyhow::Context;
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

static LOG_ENABLED: AtomicBool = AtomicBool::new(true);
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

#[cfg(test)]
mod tests;
