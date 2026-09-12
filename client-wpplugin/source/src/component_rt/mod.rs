// Built-in simulated translator removed (2026-02-11): translation now handled
// by external component API (mock-translate-api service).
pub(crate) mod contract;
pub mod key_pool;
pub(crate) mod key_registry;
pub(crate) mod loader;
pub(crate) mod non_text;
pub(crate) mod oauth;
pub(crate) mod proxy;
pub(crate) mod runner;
pub(crate) mod selector;
pub(crate) mod sign_plugin;

#[cfg(test)]
mod tests {
    // catalog: WEBUI-MOD-component-rt-mod-rs
    // oracle: L1
    // The module root is a pure wiring surface: the contract under test is
    // that the documented public entry point (key_pool) is reachable through
    // this module path and behaves as documented for the empty pool case.
    use crate::component_rt::key_pool::KeyPool;
    use crate::types::KeySelectionStrategy;

    #[test]
    fn key_pool_public_entry_point_is_wired_through_module_root() {
        let pool = KeyPool::new(Vec::new(), KeySelectionStrategy::default());
        assert!(pool.is_empty(), "an empty key list must build an empty pool");
    }
}
