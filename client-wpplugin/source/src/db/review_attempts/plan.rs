//! Frozen manual execution inputs. Credentials only live in the encrypted attempt.
use crate::types::*;
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManualPlan {
    pub(crate) format: String,
    pub(crate) wp_base: String,
    pub(crate) content: Value,
    pub(crate) relation: DiscoveredRelation,
    pub(crate) rules: Vec<DiscoveredRule>,
    pub(crate) runtimes: BTreeMap<String, FrozenRuntime>,
    pub(crate) ordered_ids: Vec<String>,
    pub(crate) rule_bindings: RuleComponentBindingsDoc,
    pub(crate) task_type_bindings: TaskTypeComponentBindingsDoc,
    pub(crate) proxy_profiles: HashMap<String, ProxyProfile>,
}



#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FrozenRuntime {
    template: ComponentTemplate,
    auth_values: HashMap<String, String>,
    language_map: HashMap<String, String>,
    supported_content_formats: Vec<String>,
    supported_formats: Vec<String>,
    supported_business_lines: Vec<String>,
    key_pool: Option<crate::component_rt::key_pool::KeyPoolSnapshot>,
    oauth_pool: Option<crate::component_rt::oauth::OAuthPoolSnapshot>,
    oauth_configs: Option<HashMap<String, OAuthConfig>>,
    proxy_profile_id: Option<String>,
    max_concurrent: u32,
    min_interval_ms: u64,
}

impl ManualPlan {
    pub(crate) fn freeze_proxies(
        registry: &ComponentRuntimeRegistry,
        profiles: &HashMap<String, ProxyProfile>,
    ) -> Result<HashMap<String, ProxyProfile>> {
        let mut frozen = HashMap::new();
        for runtime in registry.runtimes.values() {
            if let Some(id) = &runtime.proxy_profile_id {
                let mut profile = profiles
                    .get(id)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("configured proxy is missing; retained"))?;
                ensure!(profile.enabled, "configured proxy is disabled; retained");
                crate::component_rt::proxy::validate_proxy_profile(&profile)?;
                profile.username =
                    crate::component_rt::runner::resolve_credential_reference(&profile.username)?;
                profile.password =
                    crate::component_rt::runner::resolve_credential_reference(&profile.password)?;
                frozen.insert(id.clone(), profile);
            }
        }
        Ok(frozen)
    }

    pub(crate) fn proxy_pool(&self) -> Result<crate::component_rt::proxy::ProxyClientPool> {
        for runtime in self.runtimes.values() {
            if let Some(id) = &runtime.proxy_profile_id {
                ensure!(
                    self.proxy_profiles
                        .get(id)
                        .is_some_and(|profile| profile.enabled),
                    "frozen proxy is unavailable; direct fallback refused"
                );
            }
        }
        crate::component_rt::proxy::ProxyClientPool::from_frozen(&self.proxy_profiles)
    }

    pub(crate) async fn freeze_runtimes(
        registry: &ComponentRuntimeRegistry,
    ) -> Result<BTreeMap<String, FrozenRuntime>> {
        let mut saved = BTreeMap::new();
        for (id, runtime) in &registry.runtimes {
            let oauth_configs = match &runtime.oauth_manager {
                Some(manager) => Some(manager.frozen_snapshot().await?),
                None => None,
            };
            let auth_values = runtime
                .auth_values
                .iter()
                .map(|(key, value)| {
                    Ok((
                        key.clone(),
                        crate::component_rt::runner::resolve_credential_reference(value)?,
                    ))
                })
                .collect::<Result<HashMap<String, String>>>()?;
            saved.insert(
                id.clone(),
                FrozenRuntime {
                    template: runtime.template.clone(),
                    auth_values,
                    language_map: runtime.language_map.clone(),
                    supported_content_formats: runtime.supported_content_formats.clone(),
                    supported_formats: runtime.supported_formats.clone(),
                    supported_business_lines: runtime.supported_business_lines.clone(),
                    key_pool: runtime
                        .key_pool
                        .as_ref()
                        .map(|pool| pool.frozen_snapshot())
                        .transpose()?,
                    oauth_pool: runtime.oauth_pool.as_ref().map(|pool| pool.snapshot()),
                    oauth_configs,
                    proxy_profile_id: runtime.proxy_profile_id.clone(),
                    max_concurrent: runtime.runtime_max_concurrent_requests,
                    min_interval_ms: runtime.runtime_min_interval_ms,
                },
            );
        }
        Ok(saved)
    }

    pub(crate) fn registry(
        self,
        _client: &reqwest::Client,
        db: Arc<tokio::sync::Mutex<rusqlite::Connection>>,
    ) -> Result<ComponentRuntimeRegistry> {
        ensure!(
            self.format == "manual-plan-v1",
            "manual plan format differs; retained"
        );
        let proxy_pool = self.proxy_pool()?;
        let mut runtimes = HashMap::new();
        for (id, saved) in self.runtimes {
            ensure!(
                saved.oauth_pool.is_none() || saved.oauth_configs.is_some(),
                "manual OAuth plan is incomplete; retained"
            );
            let runtime = ComponentRuntime {
                template: saved.template,
                auth_values: saved.auth_values,
                language_map: saved.language_map,
                supported_content_formats: saved.supported_content_formats,
                supported_formats: saved.supported_formats,
                supported_business_lines: saved.supported_business_lines,
                key_pool: saved.key_pool.map(|snapshot| {
                    Arc::new(crate::component_rt::key_pool::KeyPool::from_snapshot(
                        snapshot,
                    ))
                }),
                oauth_pool: saved.oauth_pool.map(|snapshot| {
                    Arc::new(crate::component_rt::oauth::OAuthPool::from_snapshot(
                        snapshot,
                    ))
                }),
                oauth_manager: saved
                    .oauth_configs
                    .map(|configs| {
                        Ok::<_, anyhow::Error>(Arc::new(
                            crate::component_rt::oauth::OAuthTokenManager::from_snapshot(
                                configs,
                                proxy_pool
                                    .get_oauth_client(saved.proxy_profile_id.as_deref())?
                                    .clone(),
                            )
                            .with_recovery_db(db.clone()),
                        ))
                    })
                    .transpose()?,
                proxy_profile_id: saved.proxy_profile_id,
                runtime_max_concurrent_requests: saved.max_concurrent,
                runtime_min_interval_ms: saved.min_interval_ms,
                runtime_concurrency_sem: (saved.max_concurrent > 0)
                    .then(|| Arc::new(tokio::sync::Semaphore::new(saved.max_concurrent as usize))),
                runtime_last_request_at: (saved.min_interval_ms > 0).then(|| {
                    Arc::new(tokio::sync::Mutex::new(
                        std::time::Instant::now() - std::time::Duration::from_secs(60),
                    ))
                }),
            };
            runtimes.insert(id, runtime);
        }
        ensure!(
            self.ordered_ids.iter().all(|id| runtimes.contains_key(id)),
            "manual runtime order differs; retained"
        );
        Ok(ComponentRuntimeRegistry {
            runtimes,
            ordered_ids: self.ordered_ids,
        })
    }
}
