use serde::Serialize;
use serde_json::{json, Value};

use super::webui_proxy;

#[derive(Serialize)]
pub struct LogEntry {
    pub timestamp: String,
    pub level: String,
    pub message: String,
}

fn map_log_lines(data: &Value) -> Vec<LogEntry> {
    data.get("lines")
        .and_then(Value::as_array)
        .map(|lines| {
            lines
                .iter()
                .map(|line| {
                    // Lines are raw JSON-lines strings from the WebUI log
                    // file ({"ts","level","event","detail"}). Parse them so
                    // the panel shows the real level and timestamp instead
                    // of hardcoding "info" and dropping the time (BUG-LOG-01);
                    // non-JSON lines fall back to the legacy raw shape.
                    let raw = line
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| line.to_string());
                    match serde_json::from_str::<Value>(&raw) {
                        Ok(entry) if entry.is_object() => map_json_log_entry(entry),
                        _ => LogEntry {
                            timestamp: String::new(),
                            level: "info".to_string(),
                            message: raw,
                        },
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Map one parsed JSON log line to the frontend LogEntry shape.
fn map_json_log_entry(entry: Value) -> LogEntry {
    let timestamp = match entry.get("ts") {
        Some(serde_json::Value::Number(n)) => n.to_string(),
        Some(serde_json::Value::String(s)) => s.clone(),
        _ => String::new(),
    };
    let level = entry
        .get("level")
        .and_then(Value::as_str)
        .unwrap_or("info")
        .to_string();
    let event = entry.get("event").and_then(Value::as_str).unwrap_or("");
    let message = match entry.get("detail") {
        Some(detail) if !detail.is_null() => format!("{event} {detail}"),
        _ => event.to_string(),
    };
    let message = if message.trim().is_empty() {
        entry.to_string()
    } else {
        message
    };
    LogEntry {
        timestamp,
        level,
        message,
    }
}

/// Proxies `POST /api/logs/recent` on the embedded WebUI agent.
#[tauri::command]
pub async fn get_logs(limit: Option<u32>) -> Result<Vec<LogEntry>, String> {
    let body = json!({ "limit": limit.unwrap_or(200) });
    let data = webui_proxy::post("/api/logs/recent", body).await?;
    Ok(map_log_lines(&data))
}

/// Raw recent-logs payload for adapters that want the WebUI shape.
#[tauri::command]
pub async fn get_recent_logs(request: Value) -> Result<Value, String> {
    webui_proxy::post("/api/logs/recent", request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_recent_log_lines() {
        let data = json!({ "limit": 2, "lines": ["alpha", "beta"] });
        let logs = map_log_lines(&data);
        assert_eq!(logs.len(), 2);
        assert_eq!(logs[0].message, "alpha");
        assert_eq!(logs[1].message, "beta");
        // Non-JSON lines keep the legacy fallback shape.
        assert_eq!(logs[0].level, "info");
        assert_eq!(logs[0].timestamp, "");
    }

    #[test]
    fn parses_json_log_lines_with_level_and_timestamp() {
        let data = json!({
            "limit": 3,
            "lines": [
                "{\"ts\":1726036800,\"level\":\"error\",\"event\":\"task.failed\",\"detail\":{\"task_id\":7}}",
                "{\"ts\":1726036801,\"level\":\"warn\",\"event\":\"worker.retry\"}",
                "not json at all"
            ]
        });
        let logs = map_log_lines(&data);
        assert_eq!(logs.len(), 3);

        assert_eq!(logs[0].level, "error", "level must come from the JSON line, not a hardcoded info");
        assert_eq!(logs[0].timestamp, "1726036800");
        assert!(logs[0].message.contains("task.failed"));
        assert!(logs[0].message.contains("7"));

        assert_eq!(logs[1].level, "warn");
        assert_eq!(logs[1].timestamp, "1726036801");
        assert_eq!(logs[1].message, "worker.retry");

        // Fallback for malformed lines.
        assert_eq!(logs[2].level, "info");
        assert_eq!(logs[2].message, "not json at all");
    }

    #[test]
    fn message_falls_back_to_whole_entry_when_event_and_detail_are_empty() {
        let entry = serde_json::json!({"ts": 5, "level": "debug"});
        let mapped = map_json_log_entry(entry.clone());
        assert_eq!(mapped.level, "debug");
        assert_eq!(mapped.timestamp, "5");
        assert_eq!(mapped.message, entry.to_string());
    }
}
