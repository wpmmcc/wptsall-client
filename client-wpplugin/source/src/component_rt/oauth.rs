use anyhow::{anyhow, Context};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::{Mutex, Notify};

use crate::types::{KeySelectionStrategy, OAuthConfig};

#[derive(Debug)]
pub(crate) struct OAuthTokenManager {
    configs: Arc<Mutex<HashMap<String, OAuthConfig>>>,
    http_client: reqwest::Client,
    config_path: String,
    db_path: Option<String>,
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
            if let Some(guard) = self.try_acquire(input_len, file_size_mb) {
                // Fetch token for the selected config_id
                let token = manager.get_token(&guard.config_id_tmp).await?;
                return Ok(OAuthGuard {
                    access_token: token,
                    token_field: guard.token_field_tmp,
                    max_file_size_mb: guard.max_file_size_mb_tmp,
                    active_count: guard.active_count,
                    notify: guard.notify,
                });
            }
            self.notify.notified().await;
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
        let total_weight: u32 = self.entries.iter().map(|e| e.weight).sum();
        if total_weight == 0 {
            return self.try_acquire_round_robin(input_len, file_size_mb);
        }
        let mut rng = rand::thread_rng();
        let target = rng.gen_range(0..total_weight);
        let mut cumulative = 0u32;
        for (idx, entry) in self.entries.iter().enumerate() {
            cumulative += entry.weight;
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
    fn resolve_default_db_path() -> Option<String> {
        #[cfg(test)]
        {
            None
        }

        #[cfg(not(test))]
        {
            let path = std::env::var("WPTSALL_DB_PATH")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| "./runtime/wptsall.db".to_string());
            Some(path)
        }
    }

    pub(crate) fn new(
        configs: HashMap<String, OAuthConfig>,
        http_client: reqwest::Client,
        config_path: String,
    ) -> Self {
        Self {
            configs: Arc::new(Mutex::new(configs)),
            http_client,
            config_path,
            db_path: Self::resolve_default_db_path(),
        }
    }

    pub(crate) async fn get_token(&self, config_id: &str) -> anyhow::Result<String> {
        // Double-checked locking to prevent token-refresh thundering herd.
        //
        // Sentinel convention: cached_token == Some("") means a refresh is
        // already in-flight.  Any concurrent caller that sees the sentinel
        // sleeps 100 ms and retries instead of issuing its own refresh request.
        loop {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64;

            // --- Phase 1: check cache / sentinel under the lock ---
            let config_snapshot = {
                let mut configs = self.configs.lock().await;
                let config = configs
                    .get(config_id)
                    .ok_or_else(|| anyhow!("oauth config not found: {}", config_id))?;

                // Valid cached token — return immediately.
                if let Some(ref token) = config.cached_token {
                    if !token.is_empty() && config.cached_token_expires_at - 60 > now {
                        return Ok(token.clone());
                    }
                    // Sentinel: another task is refreshing — back off and retry.
                    if token.is_empty() {
                        drop(configs); // release lock before sleeping
                        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                        continue;
                    }
                }

                // Token is expired (or None) and no in-flight refresh yet.
                // Set sentinel to claim ownership of the refresh.
                let snapshot = config.clone();
                if let Some(entry) = configs.get_mut(config_id) {
                    entry.cached_token = Some(String::new()); // sentinel
                }
                snapshot
                // lock released here
            };

            // --- Phase 2: perform the HTTP fetch WITHOUT holding the lock ---
            let fetch_result: anyhow::Result<(String, i64, Option<String>)> =
                match config_snapshot.grant_type.as_str() {
                    "client_credentials" => self
                        .fetch_client_credentials_token(&config_snapshot)
                        .await
                        .map(|(t, e)| (t, e, None)),
                    "jwt_bearer" => self
                        .fetch_jwt_bearer_token(&config_snapshot)
                        .await
                        .map(|(t, e)| (t, e, None)),
                    "authorization_code" => {
                        let refresh_token = config_snapshot
                            .refresh_token
                            .as_deref()
                            .map(crate::component_rt::runner::resolve_credential_reference)
                            .transpose()?
                            .ok_or_else(|| {
                                anyhow!(
                                    "authorization_code config '{}' has no refresh_token; \
                                     please re-authorize via the Web UI",
                                    config_id
                                )
                            })?;
                        self.fetch_refresh_token(&config_snapshot, &refresh_token)
                            .await
                    }
                    other => Err(anyhow!(
                        "unsupported oauth grant_type: {} (config: {})",
                        other,
                        config_id
                    )),
                };

            // --- Phase 3: update cache / clear sentinel under the lock ---
            match fetch_result {
                Ok((token, expires_in, new_refresh)) => {
                    let doc = {
                        let mut configs = self.configs.lock().await;
                        if let Some(entry) = configs.get_mut(config_id) {
                            entry.cached_token = Some(token.clone());
                            entry.cached_token_expires_at = now + expires_in;
                            // Persist rotated refresh_token for authorization_code flow.
                            if let Some(rt) = new_refresh {
                                entry.refresh_token = Some(rt);
                            }
                        }
                        crate::types::VendorOAuthDoc {
                            version: 1,
                            configs: configs.clone(),
                        }
                    };
                    self.persist_doc(&doc).await;
                    return Ok(token);
                }
                Err(e) => {
                    // Clear sentinel so subsequent callers can retry.
                    let mut configs = self.configs.lock().await;
                    if let Some(entry) = configs.get_mut(config_id) {
                        if entry.cached_token.as_deref() == Some("") {
                            entry.cached_token = None;
                        }
                    }
                    return Err(e);
                }
            }
        }
    }

    async fn fetch_client_credentials_token(
        &self,
        config: &OAuthConfig,
    ) -> anyhow::Result<(String, i64)> {
        let mut form = HashMap::new();
        form.insert("grant_type", "client_credentials".to_string());
        form.insert("client_id", config.client_id.clone());
        let client_secret =
            crate::component_rt::runner::resolve_credential_reference(&config.client_secret)?;
        if !client_secret.is_empty() {
            form.insert("client_secret", client_secret);
        }
        if !config.scopes.is_empty() {
            form.insert("scope", config.scopes.clone());
        }
        for (k, v) in &config.extra_params {
            form.insert(k.as_str(), v.clone());
        }

        let resp = self
            .http_client
            .post(&config.token_url)
            .form(&form)
            .send()
            .await
            .with_context(|| format!("oauth token request failed: {}", config.token_url))?;

        let status = resp.status();
        let body: serde_json::Value = resp
            .json()
            .await
            .with_context(|| "parse oauth token response failed")?;

        if !status.is_success() {
            let error = body
                .get("error_description")
                .or_else(|| body.get("error"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error");
            return Err(anyhow!(
                "oauth token request failed (status={}): {}",
                status,
                error
            ));
        }

        let access_token = body
            .get("access_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing access_token in oauth response"))?
            .to_string();
        let expires_in = body
            .get("expires_in")
            .and_then(|v| v.as_i64())
            .unwrap_or(3600);

        Ok((access_token, expires_in))
    }

    async fn fetch_jwt_bearer_token(&self, config: &OAuthConfig) -> anyhow::Result<(String, i64)> {
        // For jwt_bearer, the client_secret is expected to be a JWT assertion
        // or we generate one. Here we send the assertion as grant_type=urn:ietf:params:oauth:grant-type:jwt-bearer
        let mut form = HashMap::new();
        form.insert(
            "grant_type",
            "urn:ietf:params:oauth:grant-type:jwt-bearer".to_string(),
        );
        let assertion =
            crate::component_rt::runner::resolve_credential_reference(&config.client_secret)?;
        if !assertion.is_empty() {
            form.insert("assertion", assertion);
        }
        form.insert("client_id", config.client_id.clone());
        if !config.scopes.is_empty() {
            form.insert("scope", config.scopes.clone());
        }
        for (k, v) in &config.extra_params {
            form.insert(k.as_str(), v.clone());
        }

        let resp = self
            .http_client
            .post(&config.token_url)
            .form(&form)
            .send()
            .await
            .with_context(|| {
                format!(
                    "oauth jwt_bearer token request failed: {}",
                    config.token_url
                )
            })?;

        let status = resp.status();
        let body: serde_json::Value = resp
            .json()
            .await
            .with_context(|| "parse oauth jwt_bearer response failed")?;

        if !status.is_success() {
            let error = body
                .get("error_description")
                .or_else(|| body.get("error"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error");
            return Err(anyhow!(
                "oauth jwt_bearer request failed (status={}): {}",
                status,
                error
            ));
        }

        let access_token = body
            .get("access_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing access_token in oauth jwt_bearer response"))?
            .to_string();
        let expires_in = body
            .get("expires_in")
            .and_then(|v| v.as_i64())
            .unwrap_or(3600);

        Ok((access_token, expires_in))
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
        let client_secret =
            crate::component_rt::runner::resolve_credential_reference(&config.client_secret)?;
        if !client_secret.is_empty() {
            form.insert("client_secret", client_secret);
        }
        if !config.scopes.is_empty() {
            form.insert("scope", config.scopes.clone());
        }

        let resp = self
            .http_client
            .post(&config.token_url)
            .form(&form)
            .send()
            .await
            .with_context(|| format!("oauth refresh_token request failed: {}", config.token_url))?;

        let status = resp.status();
        let body: serde_json::Value = resp
            .json()
            .await
            .with_context(|| "parse oauth refresh_token response failed")?;

        if !status.is_success() {
            let error = body
                .get("error_description")
                .or_else(|| body.get("error"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error");
            return Err(anyhow!(
                "oauth refresh_token request failed (status={}): {}",
                status,
                error
            ));
        }

        let access_token = body
            .get("access_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing access_token in refresh_token response"))?
            .to_string();
        let expires_in = body
            .get("expires_in")
            .and_then(|v| v.as_i64())
            .unwrap_or(3600);
        // Some providers rotate the refresh_token on each use
        let new_refresh = body
            .get("refresh_token")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        Ok((access_token, expires_in, new_refresh))
    }

    async fn persist_doc(&self, doc: &crate::types::VendorOAuthDoc) {
        let _ = crate::bindings::save_vendor_oauth(&self.config_path, doc);
        if let Some(ref db_path) = self.db_path {
            if let Ok(conn) = crate::db::open_db(db_path) {
                let _ = crate::db::migrate_from_json_if_needed(&conn);
                let _ = crate::db::vendor::save_vendor_oauth_doc(&conn, doc);
            }
        }
    }
}

#[cfg(test)]
mod tests;
