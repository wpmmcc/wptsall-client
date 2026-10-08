use anyhow::{anyhow, Context};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::{Mutex, Notify};

use crate::types::{KeySelectionStrategy, OAuthConfig};

mod recovery;

pub(crate) trait OAuthProjection: Sync {
    fn check_projection(
        &self,
        conn: &rusqlite::Connection,
        id: &str,
        saved: &OAuthConfig,
    ) -> anyhow::Result<()>;

    fn projection_credit(
        &self,
        conn: &rusqlite::Connection,
        id: &str,
        saved: &OAuthConfig,
    ) -> anyhow::Result<Option<crate::storage_capacity::StorageCredit>> {
        self.check_projection(conn, id, saved)?;
        match conn.path().filter(|path| !path.is_empty()) {
            Some(path) => crate::storage_capacity::database_recovery_credit(
                std::path::Path::new(path),
                false,
            ),
            None => Ok(None),
        }
    }
}

impl OAuthProjection for recovery::TokenExecution {
    fn check_projection(
        &self,
        conn: &rusqlite::Connection,
        id: &str,
        saved: &OAuthConfig,
    ) -> anyhow::Result<()> {
        recovery::TokenExecution::check_projection(self, conn, id, saved)
    }
}

/// Token transports cannot be constructed from an opaque provider/WP client.
#[derive(Debug, Clone)]
pub(crate) struct OAuthHttpClient {
    client: reqwest::Client,
    proxy_profile_id: Option<String>,
}

impl OAuthHttpClient {
    fn builder() -> reqwest::ClientBuilder {
        reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(15))
            .connect_timeout(std::time::Duration::from_secs(10))
    }

    pub(crate) fn direct() -> anyhow::Result<Self> {
        Ok(Self {
            client: Self::builder()
                .build()
                .context("build OAuth transport failed")?,
            proxy_profile_id: None,
        })
    }

    pub(in crate::component_rt) fn proxied(
        profile_id: &str,
        proxy: reqwest::Proxy,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(!profile_id.is_empty(), "OAuth proxy identity is missing");
        Ok(Self {
            client: Self::builder()
                .proxy(proxy)
                .build()
                .context("build proxied OAuth transport failed")?,
            proxy_profile_id: Some(profile_id.to_string()),
        })
    }
}

#[derive(Debug)]
pub(crate) enum OAuthTokenResponseFault {
    Rejected(u16),
    MissingToken,
}

impl std::fmt::Display for OAuthTokenResponseFault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected(status) => write!(
                formatter,
                "OAuth token request failed (status={status}); retained"
            ),
            Self::MissingToken => write!(
                formatter,
                "OAuth response has no usable access_token; retained"
            ),
        }
    }
}

impl std::error::Error for OAuthTokenResponseFault {}

#[derive(Debug, Clone)]
pub(crate) struct OAuthTokenManager {
    configs: Arc<Mutex<HashMap<String, OAuthConfig>>>,
    http_client: OAuthHttpClient,
    config_path: String,
    db_path: Option<String>,
    recovery_db: Option<Arc<tokio::sync::Mutex<rusqlite::Connection>>>,
    frozen: bool,
}

// ---------------------------------------------------------------------------
// OAuthPool — selects an OAuth config from a per-component pool, fetches its
// token, and holds a concurrency slot for the duration of the translation.
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub(crate) struct OAuthPoolEntry {
    pub(crate) config_id: String,
    /// Auth context key to inject the token into (e.g. "access_token" → auth.access_token).
    pub(crate) token_field: String,
    pub(crate) max_concurrent: usize,
    pub(crate) max_input_chars: usize,
    /// Maximum file size in MB for non-text translations (0.0 = unlimited).
    pub(crate) max_file_size_mb: f64,
    pub(crate) weight: u32,
    pub(crate) active_count: Arc<AtomicUsize>,
}

#[derive(Debug)]
pub(crate) struct OAuthPool {
    entries: Vec<OAuthPoolEntry>,
    strategy: KeySelectionStrategy,
    counter: AtomicUsize,
    notify: Arc<Notify>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OAuthPoolSnapshot {
    entries: Vec<(String, String, usize, usize, f64, u32)>,
    strategy: KeySelectionStrategy,
}

/// RAII guard that releases the concurrency slot when dropped.
#[derive(Debug)]
#[allow(dead_code)]
pub(crate) struct OAuthGuard {
    /// The OAuth access token ready for injection.
    pub(crate) access_token: String,
    /// Which auth.* field to inject the token into.
    pub(crate) token_field: String,
    /// Max file size in MB for non-text translations (0.0 = unlimited).
    pub(crate) max_file_size_mb: f64,
    active_count: Arc<AtomicUsize>,
    notify: Arc<Notify>,
}

impl Drop for OAuthGuard {
    fn drop(&mut self) {
        self.active_count.fetch_sub(1, Ordering::Release);
        self.notify.notify_waiters();
    }
}

impl OAuthPool {
    pub(crate) fn snapshot(&self) -> OAuthPoolSnapshot {
        OAuthPoolSnapshot {
            entries: self
                .entries
                .iter()
                .map(|entry| {
                    (
                        entry.config_id.clone(),
                        entry.token_field.clone(),
                        entry.max_concurrent,
                        entry.max_input_chars,
                        entry.max_file_size_mb,
                        entry.weight,
                    )
                })
                .collect(),
            strategy: self.strategy.clone(),
        }
    }

    pub(crate) fn from_snapshot(snapshot: OAuthPoolSnapshot) -> Self {
        Self::new_with_registry(
            snapshot
                .entries
                .into_iter()
                .map(
                    |(
                        config_id,
                        token_field,
                        max_concurrent,
                        max_input_chars,
                        max_file_size_mb,
                        weight,
                    )| OAuthPoolEntry {
                        config_id,
                        token_field,
                        max_concurrent,
                        max_input_chars,
                        max_file_size_mb,
                        weight,
                        active_count: Arc::new(AtomicUsize::new(0)),
                    },
                )
                .collect(),
            snapshot.strategy,
        )
    }

    pub(crate) fn new_with_registry(
        mut entries: Vec<OAuthPoolEntry>,
        strategy: KeySelectionStrategy,
    ) -> Self {
        let registry = crate::component_rt::key_registry::GlobalKeyRegistry::process();
        for entry in &mut entries {
            entry.active_count = registry.get_or_create(&format!("oauth:{}", entry.config_id));
        }
        let mut pool = Self::new(entries, strategy);
        pool.notify = registry.notify();
        pool
    }

    pub(crate) fn new(entries: Vec<OAuthPoolEntry>, strategy: KeySelectionStrategy) -> Self {
        Self {
            entries,
            strategy,
            counter: AtomicUsize::new(0),
            notify: Arc::new(Notify::new()),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns true if any entry in the pool has a non-zero `max_file_size_mb` limit.
    pub(crate) fn has_file_size_limits(&self) -> bool {
        self.entries.iter().any(|e| e.max_file_size_mb > 0.0)
    }

    pub(crate) fn source_byte_budget(&self) -> anyhow::Result<u64> {
        crate::component_rt::file_limits::pool_source_budget(
            self.entries.iter().map(|entry| entry.max_file_size_mb),
        )
    }

    /// Returns true if at least one entry can handle the given input size and file size.
    pub(crate) fn has_eligible(&self, input_len: usize, file_size_mb: f64) -> bool {
        self.entries.iter().any(|e| {
            (input_len == 0 || e.max_input_chars == 0 || input_len <= e.max_input_chars)
                && (file_size_mb == 0.0
                    || e.max_file_size_mb == 0.0
                    || file_size_mb <= e.max_file_size_mb)
        })
    }

    /// Select an entry from the pool (respecting concurrency limits, max_input_chars, and
    /// max_file_size_mb), fetch the corresponding OAuth token via `manager`, and return
    /// an `OAuthGuard`.
    pub(crate) async fn select(
        &self,
        manager: &OAuthTokenManager,
        input_len: usize,
        file_size_mb: f64,
    ) -> anyhow::Result<OAuthGuard> {
        if self.entries.is_empty() {
            return Err(anyhow!("oauth pool is empty"));
        }
        if !self.has_eligible(input_len, file_size_mb) {
            return Err(anyhow!(
                "no eligible oauth entry for input_len={} file_size_mb={:.1}",
                input_len,
                file_size_mb
            ));
        }
        loop {
            let released = self.notify.notified();
            tokio::pin!(released);
            released.as_mut().enable();
            if let Some(guard) = self.try_acquire(input_len, file_size_mb) {
                let config_id = guard.config_id_tmp;
                // Hold the RAII slot before the fallible/cancellable token lookup.
                let mut guard = OAuthGuard {
                    access_token: String::new(),
                    token_field: guard.token_field_tmp,
                    max_file_size_mb: guard.max_file_size_mb_tmp,
                    active_count: guard.active_count,
                    notify: guard.notify,
                };
                guard.access_token = manager.get_token(&config_id).await?;
                return Ok(guard);
            }
            released.await;
        }
    }

    fn try_acquire(&self, input_len: usize, file_size_mb: f64) -> Option<OAuthAcquireTemp> {
        match self.strategy {
            KeySelectionStrategy::Random => self.try_acquire_random(input_len, file_size_mb),
            KeySelectionStrategy::RoundRobin => {
                self.try_acquire_round_robin(input_len, file_size_mb)
            }
            KeySelectionStrategy::Weighted => self.try_acquire_weighted(input_len, file_size_mb),
        }
    }

    fn try_acquire_for_idx(
        &self,
        idx: usize,
        input_len: usize,
        file_size_mb: f64,
    ) -> Option<OAuthAcquireTemp> {
        let entry = &self.entries[idx];
        if entry.max_input_chars > 0 && input_len > entry.max_input_chars {
            return None;
        }
        if file_size_mb > 0.0
            && entry.max_file_size_mb > 0.0
            && file_size_mb > entry.max_file_size_mb
        {
            return None;
        }
        loop {
            let current = entry.active_count.load(Ordering::Acquire);
            if entry.max_concurrent > 0 && current >= entry.max_concurrent {
                return None;
            }
            if entry
                .active_count
                .compare_exchange(current, current + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Some(OAuthAcquireTemp {
                    config_id_tmp: entry.config_id.clone(),
                    token_field_tmp: entry.token_field.clone(),
                    max_file_size_mb_tmp: entry.max_file_size_mb,
                    active_count: entry.active_count.clone(),
                    notify: self.notify.clone(),
                });
            }
        }
    }

    fn try_acquire_round_robin(
        &self,
        input_len: usize,
        file_size_mb: f64,
    ) -> Option<OAuthAcquireTemp> {
        let len = self.entries.len();
        let start = self.counter.fetch_add(1, Ordering::Relaxed) % len;
        for i in 0..len {
            let idx = (start + i) % len;
            if let Some(g) = self.try_acquire_for_idx(idx, input_len, file_size_mb) {
                return Some(g);
            }
        }
        None
    }

    fn try_acquire_random(&self, input_len: usize, file_size_mb: f64) -> Option<OAuthAcquireTemp> {
        use rand::Rng;
        let len = self.entries.len();
        let start = rand::thread_rng().gen_range(0..len);
        for i in 0..len {
            let idx = (start + i) % len;
            if let Some(g) = self.try_acquire_for_idx(idx, input_len, file_size_mb) {
                return Some(g);
            }
        }
        None
    }

    fn try_acquire_weighted(
        &self,
        input_len: usize,
        file_size_mb: f64,
    ) -> Option<OAuthAcquireTemp> {
        use rand::Rng;
        let total_weight: u128 = self.entries.iter().map(|e| u128::from(e.weight)).sum();
        if total_weight == 0 {
            return self.try_acquire_round_robin(input_len, file_size_mb);
        }
        let mut rng = rand::thread_rng();
        let target = rng.gen_range(0..total_weight);
        let mut cumulative = 0u128;
        for (idx, entry) in self.entries.iter().enumerate() {
            cumulative += u128::from(entry.weight);
            if target < cumulative {
                if let Some(g) = self.try_acquire_for_idx(idx, input_len, file_size_mb) {
                    return Some(g);
                }
                break;
            }
        }
        for idx in 0..self.entries.len() {
            if let Some(g) = self.try_acquire_for_idx(idx, input_len, file_size_mb) {
                return Some(g);
            }
        }
        None
    }
}

/// Temporary struct used internally before the token is fetched.
struct OAuthAcquireTemp {
    config_id_tmp: String,
    token_field_tmp: String,
    max_file_size_mb_tmp: f64,
    active_count: Arc<AtomicUsize>,
    notify: Arc<Notify>,
}

impl OAuthTokenManager {
    pub(crate) async fn snapshot(&self) -> HashMap<String, OAuthConfig> {
        let mut configs = self.configs.lock().await.clone();
        for config in configs.values_mut() {
            if config.cached_token.as_deref() == Some("") {
                config.cached_token = None;
            }
        }
        configs
    }

    pub(crate) async fn frozen_snapshot(&self) -> anyhow::Result<HashMap<String, OAuthConfig>> {
        let mut saved = self.snapshot().await;
        if self.frozen {
            return Ok(saved);
        }
        for config in saved.values_mut() {
            Self::resolve_config(config)?;
        }
        Ok(saved)
    }

    fn resolve_config(config: &mut OAuthConfig) -> anyhow::Result<()> {
        config.client_id =
            crate::component_rt::runner::resolve_credential_reference(&config.client_id)?;
        config.client_secret =
            crate::component_rt::runner::resolve_credential_reference(&config.client_secret)?;
        config.refresh_token = config
            .refresh_token
            .as_deref()
            .map(crate::component_rt::runner::resolve_credential_reference)
            .transpose()?;
        for value in config
            .extra_params
            .values_mut()
            .chain(config.auth_extra_params.values_mut())
        {
            *value = crate::component_rt::runner::resolve_credential_reference(value)?;
        }
        Ok(())
    }

    pub(crate) fn from_snapshot(
        configs: HashMap<String, OAuthConfig>,
        http_client: OAuthHttpClient,
    ) -> Self {
        Self {
            configs: Arc::new(Mutex::new(configs)),
            http_client,
            config_path: String::new(),
            db_path: None,
            recovery_db: None,
            frozen: true,
        }
    }

    pub(crate) fn with_recovery_db(
        mut self,
        db: Arc<tokio::sync::Mutex<rusqlite::Connection>>,
    ) -> Self {
        self.recovery_db = Some(db);
        self
    }

    fn resolve_default_db_path() -> Option<String> {



        {
            Some(crate::config::db_path())
        }
    }

    pub(crate) fn new(
        configs: HashMap<String, OAuthConfig>,
        http_client: OAuthHttpClient,
        config_path: String,
    ) -> Self {
        Self {
            configs: Arc::new(Mutex::new(configs)),
            http_client,
            config_path,
            db_path: Self::resolve_default_db_path(),
            recovery_db: None,
            frozen: false,
        }
    }

    pub(crate) async fn get_token(&self, config_id: &str) -> anyhow::Result<String> {
        let now = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs(),
        )?;
        let mut config = self
            .configs
            .lock()
            .await
            .get(config_id)
            .cloned()
            .ok_or_else(|| anyhow!("oauth config not found: {config_id}"))?;
        let original = config.clone();
        let projection = self.projection_plan(config_id, &original, None).await?;
        if !self.frozen {
            Self::resolve_config(&mut config)?;
        }
        if let Some(token) = config.cached_token.as_ref().filter(|token| {
            !token.is_empty() && config.cached_token_expires_at.saturating_sub(60) > now
        }) {
            if !recovery::TokenExecution::has_checkpoint(self, config_id, &config).await? {
                self.projection_plan(config_id, &original, Some(&config))
                    .await?;
                return Ok(token.clone());
            }
        }
        match config.grant_type.as_str() {
            "client_credentials"|"jwt_bearer" => {},
            "authorization_code" => anyhow::ensure!(config.refresh_token.as_ref().is_some_and(|token| !token.trim().is_empty()),
                "authorization_code config '{config_id}' has no refresh_token; please re-authorize via the Web UI"),
            other => anyhow::bail!("unsupported oauth grant_type: {other} (config: {config_id})"),
        }
        let mut execution = recovery::TokenExecution::acquire(self, config_id, &config).await?;
        execution.restore(&mut config);
        self.projection_plan(config_id, &original, Some(&config))
            .await?;
        if let Some(token) = config.cached_token.as_ref().filter(|token| {
            !token.is_empty() && config.cached_token_expires_at.saturating_sub(60) > now
        }) {
            let token = token.clone();
            self.project_token(config_id, &original, &config, projection, Some(&execution))
                .await?;
            self.cache_token(config_id, &config).await?;
            return Ok(token);
        }
        Self::validate_token_request(&config)?;
        execution.intent().await?;
        let (token, expires_in, refresh) = match config.grant_type.as_str() {
            "client_credentials" => self
                .fetch_client_credentials_token(&config)
                .await
                .map(|(token, ttl)| (token, ttl, None))?,
            "jwt_bearer" => self
                .fetch_jwt_bearer_token(&config)
                .await
                .map(|(token, ttl)| (token, ttl, None))?,
            "authorization_code" => {
                self.fetch_refresh_token(
                    &config,
                    config
                        .refresh_token
                        .as_deref()
                        .context("refresh token missing")?,
                )
                .await?
            }
            _ => unreachable!(),
        };
        anyhow::ensure!(
            expires_in > 0,
            "OAuth returned an invalid token lifetime; retained"
        );
        let expires_at = now
            .checked_add(expires_in)
            .context("OAuth token lifetime overflow")?;
        execution.finish(&token, expires_at, refresh).await?;
        execution.restore(&mut config);
        self.project_token(config_id, &original, &config, projection, Some(&execution))
            .await?;
        self.cache_token(config_id, &config).await?;
        Ok(token)
    }

    pub(crate) fn for_http_client(&self, client: &OAuthHttpClient) -> Self {
        let mut manager = self.clone();
        manager.http_client = client.clone();
        manager
    }

    pub(crate) fn assert_proxy_profile(&self, profile_id: Option<&str>) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.http_client.proxy_profile_id.as_deref() == profile_id,
            "OAuth selected proxy transport differs; direct fallback refused"
        );
        Ok(())
    }

    async fn cache_token(&self, id: &str, saved: &OAuthConfig) -> anyhow::Result<()> {
        let mut configs = self.configs.lock().await;
        let current = configs
            .get_mut(id)
            .context("OAuth configuration disappeared; retained")?;
        current.cached_token = saved.cached_token.clone();
        current.cached_token_expires_at = saved.cached_token_expires_at;
        current.refresh_token = saved.refresh_token.clone();
        Ok(())
    }

    async fn projection_plan(
        &self,
        id: &str,
        original: &OAuthConfig,
        restored: Option<&OAuthConfig>,
    ) -> anyhow::Result<(bool, bool)> {
        if self.frozen {
            return Ok((false, false));
        }
        let db_absent = if let Some(db) = &self.recovery_db {
            Self::projection_db_plan(&*db.lock().await, id, original, restored)?
        } else if let Some(path) = self.db_path.as_deref() {
            let conn = crate::db::open_db(path)?;
            Self::projection_db_plan(&conn, id, original, restored)?
        } else {
            false
        };
        let file_absent = if self.config_path.is_empty() {
            false
        } else {
            let path = std::path::Path::new(&self.config_path);
            match path.symlink_metadata() {
                Ok(_) => {
                    let doc =
                        serde_json::from_str(&crate::bindings::load_encrypted_or_plain(path)?)?;
                    Self::verify_profile(&doc, id, original, restored)?;
                    false
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
                Err(error) => return Err(error.into()),
            }
        };
        Ok((db_absent, file_absent))
    }

    fn projection_db_plan(
        conn: &rusqlite::Connection,
        id: &str,
        original: &OAuthConfig,
        restored: Option<&OAuthConfig>,
    ) -> anyhow::Result<bool> {
        anyhow::ensure!(
            crate::db::system::get_system_config_checked(
                conn,
                "integration-save-v1:vendor_oauth_doc",
            )?
            .is_none(),
            "OAuth operator configuration save is unresolved; original checkpoint retained"
        );
        match crate::db::system::get_system_config_checked(conn, "vendor_oauth_doc")? {
            Some(raw) => {
                let doc = serde_json::from_str(&crate::db::system::decrypt_config_value(&raw)?)?;
                Self::verify_profile(&doc, id, original, restored)?;
                Ok(false)
            }
            None => Ok(true),
        }
    }

    pub(crate) fn credential_fingerprint(config: &OAuthConfig) -> anyhow::Result<String> {
        crate::db::system::private_json_digest(&serde_json::json!({
            "vendor":config.vendor_id,"grant":config.grant_type,"url":config.token_url,
            "auth_url":config.auth_url,"client_id":config.client_id,"client_secret":config.client_secret,
            "scopes":config.scopes,"extra":config.extra_params,"auth_extra":config.auth_extra_params,
        }))
    }

    fn verify_profile(
        doc: &crate::types::VendorOAuthDoc,
        id: &str,
        original: &OAuthConfig,
        restored: Option<&OAuthConfig>,
    ) -> anyhow::Result<()> {
        let current = doc
            .configs
            .get(id)
            .context("OAuth operator profile was deleted or is absent; retained")?;
        anyhow::ensure!(
            Self::credential_fingerprint(current)? == Self::credential_fingerprint(original)?,
            "OAuth operator credentials changed; retained"
        );
        if let Some(restored) = restored {
            anyhow::ensure!(
                current.refresh_token == original.refresh_token
                    || current.refresh_token == restored.refresh_token,
                "OAuth operator refresh credential changed; retained"
            );
        }
        Ok(())
    }

    fn merge_token(
        doc: &mut crate::types::VendorOAuthDoc,
        id: &str,
        original: &OAuthConfig,
        saved: &OAuthConfig,
        may_initialize: bool,
    ) -> anyhow::Result<bool> {
        if let Some(current) = doc.configs.get(id) {
            anyhow::ensure!(Self::credential_fingerprint(current)? == Self::credential_fingerprint(original)?,
                "OAuth operator credentials changed during refresh; durable token retained without replacement");
            if current.cached_token == saved.cached_token
                && current.cached_token_expires_at == saved.cached_token_expires_at
                && current.refresh_token == saved.refresh_token
            {
                return Ok(false);
            }
            anyhow::ensure!(current.refresh_token == original.refresh_token,
                "OAuth operator refresh credential changed; durable token retained without replacement");
        } else {
            anyhow::ensure!(may_initialize && doc.configs.is_empty(),
                "OAuth operator profile was deleted or is absent; durable token retained without restoration");
            doc.configs.insert(id.into(), original.clone());
        }
        let current = doc
            .configs
            .get_mut(id)
            .context("OAuth projection profile missing")?;
        current.cached_token = saved.cached_token.clone();
        current.cached_token_expires_at = saved.cached_token_expires_at;
        current.refresh_token = saved.refresh_token.clone();
        Ok(true)
    }

    async fn project_token(
        &self,
        id: &str,
        original: &OAuthConfig,
        saved: &OAuthConfig,
        projection: (bool, bool),
        execution: Option<&dyn OAuthProjection>,
    ) -> anyhow::Result<()> {
        if self.frozen {
            return Ok(());
        }
        if let Some(db) = &self.recovery_db {
            Self::project_token_db(
                &mut *db.lock().await,
                id,
                original,
                saved,
                projection.0,
                execution,
            )?;
        } else if let Some(path) = self.db_path.as_deref() {
            let mut conn = crate::db::open_db(path)?;
            Self::project_token_db(&mut conn, id, original, saved, projection.0, execution)?;
        }
        if !self.config_path.is_empty() {
            crate::bindings::mutate_vendor_oauth_if_changed(&self.config_path, |doc| {
                let missing = !std::path::Path::new(&self.config_path).exists();
                let changed = Self::merge_token(doc, id, original, saved, projection.1 && missing)?;
                Ok(((), changed))
            })?;
        }
        Ok(())
    }

    pub(crate) async fn project_authorization(
        &self,
        id: &str,
        original: &OAuthConfig,
        saved: &OAuthConfig,
        authority: &dyn OAuthProjection,
        completed: bool,
    ) -> anyhow::Result<()> {
        let projection = self.projection_plan(id, original, Some(saved)).await?;
        if completed {
            anyhow::ensure!(
                !projection.0 && !projection.1,
                "completed OAuth authorization projection is absent; retained"
            );
            if let Some(db) = &self.recovery_db {
                let conn = db.lock().await;
                authority.check_projection(&conn, id, saved)?;
                let raw = crate::db::system::get_system_config_checked(&conn, "vendor_oauth_doc")?
                    .context("completed OAuth database projection is absent; retained")?;
                let mut doc = serde_json::from_str(&crate::db::system::decrypt_config_value(&raw)?)?;
                anyhow::ensure!(
                    !Self::merge_token(&mut doc, id, original, saved, false)?,
                    "completed OAuth database projection changed; retained"
                );
            }
            if !self.config_path.is_empty() {
                let mut doc = crate::bindings::read_vendor_oauth(&self.config_path)?;
                anyhow::ensure!(
                    !Self::merge_token(&mut doc, id, original, saved, false)?,
                    "completed OAuth file projection changed; retained"
                );
            }
            return Ok(());
        }
        self.project_token(id, original, saved, projection, Some(authority))
            .await
    }

    fn project_token_db(
        conn: &mut rusqlite::Connection,
        id: &str,
        original: &OAuthConfig,
        saved: &OAuthConfig,
        may_initialize: bool,
        execution: Option<&dyn OAuthProjection>,
    ) -> anyhow::Result<()> {
        let credit = execution
            .map(|execution| execution.projection_credit(conn, id, saved))
            .transpose()?
            .flatten();
        crate::storage_capacity::with_database_credit(credit, || -> anyhow::Result<()> {
            let tx = conn.savepoint()?;
            anyhow::ensure!(
                crate::db::system::get_system_config_checked(
                    &tx,
                    "integration-save-v1:vendor_oauth_doc",
                )?
                .is_none(),
                "OAuth operator configuration save is unresolved; original checkpoint retained"
            );
            if let Some(execution) = execution {
                execution.check_projection(&tx, id, saved)?;
            }
            let prior = crate::db::system::get_system_config_checked(&tx, "vendor_oauth_doc")?;
            let mut doc = match &prior {
                Some(raw) => serde_json::from_str(&crate::db::system::decrypt_config_value(raw)?)
                    .context("damaged OAuth operator configuration; retained")?,
                None => crate::types::VendorOAuthDoc::default(),
            };
            if !Self::merge_token(
                &mut doc,
                id,
                original,
                saved,
                may_initialize && prior.is_none(),
            )? {
                tx.commit()?;
                return Ok(());
            }
            let next = crate::db::system::encrypt_config_value(&serde_json::to_string(&doc)?)?;
            let changed = match &prior {
                Some(raw) => tx.execute(
                    "UPDATE system_config SET value=?1 WHERE key='vendor_oauth_doc' AND value=?2",
                    rusqlite::params![next, raw],
                )?,
                None => tx.execute(
                    "INSERT INTO system_config(key,value) VALUES ('vendor_oauth_doc',?1)",
                    [&next],
                )?,
            };
            anyhow::ensure!(
                changed == 1
                    && crate::db::system::get_system_config_checked(&tx, "vendor_oauth_doc")?
                        .as_deref()
                        == Some(next.as_str()),
                "OAuth token projection was not committed; retained"
            );
            if let Some(execution) = execution {
                execution.check_projection(&tx, id, saved)?;
            }
            tx.commit()?;
            Ok(())
        })
    }

    pub(crate) fn validate_token_request(config: &OAuthConfig) -> anyhow::Result<url::Url> {
        let endpoint = url::Url::parse(&config.token_url)
            .map_err(|_| anyhow!("OAuth token endpoint is invalid; no request sent"))?;
        anyhow::ensure!(
            matches!(endpoint.scheme(), "http" | "https")
                && endpoint.host_str().is_some()
                && endpoint.username().is_empty()
                && endpoint.password().is_none()
                && endpoint.fragment().is_none(),
            "OAuth token endpoint scope is invalid; no request sent"
        );
        crate::component_rt::runner::assert_provider_url_allowed(endpoint.as_str()).map_err(
            |_| anyhow!("OAuth token endpoint is blocked by egress policy; no request sent"),
        )?;
        let reserved = [
            "grant_type",
            "client_id",
            "client_secret",
            "scope",
            "refresh_token",
            "code",
            "code_verifier",
            "redirect_uri",
            "assertion",
        ];
        anyhow::ensure!(
            !config
                .extra_params
                .keys()
                .any(|key| reserved.contains(&key.to_ascii_lowercase().as_str()))
                && !endpoint
                    .query_pairs()
                    .any(|(key, _)| { reserved.contains(&key.to_ascii_lowercase().as_str()) }),
            "OAuth token parameters override credential identity; no request sent"
        );
        Ok(endpoint)
    }

    pub(crate) async fn request_token(
        &self,
        config: &OAuthConfig,
        mut form: HashMap<&str, String>,
    ) -> anyhow::Result<(String, i64, Option<String>)> {
        let endpoint = Self::validate_token_request(config)?;
        for (key, value) in &config.extra_params {
            form.insert(key.as_str(), value.clone());
        }
        let mut response = self
            .http_client
            .client
            .post(endpoint)
            .timeout(std::time::Duration::from_secs(15))
            .form(&form)
            .send()
            .await
            .map_err(|_| anyhow!("OAuth token request failed; outcome unknown and retained"))?;
        let status = response.status();
        if !status.is_success() {
            return Err(OAuthTokenResponseFault::Rejected(status.as_u16()).into());
        }
        const MAX_TOKEN_RESPONSE_BYTES: usize = 1024 * 1024;
        anyhow::ensure!(
            response
                .content_length()
                .is_none_or(|length| length <= MAX_TOKEN_RESPONSE_BYTES as u64),
            "OAuth token response exceeds 1 MiB; retained"
        );
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow!("OAuth token response was interrupted; retained"))?
        {
            anyhow::ensure!(
                bytes
                    .len()
                    .checked_add(chunk.len())
                    .is_some_and(|length| length <= MAX_TOKEN_RESPONSE_BYTES),
                "OAuth token response exceeds 1 MiB; retained"
            );
            bytes.extend_from_slice(&chunk);
        }
        let body: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow!("OAuth token response is invalid JSON; retained"))?;
        let access_token = body
            .get("access_token")
            .and_then(serde_json::Value::as_str)
            .filter(|token| {
                !token.trim().is_empty() && reqwest::header::HeaderValue::from_str(token).is_ok()
            })
            .ok_or(OAuthTokenResponseFault::MissingToken)?
            .to_string();
        let expires_in = match body.get("expires_in") {
            None => 3600,
            Some(value) => value
                .as_i64()
                .filter(|lifetime| *lifetime > 0)
                .context("OAuth token lifetime is invalid; retained")?,
        };
        let refresh_token = match body.get("refresh_token") {
            None | Some(serde_json::Value::Null) => None,
            Some(value) => Some(
                value
                    .as_str()
                    .filter(|token| !token.trim().is_empty())
                    .context("OAuth refresh rotation is invalid; retained")?
                    .to_string(),
            ),
        };
        Ok((access_token, expires_in, refresh_token))
    }

    async fn fetch_client_credentials_token(
        &self,
        config: &OAuthConfig,
    ) -> anyhow::Result<(String, i64)> {
        let mut form = HashMap::new();
        form.insert("grant_type", "client_credentials".to_string());
        form.insert("client_id", config.client_id.clone());
        let client_secret = config.client_secret.clone();
        if !client_secret.is_empty() {
            form.insert("client_secret", client_secret);
        }
        if !config.scopes.is_empty() {
            form.insert("scope", config.scopes.clone());
        }
        self.request_token(config, form)
            .await
            .map(|(token, lifetime, _)| (token, lifetime))
    }

    async fn fetch_jwt_bearer_token(&self, config: &OAuthConfig) -> anyhow::Result<(String, i64)> {
        // For jwt_bearer, the client_secret is expected to be a JWT assertion
        // or we generate one. Here we send the assertion as grant_type=urn:ietf:params:oauth:grant-type:jwt-bearer
        let mut form = HashMap::new();
        form.insert(
            "grant_type",
            "urn:ietf:params:oauth:grant-type:jwt-bearer".to_string(),
        );
        let assertion = config.client_secret.clone();
        if !assertion.is_empty() {
            form.insert("assertion", assertion);
        }
        form.insert("client_id", config.client_id.clone());
        if !config.scopes.is_empty() {
            form.insert("scope", config.scopes.clone());
        }
        self.request_token(config, form)
            .await
            .map(|(token, lifetime, _)| (token, lifetime))
    }

    /// Exchange a refresh_token for a new access_token.
    /// Returns (access_token, expires_in_secs, Option<new_refresh_token>).
    async fn fetch_refresh_token(
        &self,
        config: &OAuthConfig,
        refresh_token: &str,
    ) -> anyhow::Result<(String, i64, Option<String>)> {
        let mut form: HashMap<&str, String> = HashMap::new();
        form.insert("grant_type", "refresh_token".to_string());
        form.insert("refresh_token", refresh_token.to_string());
        form.insert("client_id", config.client_id.clone());
        let client_secret = config.client_secret.clone();
        if !client_secret.is_empty() {
            form.insert("client_secret", client_secret);
        }
        if !config.scopes.is_empty() {
            form.insert("scope", config.scopes.clone());
        }

        self.request_token(config, form).await
    }

    async fn persist_doc(&self, doc: &crate::types::VendorOAuthDoc) -> anyhow::Result<()> {
        if self.frozen {
            return Ok(());
        }
        if !self.config_path.is_empty() {
            crate::bindings::save_vendor_oauth(&self.config_path, doc)?;
        }
        if let Some(ref db_path) = self.db_path {
            let conn = crate::db::open_db(db_path)?;
            crate::db::vendor::save_vendor_oauth_doc(&conn, doc)?;
        }
        Ok(())
    }
}
