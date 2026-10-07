use super::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Flow {
    format: String,
    oauth_state: String,
    config_id: String,
    config_path: String,
    config: OAuthConfig,
    owner: String,
    phase: String,
    verifier: String,
    redirect: String,
    authorize_url: String,
    expires_at: i64,
    code_digest: Option<String>,
    access_token: Option<String>,
    token_expires_at: i64,
    refresh_token: Option<String>,
}

pub(super) enum Outcome {
    Applied(i64),
    Rejected(u16),
    MissingToken,
    NetworkUnknown,
}



fn key(state: &str) -> anyhow::Result<String> {
    Ok(format!(
        "oauth-authorization-v1:{}",
        crate::db::system::private_json_digest(&json!({"state":state}))?
    ))
}

fn head_key(id: &str) -> anyhow::Result<String> {
    Ok(format!(
        "oauth-authorization-head-v1:{}",
        crate::db::system::private_json_digest(&json!({"id":id}))?
    ))
}

fn decode(raw: &str, state: &str) -> anyhow::Result<Flow> {
    anyhow::ensure!(
        raw.starts_with("V1BUQw"),
        "OAuth authorization checkpoint is not encrypted; retained"
    );
    let flow: Flow = serde_json::from_str(&crate::db::system::decrypt_config_value(raw)?)
        .context("damaged OAuth authorization checkpoint; retained")?;
    anyhow::ensure!(
        flow.format == "oauth-authorization-v1"
            && flow.oauth_state == state
            && !flow.config_id.is_empty()
            && !flow.redirect.is_empty()
            && (43..=128).contains(&flow.verifier.len())
            && uuid::Uuid::parse_str(&flow.owner)
                .is_ok_and(|owner| !owner.is_nil() && owner.to_string() == flow.owner)
            && matches!(
                flow.phase.as_str(),
                "pending" | "intent" | "ready" | "applied"
            ),
        "OAuth authorization checkpoint scope differs; retained"
    );
    anyhow::ensure!(
        (flow.phase == "pending" && flow.code_digest.is_none() && flow.access_token.is_none())
            || (flow.phase == "intent"
                && flow.code_digest.is_some()
                && flow.access_token.is_none())
            || (matches!(flow.phase.as_str(), "ready" | "applied")
                && flow.code_digest.is_some()
                && flow
                    .access_token
                    .as_ref()
                    .is_some_and(|token| !token.trim().is_empty())),
        "OAuth authorization checkpoint phase differs; retained"
    );
    Ok(flow)
}

fn load(conn: &rusqlite::Connection, state: &str) -> anyhow::Result<Option<(String, Flow)>> {
    crate::db::system::get_system_config_checked(conn, &key(state)?)?
        .map(|raw| Ok((raw.clone(), decode(&raw, state)?)))
        .transpose()
}

fn install(
    conn: &rusqlite::Connection,
    previous: Option<&str>,
    flow: &Flow,
) -> anyhow::Result<String> {
    let name = key(&flow.oauth_state)?;
    let next = crate::db::system::encrypt_config_value(&serde_json::to_string(flow)?)?;
    let changed = match previous {
        Some(previous) => conn.execute(
            "UPDATE system_config SET value=?1 WHERE key=?2 AND value=?3",
            rusqlite::params![next, name, previous],
        )?,
        None => conn.execute(
            "INSERT INTO system_config(key,value) VALUES (?1,?2)",
            rusqlite::params![name, next],
        )?,
    };
    anyhow::ensure!(
        changed == 1
            && crate::db::system::get_system_config_checked(conn, &name)?.as_deref()
                == Some(next.as_str()),
        "OAuth authorization checkpoint was not committed; retained"
    );
    Ok(next)
}

fn save(
    conn: &mut rusqlite::Connection,
    previous: Option<&str>,
    flow: &Flow,
) -> anyhow::Result<String> {
    crate::storage_capacity::with_database_credit(None, || {
        let tx = conn.savepoint()?;
        let next = install(&tx, previous, flow)?;
        tx.commit()?;
        Ok(next)
    })
}

fn head(conn: &rusqlite::Connection, flow: &Flow) -> anyhow::Result<String> {
    let raw = crate::db::system::get_system_config_checked(conn, &head_key(&flow.config_id)?)?
        .context("OAuth authorization head is absent; retained")?;
    anyhow::ensure!(raw.starts_with("V1BUQw"), "OAuth authorization head is not encrypted; retained");
    let state: String = serde_json::from_str(&crate::db::system::decrypt_config_value(&raw)?)?;
    anyhow::ensure!(state == flow.oauth_state, "OAuth authorization head changed; retained");
    Ok(raw)
}

struct Authority<'a> {
    previous: &'a str,
    flow: &'a Flow,
    head: &'a str,
    lease: &'a crate::db::unit_lock::UnitLease,
    database: Option<std::path::PathBuf>,
}

impl Authority<'_> {
    fn check(&self, conn: &rusqlite::Connection) -> anyhow::Result<()> {
        self.lease.assert_native_owner()?;
        let database = conn.path().filter(|path| !path.is_empty())
            .map(std::fs::canonicalize).transpose()?;
        anyhow::ensure!(
            database == self.database
                && load(conn, &self.flow.oauth_state)?.as_ref().map(|(raw, _)| raw.as_str())
                    == Some(self.previous)
                && head(conn, self.flow)? == self.head
                && canonical_path(&vendor_oauth_path())? == self.flow.config_path
                && crate::db::system::get_system_config_checked(
                    conn, "integration-save-v1:vendor_oauth_doc",
                )?.is_none(),
            "OAuth original authorization authority changed; retained"
        );
        Ok(())
    }

    fn credit(&self, conn: &rusqlite::Connection) -> anyhow::Result<Option<crate::storage_capacity::StorageCredit>> {
        self.check(conn)?;
        match &self.database {
            Some(path) => crate::storage_capacity::database_recovery_credit(path, false),
            None => Ok(None),
        }
    }

    fn finish(&self, conn: &mut rusqlite::Connection, next: &Flow) -> anyhow::Result<String> {
        let prior = self.flow;
        anyhow::ensure!(
            matches!((prior.phase.as_str(), next.phase.as_str()), ("intent", "ready") | ("ready", "applied"))
                && prior.oauth_state == next.oauth_state
                && prior.config_id == next.config_id
                && prior.config_path == next.config_path
                && prior.owner == next.owner
                && prior.code_digest == next.code_digest
                && prior.verifier == next.verifier
                && prior.redirect == next.redirect
                && prior.authorize_url == next.authorize_url
                && prior.expires_at == next.expires_at
                && crate::component_rt::oauth::OAuthTokenManager::credential_fingerprint(&prior.config)?
                    == crate::component_rt::oauth::OAuthTokenManager::credential_fingerprint(&next.config)?
                && next.access_token.as_ref().is_some_and(|value| !value.trim().is_empty())
                && next.token_expires_at > 0
                && (prior.phase != "ready" || (
                    prior.access_token == next.access_token
                        && prior.token_expires_at == next.token_expires_at
                        && prior.refresh_token == next.refresh_token
                )),
            "OAuth original reply does not own the saved intent; retained"
        );
        let credit = self.credit(conn)?;
        crate::storage_capacity::with_database_credit(credit, || {
            let tx = conn.savepoint()?;
            self.check(&tx)?;
            let raw = install(&tx, Some(self.previous), next)?;
            self.lease.assert_native_owner()?;
            anyhow::ensure!(head(&tx, next)? == self.head, "OAuth authorization head changed; retained");
            tx.commit()?;
            Ok(raw)
        })
    }
}

impl crate::component_rt::oauth::OAuthProjection for Authority<'_> {
    fn check_projection(&self, conn: &rusqlite::Connection, id: &str, saved: &OAuthConfig) -> anyhow::Result<()> {
        self.check(conn)?;
        anyhow::ensure!(
            matches!(self.flow.phase.as_str(), "ready" | "applied")
                && self.flow.config_id == id
                && self.flow.access_token == saved.cached_token
                && self.flow.token_expires_at == saved.cached_token_expires_at
                && saved.refresh_token == self.flow.refresh_token.clone().or_else(|| self.flow.config.refresh_token.clone())
                && crate::component_rt::oauth::OAuthTokenManager::credential_fingerprint(saved)?
                    == crate::component_rt::oauth::OAuthTokenManager::credential_fingerprint(&self.flow.config)?,
            "OAuth authorization projection differs from original Ready; retained"
        );
        Ok(())
    }
}

fn canonical_path(path: &str) -> anyhow::Result<String> {
    Ok(std::fs::canonicalize(path)?
        .to_str()
        .context("OAuth path is not UTF-8")?
        .into())
}

pub(super) async fn start(
    state: &Arc<Mutex<WebUiState>>,
    id: &str,
    config: &OAuthConfig,
    oauth_state: &str,
    verifier: &str,
    redirect: &str,
    authorize_url: &str,
) -> anyhow::Result<(String, String)> {
    let path = canonical_path(&vendor_oauth_path())?;
    let db = state.lock().await.db.clone();
    let mut conn = db.lock().await;
    let head_name = head_key(id)?;
    let _lease = crate::db::unit_lock::UnitLease::acquire(
        &conn,
        &db,
        &head_name.replace(':', "-"),
        "OAUTH_AUTHORIZATION_BUSY: original token issue is active",
    )?;
    crate::storage_capacity::with_database_credit(None, || {
    let tx = conn.savepoint()?;
    if let Some(raw) = crate::db::system::get_system_config_checked(&tx, &head_name)? {
        anyhow::ensure!(
            raw.starts_with("V1BUQw"),
            "OAuth authorization head is not encrypted; retained"
        );
        let prior_state: String =
            serde_json::from_str(&crate::db::system::decrypt_config_value(&raw)?)?;
        let (_, prior) = load(&tx, &prior_state)?
            .context("OAuth authorization head has no checkpoint; retained")?;
        anyhow::ensure!(
            prior.config_id == id && prior.config_path == path,
            "OAuth authorization head scope differs; retained"
        );
        if prior.phase == "pending" && unix_ts() as i64 <= prior.expires_at {
            anyhow::ensure!(
                crate::db::system::private_json_digest(&serde_json::to_value(&prior.config)?)?
                    == crate::db::system::private_json_digest(&serde_json::to_value(config)?)?,
                "OAuth pending operator configuration changed; original authorization retained"
            );
            return Ok((prior.authorize_url, prior.redirect));
        }
        anyhow::ensure!(
            prior.phase == "applied" || prior.phase == "pending",
            "OAUTH_AUTHORIZATION_UNKNOWN: original token issue is unresolved; retained"
        );
    }
    let flow = Flow {
        format: "oauth-authorization-v1".into(),
        oauth_state: oauth_state.into(),
        config_id: id.into(),
        config_path: path,
        config: config.clone(),
        owner: uuid::Uuid::new_v4().to_string(),
        phase: "pending".into(),
        verifier: verifier.into(),
        redirect: redirect.into(),
        authorize_url: authorize_url.into(),
        expires_at: (unix_ts() as i64)
            .checked_add(600)
            .context("OAuth expiry overflow")?,
        code_digest: None,
        access_token: None,
        token_expires_at: 0,
        refresh_token: None,
    };
    install(&tx, None, &flow)?;
    let head = crate::db::system::encrypt_config_value(&serde_json::to_string(oauth_state)?)?;
    let changed = tx.execute(
        "INSERT INTO system_config(key,value) VALUES (?1,?2)
        ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        rusqlite::params![head_name, head],
    )?;
    anyhow::ensure!(
        changed == 1
            && crate::db::system::get_system_config_checked(&tx, &head_name)?.as_deref()
                == Some(head.as_str()),
        "OAuth authorization head was not committed; checkpoint retained"
    );
    tx.commit()?;
    Ok((authorize_url.into(), redirect.into()))
    })
}

pub(super) async fn complete(
    state: &Arc<Mutex<WebUiState>>,
    oauth_state: &str,
    code: &str,
) -> anyhow::Result<Outcome> {
    let db = state.lock().await.db.clone();
    let (mut previous, mut flow, lease, original_head, database) = {
        let conn = db.lock().await;
        let (previous, flow) =
            load(&conn, oauth_state)?.context("OAuth authorization state is unknown; retained")?;
        let suffix = head_key(&flow.config_id)?.replace(':', "-");
        let lease = crate::db::unit_lock::UnitLease::acquire(
            &conn,
            &db,
            &suffix,
            "OAUTH_AUTHORIZATION_BUSY: original token issue is active",
        )?;
        let original_head = head(&conn, &flow)?;
        let database = conn.path().filter(|path| !path.is_empty())
            .map(std::fs::canonicalize).transpose()?;
        (previous, flow, lease, original_head, database)
    };
    let code_digest = crate::db::system::private_json_digest(&json!({"code":code}))?;
    if let Some(saved) = &flow.code_digest {
        anyhow::ensure!(
            saved == &code_digest,
            "OAuth authorization code differs; retained"
        );
    }
    Authority { previous: &previous, flow: &flow, head: &original_head, lease: &lease, database: database.clone() }
        .check(&*db.lock().await)?;
    anyhow::ensure!(
        canonical_path(&vendor_oauth_path())? == flow.config_path,
        "OAuth authorization configuration path changed; retained"
    );
    anyhow::ensure!(
        flow.phase != "intent",
        "OAUTH_AUTHORIZATION_UNKNOWN: original token issue is unresolved; retained"
    );
    if flow.phase == "pending" {
        anyhow::ensure!(
            unix_ts() as i64 <= flow.expires_at,
            "OAuth authorization expired; retained"
        );
        let doc: VendorOAuthDoc = load_config(state, &flow.config_path, "vendor_oauth_doc").await?;
        let current = doc
            .configs
            .get(&flow.config_id)
            .context("OAuth operator profile was deleted; retained")?;
        anyhow::ensure!(
            crate::db::system::private_json_digest(&serde_json::to_value(current)?)?
                == crate::db::system::private_json_digest(&serde_json::to_value(&flow.config)?)?,
            "OAuth operator configuration changed before exchange; retained"
        );
        use crate::component_rt::oauth::{
            OAuthHttpClient, OAuthTokenManager, OAuthTokenResponseFault,
        };
        OAuthTokenManager::validate_token_request(&flow.config)?;
        let manager = OAuthTokenManager::from_snapshot(HashMap::new(), OAuthHttpClient::direct()?);
        let form = HashMap::from([
            ("grant_type", "authorization_code".into()),
            ("code", code.into()),
            ("redirect_uri", flow.redirect.clone()),
            ("client_id", flow.config.client_id.clone()),
            ("client_secret", flow.config.client_secret.clone()),
            ("code_verifier", flow.verifier.clone()),
        ]);
        flow.owner = uuid::Uuid::new_v4().to_string();
        flow.phase = "intent".into();
        flow.code_digest = Some(code_digest);
        previous = save(&mut *db.lock().await, Some(&previous), &flow)?;
        let intent = flow.clone();
        state.lock().await.vendor_oauth_pending.remove(oauth_state);
        let (token, lifetime, refresh_token) = match manager.request_token(&flow.config, form).await
        {
            Ok(receipt) => receipt,
            Err(error) => {
                match error.downcast_ref::<OAuthTokenResponseFault>() {
                    Some(OAuthTokenResponseFault::Rejected(status)) => {
                        return Ok(Outcome::Rejected(*status));
                    }
                    Some(OAuthTokenResponseFault::MissingToken) => {
                        return Ok(Outcome::MissingToken);
                    }
                    None => {}
                }
                ;
                return Ok(Outcome::NetworkUnknown);
            }
        };
        flow.access_token = Some(token);
        flow.token_expires_at = (unix_ts() as i64)
            .checked_add(lifetime)
            .context("OAuth token lifetime overflow")?;
        flow.refresh_token = refresh_token;
        flow.phase = "ready".into();
        previous = Authority {
            previous: &previous, flow: &intent, head: &original_head, lease: &lease, database: database.clone(),
        }.finish(&mut *db.lock().await, &flow)?;
        ;
    }
    let mut saved = flow.config.clone();
    saved.cached_token = flow.access_token.clone();
    saved.cached_token_expires_at = flow.token_expires_at;
    if flow.refresh_token.is_some() {
        saved.refresh_token = flow.refresh_token.clone();
    }
    let manager = crate::component_rt::oauth::OAuthTokenManager::new(
        HashMap::from([(flow.config_id.clone(), flow.config.clone())]),
        crate::component_rt::oauth::OAuthHttpClient::direct()?,
        flow.config_path.clone(),
    )
    .with_recovery_db(db.clone());
    let authority = Authority {
        previous: &previous, flow: &flow, head: &original_head, lease: &lease, database: database.clone(),
    };
    manager
        .project_authorization(&flow.config_id, &flow.config, &saved, &authority, flow.phase == "applied")
        .await?;
    if flow.phase != "applied" {
        let mut applied = flow.clone();
        applied.phase = "applied".into();
        authority.finish(&mut *db.lock().await, &applied)?;
    }
    Ok(Outcome::Applied(
        flow.token_expires_at
            .saturating_sub(unix_ts() as i64)
            .max(0),
    ))
}
