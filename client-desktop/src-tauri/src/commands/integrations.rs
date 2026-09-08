use serde_json::{json, Value};

use super::webui_proxy;

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct InstallCatalogTemplateRequest {
    pub entry_id: String,
    pub local_id: Option<String>,
    pub name: Option<String>,
    pub vendor_name: Option<String>,
    pub remarks: Option<String>,
    pub overwrite: Option<bool>,
}

fn require_object(request: Value, name: &str) -> Result<Value, String> {
    if request.is_object() {
        Ok(request)
    } else {
        Err(format!("{name} request must be a JSON object"))
    }
}

#[tauri::command]
pub async fn list_provider_catalog() -> Result<Value, String> {
    webui_proxy::get("/api/provider-catalog").await
}

#[tauri::command]
pub async fn refresh_provider_catalog() -> Result<Value, String> {
    webui_proxy::post("/api/provider-catalog/refresh", json!({})).await
}

#[tauri::command]
pub async fn install_catalog_template(
    request: InstallCatalogTemplateRequest,
) -> Result<Value, String> {
    let value = serde_json::to_value(request).map_err(|error| error.to_string())?;
    webui_proxy::post("/api/components/local/install-from-catalog", value).await
}

#[tauri::command]
pub async fn export_integration_pack(request: Value) -> Result<Value, String> {
    webui_proxy::post(
        "/api/integrations/pack/export",
        require_object(request, "export integration pack")?,
    )
    .await
}

#[tauri::command]
pub async fn preview_integration_pack(request: Value) -> Result<Value, String> {
    webui_proxy::post(
        "/api/integrations/pack/preview",
        require_object(request, "preview integration pack")?,
    )
    .await
}

#[tauri::command]
pub async fn import_integration_pack(request: Value) -> Result<Value, String> {
    webui_proxy::post(
        "/api/integrations/pack/import",
        require_object(request, "import integration pack")?,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_object_pack_requests() {
        let error = require_object(Value::String("bad".into()), "pack").unwrap_err();
        assert!(error.contains("JSON object"));
    }
}
