use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::{Mutex, Notify};

use crate::component_rt::key_registry::GlobalKeyRegistry;
use crate::types::KeySelectionStrategy;

type KeyAuthValues = HashMap<String, String>;
type KeyPoolBaseTuple = (String, KeyAuthValues, usize, u32);
type KeyPoolExtTuple = (String, KeyAuthValues, usize, u32, usize, f64);
type KeyPoolExtRpsTuple = (String, KeyAuthValues, usize, u32, usize, f64, f64);

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct KeyPoolSnapshot {
    keys: Vec<KeySnapshot>,
    strategy: KeySelectionStrategy,
    min_interval_ms: u64,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct KeySnapshot {
    id: String,
    auth: KeyAuthValues,
    max_concurrent: usize,
    weight: u32,
    max_input_chars: usize,
    max_file_size_mb: f64,
    min_interval_ms: u64,
}

#[derive(Debug)]
pub struct KeyPool {
    keys: Vec<KeyEntry>,
    strategy: KeySelectionStrategy,
    counter: AtomicUsize,
    notify: Arc<Notify>,
    /// Minimum interval between consecutive requests per key (milliseconds).
    min_interval_ms: u64,
    /// Tracks the last request timestamp per key index.
    last_request_at: Vec<Arc<Mutex<std::time::Instant>>>,
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct KeyEntry {
    pub key_id: String,
    pub auth_values: KeyAuthValues,
    pub max_concurrent: usize,
    pub weight: u32,
    /// Maximum input text length (0 = unlimited).
    pub max_input_chars: usize,
    /// Maximum file size in MB for non-text translations (0.0 = unlimited).
    pub max_file_size_mb: f64,
    /// Minimum interval between consecutive requests for this key (milliseconds).
    pub min_interval_ms: u64,
    /// Local per-pool counter (used when no global registry is provided).
    pub active_count: Arc<AtomicUsize>,
    /// Global shared counter for this key_id (set when a GlobalKeyRegistry is used).
    /// When present, this overrides `active_count` for concurrency checking.
    pub global_count: Option<Arc<AtomicUsize>>,
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct KeyGuard {
    /// The counter to decrement on drop. Points to global_count when available, otherwise local.
    active_count: Arc<AtomicUsize>,
    notify: Arc<Notify>,
    pub key_id: String,
    pub auth_values: KeyAuthValues,
    /// Max input chars for this key (0 = unlimited). Callers can use this to enforce limits.
    pub max_input_chars: usize,
    /// Max file size in MB for non-text translations (0.0 = unlimited).
    pub max_file_size_mb: f64,
}

impl Drop for KeyGuard {
    fn drop(&mut self) {
        self.active_count.fetch_sub(1, Ordering::Release);
        self.notify.notify_waiters();
    }
}

fn min_interval_ms_from_rps(rps: f64) -> u64 {
    if !rps.is_finite() || rps <= 0.0 {
        return 0;
    }
    ((1000.0 / rps).ceil() as u64).max(1)
}

#[allow(dead_code)]
impl KeyPool {
    pub(crate) fn frozen_snapshot(&self) -> anyhow::Result<KeyPoolSnapshot> {
        let mut saved = self.snapshot();
        for key in &mut saved.keys {
            for value in key.auth.values_mut() {
                *value = crate::component_rt::runner::resolve_credential_reference(value)?;
                anyhow::ensure!(
                    !value.starts_with("env://") && !value.starts_with("file://"),
                    "frozen credential is ambiguous; retained"
                );
            }
        }
        Ok(saved)
    }

    pub(crate) fn snapshot(&self) -> KeyPoolSnapshot {
        KeyPoolSnapshot {
            keys: self
                .keys
                .iter()
                .map(|key| KeySnapshot {
                    id: key.key_id.clone(),
                    auth: key.auth_values.clone(),
                    max_concurrent: key.max_concurrent,
                    weight: key.weight,
                    max_input_chars: key.max_input_chars,
                    max_file_size_mb: key.max_file_size_mb,
                    min_interval_ms: key.min_interval_ms,
                })
                .collect(),
            strategy: self.strategy.clone(),
            min_interval_ms: self.min_interval_ms,
        }
    }

    pub(crate) fn from_snapshot(snapshot: KeyPoolSnapshot) -> Self {
        let registry = GlobalKeyRegistry::process();
        let mut pool = Self::new_ext_internal(
            snapshot
                .keys
                .iter()
                .map(|key| {
                    (
                        key.id.clone(),
                        key.auth.clone(),
                        key.max_concurrent,
                        key.weight,
                        key.max_input_chars,
                        key.max_file_size_mb,
                        0.0,
                    )
                })
                .collect(),
            snapshot.strategy,
            snapshot.min_interval_ms,
            Some(registry),
        );
        for (entry, saved) in pool.keys.iter_mut().zip(snapshot.keys) {
            entry.min_interval_ms = saved.min_interval_ms;
        }
        pool
    }

    /// Create a pool from `(key_id, auth_values, max_concurrent, weight)` tuples.
    pub fn new(keys: Vec<KeyPoolBaseTuple>, strategy: KeySelectionStrategy) -> Self {
        let keys_ext: Vec<_> = keys
            .into_iter()
            .map(|(id, av, mc, w)| (id, av, mc, w, 0usize, 0.0f64, 0.0f64))
            .collect();
        Self::new_ext_internal(keys_ext, strategy, 0, None)
    }

    pub fn new_with_rate_limit(
        keys: Vec<KeyPoolBaseTuple>,
        strategy: KeySelectionStrategy,
        min_interval_ms: u64,
    ) -> Self {
        let keys_ext: Vec<_> = keys
            .into_iter()
            .map(|(id, av, mc, w)| (id, av, mc, w, 0usize, 0.0f64, 0.0f64))
            .collect();
        Self::new_ext_internal(keys_ext, strategy, min_interval_ms, None)
    }

    /// Build a `KeyPool` where all entries share global concurrency counters via `registry`.
    pub(crate) fn new_with_registry(
        keys: Vec<KeyPoolBaseTuple>,
        strategy: KeySelectionStrategy,
        registry: &GlobalKeyRegistry,
    ) -> Self {
        let keys_ext: Vec<_> = keys
            .into_iter()
            .map(|(id, av, mc, w)| (id, av, mc, w, 0usize, 0.0f64, 0.0f64))
            .collect();
        Self::new_ext_internal(keys_ext, strategy, 0, Some(registry))
    }

    /// Create a pool from `(key_id, auth_values, max_concurrent, weight, max_input_chars, max_file_size_mb)` tuples.
    pub fn new_with_ext(keys: Vec<KeyPoolExtTuple>, strategy: KeySelectionStrategy) -> Self {
        let keys_ext: Vec<_> = keys
            .into_iter()
            .map(|(id, av, mc, w, mic, mfs)| (id, av, mc, w, mic, mfs, 0.0f64))
            .collect();
        Self::new_ext_internal(keys_ext, strategy, 0, None)
    }

    /// Create a pool from `(key_id, auth_values, max_concurrent, weight, max_input_chars, max_file_size_mb, requests_per_second)` tuples.
    pub fn new_with_ext_with_rps(
        keys: Vec<KeyPoolExtRpsTuple>,
        strategy: KeySelectionStrategy,
    ) -> Self {
        Self::new_ext_internal(keys, strategy, 0, None)
    }

    /// Create a pool with registry from `(key_id, auth_values, max_concurrent, weight, max_input_chars, max_file_size_mb)` tuples.
    pub(crate) fn new_with_registry_ext(
        keys: Vec<KeyPoolExtTuple>,
        strategy: KeySelectionStrategy,
        registry: &GlobalKeyRegistry,
    ) -> Self {
        let keys_ext: Vec<_> = keys
            .into_iter()
            .map(|(id, av, mc, w, mic, mfs)| (id, av, mc, w, mic, mfs, 0.0f64))
            .collect();
        Self::new_ext_internal(keys_ext, strategy, 0, Some(registry))
    }

    /// Create a pool with registry from `(key_id, auth_values, max_concurrent, weight, max_input_chars, max_file_size_mb, requests_per_second)` tuples.
    pub(crate) fn new_with_registry_ext_with_rps(
        keys: Vec<KeyPoolExtRpsTuple>,
        strategy: KeySelectionStrategy,
        registry: &GlobalKeyRegistry,
    ) -> Self {
        Self::new_ext_internal(keys, strategy, 0, Some(registry))
    }

    fn new_ext_internal(
        keys: Vec<KeyPoolExtRpsTuple>,
        strategy: KeySelectionStrategy,
        min_interval_ms: u64,
        registry: Option<&GlobalKeyRegistry>,
    ) -> Self {
        let notify = registry
            .map(GlobalKeyRegistry::notify)
            .unwrap_or_else(|| Arc::new(Notify::new()));
        let entries: Vec<KeyEntry> = keys
            .into_iter()
            .map(
                |(
                    key_id,
                    auth_values,
                    max_concurrent,
                    weight,
                    max_input_chars,
                    max_file_size_mb,
                    requests_per_second,
                )| {
                    let global_count = registry.map(|r| r.get_or_create(&key_id));
                    KeyEntry {
                        key_id,
                        auth_values,
                        max_concurrent,
                        weight,
                        max_input_chars,
                        max_file_size_mb,
                        min_interval_ms: min_interval_ms_from_rps(requests_per_second),
                        active_count: Arc::new(AtomicUsize::new(0)),
                        global_count,
                    }
                },
            )
            .collect();
        // Initialize last_request_at to a past time so first request is not throttled
        let past = std::time::Instant::now() - std::time::Duration::from_secs(60);
        let last_request_at = entries
            .iter()
            .map(|entry| {
                registry
                    .map(|registry| registry.rate_clock(&entry.key_id))
                    .unwrap_or_else(|| Arc::new(Mutex::new(past)))
            })
            .collect();
        Self {
            keys: entries,
            strategy,
            counter: AtomicUsize::new(0),
            notify,
            min_interval_ms,
            last_request_at,
        }
    }

    fn effective_min_interval_ms(&self, idx: usize) -> u64 {
        let key_min_interval_ms = self.keys.get(idx).map(|k| k.min_interval_ms).unwrap_or(0);
        self.min_interval_ms.max(key_min_interval_ms)
    }

    async fn enforce_min_interval_for_index(&self, idx: usize) {
        let min_interval_ms = self.effective_min_interval_ms(idx);
        if min_interval_ms == 0 {
            return;
        }
        if let Some(last_at) = self.last_request_at.get(idx) {
            let mut last = last_at.lock().await;
            let elapsed = last.elapsed();
            let min_interval = std::time::Duration::from_millis(min_interval_ms);
            if elapsed < min_interval {
                tokio::time::sleep(min_interval - elapsed).await;
            }
            *last = std::time::Instant::now();
        }
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Returns true if any key in the pool has a non-zero `max_file_size_mb` limit.
    pub fn has_file_size_limits(&self) -> bool {
        self.keys.iter().any(|k| k.max_file_size_mb > 0.0)
    }

    pub(crate) fn source_byte_budget(&self) -> anyhow::Result<u64> {
        crate::component_rt::file_limits::pool_source_budget(
            self.keys.iter().map(|key| key.max_file_size_mb),
        )
    }

    /// Returns true if at least one key can handle the given input size and file size.
    pub fn has_eligible_keys(&self, input_len: usize, file_size_mb: f64) -> bool {
        self.keys.iter().any(|k| {
            (input_len == 0 || k.max_input_chars == 0 || input_len <= k.max_input_chars)
                && (file_size_mb == 0.0
                    || k.max_file_size_mb == 0.0
                    || file_size_mb <= k.max_file_size_mb)
        })
    }

    pub async fn select_key(&self) -> anyhow::Result<KeyGuard> {
        if self.keys.is_empty() {
            return Err(anyhow::anyhow!("key pool is empty"));
        }

        loop {
            let released = self.notify.notified();
            tokio::pin!(released);
            released.as_mut().enable();
            if let Some((idx, guard)) = self.try_select_with_index() {
                // Enforce per-key/per-pool minimum interval between requests.
                self.enforce_min_interval_for_index(idx).await;
                return Ok(guard);
            }
            // All keys are at capacity, wait for one to be released
            released.await;
        }
    }

    /// Like `select_key` but only considers keys that satisfy the given input/file-size limits.
    /// Returns `Err` immediately (without waiting) if no eligible key exists in the pool.
    pub async fn select_key_with_limits(
        &self,
        input_len: usize,
        file_size_mb: f64,
    ) -> anyhow::Result<KeyGuard> {
        if self.keys.is_empty() {
            return Err(anyhow::anyhow!("key pool is empty"));
        }
        if !self.has_eligible_keys(input_len, file_size_mb) {
            return Err(anyhow::anyhow!(
                "no eligible key for input_len={} file_size_mb={:.1}",
                input_len,
                file_size_mb
            ));
        }
        loop {
            let released = self.notify.notified();
            tokio::pin!(released);
            released.as_mut().enable();
            if let Some((idx, guard)) = self.try_select_filtered_with_index(input_len, file_size_mb)
            {
                self.enforce_min_interval_for_index(idx).await;
                return Ok(guard);
            }
            // All eligible keys are at capacity, wait for one to be released
            released.await;
        }
    }

    fn try_select(&self) -> Option<KeyGuard> {
        self.try_select_with_index().map(|(_, guard)| guard)
    }

    fn try_select_with_index(&self) -> Option<(usize, KeyGuard)> {
        match self.strategy {
            KeySelectionStrategy::Random => self.try_select_random_with_index(),
            KeySelectionStrategy::RoundRobin => self.try_select_round_robin_with_index(),
            KeySelectionStrategy::Weighted => self.try_select_weighted_with_index(),
        }
    }

    fn try_select_random_with_index(&self) -> Option<(usize, KeyGuard)> {
        use rand::Rng;
        let len = self.keys.len();
        let start = rand::thread_rng().gen_range(0..len);
        for i in 0..len {
            let idx = (start + i) % len;
            if let Some(guard) = self.try_acquire(idx) {
                return Some((idx, guard));
            }
        }
        None
    }

    fn try_select_round_robin_with_index(&self) -> Option<(usize, KeyGuard)> {
        let len = self.keys.len();
        let start = self.counter.fetch_add(1, Ordering::Relaxed) % len;
        for i in 0..len {
            let idx = (start + i) % len;
            if let Some(guard) = self.try_acquire(idx) {
                return Some((idx, guard));
            }
        }
        None
    }

    fn try_select_weighted_with_index(&self) -> Option<(usize, KeyGuard)> {
        use rand::Rng;
        let total_weight: u128 = self.keys.iter().map(|k| u128::from(k.weight)).sum();
        if total_weight == 0 {
            return self.try_select_round_robin_with_index();
        }
        let mut rng = rand::thread_rng();
        let target = rng.gen_range(0..total_weight);
        let mut cumulative = 0u128;
        for (idx, key) in self.keys.iter().enumerate() {
            cumulative += u128::from(key.weight);
            if target < cumulative {
                if let Some(guard) = self.try_acquire(idx) {
                    return Some((idx, guard));
                }
                break;
            }
        }
        for idx in 0..self.keys.len() {
            if let Some(guard) = self.try_acquire(idx) {
                return Some((idx, guard));
            }
        }
        None
    }

    fn try_acquire(&self, idx: usize) -> Option<KeyGuard> {
        let entry = &self.keys[idx];
        // Use global counter when available (shared across pools), otherwise local counter.
        let counter = entry.global_count.as_ref().unwrap_or(&entry.active_count);
        loop {
            let current = counter.load(Ordering::Acquire);
            if current >= entry.max_concurrent {
                return None;
            }
            if counter
                .compare_exchange(current, current + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Some(KeyGuard {
                    active_count: counter.clone(),
                    notify: self.notify.clone(),
                    key_id: entry.key_id.clone(),
                    auth_values: entry.auth_values.clone(),
                    max_input_chars: entry.max_input_chars,
                    max_file_size_mb: entry.max_file_size_mb,
                });
            }
            // CAS failed, another thread modified it, retry
        }
    }

    /// Like `try_acquire` but additionally checks that the key satisfies the given limits.
    fn try_acquire_with_limits(
        &self,
        idx: usize,
        input_len: usize,
        file_size_mb: f64,
    ) -> Option<KeyGuard> {
        let entry = &self.keys[idx];
        if input_len > 0 && entry.max_input_chars > 0 && input_len > entry.max_input_chars {
            return None;
        }
        if file_size_mb > 0.0
            && entry.max_file_size_mb > 0.0
            && file_size_mb > entry.max_file_size_mb
        {
            return None;
        }
        self.try_acquire(idx)
    }

    fn try_select_filtered_with_index(
        &self,
        input_len: usize,
        file_size_mb: f64,
    ) -> Option<(usize, KeyGuard)> {
        match self.strategy {
            KeySelectionStrategy::Random => {
                self.try_select_random_filtered(input_len, file_size_mb)
            }
            KeySelectionStrategy::RoundRobin => {
                self.try_select_round_robin_filtered(input_len, file_size_mb)
            }
            KeySelectionStrategy::Weighted => {
                self.try_select_weighted_filtered(input_len, file_size_mb)
            }
        }
    }

    fn try_select_random_filtered(
        &self,
        input_len: usize,
        file_size_mb: f64,
    ) -> Option<(usize, KeyGuard)> {
        use rand::Rng;
        let len = self.keys.len();
        let start = rand::thread_rng().gen_range(0..len);
        for i in 0..len {
            let idx = (start + i) % len;
            if let Some(guard) = self.try_acquire_with_limits(idx, input_len, file_size_mb) {
                return Some((idx, guard));
            }
        }
        None
    }

    fn try_select_round_robin_filtered(
        &self,
        input_len: usize,
        file_size_mb: f64,
    ) -> Option<(usize, KeyGuard)> {
        let len = self.keys.len();
        let start = self.counter.fetch_add(1, Ordering::Relaxed) % len;
        for i in 0..len {
            let idx = (start + i) % len;
            if let Some(guard) = self.try_acquire_with_limits(idx, input_len, file_size_mb) {
                return Some((idx, guard));
            }
        }
        None
    }

    fn try_select_weighted_filtered(
        &self,
        input_len: usize,
        file_size_mb: f64,
    ) -> Option<(usize, KeyGuard)> {
        use rand::Rng;
        let total_weight: u128 = self.keys.iter().map(|k| u128::from(k.weight)).sum();
        if total_weight == 0 {
            return self.try_select_round_robin_filtered(input_len, file_size_mb);
        }
        let mut rng = rand::thread_rng();
        let target = rng.gen_range(0..total_weight);
        let mut cumulative = 0u128;
        for (idx, key) in self.keys.iter().enumerate() {
            cumulative += u128::from(key.weight);
            if target < cumulative {
                if let Some(guard) = self.try_acquire_with_limits(idx, input_len, file_size_mb) {
                    return Some((idx, guard));
                }
                break;
            }
        }
        for idx in 0..self.keys.len() {
            if let Some(guard) = self.try_acquire_with_limits(idx, input_len, file_size_mb) {
                return Some((idx, guard));
            }
        }
        None
    }
}
