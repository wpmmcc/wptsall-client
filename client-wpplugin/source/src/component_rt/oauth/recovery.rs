//! Token issue intent precedes egress. Unknown rotations are never reissued.
use super::OAuthTokenManager;
use anyhow::{ensure, Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, Weak};

type LocalClaims = std::collections::HashMap<String, Weak<tokio::sync::Mutex<()>>>;

async fn local_claim(key: String) -> tokio::sync::OwnedMutexGuard<()> {
    static CLAIMS: OnceLock<Mutex<LocalClaims>> = OnceLock::new();
    let owner = {
        let mut claims = CLAIMS
            .get_or_init(|| Mutex::new(LocalClaims::new()))
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        claims.retain(|_, owner| owner.strong_count() > 0);
        match claims.get(&key).and_then(Weak::upgrade) {
            Some(owner) => owner,
            None => {
                let owner = Arc::new(tokio::sync::Mutex::new(()));
                claims.insert(key, Arc::downgrade(&owner));
                owner
            }
        }
    };
    owner.lock_owned().await
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Checkpoint {
    format: String,
    binding: String,
    config_id: String,
    owner: String,
    state: String,
    refresh_inputs: Vec<Option<String>>,
    access_token: Option<String>,
    expires_at: i64,
    refresh_token: Option<String>,
}

enum Store {
    Database {
        db: Arc<tokio::sync::Mutex<Connection>>,
        key: String,
        _lease: crate::db::unit_lock::UnitLease,
        _profile: crate::db::unit_lock::UnitLease,
    },
    File {
        path: PathBuf,
        _lease: crate::bindings::native_lock::NativeLease,
    },
}

pub(super) struct TokenExecution {
    store: Store,
    raw: Option<String>,
    checkpoint: Checkpoint,
    _local: tokio::sync::OwnedMutexGuard<()>,
}

impl TokenExecution {
    fn identity(id: &str, config: &crate::types::OAuthConfig) -> Result<(String, String)> {
        let binding = crate::db::system::private_json_digest(&serde_json::json!({
            "grant":config.grant_type,"url":config.token_url,
            "client_id":config.client_id,"client_secret":config.client_secret,
            "scopes":config.scopes,"extra":config.extra_params,"auth_extra":config.auth_extra_params,
        }))?;
        let identity = crate::db::system::private_json_digest(
            &serde_json::json!({"id":id,"binding":binding}),
        )?;
        Ok((binding, identity))
    }

    pub(super) async fn has_checkpoint(
        manager: &OAuthTokenManager,
        id: &str,
        config: &crate::types::OAuthConfig,
    ) -> Result<bool> {
        let (_, identity) = Self::identity(id, config)?;
        let key = format!("oauth-token-checkpoint-v1:{identity}");
        if let Some(db) = &manager.recovery_db {
            let conn = db.lock().await;
            crate::db::vendor::assert_oauth_authorization_settled(&conn, id)?;
            return Ok(crate::db::system::get_system_config_checked(&conn, &key)?.is_some());
        }
        if let Some(path) = manager.db_path.as_deref() {
            return crate::db::with_read_only_db(path, |conn| {
                crate::db::vendor::assert_oauth_authorization_settled(conn, id)?;
                Ok(crate::db::system::get_system_config_checked(conn, &key)?.is_some())
            });
        }
        if manager.config_path.is_empty() {
            return Ok(false);
        }
        let configured = PathBuf::from(&manager.config_path);
        let name = configured
            .file_name()
            .context("OAuth configuration filename missing")?
            .to_string_lossy();
        let parent = configured
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| std::path::Path::new("."));
        match parent
            .join(format!("{name}.token-{identity}.checkpoint"))
            .symlink_metadata()
        {
            Ok(meta) => {
                ensure!(
                    meta.file_type().is_file(),
                    "OAuth checkpoint is not a regular file"
                );
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    pub(super) async fn acquire(
        manager: &OAuthTokenManager,
        id: &str,
        config: &crate::types::OAuthConfig,
    ) -> Result<Self> {
        let (binding, identity) = Self::identity(id, config)?;
        let db = match &manager.recovery_db {
            Some(db) => Some(db.clone()),
            None => manager
                .db_path
                .as_deref()
                .map(|path| {
                    crate::db::open_db(path).map(|conn| Arc::new(tokio::sync::Mutex::new(conn)))
                })
                .transpose()?,
        };
        let (store, local) = if let Some(db) = db {
            let location = match db.lock().await.path().filter(|path| !path.is_empty()) {
                Some(path) => std::path::Path::new(path)
                    .canonicalize()?
                    .display()
                    .to_string(),
                None => format!("memory:{}", Arc::as_ptr(&db) as usize),
            };
            let local = local_claim(format!("{location}:oauth:{identity}")).await;
            let profile = {
                let conn = db.lock().await;
                crate::db::vendor::assert_oauth_authorization_settled(&conn, id)?;
                crate::db::unit_lock::UnitLease::acquire(
                    &conn,
                    &db,
                    &crate::db::vendor::oauth_profile_lock_suffix(id)?,
                    "OAUTH_REFRESH_BUSY: original authorization or token issue is active",
                )?
            };
            let lease = crate::db::unit_lock::UnitLease::acquire(
                &*db.lock().await,
                &db,
                &format!("oauth-{identity}"),
                "OAUTH_REFRESH_BUSY: token issue is already active",
            )?;
            (
                Store::Database {
                    db,
                    key: format!("oauth-token-checkpoint-v1:{identity}"),
                    _lease: lease,
                    _profile: profile,
                },
                local,
            )
        } else {
            ensure!(
                !manager.config_path.is_empty(),
                "OAuth refresh requires durable recovery storage; retained"
            );
            let configured = PathBuf::from(&manager.config_path);
            let parent = configured
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or_else(|| std::path::Path::new("."));
            std::fs::create_dir_all(parent).context("create OAuth checkpoint directory")?;
            let parent = parent.canonicalize()?;
            let name = configured
                .file_name()
                .context("OAuth configuration filename missing")?
                .to_string_lossy();
            let path = parent.join(format!("{name}.token-{identity}.checkpoint"));
            let lock_path = parent.join(format!("{name}.token-{identity}.lock"));
            let local = local_claim(lock_path.display().to_string()).await;
            let lease = crate::bindings::native_lock::NativeLease::acquire(
                &lock_path,
                "OAUTH_REFRESH_BUSY: token issue is already active",
            )?;
            (
                Store::File {
                    path,
                    _lease: lease,
                },
                local,
            )
        };
        let raw = Self::read(&store).await?;
        let checkpoint = match &raw {
            Some(raw) => {
                ensure!(
                    raw.starts_with("V1BUQw"),
                    "OAuth checkpoint is not encrypted; retained"
                );
                let saved: Checkpoint =
                    serde_json::from_str(&crate::db::system::decrypt_config_value(raw)?)
                        .context("damaged OAuth checkpoint; retained")?;
                ensure!(
                    saved.format == "oauth-token-checkpoint-v1"
                        && saved.binding == binding
                        && saved.config_id == id
                        && uuid::Uuid::parse_str(&saved.owner)
                            .is_ok_and(|owner| !owner.is_nil() && owner.to_string() == saved.owner)
                        && matches!(saved.state.as_str(), "intent" | "ready")
                        && saved.refresh_inputs.contains(&config.refresh_token),
                    "OAuth checkpoint scope differs; retained"
                );
                ensure!(
                    saved.state == "ready",
                    "OAUTH_REFRESH_UNKNOWN: original token issue is unresolved; retained"
                );
                ensure!(
                    saved
                        .access_token
                        .as_ref()
                        .is_some_and(|token| !token.trim().is_empty()),
                    "OAuth checkpoint token is incomplete; retained"
                );
                saved
            }
            None => Checkpoint {
                format: "oauth-token-checkpoint-v1".into(),
                binding,
                config_id: id.into(),
                owner: uuid::Uuid::new_v4().to_string(),
                state: "ready".into(),
                refresh_inputs: vec![config.refresh_token.clone()],
                access_token: None,
                expires_at: 0,
                refresh_token: config.refresh_token.clone(),
            },
        };
        Ok(Self {
            store,
            raw,
            checkpoint,
            _local: local,
        })
    }

    async fn read(store: &Store) -> Result<Option<String>> {
        match store {
            Store::Database { db, key, _lease, _profile } => {
                _lease.assert_native_owner()?;
                _profile.assert_native_owner()?;
                crate::db::system::get_system_config_checked(&*db.lock().await, key)
            }
            Store::File { path, _lease } => {
                _lease.assert_owner()?;
                match path.symlink_metadata() {
                    Ok(meta) => ensure!(
                        meta.file_type().is_file(),
                        "OAuth checkpoint is not a regular file"
                    ),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                    Err(error) => return Err(error.into()),
                }
                let raw = std::fs::read_to_string(path)?;
                _lease.assert_owner()?;
                Ok(Some(raw))
            }
        }
    }

    pub(super) fn restore(&self, config: &mut crate::types::OAuthConfig) {
        if self.raw.is_some() {
            config.cached_token = self.checkpoint.access_token.clone();
            config.cached_token_expires_at = self.checkpoint.expires_at;
            config.refresh_token = self.checkpoint.refresh_token.clone();
        }
    }

    pub(super) fn check_projection(
        &self,
        conn: &Connection,
        id: &str,
        saved: &crate::types::OAuthConfig,
    ) -> Result<()> {
        let Store::Database { key, _lease, _profile, .. } = &self.store else {
            anyhow::bail!("OAuth token projection has no database owner; retained");
        };
        _lease.assert_native_owner()?;
        _profile.assert_native_owner()?;
        let (binding, _) = Self::identity(id, saved)?;
        ensure!(
            self.raw.is_some()
                && self.checkpoint.format == "oauth-token-checkpoint-v1"
                && self.checkpoint.state == "ready"
                && self.checkpoint.config_id == id
                && self.checkpoint.binding == binding
                && uuid::Uuid::parse_str(&self.checkpoint.owner).is_ok_and(|owner| {
                    !owner.is_nil() && owner.to_string() == self.checkpoint.owner
                })
                && saved
                    .cached_token
                    .as_ref()
                    .is_some_and(|token| !token.trim().is_empty())
                && self.checkpoint.access_token == saved.cached_token
                && self.checkpoint.expires_at == saved.cached_token_expires_at
                && self.checkpoint.refresh_token == saved.refresh_token
                && crate::db::system::get_system_config_checked(conn, key)? == self.raw,
            "OAuth token projection differs from original Ready; retained"
        );
        Ok(())
    }

    pub(super) fn projection_credit(
        &self,
        conn: &Connection,
        id: &str,
        saved: &crate::types::OAuthConfig,
    ) -> Result<Option<crate::storage_capacity::StorageCredit>> {
        self.check_projection(conn, id, saved)?;
        match conn.path().filter(|path| !path.is_empty()) {
            Some(path) => {
                crate::storage_capacity::database_recovery_credit(std::path::Path::new(path), false)
            }
            None => Ok(None),
        }
    }

    async fn save(&mut self, original_reply: bool) -> Result<()> {
        if original_reply {
            let prior: Checkpoint =
                serde_json::from_str(&crate::db::system::decrypt_config_value(
                    self.raw
                        .as_deref()
                        .context("OAuth reply has no saved original intent; retained")?,
                )?)?;
            ensure!(
                prior.format == "oauth-token-checkpoint-v1"
                    && prior.state == "intent"
                    && self.checkpoint.state == "ready"
                    && prior.binding == self.checkpoint.binding
                    && prior.config_id == self.checkpoint.config_id
                    && prior.owner == self.checkpoint.owner
                    && uuid::Uuid::parse_str(&prior.owner)
                        .is_ok_and(|owner| { !owner.is_nil() && owner.to_string() == prior.owner })
                    && self
                        .checkpoint
                        .refresh_inputs
                        .starts_with(&prior.refresh_inputs),
                "OAuth reply does not own the saved intent; retained"
            );
        }
        let next =
            crate::db::system::encrypt_config_value(&serde_json::to_string(&self.checkpoint)?)?;
        match &self.store {
            Store::Database { db, key, _lease, _profile } => {
                let mut conn = db.lock().await;
                _lease.assert_native_owner()?;
                _profile.assert_native_owner()?;
                let credit = match (original_reply, conn.path().filter(|path| !path.is_empty())) {
                    (true, Some(path)) => crate::storage_capacity::database_recovery_credit(
                        std::path::Path::new(path),
                        false,
                    )?,
                    _ => None,
                };
                crate::storage_capacity::with_database_credit(credit, || -> Result<()> {
                    let tx = conn.savepoint()?;
                    ensure!(
                        crate::db::system::get_system_config_checked(&tx, key)? == self.raw,
                        "OAuth owner changed; original checkpoint retained"
                    );
                    let changed = match &self.raw {
                        Some(previous) => tx.execute(
                            "UPDATE system_config SET value=?1 WHERE key=?2 AND value=?3",
                            params![next, key, previous],
                        )?,
                        None => tx.execute(
                            "INSERT INTO system_config(key,value) VALUES (?1,?2)",
                            params![key, next],
                        )?,
                    };
                    ensure!(
                        changed == 1
                            && crate::db::system::get_system_config_checked(&tx, key)?.as_deref()
                                == Some(next.as_str()),
                        "OAuth checkpoint was not committed; retained"
                    );
                    _lease.assert_native_owner()?;
                    _profile.assert_native_owner()?;
                    tx.commit()?;
                    Ok(())
                })?;
            }
            Store::File { path, _lease } => {
                ensure!(
                    Self::read(&self.store).await? == self.raw,
                    "OAuth owner changed; original checkpoint retained"
                );
                _lease.assert_owner()?;
                if original_reply {
                    let credit = crate::storage_capacity::file_recovery_credit(path, false)?;
                    crate::bindings::atomic_file::install_original_file_reply(path, next.as_bytes(), credit)?;
                } else if self.raw.is_some() {
                    crate::bindings::atomic_file::install_uncredited(path, next.as_bytes())?;
                } else {
                    crate::bindings::atomic_file::install_new_uncredited(path, next.as_bytes())?;
                }
                ensure!(
                    Self::read(&self.store).await?.as_deref() == Some(next.as_str()),
                    "OAuth checkpoint installation differs; retained"
                );
            }
        }
        self.raw = Some(next);
        Ok(())
    }

    pub(super) async fn intent(&mut self) -> Result<()> {
        if let Store::File { path, _lease } = &self.store {
            _lease.assert_owner()?;
            // Reserve the exact file's future reply under ordinary admission.
            // A previous Ready or ambient DB booking cannot finance a new issue.
            crate::storage_capacity::file_recovery_credit(path, true)?;
        }
        self.checkpoint.owner = uuid::Uuid::new_v4().to_string();
        self.checkpoint.state = "intent".into();
        self.save(false).await
    }

    pub(super) async fn finish(
        &mut self,
        token: &str,
        expires_at: i64,
        refresh: Option<String>,
    ) -> Result<()> {
        ensure!(
            !token.trim().is_empty(),
            "OAuth returned an empty access token; retained"
        );
        ensure!(
            refresh
                .as_ref()
                .is_none_or(|value| !value.trim().is_empty()),
            "OAuth returned an empty refresh token; retained"
        );
        if let Some(refresh) = refresh {
            self.checkpoint.refresh_token = Some(refresh);
        }
        if !self
            .checkpoint
            .refresh_inputs
            .contains(&self.checkpoint.refresh_token)
        {
            self.checkpoint
                .refresh_inputs
                .push(self.checkpoint.refresh_token.clone());
        }
        self.checkpoint.access_token = Some(token.into());
        self.checkpoint.expires_at = expires_at;
        self.checkpoint.state = "ready".into();
        self.save(true).await
    }
}
