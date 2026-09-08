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
