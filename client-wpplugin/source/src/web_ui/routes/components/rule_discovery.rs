use serde::Serialize;
use serde_json::json;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::time::{timeout, Duration};

use std::collections::HashSet;
use std::sync::Arc;

use crate::logging::snippet;
use crate::web_ui::fetch_domains_for_session;

use super::super::errors::{maybe_write_upstream_api_error, write_session_required};
use super::super::http::write_http_response;

const RULE_DISCOVERY_HTTP_TIMEOUT_SECS: u64 = 12;
const RULE_DISCOVERY_MAX_RELATIONS_PER_DOMAIN: usize = 8;

#[derive(Debug, Serialize)]
struct RuleDiscoveryFieldSummary {
    field_name: String,
    content_format: String,
    source_role: String,
    storage: String,
    required_slot_key: String,
    suggested_task_type: String,
}

#[derive(Debug, Serialize)]
struct RuleDiscoveryItem {
    api_base_url: String,
    relation_id: i64,
    source_lang: String,
    target_lang: String,
    plugin_slug: String,
    plugin_name: String,
    rule_id: i64,
    rule_name: String,
    data_type: String,
    object_name: String,
    business_line: String,
    source_group: String,
    routing_profile: String,
    delivery_target: String,
    required_component_slots: Vec<String>,
    required_content_formats: Vec<String>,
    fields: Vec<RuleDiscoveryFieldSummary>,
}

#[derive(Debug, Serialize)]
struct RuleDiscoveryIssue {
    api_base_url: String,
    relation_id: Option<i64>,
    stage: String,
    error: String,
}

#[derive(Debug, Serialize)]
struct RuleDiscoverySummary {
    domains_checked: usize,
    relations_checked: usize,
    rules_checked: usize,
    fields_checked: usize,
    issues: usize,
}

#[derive(Debug, Serialize)]
struct RuleDiscoveryResponse {
    summary: RuleDiscoverySummary,
    items: Vec<RuleDiscoveryItem>,
    issues: Vec<RuleDiscoveryIssue>,
}

pub(crate) async fn handle_rule_component_binding_discovery(
    socket: &mut TcpStream,
    state: &Arc<Mutex<crate::types::WebUiState>>,
) -> anyhow::Result<()> {
    let (server_base, session_token, device_id, domains_snapshot, domain_token_bindings, client) = {
        let guard = state.lock().await;
        (
            guard.server_base.clone(),
            guard.session_token.clone(),
            guard.device_id.clone(),
            guard.domains.clone(),
            guard.domain_token_bindings.clone(),
            guard.http_client.clone(),
        )
    };

    let domains = if !domains_snapshot.is_empty() {
        domains_snapshot
    } else if !crate::config::server_control_plane_enabled() {
        // P0-LF-03 5.4: local mode discovers domains from the LOCAL
        // domain-token bindings only; the website is never contacted and
        // an empty local site list is an empty success with an explicit
        // local configuration issue (never SESSION_REQUIRED).
        let local_domains =
            crate::bindings::domain_token_binding_local_sites(&domain_token_bindings);
        if local_domains.is_empty() {
            let issues = vec![RuleDiscoveryIssue {
                api_base_url: String::new(),
                relation_id: None,
                stage: "local_config".to_string(),
                error: "no local sites configured; add a site with its WP client token and route secret in local mode"
                    .to_string(),
            }];
            let payload = json!({
                "success": true,
                "data": RuleDiscoveryResponse {
                    summary: RuleDiscoverySummary {
                        domains_checked: 0,
                        relations_checked: 0,
                        rules_checked: 0,
                        fields_checked: 0,
                        issues: 1,
                    },
                    items: Vec::new(),
                    issues,
                }
            });
            return write_http_response(
                socket,
                "200 OK",
                "application/json",
                &serde_json::to_vec(&payload)?,
            )
            .await;
        }
        local_domains
    } else {
        let Some(session_token) = session_token.as_deref() else {
            return write_session_required(socket).await;
        };
        match fetch_domains_for_session(&client, &server_base, session_token).await {
            Ok(items) => items,
            Err(err) => {
                if let Some(result) = maybe_write_upstream_api_error(socket, &err).await {
                    return result;
                }
                return Err(err);
            }
        }
    };

    let worker_id = crate::worker::build_worker_config(&device_id).worker_id;
    let wp_client_token_fallback = crate::config::env_or("WPTSALL_WP_CLIENT_TOKEN", "");
    let occupied_domain_bases: HashSet<String> = domains
        .iter()
        .filter(|d| d.site_status != "local_dev")
        .map(|d| crate::bindings::normalize_domain_base(&d.api_base_url))
        .filter(|value| !value.is_empty())
        .collect();

    let mut summary = RuleDiscoverySummary {
        domains_checked: 0,
        relations_checked: 0,
        rules_checked: 0,
        fields_checked: 0,
        issues: 0,
    };
    let mut items: Vec<RuleDiscoveryItem> = Vec::new();
    let mut issues: Vec<RuleDiscoveryIssue> = Vec::new();

    for domain in &domains {
        let is_local_dev = domain.site_status == "local_dev";
        // Free users still proceed — do NOT skip non-active domains.

        let (domain_api_base, wp_client_token, route_secret) = if is_local_dev {
            let Some((local_base, token, secret)) = crate::bindings::resolve_local_dev_binding(
                &domain_token_bindings,
                &occupied_domain_bases,
            ) else {
                continue;
            };
            (local_base, token, secret)
        } else {
            let Some(token) = crate::bindings::resolve_wp_client_token_for_domain(
                &domain.api_base_url,
                &domain_token_bindings,
                &wp_client_token_fallback,
            ) else {
                continue;
            };
            let route_secret = crate::bindings::resolve_route_secret_for_domain(
                &domain.api_base_url,
                &domain_token_bindings,
            )
            .or_else(|| domain.route_secret.clone());
            (domain.api_base_url.clone(), token, route_secret)
        };

        let domain_base = crate::bindings::normalize_domain_base(&domain_api_base);
        let Some(wp_base) = route_secret
            .as_deref()
            .and_then(|secret| crate::bindings::build_wp_base_url(&domain_base, secret))
        else {
            continue;
        };

        let relations_url = format!("{}/site-relations", wp_base);
        let mut relations = match timeout(
            Duration::from_secs(RULE_DISCOVERY_HTTP_TIMEOUT_SECS),
            crate::auth::wp_get_json_with_transport_and_secret::<crate::types::RelationsResponse>(
                &client,
                &relations_url,
                &wp_client_token,
                &worker_id,
                route_secret.as_deref(),
            ),
        )
        .await
        {
            Ok(Ok(relations)) => relations.relations,
            Ok(Err(err)) => {
                issues.push(RuleDiscoveryIssue {
                    api_base_url: domain_api_base.clone(),
                    relation_id: None,
                    stage: "fetch_relations".to_string(),
                    error: snippet(&format!("{:#}", err)),
                });
                continue;
            }
            Err(_) => {
                issues.push(RuleDiscoveryIssue {
                    api_base_url: domain_api_base.clone(),
                    relation_id: None,
                    stage: "fetch_relations_timeout".to_string(),
                    error: format!("timed out after {}s", RULE_DISCOVERY_HTTP_TIMEOUT_SECS),
                });
                continue;
            }
        };

        let relation_limit = domain
            .max_relations
            .and_then(|value| {
                if value > 0 {
                    Some(value as usize)
                } else {
                    None
                }
            })
            .unwrap_or(RULE_DISCOVERY_MAX_RELATIONS_PER_DOMAIN)
            .min(RULE_DISCOVERY_MAX_RELATIONS_PER_DOMAIN);
        if relations.len() > relation_limit {
            issues.push(RuleDiscoveryIssue {
                api_base_url: domain_api_base.clone(),
                relation_id: None,
                stage: "relations_truncated".to_string(),
                error: format!(
                    "truncated relations from {} to {} for UI responsiveness",
                    relations.len(),
                    relation_limit
                ),
            });
            relations.truncate(relation_limit);
        }

        summary.domains_checked += 1;

        for relation in relations {
            summary.relations_checked += 1;
            let rules_url = format!("{}/rules?relation_id={}", wp_base, relation.id);
            let rules = match timeout(
                Duration::from_secs(RULE_DISCOVERY_HTTP_TIMEOUT_SECS),
                crate::auth::wp_get_json_with_transport_and_secret::<crate::types::RulesResponse>(
                    &client,
                    &rules_url,
                    &wp_client_token,
                    &worker_id,
                    route_secret.as_deref(),
                ),
            )
            .await
            {
                Ok(Ok(rules)) => rules.rules,
                Ok(Err(err)) => {
                    issues.push(RuleDiscoveryIssue {
                        api_base_url: domain_api_base.clone(),
                        relation_id: Some(relation.id),
                        stage: "fetch_rules".to_string(),
                        error: snippet(&format!("{:#}", err)),
                    });
                    continue;
                }
                Err(_) => {
                    issues.push(RuleDiscoveryIssue {
                        api_base_url: domain_api_base.clone(),
                        relation_id: Some(relation.id),
                        stage: "fetch_rules_timeout".to_string(),
                        error: format!("timed out after {}s", RULE_DISCOVERY_HTTP_TIMEOUT_SECS),
                    });
                    continue;
                }
            };

            for rule in &rules {
                summary.rules_checked += 1;
                let translate_fields = if !rule.translate_fields.is_empty() {
                    rule.translate_fields.clone()
                } else {
                    crate::task_engine::pipeline::extract_translate_fields(&rule.field_capabilities)
                };
                let model = relation
                    .models
                    .iter()
                    .find(|item| item.model_id == rule.model_id);
                let mut fields: Vec<RuleDiscoveryFieldSummary> = Vec::new();

                for field_name in translate_fields {
                    let raw_content_format =
                        super::super::worker::resolve_preflight_field_content_format(
                            rule,
                            &field_name,
                        )
                        .unwrap_or_else(|| "plain_text".to_string());
                    let content_format = super::super::worker::normalize_preflight_content_format(
                        &raw_content_format,
                    );
                    let source_role = super::super::worker::resolve_preflight_field_source_role(
                        rule,
                        &field_name,
                    );
                    let selection = super::super::worker::selection_requirements_for_field(
                        rule,
                        &field_name,
                        &content_format,
                    );
                    let storage = rule
                        .field_storage_map
                        .get(&field_name)
                        .map(|value| value.trim().to_string())
                        .filter(|value| !value.is_empty())
                        .unwrap_or_else(|| "-".to_string());

                    summary.fields_checked += 1;
                    fields.push(RuleDiscoveryFieldSummary {
                        field_name,
                        content_format,
                        source_role,
                        storage,
                        required_slot_key: selection.required_slot_key,
                        suggested_task_type: selection.task_type.to_string(),
                    });
                }

                fields.sort_by(|a, b| a.field_name.cmp(&b.field_name));

                items.push(RuleDiscoveryItem {
                    api_base_url: domain_api_base.clone(),
                    relation_id: relation.id,
                    source_lang: relation.source_lang.clone(),
                    target_lang: relation.target_lang.clone(),
                    plugin_slug: model
                        .map(|item| item.plugin_slug.trim().to_string())
                        .filter(|value| !value.is_empty())
                        .unwrap_or_default(),
                    plugin_name: model
                        .map(|item| item.plugin_name.trim().to_string())
                        .filter(|value| !value.is_empty())
                        .unwrap_or_default(),
                    rule_id: rule.id,
                    rule_name: rule.name.trim().to_string(),
                    data_type: rule.data_type.trim().to_string(),
                    object_name: rule.object_name.trim().to_string(),
                    business_line: super::super::worker::preflight_business_line_for_rule(rule)
                        .to_string(),
                    source_group: super::super::worker::normalize_preflight_label(
                        &rule.source_group,
                    ),
                    routing_profile: super::super::worker::normalize_preflight_label(
                        &rule.routing_profile,
                    ),
                    delivery_target: super::super::worker::normalize_preflight_label(
                        &rule.delivery_target,
                    ),
                    required_component_slots: rule.required_component_slots.clone(),
                    required_content_formats: rule.required_content_formats.clone(),
                    fields,
                });
            }
        }
    }

    summary.issues = issues.len();
    items.sort_by(|a, b| {
        a.api_base_url
            .cmp(&b.api_base_url)
            .then_with(|| a.relation_id.cmp(&b.relation_id))
            .then_with(|| a.rule_id.cmp(&b.rule_id))
    });

    let payload = json!({
        "success": true,
        "data": RuleDiscoveryResponse {
            summary,
            items,
            issues,
        }
    });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}
