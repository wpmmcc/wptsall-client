use serde_json::{json, Value};

use super::webui_proxy;

fn encode_query_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn query_path(base: &str, pairs: &[(&str, Option<String>)]) -> String {
    let mut parts = Vec::new();
    for (key, value) in pairs {
        if let Some(v) = value {
            let trimmed = v.trim();
            if !trimmed.is_empty() {
                parts.push(format!("{key}={}", encode_query_component(trimmed)));
            }
        }
    }
    if parts.is_empty() {
        base.to_string()
    } else {
        format!("{base}?{}", parts.join("&"))
    }
}

/// Proxies `GET /api/translations` on the embedded WebUI agent (History page).
#[tauri::command]
pub async fn list_translations(
    page: Option<u32>,
    limit: Option<u32>,
    domain: Option<String>,
    status: Option<String>,
    search: Option<String>,
) -> Result<Value, String> {
    let path = query_path(
        "/api/translations",
        &[
            ("page", page.map(|v| v.to_string())),
            ("limit", limit.map(|v| v.to_string())),
            ("domain", domain),
            ("status", status),
            ("search", search),
        ],
    );
    webui_proxy::get(&path).await
}

#[tauri::command]
pub async fn retry_translation(id: String) -> Result<Value, String> {
    let id = id.trim();
    if id.is_empty() {
        return Err("translation id is required".into());
    }
    webui_proxy::post(&format!("/api/translations/{id}/retry"), Value::Null).await
}

#[tauri::command]
pub async fn batch_retry_translations(request: Value) -> Result<Value, String> {
    webui_proxy::post("/api/translations/batch-retry", request).await
}

#[tauri::command]
pub async fn batch_delete_translations(request: Value) -> Result<Value, String> {
    webui_proxy::post("/api/translations/batch-delete", request).await
}

#[tauri::command]
pub async fn update_translation(id: String, text: String) -> Result<(), String> {
    let item_id = id.trim();
    if item_id.is_empty() {
        return Err("item id is required".into());
    }
    webui_proxy::put(
        &format!("/api/items/{item_id}/translated"),
        json!({ "content": text }),
    )
    .await
    .map(|_| ())
}

#[tauri::command]
pub async fn list_jobs(
    domain: Option<String>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Value, String> {
    let path = query_path(
        "/api/jobs",
        &[
            ("domain", domain),
            ("limit", limit.map(|v| v.to_string())),
            ("offset", offset.map(|v| v.to_string())),
        ],
    );
    webui_proxy::get(&path).await
}

#[tauri::command]
pub async fn get_job(job_id: String) -> Result<Value, String> {
    let job_id = job_id.trim();
    if job_id.is_empty() {
        return Err("job id is required".into());
    }
    webui_proxy::get(&format!("/api/jobs/{job_id}")).await
}

#[tauri::command]
pub async fn list_job_items(job_id: String, status: Option<String>) -> Result<Value, String> {
    let job_id = job_id.trim();
    if job_id.is_empty() {
        return Err("job id is required".into());
    }
    let path = query_path(
        &format!("/api/jobs/{job_id}/items"),
        &[("status", status)],
    );
    webui_proxy::get(&path).await
}

#[tauri::command]
pub async fn get_item_content(item_id: String) -> Result<Value, String> {
    let item_id = item_id.trim();
    if item_id.is_empty() {
        return Err("item id is required".into());
    }
    webui_proxy::get(&format!("/api/items/{item_id}/content")).await
}

#[tauri::command]
pub async fn save_item_translated(item_id: String, request: Value) -> Result<Value, String> {
    let item_id = item_id.trim();
    if item_id.is_empty() {
        return Err("item id is required".into());
    }
    webui_proxy::put(&format!("/api/items/{item_id}/translated"), request).await
}

#[tauri::command]
pub async fn approve_item(item_id: String) -> Result<Value, String> {
    let item_id = item_id.trim();
    if item_id.is_empty() {
        return Err("item id is required".into());
    }
    webui_proxy::post(&format!("/api/items/{item_id}/approve"), Value::Null).await
}

#[tauri::command]
pub async fn resubmit_item(item_id: String) -> Result<Value, String> {
    let item_id = item_id.trim();
    if item_id.is_empty() {
        return Err("item id is required".into());
    }
    webui_proxy::post(&format!("/api/items/{item_id}/resubmit"), Value::Null).await
}

#[tauri::command]
pub async fn retranslate_item(item_id: String) -> Result<Value, String> {
    let item_id = item_id.trim();
    if item_id.is_empty() {
        return Err("item id is required".into());
    }
    webui_proxy::post(&format!("/api/items/{item_id}/retranslate"), Value::Null).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_job_query_paths() {
        assert_eq!(
            query_path(
                "/api/jobs",
                &[
                    ("domain", Some("https://example.test".into())),
                    ("limit", Some("20".into())),
                    ("offset", None),
                ]
            ),
            "/api/jobs?domain=https%3A%2F%2Fexample.test&limit=20"
        );
        assert_eq!(query_path("/api/jobs", &[]), "/api/jobs");
    }

    #[test]
    fn builds_translation_query_paths() {
        assert_eq!(
            query_path(
                "/api/translations",
                &[
                    ("page", Some("2".into())),
                    ("limit", Some("20".into())),
                    ("domain", Some("blog.example.test".into())),
                    ("status", Some("failed".into())),
                    ("search", None),
                ]
            ),
            "/api/translations?page=2&limit=20&domain=blog.example.test&status=failed"
        );
    }
}
