use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use crate::bindings::{normalize_domain_base, validate_pairing_ends};
use crate::config::{sync_pairs_file, sync_peer_credentials_file, sync_state_file};
use crate::logging::unix_ts;
use crate::sync_engine::{
    build_translator, credential_status, delete_pair_everywhere, find_pair_in_doc,
    find_pair_in_doc_mut, find_peer_credential, load_peer_credentials, load_sync_pairs,
    pair_with_site, remove_peer_credential, sync_pair_run, update_sync_pairs, upsert_pair_in_doc,
    ConflictStrategy, PairingRole, PeerCredentialsDoc, SyncDirection, SyncFrequency, SyncMode,
    SyncPair, SyncPairStatus,
};
use crate::types::{PluginIdentity, WebUiState};

use super::errors::{err_public, write_error_response, write_error_response_with_status};
use super::http::write_http_response;

#[derive(Debug, Deserialize)]
pub(crate) struct SyncPairUpsertRequest {
    pub id: Option<String>,
    pub name: Option<String>,
    pub source_domain: String,
    pub target_domain: String,
    pub direction: Option<SyncDirection>,
    pub sync_mode: Option<SyncMode>,
    pub source_lang: Option<String>,
    pub target_lang: Option<String>,
    pub conflict_strategy: Option<ConflictStrategy>,
    pub sync_frequency: Option<SyncFrequency>,
    pub post_types: Option<Vec<String>>,
    pub status: Option<SyncPairStatus>,
    pub translate_component_id: Option<String>,
    #[serde(default)]
    pub field_actions: Option<Vec<crate::sync_engine::types::SyncFieldAction>>,
    #[serde(default)]
    pub review_before_push: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SyncPairDeleteRequest {
    pub id: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SyncPairPairRequest {
    pub domain: String,
    pub pairing_code: String,
    /// "source" (client pulls from this site) or "target" (client pushes to it).
    pub role: String,
    #[serde(default)]
    pub source_lang: Option<String>,
    #[serde(default)]
    pub target_lang: Option<String>,
    #[serde(default)]
    pub sync_mode: Option<String>,
    /// Canonical conflict strategy declared on the handshake (X-1,
    /// tasks/5.3falsh2/12 批 B). Absent → a stored pair covering the
    /// domain, else the protocol default `lww`.
    #[serde(default)]
    pub conflict_strategy: Option<ConflictStrategy>,
}

fn normalize_sync_domain(raw: &str) -> String {
    normalize_domain_base(raw.trim())
}

/// Runtime source of truth for domain bindings: the shared `WebUiState`
/// doc (kept fresh by the upsert/identity-test/worker handlers; persisted
/// to sqlite or file by those same handlers). Reading the JSON file
/// directly would miss every binding stored through the sqlite runtime
/// path — the exact bug the SIM-02/SIM-03 journeys caught.
async fn state_domain_token_bindings(
    state: &Arc<Mutex<WebUiState>>,
) -> crate::types::DomainTokenBindingsDoc {
    let guard = state.lock().await;
    guard.domain_token_bindings.clone()
}

async fn sync_storage_error(
    socket: &mut TcpStream,
    code: &str,
    error: &anyhow::Error,
) -> anyhow::Result<()> {
    write_error_response_with_status(
        socket,
        "500 Internal Server Error",
        code,
        &err_public(error),
    )
    .await
}

pub(super) async fn handle_sync_pairs_list(
    socket: &mut TcpStream,
    _state: &Arc<Mutex<WebUiState>>,
) -> anyhow::Result<()> {
    let path = sync_pairs_file();
    let doc = match load_sync_pairs(&path) {
        Ok(doc) => doc,
        Err(err) => return sync_storage_error(socket, "SYNC_PAIRS_READ_FAILED", &err).await,
    };

    // Credential status per paired domain (no secret material).
    let creds = load_peer_credentials(&sync_peer_credentials_file()).unwrap_or_default();
    let credentials: Vec<serde_json::Value> = creds.peers.iter().map(credential_status).collect();

    let payload = json!({
        "success": true,
        "data": {
            "schema_version": doc.schema_version,
            "pairs": doc.pairs,
            "credentials": credentials,
            "client_origin_uuid": creds.client_origin_uuid,
            "updated_at": doc.updated_at,
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

pub(super) async fn handle_sync_pairs_upsert(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
) -> anyhow::Result<()> {
    let req: SyncPairUpsertRequest = match serde_json::from_slice(body) {
        Ok(r) => r,
        Err(e) => {
            return write_error_response(
                socket,
                "INVALID_PAYLOAD",
                &format!("Invalid sync pair payload: {e}"),
            )
            .await;
        }
    };

    let source = normalize_sync_domain(&req.source_domain);
    let target = normalize_sync_domain(&req.target_domain);

    if source.is_empty() {
        return write_error_response(socket, "INVALID_SOURCE_DOMAIN", "源站点域名不能为空").await;
    }
    if target.is_empty() {
        return write_error_response(socket, "INVALID_TARGET_DOMAIN", "目标站点域名不能为空").await;
    }
    if source == target {
        return write_error_response(
            socket,
            "SELF_PAIRING_PROHIBITED",
            "源站点与目标站点不能相同",
        )
        .await;
    }

    // Verify both domains are bound in domain tokens (runtime source of
    // truth — sqlite-backed under the WebUI, file-backed otherwise).
    let bindings_doc = state_domain_token_bindings(state).await;

    let source_entry = bindings_doc.domains.get(&source);
    if source_entry.is_none() {
        return write_error_response(
            socket,
            "SOURCE_DOMAIN_NOT_BOUND",
            &format!("源站点 '{source}' 尚未在站点管理中绑定凭证"),
        )
        .await;
    }

    let target_entry = bindings_doc.domains.get(&target);
    if target_entry.is_none() {
        return write_error_response(
            socket,
            "TARGET_DOMAIN_NOT_BOUND",
            &format!("目标站点 '{target}' 尚未在站点管理中绑定凭证"),
        )
        .await;
    }

    // Identity Contract v1.1 §5: validate both ends for cross-site sync
    let source_identity = source_entry.and_then(|e| e.plugin_identity);
    let target_identity = target_entry.and_then(|e| e.plugin_identity);
    if let Err(rej) = validate_pairing_ends(source_identity, target_identity) {
        return write_error_response(socket, rej.code, &rej.message).await;
    }

    let sync_mode = req.sync_mode.unwrap_or_default();
    let translate_component_id = req
        .translate_component_id
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let field_actions = crate::sync_engine::field_plan::normalize_field_actions(
        req.field_actions.clone().unwrap_or_default(),
    );
    let component_required = if field_actions.is_empty() {
        sync_mode == SyncMode::SyncAndTranslate
    } else {
        field_actions.iter().any(|row| {
            row.action == "translate" && row.component_id.as_deref().unwrap_or("").trim().is_empty()
        }) && translate_component_id.is_none()
    };
    if component_required && translate_component_id.is_none() {
        return write_error_response(
            socket,
            "TRANSLATE_COMPONENT_REQUIRED",
            "级联翻译模式必须选择一个翻译组件",
        )
        .await;
    }

    let path = sync_pairs_file();
    let pair_id = req
        .id
        .filter(|id| !id.trim().is_empty())
        .unwrap_or_else(|| format!("pair-{}", uuid::Uuid::new_v4().simple()));

    let default_name = format!("{source} -> {target}");
    let name = req
        .name
        .filter(|n| !n.trim().is_empty())
        .unwrap_or(default_name);

    let pair = match update_sync_pairs(&path, |doc| {
        let existing = find_pair_in_doc(doc, &pair_id).cloned();
        let created_at = existing.as_ref().map(|p| p.created_at).unwrap_or(0);
        let last_sync_at = existing.as_ref().and_then(|p| p.last_sync_at);
        let last_seen_source_id = existing.as_ref().and_then(|p| p.last_seen_source_id);
        let last_sync_count = existing.as_ref().and_then(|p| p.last_sync_count);
        let last_error = existing.as_ref().and_then(|p| p.last_error.clone());
        let existing_review_before_push = existing
            .as_ref()
            .map(|p| p.review_before_push)
            .unwrap_or(false);

        let pair = SyncPair {
            id: pair_id,
            name,
            source_domain: source.to_string(),
            target_domain: target.to_string(),
            direction: req.direction.unwrap_or_default(),
            sync_mode,
            source_lang: req
                .source_lang
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "en_US".to_string()),
            target_lang: req
                .target_lang
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "zh_CN".to_string()),
            conflict_strategy: req.conflict_strategy.unwrap_or_default(),
            sync_frequency: req.sync_frequency.unwrap_or_default(),
            post_types: req.post_types.unwrap_or_else(|| vec!["post".to_string()]),
            status: req.status.unwrap_or_default(),
            last_sync_at,
            last_seen_source_id,
            last_sync_count,
            last_error,
            translate_component_id,
            field_actions: crate::sync_engine::field_plan::normalize_field_actions(
                req.field_actions.clone().unwrap_or_default(),
            ),
            review_before_push: req
                .review_before_push
                .unwrap_or(existing_review_before_push),
            created_at,
            updated_at: unix_ts(),
        };

        upsert_pair_in_doc(doc, pair.clone());
        Ok(pair)
    }) {
        Ok(pair) => pair,
        Err(err) => return sync_storage_error(socket, "SYNC_PAIRS_SAVE_FAILED", &err).await,
    };
    crate::logging::log_event_global(
        "info",
        "sync_pair.upserted",
        json!({ "pair_id": pair.id, "name": pair.name, "source_domain": pair.source_domain, "target_domain": pair.target_domain }),
    );

    let payload = json!({
        "success": true,
        "data": {
            "pair": pair
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

pub(super) async fn handle_sync_pairs_delete(
    socket: &mut TcpStream,
    _state: &Arc<Mutex<WebUiState>>,
    pair_id: &str,
) -> anyhow::Result<()> {
    // Removes the pair AND its incremental sync state (known-uuid map,
    // digest cursor) so a re-created pair starts clean.
    let deleted = match delete_pair_everywhere(pair_id) {
        Ok(deleted) => deleted,
        Err(err) => return sync_storage_error(socket, "SYNC_PAIRS_DELETE_FAILED", &err).await,
    };
    crate::logging::log_event_global(
        "warn",
        "sync_pair.deleted",
        json!({ "pair_id": pair_id, "deleted": deleted }),
    );

    let payload = json!({
        "success": true,
        "data": {
            "deleted": deleted,
            "id": pair_id
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

pub(super) async fn handle_sync_pairs_pause(
    socket: &mut TcpStream,
    _state: &Arc<Mutex<WebUiState>>,
    pair_id: &str,
) -> anyhow::Result<()> {
    let path = sync_pairs_file();
    let updated = update_sync_pairs(&path, |doc| {
        if let Some(pair) = find_pair_in_doc_mut(doc, pair_id) {
            pair.status = SyncPairStatus::Paused;
            pair.updated_at = unix_ts();
            let updated_pair = pair.clone();
            doc.updated_at = unix_ts();
            return Ok(Some(updated_pair));
        }
        Ok(None)
    });
    let updated = match updated {
        Ok(pair) => pair,
        Err(err) => return sync_storage_error(socket, "SYNC_PAIRS_SAVE_FAILED", &err).await,
    };
    if let Some(updated_pair) = updated {
        crate::logging::log_event_global("info", "sync_pair.paused", json!({ "pair_id": pair_id }));
        let payload = json!({
            "success": true,
            "data": {
                "pair": updated_pair
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

    write_error_response(socket, "PAIR_NOT_FOUND", "Sync pair not found").await
}

pub(super) async fn handle_sync_pairs_resume(
    socket: &mut TcpStream,
    _state: &Arc<Mutex<WebUiState>>,
    pair_id: &str,
) -> anyhow::Result<()> {
    let path = sync_pairs_file();
    let updated = update_sync_pairs(&path, |doc| {
        if let Some(pair) = find_pair_in_doc_mut(doc, pair_id) {
            pair.status = SyncPairStatus::Active;
            pair.updated_at = unix_ts();
            let updated_pair = pair.clone();
            doc.updated_at = unix_ts();
            return Ok(Some(updated_pair));
        }
        Ok(None)
    });
    let updated = match updated {
        Ok(pair) => pair,
        Err(err) => return sync_storage_error(socket, "SYNC_PAIRS_SAVE_FAILED", &err).await,
    };
    if let Some(updated_pair) = updated {
        crate::logging::log_event_global(
            "info",
            "sync_pair.resumed",
            json!({ "pair_id": pair_id }),
        );
        let payload = json!({
            "success": true,
            "data": {
                "pair": updated_pair
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

    write_error_response(socket, "PAIR_NOT_FOUND", "Sync pair not found").await
}

/// Real run trigger: validates preconditions, then spawns the engine run
/// in the background (the UI polls the pair list for the outcome). The
/// response is honest — `triggered: true` means a run was actually started.
pub(super) async fn handle_sync_pairs_run(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    pair_id: &str,
) -> anyhow::Result<()> {
    let path = sync_pairs_file();
    let doc = match load_sync_pairs(&path) {
        Ok(doc) => doc,
        Err(err) => return sync_storage_error(socket, "SYNC_PAIRS_READ_FAILED", &err).await,
    };
    let Some(pair) = find_pair_in_doc(&doc, pair_id).cloned() else {
        return write_error_response(socket, "PAIR_NOT_FOUND", "Sync pair not found").await;
    };
    if pair.status == SyncPairStatus::Paused {
        return write_error_response(socket, "PAIR_PAUSED", "同步对已暂停，请先恢复再运行").await;
    }
    if let Err(err) = crate::sync_engine::load_sync_state(&sync_state_file()) {
        return sync_storage_error(socket, "SYNC_STATE_READ_FAILED", &err).await;
    }
    if pair.review_before_push || pair.conflict_strategy == ConflictStrategy::ManualReview {
        if let Err(err) =
            crate::sync_engine::load_sync_review(&crate::sync_engine::sync_review_file())
        {
            return sync_storage_error(socket, "SYNC_REVIEW_READ_FAILED", &err).await;
        }
    }

    // Credential preflight: both ends must be paired before spawning.
    let credentials = load_peer_credentials(&sync_peer_credentials_file())?;
    if find_peer_credential(&credentials, &pair.source_domain).is_none() {
        return write_error_response(
            socket,
            "SOURCE_NOT_PAIRED",
            &format!("源站点 {} 尚未完成 WPMMCC 配对", pair.source_domain),
        )
        .await;
    }
    if find_peer_credential(&credentials, &pair.target_domain).is_none() {
        return write_error_response(
            socket,
            "TARGET_NOT_PAIRED",
            &format!("目标站点 {} 尚未完成 WPMMCC 配对", pair.target_domain),
        )
        .await;
    }
    if pair.sync_mode == SyncMode::SyncAndTranslate
        && pair
            .translate_component_id
            .as_deref()
            .unwrap_or("")
            .trim()
            .is_empty()
    {
        return write_error_response(
            socket,
            "TRANSLATE_COMPONENT_REQUIRED",
            "级联翻译模式必须选择一个翻译组件",
        )
        .await;
    }

    let (http_client, log_file) = {
        let guard = state.lock().await;
        (guard.http_client.clone(), crate::config::log_file_path())
    };

    // Fire-and-forget engine run; bookkeeping (last_sync_at / counts /
    // last_error) lands in sync-pairs.json when the run finishes.
    let pair_id_owned = pair.id.clone();
    let translate_component = pair.translate_component_id.clone();
    tokio::spawn(async move {
        let translator = match pair.sync_mode {
            SyncMode::SyncAndTranslate => {
                let component_id = translate_component.as_deref().unwrap_or("");
                match build_translator(&http_client, component_id, &log_file).await {
                    Ok(t) => Some(t),
                    Err(err) => {
                        let _ = crate::logging::log_event(
                            &log_file,
                            "error",
                            "sync_engine.run_trigger_translator_failed",
                            json!({ "pair_id": pair_id_owned, "error": format!("{err:#}") }),
                        );
                        // Surface as a pair-level error via the engine's
                        // own bookkeeping (it records the same condition).
                        None
                    }
                }
            }
            SyncMode::SyncOnly => None,
        };
        if let Err(err) = sync_pair_run(
            &http_client,
            &pair_id_owned,
            &credentials,
            translator.as_ref(),
            &log_file,
        )
        .await
        {
            let _ = crate::logging::log_event(
                &log_file,
                "error",
                "sync_engine.run_trigger_failed",
                json!({ "pair_id": pair_id_owned, "error": format!("{err:#}") }),
            );
        }
    });

    let payload = json!({
        "success": true,
        "data": {
            "pair_id": pair.id,
            "triggered": true,
            "async": true,
            "timestamp": unix_ts(),
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

/// Pair one bound site as a WPMMCC peer using a one-time pairing code from
/// the site's admin page. Derives + stores the HMAC shared secret.
pub(super) async fn handle_sync_pairs_pair(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
) -> anyhow::Result<()> {
    let req: SyncPairPairRequest = match serde_json::from_slice(body) {
        Ok(r) => r,
        Err(e) => {
            return write_error_response(
                socket,
                "INVALID_PAYLOAD",
                &format!("Invalid pairing payload: {e}"),
            )
            .await;
        }
    };

    let domain = normalize_sync_domain(&req.domain);
    if domain.is_empty() {
        return write_error_response(socket, "INVALID_DOMAIN", "站点域名不能为空").await;
    }

    let role = match req.role.trim() {
        "source" => PairingRole::Source,
        "target" => PairingRole::Target,
        other => {
            return write_error_response(
                socket,
                "INVALID_ROLE",
                &format!("无效的配对角色 '{other}'（应为 source 或 target）"),
            )
            .await;
        }
    };

    // The site must be bound AND identity-verified as the WPMMCC plugin
    // (runtime source of truth — sqlite-backed under the WebUI).
    let bindings_doc = state_domain_token_bindings(state).await;
    let entry = bindings_doc.domains.get(&domain);
    let identity = entry.and_then(|e| e.plugin_identity);
    if identity != Some(PluginIdentity::Wpmmcc) {
        return write_error_response(
            socket,
            "IDENTITY_MISMATCH",
            &format!(
                "站点 {} 未验证为 WPMMCC 插件（当前身份: {}），请先在站点管理中完成绑定与身份验证",
                domain,
                identity.map(|i| i.as_wire_str()).unwrap_or("未知")
            ),
        )
        .await;
    }

    let source_lang = req
        .source_lang
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "en_US".to_string());
    let target_lang = req
        .target_lang
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "zh_CN".to_string());
    let sync_mode = req
        .sync_mode
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "sync_only".to_string());

    // X-1 (tasks/5.3falsh2/12 批 B): the pairing-chosen conflict strategy
    // rides the handshake so the receiving site's arbiter honors it. When
    // the request omits it, fall back to a stored pair covering this
    // domain (re-pairing re-declares that pair's strategy), else the
    // protocol default lww (older receiving sites default peers to lww
    // too, so both ends agree).
    let pairs = match load_sync_pairs(&sync_pairs_file()) {
        Ok(doc) => doc,
        Err(err) => return sync_storage_error(socket, "SYNC_PAIRS_READ_FAILED", &err).await,
    };
    let conflict_strategy = req
        .conflict_strategy
        .unwrap_or_else(|| {
            let doc = &pairs;
            doc.pairs
                .iter()
                .find(|p| p.source_domain == domain || p.target_domain == domain)
                .map(|p| p.conflict_strategy)
                .unwrap_or_default()
        })
        .as_wire_str()
        .to_string();

    let http_client = {
        let guard = state.lock().await;
        guard.http_client.clone()
    };

    let credentials_path = sync_peer_credentials_file();
    match pair_with_site(
        &http_client,
        &credentials_path,
        &domain,
        &req.pairing_code,
        role,
        &source_lang,
        &target_lang,
        &sync_mode,
        &conflict_strategy,
    )
    .await
    {
        Ok(credential) => {
            crate::logging::log_event_global(
                "info",
                "sync_pair.paired",
                json!({ "target_domain": credential.domain, "peer_uuid": credential.peer_uuid, "paired_as": credential.paired_as }),
            );
            let payload = json!({
                "success": true,
                "data": {
                    "domain": credential.domain,
                    "peer_uuid": credential.peer_uuid,
                    "peer_name": credential.peer_name,
                    "key_scheme": credential.key_scheme,
                    "negotiated_direction": credential.negotiated_direction,
                    "conflict_strategy": conflict_strategy,
                    "paired_as": credential.paired_as,
                    "paired_at": credential.paired_at,
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
        Err(err) => write_error_response(socket, "PAIRING_FAILED", &format!("{err:#}")).await,
    }
}

/// Credential status for every stored peer (secret-free view).
pub(super) async fn handle_sync_pair_credentials_list(
    socket: &mut TcpStream,
    _state: &Arc<Mutex<WebUiState>>,
) -> anyhow::Result<()> {
    let creds = load_peer_credentials(&sync_peer_credentials_file())?;
    let items: Vec<serde_json::Value> = creds.peers.iter().map(credential_status).collect();
    let payload = json!({
        "success": true,
        "data": {
            "client_origin_uuid": creds.client_origin_uuid,
            "credentials": items,
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

/// Forget one domain's peer credential (local only; the site-side peer row
/// stays until the site admin removes it).
pub(super) async fn handle_sync_pair_credential_delete(
    socket: &mut TcpStream,
    _state: &Arc<Mutex<WebUiState>>,
    domain: &str,
) -> anyhow::Result<()> {
    let domain = normalize_sync_domain(domain);
    let removed = remove_peer_credential(&sync_peer_credentials_file(), &domain)?;
    if !removed {
        return write_error_response(
            socket,
            "CREDENTIAL_NOT_FOUND",
            &format!("站点 {domain} 没有已存储的配对凭证"),
        )
        .await;
    }
    crate::logging::log_event_global(
        "warn",
        "credential.peer_deleted",
        json!({ "domain": domain }),
    );
    let payload = json!({
        "success": true,
        "data": { "domain": domain, "deleted": true }
    });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}
