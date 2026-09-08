use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::webui_proxy;

#[derive(Serialize)]
pub struct ComponentInfo {
    pub id: String,
    pub name: String,
    pub provider_type: String,
    pub configured: bool,
}

#[derive(Debug, Deserialize)]
pub struct ConfigureMockComponentRequest {
    #[serde(default = "default_mock_component_id")]
    pub id: String,
    #[serde(default = "default_mock_api_base")]
    pub api_base: String,
    #[serde(default = "default_mock_model")]
    pub model: String,
    #[serde(default = "default_mock_api_key")]
    pub api_key: String,
}

fn default_mock_component_id() -> String {
    "desktop-local-openai".to_string()
}

fn default_mock_api_base() -> String {
    "http://127.0.0.1:9090".to_string()
}

fn default_mock_model() -> String {
    "mock-openai-v1".to_string()
}

fn default_mock_api_key() -> String {
    "mock-translate-dev-key-2026".to_string()
}

const TEXT_BUSINESS_LINES: &[&str] = &[
    "custom_model",
    "post_content",
    "taxonomy_content",
    "plugin_i18n",
    "config_i18n",
    "theme_i18n",
];

pub(crate) fn map_component_info(value: &Value) -> ComponentInfo {
    ComponentInfo {
        id: value
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        name: value
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        provider_type: value
            .get("provider_type")
            .or_else(|| value.get("kind"))
            .or_else(|| value.get("vendor_name"))
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        configured: value
            .get("configured")
            .or_else(|| value.get("enabled"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
}

#[tauri::command]
pub async fn list_components() -> Result<Vec<ComponentInfo>, String> {
    let data = webui_proxy::get("/api/components/local").await?;
    Ok(data
        .get("items")
        .and_then(Value::as_array)
        .map(|items| items.iter().map(map_component_info).collect())
        .unwrap_or_default())
}

#[tauri::command]
pub async fn configure_component(id: String, config: serde_json::Value) -> Result<(), String> {
    let id = id.trim();
    if id.is_empty() {
        return Err("component id is required".into());
    }
    webui_proxy::put(&format!("/api/components/local/{id}"), config)
        .await
        .map(|_| ())
}

#[tauri::command]
pub async fn configure_mock_component(
    request: ConfigureMockComponentRequest,
) -> Result<ComponentInfo, String> {
    let id = request.id.trim();
    if id.is_empty() {
        return Err("component id is required".into());
    }
    let api_base = request.api_base.trim().trim_end_matches('/');
    if api_base.is_empty() {
        return Err("mock api_base is required".into());
    }
    let model = request.model.trim();
    if model.is_empty() {
        return Err("mock model is required".into());
    }
    let api_key = request.api_key.trim();
    if api_key.is_empty() {
        return Err("mock api_key is required".into());
    }

    let existing = webui_proxy::get("/api/components/local")
        .await?
        .get("items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .any(|item| item.get("id").and_then(Value::as_str) == Some(id))
        })
        .unwrap_or(false);

    let component_body = json!({
        "id": id,
        "name": "Desktop Local OpenAI Mock",
        "kind": "openai_compatible",
        "api_base": api_base,
        "model": model,
        "enabled": true,
        "remarks": "Created by Desktop real-user E2E / local Docker Lab smoke",
    });
    if existing {
        webui_proxy::put(&format!("/api/components/local/{id}"), component_body).await?;
    } else {
        webui_proxy::post("/api/components/local", component_body).await?;
    }

    webui_proxy::post(
        "/api/components/bindings/upsert",
        json!({
            "component_id": id,
            "auth": { "api_key": api_key },
        }),
    )
    .await?;

    webui_proxy::post(
        "/api/task-type-components/upsert",
        json!({ "task_type": "text", "component_id": id }),
    )
    .await?;
    for business_line in TEXT_BUSINESS_LINES {
        webui_proxy::post(
            "/api/task-type-components/upsert",
            json!({
                "task_type": "text",
                "business_line": business_line,
                "component_id": id,
            }),
        )
        .await?;
    }

    Ok(ComponentInfo {
        id: id.to_string(),
        name: "Desktop Local OpenAI Mock".to_string(),
        provider_type: "openai_compatible".to_string(),
        configured: true,
    })
}

#[tauri::command]
pub async fn quick_test_component(
    component_id: String,
    request: Option<Value>,
) -> Result<Value, String> {
    let component_id = component_id.trim();
    if component_id.is_empty() {
        return Err("component id is required".into());
    }
    let encoded = component_id
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect::<String>();
    webui_proxy::post(
        &format!("/api/components/local/{encoded}/quick-test"),
        request.unwrap_or_else(|| json!({})),
    )
    .await
}

#[tauri::command]
pub async fn create_component_version(
    component_id: String,
    request: Value,
) -> Result<Value, String> {
    let component_id = component_id.trim();
    if component_id.is_empty() {
        return Err("component id is required".into());
    }
    let encoded = component_id
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect::<String>();
    webui_proxy::post(
        &format!("/api/components/local/{encoded}/versions"),
        request,
    )
    .await
}

#[tauri::command]
pub async fn upsert_rule_component_binding(request: Value) -> Result<Value, String> {
    webui_proxy::post("/api/rule-component-bindings/upsert", request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_local_component_info() {
        let component = map_component_info(&json!({
            "id": "mock-openai",
            "name": "Mock OpenAI",
            "kind": "openai_compatible",
            "enabled": true
        }));
        assert_eq!(component.id, "mock-openai");
        assert_eq!(component.provider_type, "openai_compatible");
        assert!(component.configured);
    }
}
