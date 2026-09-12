use super::*;

// Serialize all tests that read/write the process-global LOG_ENABLED / LOG_MIN_LEVEL.
// Without this, parallel test threads can corrupt each other's expected state.
static GLOBAL_LOG_LOCK: Mutex<()> = Mutex::new(());

/// RAII guard: restores LOG_ENABLED and LOG_MIN_LEVEL when dropped.
struct LogStateGuard {
    prev_enabled: bool,
    prev_level: &'static str,
}
impl Drop for LogStateGuard {
    fn drop(&mut self) {
        set_log_enabled(self.prev_enabled);
        set_log_min_level(self.prev_level);
        // Close any test log writer to avoid leaking into other tests.
        let mut g = LOG_WRITER.lock().unwrap_or_else(|e| e.into_inner());
        *g = None;
    }
}

/// Acquires the global log lock, snapshots current state, applies (enabled, level),
/// and returns a tuple of (MutexGuard, LogStateGuard) that restores state on drop.
fn acquire_log_state(
    enabled: bool,
    level: &str,
) -> (std::sync::MutexGuard<'static, ()>, LogStateGuard) {
    let guard = GLOBAL_LOG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let state = LogStateGuard {
        prev_enabled: get_log_enabled(),
        prev_level: get_log_min_level_str(),
    };
    set_log_enabled(enabled);
    set_log_min_level(level);
    // Reset any existing log writer so tests don't interfere with each other.
    {
        let mut g = LOG_WRITER.lock().unwrap_or_else(|e| e.into_inner());
        *g = None;
    }
    (guard, state)
}

// -----------------------------------------------------------------------
// level_to_u8 — pure function, no global state
// -----------------------------------------------------------------------

#[test]
fn level_to_u8_debug() {
    assert_eq!(level_to_u8("debug"), 0);
}

#[test]
fn level_to_u8_info() {
    assert_eq!(level_to_u8("info"), 1);
}

#[test]
fn level_to_u8_warn() {
    assert_eq!(level_to_u8("warn"), 2);
}

#[test]
fn level_to_u8_warning_alias() {
    assert_eq!(level_to_u8("warning"), 2);
}

#[test]
fn level_to_u8_error() {
    assert_eq!(level_to_u8("error"), 3);
}

#[test]
fn level_to_u8_unknown_defaults_to_info() {
    assert_eq!(level_to_u8("trace"), 1);
}

#[test]
fn level_to_u8_empty_defaults_to_info() {
    assert_eq!(level_to_u8(""), 1);
}

// -----------------------------------------------------------------------
// get_log_min_level_str round-trips — touches global state
// -----------------------------------------------------------------------

#[test]
fn min_level_round_trips_debug() {
    let (_lock, _state) = acquire_log_state(true, "debug");
    assert_eq!(get_log_min_level_str(), "debug");
}

#[test]
fn min_level_round_trips_info() {
    let (_lock, _state) = acquire_log_state(true, "info");
    assert_eq!(get_log_min_level_str(), "info");
}

#[test]
fn min_level_round_trips_warn() {
    let (_lock, _state) = acquire_log_state(true, "warn");
    assert_eq!(get_log_min_level_str(), "warn");
}

#[test]
fn min_level_round_trips_error() {
    let (_lock, _state) = acquire_log_state(true, "error");
    assert_eq!(get_log_min_level_str(), "error");
}

#[test]
fn min_level_warning_alias_returns_warn() {
    // "warning" maps to u8=2, which get_log_min_level_str returns as "warn"
    let (_lock, _state) = acquire_log_state(true, "warning");
    assert_eq!(get_log_min_level_str(), "warn");
}

// -----------------------------------------------------------------------
// get_log_enabled toggle — touches global state
// -----------------------------------------------------------------------

#[test]
fn log_enabled_toggle() {
    let (_lock, _state) = acquire_log_state(false, "info");
    assert!(!get_log_enabled());
    set_log_enabled(true);
    assert!(get_log_enabled());
}

// -----------------------------------------------------------------------
// resolve_log_enabled — release default is disabled
// -----------------------------------------------------------------------

#[test]
fn resolve_log_enabled_defaults_to_disabled() {
    assert!(!resolve_log_enabled(None, None), "fresh install must default to disabled");
}

#[test]
fn resolve_log_enabled_only_explicit_true_enables() {
    assert!(resolve_log_enabled(Some("true"), None));
    assert!(resolve_log_enabled(Some("True"), None));
    assert!(resolve_log_enabled(Some(" true "), None));
    assert!(!resolve_log_enabled(Some("false"), None));
    assert!(!resolve_log_enabled(Some("1"), None), "DB value must be exactly true");
    assert!(!resolve_log_enabled(Some("garbage"), None));
}

#[test]
fn resolve_log_enabled_env_overrides_db() {
    assert!(resolve_log_enabled(Some("false"), Some("1")));
    assert!(resolve_log_enabled(None, Some("true")));
    assert!(resolve_log_enabled(None, Some("YES")));
    assert!(resolve_log_enabled(None, Some("on")));
    assert!(!resolve_log_enabled(Some("true"), Some("0")));
    assert!(!resolve_log_enabled(None, Some("off")));
}

// -----------------------------------------------------------------------
// log_event: disabled → no write
// -----------------------------------------------------------------------

#[test]
fn log_event_disabled_skips_write() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let log_path = dir.path().join("disabled.log");
    let log_str = log_path.to_string_lossy().to_string();

    let (_lock, _state) = acquire_log_state(false, "debug");
    log_event(&log_str, "info", "should.not.appear", serde_json::json!({}))
        .expect("disabled log_event should return Ok");

    // File must not exist or be empty
    assert!(
        !log_path.exists() || std::fs::read_to_string(&log_path).unwrap().is_empty(),
        "log file should be empty when logging is disabled"
    );
}

// -----------------------------------------------------------------------
// log_event: level filtering
// -----------------------------------------------------------------------

#[test]
fn log_event_level_below_min_skipped() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let log_path = dir.path().join("below_min.log");
    let log_str = log_path.to_string_lossy().to_string();

    let (_lock, _state) = acquire_log_state(true, "warn");
    // "info" (1) < "warn" (2) — should be skipped
    log_event(&log_str, "info", "should.be.skipped", serde_json::json!({})).expect("ok");

    // Flush to ensure any buffered data is written
    flush_log();

    assert!(
        !log_path.exists() || std::fs::read_to_string(&log_path).unwrap().is_empty(),
        "info event should be filtered when min_level=warn"
    );
}

#[test]
fn log_event_level_at_min_written() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let log_path = dir.path().join("at_min.log");
    let log_str = log_path.to_string_lossy().to_string();

    let (_lock, _state) = acquire_log_state(true, "warn");
    log_event(&log_str, "warn", "at.min.level", serde_json::json!({})).expect("ok");
    flush_log();

    let content = std::fs::read_to_string(&log_path).expect("read log");
    assert!(
        content.contains("at.min.level"),
        "warn event should be written when min_level=warn"
    );
}

#[test]
fn log_event_level_above_min_written() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let log_path = dir.path().join("above_min.log");
    let log_str = log_path.to_string_lossy().to_string();

    let (_lock, _state) = acquire_log_state(true, "warn");
    log_event(&log_str, "error", "above.min.level", serde_json::json!({})).expect("ok");
    // error level is flushed immediately, but call flush_log for safety
    flush_log();

    let content = std::fs::read_to_string(&log_path).expect("read log");
    assert!(
        content.contains("above.min.level"),
        "error event should be written when min_level=warn"
    );
}

#[test]
fn log_event_debug_written_when_min_debug() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let log_path = dir.path().join("debug_min.log");
    let log_str = log_path.to_string_lossy().to_string();

    let (_lock, _state) = acquire_log_state(true, "debug");
    log_event(&log_str, "debug", "debug.event", serde_json::json!({})).expect("ok");
    flush_log();

    let content = std::fs::read_to_string(&log_path).expect("read log");
    assert!(
        content.contains("debug.event"),
        "debug event should be written when min_level=debug"
    );
}

#[test]
fn log_event_debug_skipped_when_min_info() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let log_path = dir.path().join("debug_skipped.log");
    let log_str = log_path.to_string_lossy().to_string();

    let (_lock, _state) = acquire_log_state(true, "info");
    log_event(&log_str, "debug", "debug.skipped", serde_json::json!({})).expect("ok");
    flush_log();

    assert!(
        !log_path.exists() || std::fs::read_to_string(&log_path).unwrap().is_empty(),
        "debug event should be filtered when min_level=info"
    );
}

// -----------------------------------------------------------------------
// Existing tests — now wrapped with the global lock so they are safe
// when other tests temporarily mutate LOG_ENABLED / LOG_MIN_LEVEL
// -----------------------------------------------------------------------

#[test]
fn session_token_prefix_short_token() {
    assert_eq!(session_token_prefix("abc"), "abc");
}

#[test]
fn session_token_prefix_long_token() {
    let token = "abcdefghijklmnopqrstuvwxyz";
    assert_eq!(session_token_prefix(token), "abcdefghijkl");
}

#[test]
fn session_token_prefix_exactly_12() {
    assert_eq!(session_token_prefix("123456789012"), "123456789012");
}

#[test]
fn snippet_short_string() {
    let s = "hello world";
    assert_eq!(snippet(s), "hello world");
}

#[test]
fn snippet_long_string_truncated() {
    let s = "x".repeat(300);
    let result = snippet(&s);
    assert!(result.ends_with("..."));
    assert!(result.len() < 230);
}

#[test]
fn mask_email_normal() {
    assert_eq!(mask_email("alice@example.com"), "a***e@example.com");
}

#[test]
fn mask_email_short_local() {
    assert_eq!(mask_email("ab@example.com"), "ab***@example.com");
}

#[test]
fn mask_email_single_char_local() {
    assert_eq!(mask_email("a@example.com"), "a***@example.com");
}

#[test]
fn unix_ts_returns_nonzero() {
    let ts = unix_ts();
    assert!(
        ts > 1_000_000_000,
        "timestamp should be recent epoch: {}",
        ts
    );
}

#[test]
fn log_event_writes_to_file() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let log_path = dir.path().join("test.log");
    let log_str = log_path.to_string_lossy().to_string();

    // Acquire lock + guarantee enabled state so this test is not affected
    // by other tests that temporarily disable logging.
    let (_lock, _state) = acquire_log_state(true, "info");
    log_event(
        &log_str,
        "info",
        "test.event",
        serde_json::json!({"key": "value"}),
    )
    .expect("log_event should succeed");
    flush_log();

    let content = std::fs::read_to_string(&log_path).expect("read log");
    assert!(content.contains("test.event"));
    assert!(content.contains("info"));
}

#[test]
fn log_rotation_creates_backup() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let log_path = dir.path().join("rotate.log");
    let log_str = log_path.to_string_lossy().to_string();

    let (_lock, _state) = acquire_log_state(true, "info");
    init_log_file(&log_str).expect("init");

    // Write enough data to trigger rotation (just over MAX_LOG_SIZE)
    // We simulate this by directly setting file_bytes high.
    {
        let mut guard = LOG_WRITER.lock().unwrap();
        if let Some(lw) = guard.as_mut() {
            lw.file_bytes = MAX_LOG_SIZE; // trigger rotation on next write
        }
    }

    log_event(&log_str, "error", "after.rotation", serde_json::json!({}))
        .expect("should rotate and write");
    flush_log();

    // Backup .1 should exist
    let backup1 = format!("{}.1", log_str);
    assert!(
        Path::new(&backup1).exists(),
        "backup .1 should exist after rotation"
    );

    // New log file should contain the post-rotation event
    let content = std::fs::read_to_string(&log_path).expect("read new log");
    assert!(content.contains("after.rotation"));
}

#[test]
fn error_level_flushes_immediately() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let log_path = dir.path().join("error_flush.log");
    let log_str = log_path.to_string_lossy().to_string();

    let (_lock, _state) = acquire_log_state(true, "info");
    log_event(&log_str, "error", "critical.error", serde_json::json!({})).expect("ok");

    // Should be flushed immediately without calling flush_log()
    let content = std::fs::read_to_string(&log_path).expect("read log");
    assert!(
        content.contains("critical.error"),
        "error events should be flushed immediately"
    );
}

// ---------------------------------------------------------------------------
// Central log redaction (BUG-LOG-02, v2.1.3)
// ---------------------------------------------------------------------------

#[test]
fn redact_value_for_log_redacts_sensitive_keys_at_any_depth() {
    let detail = serde_json::json!({
        "component": "mock",
        "nested": {
            "api_key": "sk-live-123",
            "meta": { "Authorization": "Bearer abc", "keep": "ok" }
        },
        "items": [
            { "token": "t1", "name": "n1" },
            { "name": "n2" }
        ],
        "count": 5
    });

    let redacted = redact_value_for_log(detail);

    assert_eq!(redacted["nested"]["api_key"], "[REDACTED]");
    assert_eq!(redacted["nested"]["meta"]["Authorization"], "[REDACTED]");
    assert_eq!(redacted["nested"]["meta"]["keep"], "ok");
    assert_eq!(redacted["items"][0]["token"], "[REDACTED]");
    assert_eq!(redacted["items"][0]["name"], "n1");
    assert_eq!(redacted["component"], "mock");
    assert_eq!(redacted["count"], 5);

    let rendered = redacted.to_string();
    assert!(!rendered.contains("sk-live-123"), "raw api key must not survive");
    assert!(!rendered.contains("Bearer abc"), "raw bearer token must not survive");
}

#[test]
fn redact_string_for_log_scrubs_bearer_and_url_userinfo() {
    let header = "Authorization header: Bearer eyJhbGciOi.9301 sent; retry later";
    let out = redact_string_for_log(header);
    assert!(out.contains("Bearer [REDACTED]"), "bearer token redacted: {out}");
    assert!(!out.contains("eyJhbGciOi.9301"));
    assert!(out.contains("retry later"), "surrounding text kept: {out}");

    let url = "fetch https://user:pass@example.com/path?token=abc&x=1 done";
    let out = redact_string_for_log(url);
    assert!(out.contains("https://[REDACTED]@example.com"), "userinfo redacted: {out}");
    assert!(out.contains("token=[REDACTED]"), "query token redacted: {out}");
    assert!(!out.contains("pass@"));
    assert!(out.contains("&x=1"));
    assert!(out.contains("done"));
}

#[test]
fn redact_string_for_log_handles_quoted_values() {
    let s = r#"config token="abc-secret" more"#;
    let out = redact_string_for_log(s);
    assert!(out.contains(r#"token="[REDACTED]""#), "quoted value redacted in place: {out}");
    assert!(!out.contains("abc-secret"));
    assert!(out.contains("more"));
}

#[test]
fn redact_string_for_log_preserves_plain_and_multibyte_strings() {
    for s in [
        "task 12 finished",
        "событие задача",
        "a/b?c=d",
        "no credentials here",
    ] {
        assert_eq!(redact_string_for_log(s), s);
    }
}

#[test]
fn log_event_persists_redacted_detail_only() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let log_path = dir.path().join("redact.log");
    let log_str = log_path.to_string_lossy().to_string();

    let (_lock, _state) = acquire_log_state(true, "info");
    log_event(
        &log_str,
        "info",
        "component.request",
        serde_json::json!({ "url": "https://k:key@host/x", "api_key": "raw-key", "ok": 1 }),
    )
    .expect("ok");
    // info-level events are buffered; flush before reading.
    flush_log();

    let content = std::fs::read_to_string(&log_path).expect("read log");
    assert!(content.contains("[REDACTED]"), "redaction marker present: {content}");
    assert!(!content.contains("raw-key"), "raw key must never hit disk");
    assert!(!content.contains("k:key@"), "url userinfo must never hit disk");
}

// ---------------------------------------------------------------------------
// Millisecond timestamps + file metadata header (audit 3.3, v2.1.4)
// ---------------------------------------------------------------------------

#[test]
fn unix_ts_ms_carries_millisecond_resolution() {
    let secs = unix_ts();
    let ms = unix_ts_ms();
    assert!(ms >= secs * 1000, "ts_ms must be at least ts seconds in ms: {ms} vs {secs}");
    assert!(ms < (secs + 5) * 1000, "ts_ms must stay in the same second window: {ms} vs {secs}");
}

#[test]
fn log_event_entries_carry_ts_ms_alongside_ts() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let log_path = dir.path().join("ts_ms.log");
    let log_str = log_path.to_string_lossy().to_string();

    let (_lock, _state) = acquire_log_state(true, "info");
    log_event(&log_str, "info", "ts.field.check", serde_json::json!({})).expect("ok");
    flush_log();

    let content = std::fs::read_to_string(&log_path).expect("read log");
    let entry_line = content
        .lines()
        .find(|l| l.contains("ts.field.check"))
        .expect("event line present");
    let entry: serde_json::Value = serde_json::from_str(entry_line).expect("line is JSON");
    let ts = entry["ts"].as_u64().expect("ts is u64 seconds");
    let ts_ms = entry["ts_ms"].as_u64().expect("ts_ms is u64 milliseconds");
    assert!(ts_ms >= ts * 1000 && ts_ms < (ts + 2) * 1000, "ts_ms consistent with ts");
}

#[test]
fn fresh_log_file_leads_with_metadata_header() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let log_path = dir.path().join("header.log");
    let log_str = log_path.to_string_lossy().to_string();

    let (_lock, _state) = acquire_log_state(true, "info");
    init_log_file(&log_str).expect("init");
    flush_log();

    let content = std::fs::read_to_string(&log_path).expect("read log");
    let first_line = content.lines().next().expect("header line present");
    let header: serde_json::Value =
        serde_json::from_str(first_line).expect("header must be a JSON line");
    assert_eq!(header["event"], "log_file_header");
    assert_eq!(header["level"], "info");
    assert_eq!(header["detail"]["version"], env!("CARGO_PKG_VERSION"));
    assert!(
        header["detail"]["os"].as_str().is_some_and(|s| !s.is_empty()),
        "os metadata present"
    );
    assert!(
        header["detail"]["arch"].as_str().is_some_and(|s| !s.is_empty()),
        "arch metadata present"
    );
    assert!(header["detail"]["pid"].as_u64().is_some(), "pid metadata present");
}

#[test]
fn header_is_not_duplicated_on_reopen() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let log_path = dir.path().join("header_once.log");
    let log_str = log_path.to_string_lossy().to_string();

    let (_lock, _state) = acquire_log_state(true, "info");
    init_log_file(&log_str).expect("init 1");
    init_log_file(&log_str).expect("init 2 (reopen)");
    flush_log();

    let content = std::fs::read_to_string(&log_path).expect("read log");
    let headers = content
        .lines()
        .filter(|l| l.contains("log_file_header"))
        .count();
    assert_eq!(headers, 1, "reopen on a non-empty file must not write another header");
}

#[test]
fn rotate_log_falls_back_to_truncate_when_rename_is_blocked() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let log_path = dir.path().join("rotate_blocked.log");
    let log_str = log_path.to_string_lossy().to_string();

    // Synthetic fixture content; no real credentials.
    std::fs::write(&log_path, "content-content-content\n").expect("seed log");

    // Block the current→.1 rename the way Windows viewers/AV do. The chain
    // shift must be unable to clear the .1 slot first:
    //   .1 = dir (non-empty) → final rename(file → dir) fails with
    //        EISDIR/ENOTEMPTY on Unix and ACCESS_DENIED on Windows. The dir
    //        must be NON-empty: an empty dir can be REPLACED by the file on
    //        Windows (MoveFileEx REPLACE_EXISTING), which consumed the live
    //        log file on the 3-OS CI matrix instead of forcing the fallback.
    //   .2 = file  → i=1 rename(dir .1 → file .2) fails, .1 stays pinned
    //   .3 = dir (non-empty) → i=2 rename(file .2 → dir .3) fails for the
    //        same replaceability reason — an EMPTY .3 let Windows shift the
    //        whole chain (.2→.3, .1→.2, live→.1) so the live path vanished
    //        instead of falling back.
    let pin_dir = format!("{}.1", log_str);
    std::fs::create_dir(&pin_dir).expect("dir at .1");
    std::fs::write(
        std::path::Path::new(&pin_dir).join("pin"),
        "pin",
    )
    .expect("non-empty pin inside .1");
    std::fs::write(format!("{}.2", log_str), "pin").expect("file pin at .2");
    let pin_dir3 = format!("{}.3", log_str);
    std::fs::create_dir(&pin_dir3).expect("dir at .3");
    std::fs::write(
        std::path::Path::new(&pin_dir3).join("pin"),
        "pin",
    )
    .expect("non-empty pin inside .3");

    // Must not error even though the rename path is blocked: the
    // copy+truncate fallback keeps size bounds enforceable.
    rotate_log(&log_str).expect("rotate falls back instead of failing");

    let truncated_len = std::fs::metadata(&log_path).map(|m| m.len()).unwrap_or(1);
    assert_eq!(truncated_len, 0, "fallback must truncate the live file");

    let _ = std::fs::remove_dir_all(format!("{}.1", log_str));
    let _ = std::fs::remove_file(format!("{}.2", log_str));
    let _ = std::fs::remove_dir_all(format!("{}.3", log_str));
}
