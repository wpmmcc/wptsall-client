use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::webui_proxy;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SiteInfo {
    pub id: String,
    pub domain: String,
    pub wp_url: String,
    pub connected: bool,
    pub languages: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AddSiteRequest {
    pub wp_url: String,
    pub token: String,
    #[serde(default)]
    pub route_secret: Option<String>,
}

#[derive(Deserialize)]
pub struct ImportSiteConnectionRequest {
    pub site_connection_pack: Value,
    #[serde(default)]
    pub pairing_code: Option<String>,
    #[serde(default)]
    pub device_label: Option<String>,
}

#[derive(Serialize)]
pub struct ImportedSiteConnection {
    pub api_base_url: String,
    pub token_prefix: String,
    pub token_len: usize,
    pub route_secret_set: bool,
    pub pairing_claimed: bool,
}

const CLIENT_ROUTE_MARKER: &str = "/wp-json/wptsall/v2/";

pub fn parse_site_binding_input(
    wp_url: &str,
    route_secret: Option<&str>,
) -> (String, String) {
    let trimmed = wp_url.trim().trim_end_matches('/');
    if let Some(index) = trimmed.find(CLIENT_ROUTE_MARKER) {
        let api_base_url = trimmed[..index].trim_end_matches('/').to_string();
        let rest = &trimmed[index + CLIENT_ROUTE_MARKER.len()..];
        let parsed_secret = rest
            .split('/')
            .next()
            .unwrap_or_default()
            .trim()
            .to_string();
        let final_secret = route_secret
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(&parsed_secret)
            .to_string();
        return (api_base_url, final_secret);
    }

    (
        trimmed.to_string(),
        route_secret
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or_default()
            .to_string(),
    )
}

fn site_from_domain(value: &Value) -> SiteInfo {
    let domain = value
        .get("api_base_url")
        .or_else(|| value.get("domain"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim_end_matches('/')
        .to_string();
    SiteInfo {
        id: domain.clone(),
        domain: domain.clone(),
        wp_url: domain,
        connected: value
            .get("connected")
            .and_then(Value::as_bool)
            .unwrap_or_else(|| {
                value
                    .get("site_status")
                    .and_then(Value::as_str)
                    .map(|status| status == "active")
                    .unwrap_or(false)
            }),
        languages: value
            .get("languages")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(ToString::to_string)
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn site_from_binding(value: &Value) -> SiteInfo {
    let domain = value
        .get("api_base_url")
        .or_else(|| value.get("domain"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim_end_matches('/')
        .to_string();
    SiteInfo {
        id: domain.clone(),
        domain: domain.clone(),
        wp_url: domain,
        connected: value
            .get("token_configured")
            .or_else(|| value.get("has_token"))
            .and_then(Value::as_bool)
            .unwrap_or(true),
        languages: Vec::new(),
    }
}

#[tauri::command]
pub async fn list_sites() -> Result<Vec<SiteInfo>, String> {
    let status = webui_proxy::get("/api/status").await?;
    let mut sites: Vec<SiteInfo> = status
        .get("domains")
        .and_then(Value::as_array)
        .map(|items| items.iter().map(site_from_domain).collect())
        .unwrap_or_default();

    if sites.is_empty() {
        sites = status
            .get("domain_token_bindings")
            .and_then(Value::as_array)
            .map(|items| items.iter().map(site_from_binding).collect())
            .unwrap_or_default();
    }
    Ok(sites)
}

#[tauri::command]
pub async fn add_site(request: AddSiteRequest) -> Result<SiteInfo, String> {
    let (api_base_url, route_secret) =
        parse_site_binding_input(&request.wp_url, request.route_secret.as_deref());
    if api_base_url.is_empty() {
        return Err("wp_url is required".into());
    }
    if request.token.trim().is_empty() {
        return Err("device-scoped WP client token is required".into());
    }
    if route_secret.is_empty() {
        return Err("route_secret is required; pass it explicitly or use the full /wp-json/wptsall/v2/<secret>/client URL".into());
    }

    webui_proxy::post(
        "/api/domain-tokens/upsert",
        json!({
            "api_base_url": api_base_url,
            "wp_client_token": request.token.trim(),
            "route_secret": route_secret,
        }),
    )
    .await?;

    let connected = test_connection(api_base_url.clone()).await.unwrap_or(false);
    Ok(SiteInfo {
        id: api_base_url.clone(),
        domain: api_base_url.clone(),
        wp_url: api_base_url,
        connected,
        languages: Vec::new(),
    })
}

#[tauri::command]
pub async fn import_site_connection(
    request: ImportSiteConnectionRequest,
) -> Result<ImportedSiteConnection, String> {
    let data = webui_proxy::post(
        "/api/site-connections/import",
        json!({
            "site_connection_pack": request.site_connection_pack,
            "pairing_code": request.pairing_code.unwrap_or_default(),
            "device_label": request.device_label.unwrap_or_default(),
        }),
    )
    .await?;
    Ok(ImportedSiteConnection {
        api_base_url: data
            .get("api_base_url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        token_prefix: data
            .get("token_prefix")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        token_len: data
            .get("token_len")
            .and_then(Value::as_u64)
            .unwrap_or_default() as usize,
        route_secret_set: data
            .get("route_secret_set")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        pairing_claimed: data
            .get("pairing_claimed")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

#[tauri::command]
pub async fn remove_site(site_id: String) -> Result<(), String> {
    webui_proxy::post(
        "/api/domain-tokens/delete",
        json!({ "api_base_url": site_id.trim() }),
    )
    .await
    .map(|_| ())
}

#[tauri::command]
pub async fn test_connection(site_id: String) -> Result<bool, String> {
    webui_proxy::post(
        "/api/domain-tokens/test",
        json!({ "api_base_url": site_id.trim() }),
    )
    .await
    .map(|_| true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_protocol_v2_client_url() {
        let (base, secret) = parse_site_binding_input(
            "http://127.0.0.1:9181/wp-json/wptsall/v2/abc123/client",
            None,
        );
        assert_eq!(base, "http://127.0.0.1:9181");
        assert_eq!(secret, "abc123");
    }

    #[test]
    fn explicit_route_secret_overrides_url_secret() {
        let (base, secret) = parse_site_binding_input(
            "https://example.test/wp-json/wptsall/v2/from-url/client",
            Some("from-form"),
        );
        assert_eq!(base, "https://example.test");
        assert_eq!(secret, "from-form");
    }
}
