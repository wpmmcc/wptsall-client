use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize)]
pub(crate) struct ApiResponse<T> {
    pub(crate) success: bool,
    pub(crate) data: T,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiErrorResponse {
    pub(crate) success: bool,
    pub(crate) error: ApiErrorBody,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiErrorBody {
    pub(crate) code: String,
    pub(crate) message: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LoginData {
    pub(crate) session_token: String,
    #[serde(default)]
    pub(crate) expires_in: i64,
    pub(crate) kicked_previous_session: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OAuthTokenData {
    pub(crate) session_token: String,
    pub(crate) expires_in: i64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ClientHeartbeatData {
    pub(crate) alive: bool,
    pub(crate) relogin_required: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DomainsData {
    pub(crate) items: Vec<DomainItem>,
}

pub(crate) fn parse_domains_data_value(domains_raw: Value) -> Result<DomainsData> {
    if let Ok(resp) = serde_json::from_value::<ApiResponse<DomainsData>>(domains_raw.clone()) {
        return Ok(resp.data);
    }
    if let Ok(data) = serde_json::from_value::<DomainsData>(domains_raw.clone()) {
        return Ok(data);
    }
    Err(anyhow!(
        "client domains: unsupported response shape ({})",
        domains_raw
    ))
}

#[derive(Debug, Deserialize)]
pub(crate) struct DomainItem {
    pub(crate) api_base_url: String,
    #[serde(alias = "license_status")]
    pub(crate) site_status: String,
    #[serde(default)]
    pub(crate) route_secret: Option<String>,
    #[serde(default)]
    pub(crate) max_relations: Option<u32>,
    #[serde(default)]
    pub(crate) plan_expires_at: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_domains_accepts_wrapped_shape() {
        let parsed = parse_domains_data_value(json!({
            "success": true,
            "data": {
                "items": [
                    {
                        "api_base_url": "https://blog.wpmm.cc/wp-json/wptsall/v2/client",
                        "license_status": "active",
                        "route_secret": "abc123"
                    }
                ]
            }
        }))
        .expect("wrapped domains payload should parse");

        assert_eq!(parsed.items.len(), 1);
        assert_eq!(parsed.items[0].site_status, "active");
    }

    #[test]
    fn parse_domains_accepts_plain_items_shape() {
        let parsed = parse_domains_data_value(json!({
            "items": [
                {
                    "api_base_url": "https://blog.wpmm.cc/wp-json/wptsall/v2/client",
                    "license_status": "active",
                    "route_secret": "abc123"
                }
            ]
        }))
        .expect("plain domains payload should parse");

        assert_eq!(parsed.items.len(), 1);
        assert_eq!(parsed.items[0].route_secret.as_deref(), Some("abc123"));
        assert_eq!(parsed.items[0].site_status, "active");
    }
}

#[derive(Debug, Serialize, Clone)]
pub(crate) struct DomainStatusItem {
    pub(crate) api_base_url: String,
    pub(crate) site_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) route_secret: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_relations: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) plan_expires_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TasksData {
    pub(crate) items: Vec<ClientTask>,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct WpClientPingData {
    #[serde(default)]
    pub(crate) sync_execution_mode: String,
    #[serde(default)]
    pub(crate) local_executor_enabled: bool,
    #[serde(default)]
    pub(crate) encryption_supported: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct ClientTask {
    pub(crate) task_id: i64,
    pub(crate) payload: Value,
    #[serde(default)]
    pub(crate) source_lang: Option<String>,
    #[serde(default)]
    pub(crate) target_lang: Option<String>,
    #[serde(default)]
    pub(crate) priority: Option<Value>,
    #[serde(default)]
    pub(crate) retry_count: Option<i32>,
    #[serde(default)]
    pub(crate) template: Option<String>,
    #[serde(default)]
    pub(crate) relation_id: Option<i64>,
    #[serde(default)]
    pub(crate) target_type: Option<String>,
    #[serde(default)]
    pub(crate) updated_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct StatusPayload<'a> {
    pub(crate) status: &'a str,
    pub(crate) progress: i32,
    pub(crate) message: &'a str,
}
