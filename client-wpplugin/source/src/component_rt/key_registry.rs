//! Global concurrency registry for API keys.
//!
//! When the same key_id is used by multiple `KeyPool` instances (e.g., a
//! vendor key referenced from two different component runtimes), each pool
//! would normally maintain an independent `active_count`. This can cause the
//! real concurrent request count to exceed the per-key limit.
//!
//! `GlobalKeyRegistry` solves this by providing a shared `Arc<AtomicUsize>`
//! counter for each `key_id`. All `KeyPool` instances that receive a registry
//! use the global counter instead of their local one.

use std::collections::HashMap;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};

/// Global per-key concurrency counter shared across all `KeyPool` instances.
///
/// The registry maps `key_id → Arc<AtomicUsize>` so that every pool entry
/// for the same key shares the same underlying counter.
#[derive(Clone, Default)]
pub(crate) struct GlobalKeyRegistry {
    inner: Arc<Mutex<HashMap<String, Arc<AtomicUsize>>>>,
}

impl GlobalKeyRegistry {
    #[allow(dead_code)]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Return (or create) the shared counter for `key_id`.
    pub(crate) fn get_or_create(&self, key_id: &str) -> Arc<AtomicUsize> {
        let mut map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        map.entry(key_id.to_string())
            .or_insert_with(|| Arc::new(AtomicUsize::new(0)))
            .clone()
    }
}

#[cfg(test)]
mod tests {
    // catalog: WEBUI-MOD-component-rt-key-registry-rs
    // oracle: L1
    use super::*;

    #[test]
    fn same_key_shares_one_counter_across_lookups() {
        let registry = GlobalKeyRegistry::new();
        let a = registry.get_or_create("key-a");
        let b = registry.get_or_create("key-a");
        assert!(
            Arc::ptr_eq(&a, &b),
            "the same key_id must resolve to one shared counter"
        );

        a.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            b.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "increments through one handle must be visible through the other"
        );
    }

    #[test]
    fn different_keys_get_independent_counters() {
        let registry = GlobalKeyRegistry::new();
        let a = registry.get_or_create("key-a");
        let b = registry.get_or_create("key-b");
        assert!(!Arc::ptr_eq(&a, &b));

        a.fetch_add(3, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            b.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "per-key limits must not bleed across key ids"
        );
    }

    #[test]
    fn cloned_registries_share_the_underlying_counters() {
        let registry = GlobalKeyRegistry::new();
        let clone = registry.clone();
        let a = registry.get_or_create("key-a");
        let b = clone.get_or_create("key-a");
        assert!(
            Arc::ptr_eq(&a, &b),
            "registry clones (one per KeyPool instance) must see the same counters"
        );
    }
}
