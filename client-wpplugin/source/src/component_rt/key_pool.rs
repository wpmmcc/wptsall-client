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
        let notify = Arc::new(Notify::new());
        let key_count = keys.len();
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
        let last_request_at = (0..key_count).map(|_| Arc::new(Mutex::new(past))).collect();
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
            if let Some((idx, guard)) = self.try_select_with_index() {
                // Enforce per-key/per-pool minimum interval between requests.
                self.enforce_min_interval_for_index(idx).await;
                return Ok(guard);
            }
            // All keys are at capacity, wait for one to be released
            self.notify.notified().await;
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
            if let Some((idx, guard)) = self.try_select_filtered_with_index(input_len, file_size_mb)
            {
                self.enforce_min_interval_for_index(idx).await;
                return Ok(guard);
            }
            // All eligible keys are at capacity, wait for one to be released
            self.notify.notified().await;
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
        let total_weight: u32 = self.keys.iter().map(|k| k.weight).sum();
        if total_weight == 0 {
            return self.try_select_round_robin_with_index();
        }
        let mut rng = rand::thread_rng();
        let target = rng.gen_range(0..total_weight);
        let mut cumulative = 0u32;
        for (idx, key) in self.keys.iter().enumerate() {
            cumulative += key.weight;
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
        let total_weight: u32 = self.keys.iter().map(|k| k.weight).sum();
        if total_weight == 0 {
            return self.try_select_round_robin_filtered(input_len, file_size_mb);
        }
        let mut rng = rand::thread_rng();
        let target = rng.gen_range(0..total_weight);
        let mut cumulative = 0u32;
        for (idx, key) in self.keys.iter().enumerate() {
            cumulative += key.weight;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_key(
        id: &str,
        max: usize,
        weight: u32,
    ) -> (String, HashMap<String, String>, usize, u32) {
        let mut auth = HashMap::new();
        auth.insert("api_key".to_string(), format!("key-{}", id));
        (id.to_string(), auth, max, weight)
    }

    #[test]
    fn key_pool_empty_is_empty() {
        let pool = KeyPool::new(vec![], KeySelectionStrategy::RoundRobin);
        assert!(pool.is_empty());
    }

    #[test]
    fn key_pool_not_empty() {
        let pool = KeyPool::new(vec![make_key("a", 5, 1)], KeySelectionStrategy::RoundRobin);
        assert!(!pool.is_empty());
    }

    #[tokio::test]
    async fn round_robin_selects_in_order() {
        let pool = KeyPool::new(
            vec![
                make_key("a", 10, 1),
                make_key("b", 10, 1),
                make_key("c", 10, 1),
            ],
            KeySelectionStrategy::RoundRobin,
        );
        let g1 = pool.select_key().await.unwrap();
        let g2 = pool.select_key().await.unwrap();
        let g3 = pool.select_key().await.unwrap();
        // Round robin should cycle through keys
        assert_ne!(g1.key_id, g2.key_id);
        assert_ne!(g2.key_id, g3.key_id);
        // After all three are different, the 4th should wrap
        let g4 = pool.select_key().await.unwrap();
        assert_eq!(g1.key_id, g4.key_id);
    }

    #[tokio::test]
    async fn key_guard_drop_releases_slot() {
        let pool = KeyPool::new(vec![make_key("a", 1, 1)], KeySelectionStrategy::RoundRobin);
        {
            let _guard = pool.select_key().await.unwrap();
            // While guard is held, try_select should fail
            assert!(pool.try_select().is_none());
        }
        // After guard is dropped, should be available
        assert!(pool.try_select().is_some());
    }

    #[tokio::test]
    async fn random_strategy_works() {
        let pool = KeyPool::new(
            vec![make_key("a", 10, 1), make_key("b", 10, 1)],
            KeySelectionStrategy::Random,
        );
        let guard = pool.select_key().await.unwrap();
        assert!(guard.key_id == "a" || guard.key_id == "b");
    }

    #[tokio::test]
    async fn weighted_strategy_works() {
        let pool = KeyPool::new(
            vec![make_key("a", 10, 100), make_key("b", 10, 1)],
            KeySelectionStrategy::Weighted,
        );
        // With weight 100 vs 1, "a" should be selected most of the time
        let mut a_count = 0;
        for _ in 0..20 {
            let guard = pool.select_key().await.unwrap();
            if guard.key_id == "a" {
                a_count += 1;
            }
        }
        assert!(
            a_count > 10,
            "weighted key 'a' should be selected most often, got {}",
            a_count
        );
    }

    #[tokio::test]
    async fn max_file_size_mb_propagated_to_guard() {
        let mut auth = HashMap::new();
        auth.insert("api_key".to_string(), "sk-test".to_string());
        let pool = KeyPool::new_with_ext(
            vec![("key-a".to_string(), auth, 5, 1, 0usize, 25.5f64)],
            KeySelectionStrategy::RoundRobin,
        );
        let guard = pool.select_key().await.unwrap();
        assert_eq!(guard.max_file_size_mb, 25.5);
    }

    #[tokio::test]
    async fn zero_max_file_size_mb_on_legacy_new() {
        // KeyPool::new (4-tuple) should default max_file_size_mb to 0.0
        let pool = KeyPool::new(vec![make_key("a", 5, 1)], KeySelectionStrategy::RoundRobin);
        let guard = pool.select_key().await.unwrap();
        assert_eq!(guard.max_file_size_mb, 0.0);
    }

    fn make_key_ext(
        id: &str,
        max: usize,
        weight: u32,
        max_input_chars: usize,
        max_file_size_mb: f64,
    ) -> (String, HashMap<String, String>, usize, u32, usize, f64) {
        let mut auth = HashMap::new();
        auth.insert("api_key".to_string(), format!("key-{}", id));
        (
            id.to_string(),
            auth,
            max,
            weight,
            max_input_chars,
            max_file_size_mb,
        )
    }

    fn make_key_ext_with_rps(
        id: &str,
        max: usize,
        weight: u32,
        max_input_chars: usize,
        max_file_size_mb: f64,
        requests_per_second: f64,
    ) -> (String, HashMap<String, String>, usize, u32, usize, f64, f64) {
        let mut auth = HashMap::new();
        auth.insert("api_key".to_string(), format!("key-{}", id));
        (
            id.to_string(),
            auth,
            max,
            weight,
            max_input_chars,
            max_file_size_mb,
            requests_per_second,
        )
    }

    #[test]
    fn test_has_eligible_keys_by_input_len() {
        let pool = KeyPool::new_with_ext(
            vec![make_key_ext("a", 5, 1, 10, 0.0)],
            KeySelectionStrategy::RoundRobin,
        );
        assert!(
            pool.has_eligible_keys(5, 0.0),
            "input_len=5 should pass max_input_chars=10"
        );
        assert!(
            !pool.has_eligible_keys(20, 0.0),
            "input_len=20 should fail max_input_chars=10"
        );
    }

    #[test]
    fn test_has_eligible_keys_by_file_size() {
        let pool = KeyPool::new_with_ext(
            vec![make_key_ext("a", 5, 1, 0, 5.0)],
            KeySelectionStrategy::RoundRobin,
        );
        assert!(
            pool.has_eligible_keys(0, 2.0),
            "file_size=2.0 should pass max_file_size_mb=5.0"
        );
        assert!(
            !pool.has_eligible_keys(0, 10.0),
            "file_size=10.0 should fail max_file_size_mb=5.0"
        );
    }

    #[tokio::test]
    async fn test_select_with_limits_filters_by_input_len() {
        let pool = KeyPool::new_with_ext(
            vec![
                make_key_ext("small", 5, 1, 5, 0.0),
                make_key_ext("large", 5, 1, 0, 0.0), // 0 = unlimited
            ],
            KeySelectionStrategy::RoundRobin,
        );
        // input_len=100 should skip "small" (max_input_chars=5) and return "large"
        let guard = pool.select_key_with_limits(100, 0.0).await.unwrap();
        assert_eq!(guard.key_id, "large");
    }

    #[tokio::test]
    async fn test_select_with_limits_no_eligible_fails_fast() {
        let pool = KeyPool::new_with_ext(
            vec![make_key_ext("small", 5, 1, 5, 0.0)],
            KeySelectionStrategy::RoundRobin,
        );
        // input_len=100 exceeds max_input_chars=5, should fail immediately (not hang)
        let result = pool.select_key_with_limits(100, 0.0).await;
        assert!(result.is_err(), "should fail when no eligible key exists");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("no eligible key"),
            "expected 'no eligible key' error, got: {}",
            err
        );
    }

    #[tokio::test]
    async fn test_select_with_limits_file_size_filter() {
        let pool = KeyPool::new_with_ext(
            vec![
                make_key_ext("small", 5, 1, 0, 1.0),
                make_key_ext("big", 5, 1, 0, 0.0), // 0.0 = unlimited
            ],
            KeySelectionStrategy::RoundRobin,
        );
        // file_size=5.0 MB should skip "small" (max_file_size_mb=1.0) and return "big"
        let guard = pool.select_key_with_limits(0, 5.0).await.unwrap();
        assert_eq!(guard.key_id, "big");
    }

    /// Verifies that dropping a guard via `notify_waiters()` unblocks all waiting tasks,
    /// not just one. With `notify_one()` the second and third waiters could hang forever.
    #[tokio::test]
    async fn test_notify_waiters_unblocks_all() {
        use std::sync::Arc;
        use tokio::time::{timeout, Duration};

        // Pool with a single key that allows only 1 concurrent acquisition.
        let pool = Arc::new(KeyPool::new(
            vec![make_key("only", 1, 1)],
            KeySelectionStrategy::RoundRobin,
        ));

        // Acquire the sole slot so all spawned tasks will have to wait.
        let first_guard = pool.select_key().await.unwrap();

        // Spawn 3 tasks that each try to acquire a guard. They will all block because
        // the key is already held by `first_guard`.
        let mut handles = Vec::new();
        for _ in 0..3 {
            let pool_clone = Arc::clone(&pool);
            handles.push(tokio::spawn(async move {
                pool_clone.select_key().await.unwrap();
                // Guard is acquired; drop it immediately.
            }));
        }

        // Give the spawned tasks a moment to reach the `.notified().await` wait point.
        tokio::time::sleep(Duration::from_millis(20)).await;

        // Dropping first_guard calls notify_waiters(), which wakes all 3 waiters at once.
        drop(first_guard);

        // All 3 tasks must complete within a generous timeout. If notify_one() were used
        // instead, only one waiter would be woken per drop and the other two would hang.
        for handle in handles {
            timeout(Duration::from_secs(2), handle)
                .await
                .expect("task timed out — notify_waiters() did not wake all waiters")
                .expect("task panicked");
        }
    }

    #[tokio::test]
    async fn key_specific_rps_rate_limit_is_enforced() {
        use tokio::time::{Duration, Instant};

        let pool = KeyPool::new_with_ext_with_rps(
            vec![make_key_ext_with_rps("only", 1, 1, 0, 0.0, 4.0)], // 4 RPS -> 250ms
            KeySelectionStrategy::RoundRobin,
        );

        let first = pool.select_key().await.unwrap();
        drop(first);

        let start = Instant::now();
        let second = pool.select_key().await.unwrap();
        let waited = start.elapsed();
        drop(second);

        assert!(
            waited >= Duration::from_millis(200),
            "expected key-level rate limit wait >=200ms, got {:?}",
            waited
        );
    }
}
