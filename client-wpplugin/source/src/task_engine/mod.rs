pub mod discoverer;
pub(crate) mod executor;
pub(crate) mod pipeline;
pub(crate) mod submitter;
pub(crate) mod workflow_dsl;
pub(crate) mod workflow_interpreter;
pub(crate) mod workflow_policy;

#[cfg(test)]
mod tests {
    // catalog: WEBUI-MOD-task-engine-mod-rs
    // oracle: L1
    // Module-root wiring contract: the documented engine surface must be
    // reachable through this module path. The discoverer pipeline itself
    // is exercised in discoverer/tests.rs; here the RAII run-scope override
    // is driven through its public entry point with a no-op value (0) so
    // the process-global override is observably unchanged (0 -> 0 -> 0)
    // and no parallel test can be affected.
    use crate::task_engine::discoverer::scoped_max_items_per_run_override;

    #[test]
    fn discoverer_surface_is_wired_through_module_root() {
        // No override requested -> no guard.
        assert!(scoped_max_items_per_run_override(None).is_none());

        // A guard round-trips the process-global override and restores it
        // on drop (RAII); using 0 keeps every intermediate value identical
        // to the un-set state.
        {
            let _guard = scoped_max_items_per_run_override(Some(0));
        }
        // Dropping again with another 0 must be equally inert.
        {
            let _guard = scoped_max_items_per_run_override(Some(0));
            let _also = scoped_max_items_per_run_override(Some(0));
        }
    }
}
