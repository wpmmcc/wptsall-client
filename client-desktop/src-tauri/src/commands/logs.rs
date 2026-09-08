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
                    let message = line
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| line.to_string());
                    LogEntry {
                        timestamp: String::new(),
                        level: "info".to_string(),
                        message,
                    }
                })
                .collect()
        })
        .unwrap_or_default()
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
    }
}
