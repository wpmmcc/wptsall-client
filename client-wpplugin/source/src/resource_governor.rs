//! Global resource governor for multi-domain concurrent processing.
//!
//! Controls:
//! - Maximum concurrent domains being processed
//! - Global translation API concurrency budget
//! - Global WP callback concurrency budget
//!
//! Each semaphore is independent so that, e.g., 3 domains can share 30 translation
//! slots without any single domain monopolising the pool.

use std::sync::Arc;
use tokio::sync::Semaphore;

use crate::config::env_usize;

/// Default maximum concurrent domains.
const DEFAULT_DOMAIN_CONCURRENCY: usize = 3;
/// Default global translation API concurrency.
const DEFAULT_GLOBAL_TRANSLATION_CONCURRENCY: usize = 30;
/// Default global WP callback concurrency.
const DEFAULT_GLOBAL_CALLBACK_CONCURRENCY: usize = 12;

/// Centralized resource budgets shared across all domain workers.
pub(crate) struct ResourceGovernor {
    /// Limits how many domains are processed concurrently.
    pub(crate) domain_sem: Arc<Semaphore>,
    /// Global budget for outbound translation API calls.
    pub(crate) global_translation_sem: Arc<Semaphore>,
    /// Global budget for WP callback submissions.
    pub(crate) global_callback_sem: Arc<Semaphore>,
    /// Config snapshot for logging/introspection.
    pub(crate) domain_concurrency: usize,
    pub(crate) global_translation_concurrency: usize,
    pub(crate) global_callback_concurrency: usize,
}

impl ResourceGovernor {
    /// Create a new governor reading limits from environment variables.
    ///
    /// | Variable | Default | Description |
    /// |----------|---------|-------------|
    /// | `WPTSALL_DOMAIN_CONCURRENCY` | 3 | Max concurrent domains |
    /// | `WPTSALL_GLOBAL_TRANSLATION_CONCURRENCY` | 30 | Global translation API concurrency |
    /// | `WPTSALL_GLOBAL_CALLBACK_CONCURRENCY` | 12 | Global WP callback concurrency |
    pub(crate) fn from_env() -> Self {
        let domain_concurrency =
            env_usize("WPTSALL_DOMAIN_CONCURRENCY", DEFAULT_DOMAIN_CONCURRENCY).max(1);
        let global_translation_concurrency = env_usize(
            "WPTSALL_GLOBAL_TRANSLATION_CONCURRENCY",
            DEFAULT_GLOBAL_TRANSLATION_CONCURRENCY,
        )
        .max(1);
        let global_callback_concurrency = env_usize(
            "WPTSALL_GLOBAL_CALLBACK_CONCURRENCY",
            DEFAULT_GLOBAL_CALLBACK_CONCURRENCY,
        )
        .max(1);

        Self {
            domain_sem: Arc::new(Semaphore::new(domain_concurrency)),
            global_translation_sem: Arc::new(Semaphore::new(global_translation_concurrency)),
            global_callback_sem: Arc::new(Semaphore::new(global_callback_concurrency)),
            domain_concurrency,
            global_translation_concurrency,
            global_callback_concurrency,
        }
    }

    /// Create a governor with explicit limits (for Web UI where DB-stored config overrides env).
    pub(crate) fn with_limits(
        domain_concurrency: usize,
        global_translation_concurrency: usize,
        global_callback_concurrency: usize,
    ) -> Self {
        let dc = domain_concurrency.max(1);
        let gtc = global_translation_concurrency.max(1);
        let gcc = global_callback_concurrency.max(1);
        Self {
            domain_sem: Arc::new(Semaphore::new(dc)),
            global_translation_sem: Arc::new(Semaphore::new(gtc)),
            global_callback_sem: Arc::new(Semaphore::new(gcc)),
            domain_concurrency: dc,
            global_translation_concurrency: gtc,
            global_callback_concurrency: gcc,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_governor_has_sane_defaults() {
        let gov = ResourceGovernor::with_limits(
            DEFAULT_DOMAIN_CONCURRENCY,
            DEFAULT_GLOBAL_TRANSLATION_CONCURRENCY,
            DEFAULT_GLOBAL_CALLBACK_CONCURRENCY,
        );
        assert_eq!(gov.domain_concurrency, 3);
        assert_eq!(gov.global_translation_concurrency, 30);
        assert_eq!(gov.global_callback_concurrency, 12);
    }

    #[test]
    fn governor_enforces_minimum_one() {
        let gov = ResourceGovernor::with_limits(0, 0, 0);
        assert_eq!(gov.domain_concurrency, 1);
        assert_eq!(gov.global_translation_concurrency, 1);
        assert_eq!(gov.global_callback_concurrency, 1);
    }

    #[tokio::test]
    async fn domain_semaphore_limits_concurrency() {
        let gov = ResourceGovernor::with_limits(2, 10, 10);
        let _p1 = gov.domain_sem.acquire().await.unwrap();
        let _p2 = gov.domain_sem.acquire().await.unwrap();
        // Third acquire would block — verify with try_acquire
        assert!(gov.domain_sem.try_acquire().is_err());
    }
}
