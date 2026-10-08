use super::*;
use crate::db::jobs;
use std::sync::atomic::{AtomicUsize, Ordering};

#[path = "language_pack_process.rs"]
mod process;

struct PackFixture {
    _key: crate::db::TestEnvVarGuard,
    root: std::path::PathBuf,
    db: Arc<tokio::sync::Mutex<rusqlite::Connection>>,
    job: i64,
    relation: DiscoveredRelation,
    source: Vec<LanguagePackItem>,
    payload: I18nCallbackPayload,
    params: DiscoveryTaskParams,
}

impl PackFixture {
    async fn new() -> Self {
        let key = crate::db::owned_mock_bindings_key();
        let root = tempfile::tempdir().unwrap().keep();
        let conn = crate::db::open_db(root.join("owned.sqlite").to_str().unwrap()).unwrap();
        let job = jobs::create_job(
            &conn,
            &jobs::CreateJobRequest {
                domain: "http://127.0.0.1/site-a/wp-json/wptsall/v2/owned/client".into(),
                relation_id: 7,
                business_line: "discovery".into(),
                triggered_by: "auto".into(),
            },
        )
        .unwrap();
        Self {
            _key: key,
            root,
            db: Arc::new(tokio::sync::Mutex::new(conn)),
            job,
            relation: sample_relation(),
            source: vec![LanguagePackItem {
                object_id: 81,
                text_domain: "owned-pack".into(),
                complete_data: LanguagePackCompleteData {
                    entry_id: 101,
                    msgid: "Owned source".into(),
                    msgctxt: "Owned context".into(),
                    msgid_plural: "Owned sources".into(),
                    plural_index: Some(1),
                    text_domain: "owned-pack".into(),
                },
            }],
            payload: I18nCallbackPayload {
                business_line: "plugin_i18n".into(),
                relation_id: 7,
                client_task_id: "owned-pack-callback".into(),
                worker_id: "owned-worker".into(),
                source_lang: "en".into(),
                target_lang: "zh".into(),
                entries: vec![I18nCallbackEntry {
                    entry_id: 101,
                    msgstr: "Owned saved translation".into(),
                }],
            },
            params: DiscoveryTaskParams::default(),
        }
    }

    async fn persist(&self) -> anyhow::Result<PersistedLanguagePackBatch> {
        self.persist_for("http://127.0.0.1/site-a/wp-json/wptsall/v2/owned/client")
            .await
    }

    async fn persist_for(&self, base: &str) -> anyhow::Result<PersistedLanguagePackBatch> {
        persist_language_pack_batch_for_sync(
            &self.db,
            self.job,
            self.root.to_str().unwrap(),
            "127.0.0.1",
            base,
            &self.relation,
            &self.relation,
            "plugin_i18n",
            "plugin",
            &self.source,
            &self.payload.entries,
            &self.payload.client_task_id,
            Some("owned-private-route-secret"),
            &self.payload,
            "comp-selected",
            &self.params,
        )
        .await
    }
}

#[tokio::test]
async fn language_pack_recovery_artifacts_encrypt_content_and_route_secret() {
    let f = PackFixture::new().await;
    let saved = f.persist().await.unwrap();
    let item = jobs::get_item_checked(&*f.db.lock().await, saved.item_id)
        .unwrap()
        .unwrap();
    for path in [&item.raw_path, &item.translated_path] {
        let bytes = std::fs::read(path).unwrap();
        assert!(
            bytes.starts_with(b"WPTC"),
            "saved pack artifact must be encrypted"
        );
        assert!(!bytes
            .windows(26)
            .any(|v| v == b"owned-private-route-secret"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}

#[tokio::test]
async fn language_pack_recovery_changed_body_retains_original_artifact_and_item() {
    let mut f = PackFixture::new().await;
    let saved = f.persist().await.unwrap();
    let before_bytes = std::fs::read(&saved.translated_path).unwrap();
    let before = serde_json::to_value(
        jobs::get_item_checked(&*f.db.lock().await, saved.item_id)
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    f.payload.entries[0].msgstr = "A different paid result".into();
    f.payload.client_task_id = "different-callback".into();
    assert!(
        f.persist().await.is_err(),
        "unresolved saved batch must fence changed body"
    );
    assert_eq!(std::fs::read(&saved.translated_path).unwrap(), before_bytes);
    assert_eq!(
        serde_json::to_value(
            jobs::get_item_checked(&*f.db.lock().await, saved.item_id)
                .unwrap()
                .unwrap()
        )
        .unwrap(),
        before
    );
}

#[tokio::test]
async fn language_pack_recovery_completed_replay_never_reopens_the_item() {
    let f = PackFixture::new().await;
    let saved = f.persist().await.unwrap();
    {
        let conn = f.db.lock().await;
        jobs::update_item_status(&conn, saved.item_id, "done", None).unwrap();
    }
    let bytes = std::fs::read(&saved.translated_path).unwrap();
    let replay = f.persist().await.unwrap();
    assert_eq!(replay.item_id, saved.item_id);
    assert_eq!(std::fs::read(&saved.translated_path).unwrap(), bytes);
    assert_eq!(
        jobs::get_item_checked(&*f.db.lock().await, saved.item_id)
            .unwrap()
            .unwrap()
            .status,
        "done"
    );
}

#[tokio::test]
async fn language_pack_recovery_failed_projection_cannot_replace_saved_files() {
    let mut f = PackFixture::new().await;
    let saved = f.persist().await.unwrap();
    let bytes = std::fs::read(&saved.translated_path).unwrap();
    f.db.lock()
        .await
        .execute_batch(
            "CREATE TRIGGER refuse_pack_path BEFORE UPDATE OF translated_path ON translation_items
         BEGIN SELECT RAISE(ABORT, 'owned path fault'); END;",
        )
        .unwrap();
    f.payload.entries[0].msgstr = "Uncommitted replacement".into();
    assert!(f.persist().await.is_err());
    assert_eq!(std::fs::read(&saved.translated_path).unwrap(), bytes);
}

#[tokio::test]
async fn language_pack_recovery_same_host_installations_have_separate_artifacts() {
    let f = PackFixture::new().await;
    let a = f.persist().await.unwrap();
    let b = f
        .persist_for("http://127.0.0.1/site-b/wp-json/wptsall/v2/owned/client")
        .await
        .unwrap();
    assert_ne!(a.translated_path, b.translated_path);
    assert_ne!(a.item_id, b.item_id);
}

#[tokio::test]
async fn language_pack_recovery_missing_or_duplicate_source_refuses_projection() {
    let mut f = PackFixture::new().await;
    f.source.clear();
    assert!(f.persist().await.is_err());
    assert_eq!(
        jobs::list_items_by_job(&*f.db.lock().await, f.job, None)
            .unwrap()
            .len(),
        0
    );
    let mut f = PackFixture::new().await;
    f.payload.entries.push(f.payload.entries[0].clone());
    assert!(f.persist().await.is_err());
    assert_eq!(
        jobs::list_items_by_job(&*f.db.lock().await, f.job, None)
            .unwrap()
            .len(),
        0
    );
}

async fn pack_provider() -> (
    reqwest::Client,
    ComponentRuntime,
    Arc<AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = vec![0; 8192];
            socket.read(&mut bytes).await.unwrap();
            observed.fetch_add(1, Ordering::SeqCst);
            let body = r#"{"text":"Owned provider translation"}"#;
            socket.write_all(format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(), body
            ).as_bytes()).await.unwrap();
        }
    });
    let mut registry = sample_registry();
    let mut runtime = registry.runtimes.remove("comp-selected").unwrap();
    runtime.template.request.url = format!("http://{addr}/translate");
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    (client, runtime, calls, server)
}

#[tokio::test]
async fn language_pack_recovery_paid_entry_replays_without_another_provider_call() {
    let f = PackFixture::new().await;
    let (client, runtime, calls, server) = pack_provider().await;
    let constraints = EffectiveConstraints::resolve(None, None, None, 10_000, "none");
    for _ in 0..2 {
        let translated = translate_language_pack_entry(
            &client,
            &runtime,
            Some(&f.db),
            "http://127.0.0.1/site-a/wp-json/wptsall/v2/owned/client",
            &f.relation,
            "plugin_i18n",
            "plugin",
            &f.source[0],
            &f.params,
            &constraints,
        )
        .await
        .unwrap();
        assert_eq!(translated, "Owned provider translation");
    }
    server.abort();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "paid entry must have a durable identity"
    );
}

#[tokio::test]
async fn language_pack_recovery_no_authority_refuses_before_provider_egress() {
    let f = PackFixture::new().await;
    let (client, runtime, calls, server) = pack_provider().await;
    let constraints = EffectiveConstraints::resolve(None, None, None, 10_000, "none");
    let result = translate_language_pack_entry(
        &client,
        &runtime,
        None,
        "http://127.0.0.1/site-a/wp-json/wptsall/v2/owned/client",
        &f.relation,
        "plugin_i18n",
        "plugin",
        &f.source[0],
        &f.params,
        &constraints,
    )
    .await;
    server.abort();
    assert!(
        result.is_err(),
        "language-pack fees require durable authority"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

async fn pack_manual_plan(
    f: &PackFixture,
    saved: &PersistedLanguagePackBatch,
    runtime: ComponentRuntime,
    request: &str,
) -> crate::web_ui::test_support::WebUiTestHarness {
    let item = jobs::get_item_checked(&*f.db.lock().await, saved.item_id)
        .unwrap()
        .unwrap();
    let mut content: serde_json::Value = serde_json::from_str(
        &crate::bindings::load_encrypted_or_plain(std::path::Path::new(&item.raw_path)).unwrap(),
    )
    .unwrap();
    content["__manual_i18n_envelope"] = serde_json::from_str(
        &crate::bindings::load_encrypted_or_plain(std::path::Path::new(&item.translated_path))
            .unwrap(),
    )
    .unwrap();
    content["__manual_i18n_limits"] = json!({"max_input_chars":10_000,"split_strategy":"none"});
    let registry = ComponentRuntimeRegistry {
        runtimes: HashMap::from([("comp-selected".into(), runtime)]),
        ordered_ids: vec!["comp-selected".into()],
    };
    let plan = crate::db::review_attempts::plan::ManualPlan {
        format: "manual-plan-v1".into(),
        wp_base: item.domain.clone(),
        content,
        relation: f.relation.clone(),
        rules: vec![],
        runtimes: crate::db::review_attempts::plan::ManualPlan::freeze_runtimes(&registry)
            .await
            .unwrap(),
        ordered_ids: registry.ordered_ids.clone(),
        rule_bindings: RuleComponentBindingsDoc::default(),
        task_type_bindings: TaskTypeComponentBindingsDoc::default(),
        proxy_profiles: HashMap::new(),
    };
    let lease = crate::db::unit_lock::UnitLease::item(&f.db, saved.item_id)
        .await
        .unwrap();
    crate::db::review_attempts::scope(
        &*f.db.lock().await,
        &f.db,
        &lease,
        &item,
        &serde_json::to_value(plan).unwrap(),
        request,
    )
    .unwrap();
    drop(lease);
    let harness = crate::web_ui::test_support::WebUiTestHarness::new("", None)
        .await
        .unwrap();
    harness.state.lock().await.db = f.db.clone();
    harness
}

#[tokio::test]
async fn language_pack_recovery_manual_request_replays_encrypted_frozen_result() {
    let f = PackFixture::new().await;
    let saved = review_pack(&f).await;
    let original = std::fs::read(&saved.translated_path).unwrap();
    let (client, runtime, calls, server) = pack_provider().await;
    drop(client);
    let request = "550e8400-e29b-41d4-a716-446655440055";
    let harness = pack_manual_plan(&f, &saved, runtime, request).await;
    for _ in 0..2 {
        let response = harness
            .post_json(
                &format!("/api/items/{}/retranslate", saved.item_id),
                json!({"request_id":request,"resume_only":true}),
            )
            .await
            .unwrap();
        assert!(
            response.status_line.contains("200"),
            "language-pack manual request must resume: {} {}",
            response.status_line,
            response.body
        );
    }
    server.abort();
    let item = jobs::get_item_checked(&*f.db.lock().await, saved.item_id)
        .unwrap()
        .unwrap();
    assert_eq!(item.status, "pending_review");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "one entry, one paid request"
    );
    assert_ne!(item.translated_path, saved.translated_path);
    assert!(std::fs::read(&item.translated_path)
        .unwrap()
        .starts_with(b"WPTC"));
    assert_eq!(std::fs::read(&saved.translated_path).unwrap(), original);
    let result: serde_json::Value = serde_json::from_str(
        &crate::bindings::load_encrypted_or_plain(std::path::Path::new(&item.translated_path))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(result["payload"]["entries"][0]["entry_id"], 101);
    assert_eq!(
        result["payload"]["entries"][0]["msgstr"],
        "Owned provider translation"
    );
    assert_eq!(
        crate::db::review_attempts::pending_request(&*f.db.lock().await, saved.item_id).unwrap(),
        None
    );
}

#[tokio::test]
async fn language_pack_recovery_manual_orphan_projection_resumes_without_source_or_fee() {
    let f = PackFixture::new().await;
    let saved = review_pack(&f).await;
    let (_, runtime, calls, server) = pack_provider().await;
    let request = "550e8400-e29b-41d4-a716-446655440056";
    let harness = pack_manual_plan(&f, &saved, runtime, request).await;
    f.db.lock().await.execute_batch(
        "CREATE TRIGGER refuse_manual_pack BEFORE UPDATE OF translated_path ON translation_items
         BEGIN SELECT RAISE(ABORT, 'owned manual projection fault'); END;"
    ).unwrap();
    let response = harness
        .post_json(
            &format!("/api/items/{}/retranslate", saved.item_id),
            json!({"request_id":request,"resume_only":true}),
        )
        .await
        .unwrap();
    assert!(!response.status_line.contains("200"));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "fault must occur after saving a paid pack result"
    );
    server.abort();
    let item = jobs::get_item_checked(&*f.db.lock().await, saved.item_id)
        .unwrap()
        .unwrap();
    std::fs::remove_file(&item.raw_path).unwrap();
    f.db.lock()
        .await
        .execute_batch("DROP TRIGGER refuse_manual_pack")
        .unwrap();
    let response = harness
        .post_json(
            &format!("/api/items/{}/retranslate", saved.item_id),
            json!({"request_id":request,"resume_only":true}),
        )
        .await
        .unwrap();
    assert!(
        response.status_line.contains("200"),
        "orphan recovery failed: {}",
        response.body
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        crate::db::review_attempts::pending_request(&*f.db.lock().await, saved.item_id).unwrap(),
        None
    );
}

struct FreshPackEndpoint {
    base: String,
    requests: Arc<std::sync::Mutex<Vec<String>>>,
    worker: tokio::task::JoinHandle<()>,
}

impl Drop for FreshPackEndpoint {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

impl FreshPackEndpoint {
    async fn start(relation: &DiscoveredRelation, translated: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = requests.clone();
        let relations = json!({"relations":[relation]}).to_string();
        let translated = translated.to_owned();
        let worker = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut bytes = Vec::new();
                let mut buffer = [0; 4096];
                loop {
                    let n = socket.read(&mut buffer).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buffer[..n]);
                    if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if bytes.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                let request = String::from_utf8(bytes).unwrap();
                let line = request.lines().next().unwrap_or("");
                let body = if line.contains("/site-relations") {
                    relations.clone()
                } else if line.contains("/token") {
                    json!({"access_token":"owned-fresh-pack-token","expires_in":3600}).to_string()
                } else {
                    json!({"text":translated}).to_string()
                };
                observed.lock().unwrap().push(request);
                let signature = crate::web_ui::test_support::sign_wp_plaintext_response(
                    "owned-fresh-pack-site-token",
                    body.as_bytes(),
                );
                let _ = socket.write_all(format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-WPTSALL-Response-Signature: {signature}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                ).as_bytes()).await;
            }
        });
        Self {
            base,
            requests,
            worker,
        }
    }
}

async fn fresh_manual_pack_contract(variant: &str) {
    let f = PackFixture::new().await;
    let _transport = crate::db::TestEnvVarGuard::set("WPTSALL_WP_TRANSPORT_ENCRYPT", "off");
    let _input_limit = crate::db::TestEnvVarGuard::set("WPTSALL_DEFAULT_MAX_INPUT_CHARS", "0");
    let wp = FreshPackEndpoint::start(&f.relation, "Owned WP, not a provider").await;
    let snapshot = FreshPackEndpoint::start(&f.relation, "Wrong snapshot provider").await;
    let selected = FreshPackEndpoint::start(&f.relation, "Owned selected translation").await;
    let proxy = FreshPackEndpoint::start(&f.relation, "Owned selected translation").await;
    let base = format!("{}/site-a/wp-json/wptsall/v2/owned/client", wp.base);
    let saved = review_pack_for(&f, &base).await;
    let original = std::fs::read(&saved.translated_path).unwrap();
    let harness = crate::web_ui::test_support::WebUiTestHarness::new(&wp.base, None)
        .await
        .unwrap();
    let _db_path = crate::db::TestEnvVarGuard::set(
        "WPTSALL_DB_PATH",
        f.root.join("owned.sqlite").display().to_string(),
    );
    let oauth = variant == "oauth_proxy";
    let mut template =
        serde_json::to_value(sample_registry().runtimes["comp-selected"].template.clone()).unwrap();
    template["request"]["url"] = json!(format!("{}/translate-snapshot", snapshot.base));
    template["editable_params"]
        .as_array_mut()
        .unwrap()
        .push(json!({"path":"request.url"}));
    if oauth {
        template["auth"] =
            json!({"fields":[{"name":"access_token","required":true,"secret":true}]});
        template["request"]["headers"] = json!({"Authorization":"Bearer {{auth.access_token}}"});
    }
    let mut version = json!({
        "version":"1","created_at":"1","auth_type":if oauth {"oauth"} else {"key"},
        "config_overrides":{"request.url":format!("{}/translate-version",selected.base)},
    });
    if matches!(variant, "oauth_proxy" | "missing_proxy") {
        version["proxy_profile_id"] = json!("owned-fresh-pack-proxy");
    }
    let doc: ComponentsLocalDoc = serde_json::from_value(json!({
        "components":{"comp-selected":{
            "vendor_id":"owned","name":"Owned selected pack","kind":"text","enabled":true,
            "template_id":"comp-selected","created_at":"1","active_version":"v1",
            "template_json":template,"versions":{"v1":version},
        }}
    }))
    .unwrap();
    {
        let conn = f.db.lock().await;
        crate::db::system::set_system_config(&conn, "json_migration_done", "1").unwrap();
        crate::db::components::save_local_components_doc(&conn, &doc).unwrap();
    }
    {
        let mut state = harness.state.lock().await;
        state.db = f.db.clone();
        state.domain_token_bindings.domains.insert(
            crate::bindings::normalize_domain_base(&base),
            DomainTokenBindingEntry {
                wp_client_token: "owned-fresh-pack-site-token".into(),
                route_secret: "owned".into(),
                ..Default::default()
            },
        );
        if oauth {
            state.component_bindings.components.insert(
                "comp-selected".into(),
                serde_json::from_value(json!({"oauth_ids":["owned-fresh-pack-oauth"]})).unwrap(),
            );
        }
    }
    if oauth {
        let proxy_url = url::Url::parse(&proxy.base).unwrap();
        crate::bindings::save_proxy_profiles(
            &crate::config::proxy_profiles_file(),
            &ProxyProfilesDoc {
                version: 2,
                profiles: HashMap::from([(
                    "owned-fresh-pack-proxy".into(),
                    ProxyProfile {
                        name: "Owned fresh pack proxy".into(),
                        protocol: "http".into(),
                        host: "127.0.0.1".into(),
                        port: proxy_url.port().unwrap(),
                        username: String::new(),
                        password: String::new(),
                        enabled: true,
                    },
                )]),
            },
        )
        .unwrap();
        let config: OAuthConfig = serde_json::from_value(json!({
            "vendor_id":"owned","label":"Owned fresh pack OAuth","grant_type":"client_credentials",
            "token_url":format!("{}/token",selected.base),"client_id":"owned-fresh-pack-client",
            "client_secret":"owned-fresh-pack-mock-secret",
        }))
        .unwrap();
        crate::bindings::save_vendor_oauth(
            &crate::config::vendor_oauth_file(),
            &VendorOAuthDoc {
                version: 2,
                configs: HashMap::from([("owned-fresh-pack-oauth".into(), config)]),
            },
        )
        .unwrap();
    }
    if variant == "damaged_keys" {
        std::fs::write(crate::config::vendor_keys_file(), "{").unwrap();
    }
    let request = "631b8b74-913a-4972-8892-33a892ce0730";
    assert!(
        crate::db::review_attempts::saved_plan(&*f.db.lock().await, saved.item_id, request)
            .unwrap()
            .is_none(),
        "fresh-route proof must not seed a frozen manual plan"
    );
    let response = harness
        .post_json(
            &format!("/api/items/{}/retranslate", saved.item_id),
            json!({"request_id":request}),
        )
        .await
        .unwrap();
    if matches!(variant, "missing_proxy" | "damaged_keys") {
        assert!(
            !response.status_line.contains("200"),
            "invalid fresh runtime must refuse before any source or provider request: {}",
            response.body
        );
        assert!(wp.requests.lock().unwrap().is_empty());
        assert!(snapshot.requests.lock().unwrap().is_empty());
        assert!(selected.requests.lock().unwrap().is_empty());
        assert!(proxy.requests.lock().unwrap().is_empty());
        assert_eq!(std::fs::read(&saved.translated_path).unwrap(), original);
        assert!(crate::db::review_attempts::saved_plan(
            &*f.db.lock().await,
            saved.item_id,
            request
        )
        .unwrap()
        .is_none());
        return;
    }
    assert!(
        response.status_line.contains("200"),
        "fresh selected pack runtime must build: {}",
        response.body
    );
    assert!(
        snapshot.requests.lock().unwrap().is_empty(),
        "selected version must not dispatch the old snapshot URL"
    );
    assert_eq!(
        wp.requests.lock().unwrap().len(),
        1,
        "only site-relations, no generic content rules"
    );
    let observed = if oauth {
        &proxy.requests
    } else {
        &selected.requests
    };
    let requests = observed.lock().unwrap().clone();
    assert_eq!(requests.len(), if oauth { 2 } else { 1 });
    let provider = requests.last().unwrap();
    assert!(provider
        .lines()
        .next()
        .unwrap()
        .contains("/translate-version"));
    assert!(
        provider.contains("Owned sources"),
        "plural source, not an earlier translation"
    );
    if oauth {
        assert!(
            selected.requests.lock().unwrap().is_empty(),
            "both OAuth and provider must use the chosen proxy"
        );
        assert!(requests[0].lines().next().unwrap().contains("/token"));
        assert!(provider
            .to_ascii_lowercase()
            .contains("authorization: bearer owned-fresh-pack-token"));
    }
    let item = jobs::get_item_checked(&*f.db.lock().await, saved.item_id)
        .unwrap()
        .unwrap();
    let result: serde_json::Value = serde_json::from_str(
        &crate::bindings::load_encrypted_or_plain(std::path::Path::new(&item.translated_path))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(result["payload"]["entries"][0]["entry_id"], 101);
    assert_eq!(
        result["payload"]["entries"][0]["msgstr"],
        "Owned selected translation"
    );
    assert_eq!(item.status, "pending_review");
    assert!(std::fs::read(&item.translated_path)
        .unwrap()
        .starts_with(b"WPTC"));
    assert_eq!(std::fs::read(&saved.translated_path).unwrap(), original);
    let frozen =
        crate::db::review_attempts::saved_plan(&*f.db.lock().await, saved.item_id, request)
            .unwrap()
            .unwrap();
    assert_eq!(
        frozen["content"]["__manual_i18n_limits"]["max_input_chars"],
        0
    );
    assert_eq!(
        frozen["runtimes"]["comp-selected"]["template"]["request"]["url"],
        format!("{}/translate-version", selected.base)
    );
    if oauth {
        assert_eq!(
            frozen["runtimes"]["comp-selected"]["proxy_profile_id"],
            "owned-fresh-pack-proxy"
        );
        assert!(frozen["runtimes"]["comp-selected"]["oauth_configs"].is_object());
    }
    // Only owned artifacts/configuration are changed. Completed receipt replay
    // must not consult current provider, source or configuration authority.
    std::fs::remove_file(&item.raw_path).unwrap();
    crate::db::system::set_system_config(&*f.db.lock().await, "local_components_doc", "{").unwrap();
    let again = harness
        .post_json(
            &format!("/api/items/{}/retranslate", saved.item_id),
            json!({"request_id":request,"resume_only":true}),
        )
        .await
        .unwrap();
    assert!(
        again.status_line.contains("200"),
        "receipt replay must precede current runtime loading: {}",
        again.body
    );
    assert_eq!(again.body["data"]["request_id"], request);
    assert_eq!(again.body["data"]["replayed"], true);
    assert_eq!(observed.lock().unwrap().len(), requests.len());
    assert_eq!(wp.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn language_pack_recovery_fresh_manual_plan_uses_selected_version_and_replays_without_current_inputs(
) {
    fresh_manual_pack_contract("version").await;
}

#[tokio::test]
async fn language_pack_recovery_fresh_manual_plan_freezes_selected_oauth_and_proxy() {
    fresh_manual_pack_contract("oauth_proxy").await;
}

#[tokio::test]
async fn language_pack_recovery_fresh_manual_plan_missing_proxy_refuses_before_network() {
    fresh_manual_pack_contract("missing_proxy").await;
}

#[tokio::test]
async fn language_pack_recovery_fresh_manual_plan_damaged_keys_refuses_before_network() {
    fresh_manual_pack_contract("damaged_keys").await;
}

async fn review_pack(f: &PackFixture) -> PersistedLanguagePackBatch {
    review_pack_for(f, "http://127.0.0.1/site-a/wp-json/wptsall/v2/owned/client").await
}

async fn review_pack_for(f: &PackFixture, base: &str) -> PersistedLanguagePackBatch {
    persist_language_pack_batch_for_review(
        &f.db,
        f.job,
        f.root.to_str().unwrap(),
        base,
        &f.relation,
        &f.relation,
        "plugin_i18n",
        "plugin",
        &f.source,
        &f.payload.entries,
        &f.payload.client_task_id,
        Some("owned-private-route-secret"),
        &f.payload,
        "comp-selected",
        &f.params,
    )
    .await
    .unwrap()
}

async fn pack_callback_endpoint(
    ack: bool,
) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!(
        "http://{}/site-a/wp-json/wptsall/v2/owned/client",
        listener.local_addr().unwrap()
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = [0; 8192];
            socket.read(&mut bytes).await.unwrap();
            count.fetch_add(1, Ordering::SeqCst);
            let body = if ack {
                r#"{"success":true,"result_id":81,"protocol":"v2","result_status":"synced","entries_updated":1,"entries_rejected":0}"#
            } else {
                r#"{"code":"owned_unknown_effect","message":"Owned unavailable acknowledgement"}"#
            };
            let signature = crate::web_ui::test_support::sign_wp_plaintext_response(
                "owned-pack-token",
                body.as_bytes(),
            );
            let status = if ack { "200 OK" } else { "403 Forbidden" };
            socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nX-WPTSALL-Response-Signature: {signature}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        }
    });
    (base, calls, server)
}

async fn unknown_pack_delivery(action: &str) {
    let f = PackFixture::new().await;
    let _retry = crate::db::TestEnvVarGuard::set("WPTSALL_RETRY_MAX", "0");
    let (base, calls, server) = pack_callback_endpoint(false).await;
    let saved = review_pack_for(&f, &base).await;
    let harness = crate::web_ui::test_support::WebUiTestHarness::new("", None)
        .await
        .unwrap();
    {
        let mut state = harness.state.lock().await;
        state.db = f.db.clone();
        state.domain_token_bindings.domains.insert(
            crate::bindings::normalize_domain_base(&base),
            DomainTokenBindingEntry {
                wp_client_token: "owned-pack-token".into(),
                route_secret: "owned".into(),
                ..Default::default()
            },
        );
    }
    let response = harness
        .post_json(&format!("/api/items/{}/approve", saved.item_id), json!({}))
        .await
        .unwrap();
    assert!(!response.status_line.contains("200"));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "reach an actual callback, not an input failure"
    );
    server.abort();
    let before = jobs::get_item_checked(&*f.db.lock().await, saved.item_id)
        .unwrap()
        .unwrap();
    assert_eq!(before.status, "pending_review");
    let bytes = std::fs::read(&before.translated_path).unwrap();
    let response = match action {
        "content" => harness
            .get_json(&format!("/api/items/{}/content", saved.item_id))
            .await
            .unwrap(),
        "edit" => harness
            .put_json(
                &format!("/api/items/{}/translated", saved.item_id),
                json!({"content":{"entries":[{"entry_id":101,"msgstr":"Different result"}]}}),
            )
            .await
            .unwrap(),
        "override" => harness
            .put_json(
                &format!("/api/items/{}/override", saved.item_id),
                json!({"source_lang":"fr"}),
            )
            .await
            .unwrap(),
        "reject" => harness
            .post_json(
                &format!("/api/items/{}/reject", saved.item_id),
                json!({"reason":"Replacement intent"}),
            )
            .await
            .unwrap(),
        _ => unreachable!(),
    };
    if action == "content" {
        assert!(
            response.status_line.contains("200"),
            "read the retained result"
        );
        assert_eq!(
            response.body["data"]["delivery_unresolved"], true,
            "both review clients must see the unresolved delivery, not editable content"
        );
        assert_eq!(
            response.body["data"]["translated"]["payload"]["entries"][0]["entry_id"],
            101
        );
    } else {
        assert!(
            response.status_line.contains("409"),
            "unresolved callback must fence {action}: {}",
            response.body
        );
    }
    assert_eq!(std::fs::read(&before.translated_path).unwrap(), bytes);
    let after = jobs::get_item_checked(&*f.db.lock().await, saved.item_id)
        .unwrap()
        .unwrap();
    assert_eq!(after.client_task_id, before.client_task_id);
    assert_eq!(after.status, before.status);
    assert_eq!(after.effective_source_lang, before.effective_source_lang);
}

#[tokio::test]
async fn language_pack_recovery_content_exposes_unknown_delivery_without_mutating_result() {
    unknown_pack_delivery("content").await;
}

#[tokio::test]
async fn language_pack_recovery_unknown_delivery_blocks_edit() {
    unknown_pack_delivery("edit").await;
}

#[tokio::test]
async fn language_pack_recovery_unknown_delivery_blocks_override() {
    unknown_pack_delivery("override").await;
}

#[tokio::test]
async fn language_pack_recovery_unknown_delivery_blocks_rejection() {
    unknown_pack_delivery("reject").await;
}

#[tokio::test]
async fn language_pack_recovery_delivery_intent_failure_refuses_before_network() {
    let f = PackFixture::new().await;
    let (base, calls, server) = pack_callback_endpoint(true).await;
    let saved = f.persist_for(&base).await.unwrap();
    f.db.lock()
        .await
        .execute_batch(
            "CREATE TRIGGER deny_pack_intent BEFORE INSERT ON system_config
         WHEN NEW.key LIKE 'language-pack-delivery-v1:%'
         BEGIN SELECT RAISE(IGNORE); END;",
        )
        .unwrap();
    let result = crate::task_engine::pipeline::sync_i18n_item_to_wp(
        &f.db,
        &reqwest::Client::builder().no_proxy().build().unwrap(),
        saved.item_id,
        &saved.translated_path,
        &base,
        "owned-pack-token",
        &discovery_test_worker_config(),
        "/dev/null",
        &Arc::new(tokio::sync::Semaphore::new(1)),
    )
    .await;
    server.abort();
    assert!(
        result.is_err(),
        "uncommitted callback intent cannot authorize egress"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn language_pack_recovery_closed_delivery_budget_refuses_before_network() {
    let f = PackFixture::new().await;
    let (base, calls, server) = pack_callback_endpoint(true).await;
    let saved = f.persist_for(&base).await.unwrap();
    f.db.lock()
        .await
        .execute(
            "UPDATE translation_items SET retry_count=max_retries WHERE id=?1",
            [saved.item_id],
        )
        .unwrap();
    let result = crate::task_engine::pipeline::sync_i18n_item_to_wp(
        &f.db,
        &reqwest::Client::builder().no_proxy().build().unwrap(),
        saved.item_id,
        &saved.translated_path,
        &base,
        "owned-pack-token",
        &discovery_test_worker_config(),
        "/dev/null",
        &Arc::new(tokio::sync::Semaphore::new(1)),
    )
    .await;
    server.abort();
    assert!(
        result.is_err(),
        "closed delivery budget is not permission to resubmit"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn language_pack_recovery_capacity_full_refuses_fresh_and_replays_paid_entry() {
    let f = PackFixture::new().await;
    let (client, runtime, calls, server) = pack_provider().await;
    f.db.lock()
        .await
        .execute(
            "INSERT INTO system_config(key,value) VALUES ('storage_max_retained_units','1')",
            [],
        )
        .unwrap();
    let constraints = EffectiveConstraints::resolve(None, None, None, 10_000, "none");
    let translate = |source| {
        translate_language_pack_entry(
            &client,
            &runtime,
            Some(&f.db),
            "http://127.0.0.1/site-a/wp-json/wptsall/v2/owned/client",
            &f.relation,
            "plugin_i18n",
            "plugin",
            source,
            &f.params,
            &constraints,
        )
    };
    assert_eq!(
        translate(&f.source[0]).await.unwrap(),
        "Owned provider translation"
    );
    let mut fresh = f.source[0].clone();
    fresh.complete_data.entry_id = 102;
    assert!(format!("{:#}", translate(&fresh).await.unwrap_err()).contains("STORAGE_CAPACITY"));
    assert_eq!(
        translate(&f.source[0]).await.unwrap(),
        "Owned provider translation"
    );
    server.abort();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn language_pack_recovery_saved_entry_replays_with_database_query_only() {
    let f = PackFixture::new().await;
    let (client, runtime, calls, server) = pack_provider().await;
    let constraints = EffectiveConstraints::resolve(None, None, None, 10_000, "none");
    let translate = || {
        translate_language_pack_entry(
            &client,
            &runtime,
            Some(&f.db),
            "http://127.0.0.1/site-a/wp-json/wptsall/v2/owned/client",
            &f.relation,
            "plugin_i18n",
            "plugin",
            &f.source[0],
            &f.params,
            &constraints,
        )
    };
    translate().await.unwrap();
    f.db.lock()
        .await
        .execute_batch("PRAGMA query_only=ON")
        .unwrap();
    assert_eq!(translate().await.unwrap(), "Owned provider translation");
    server.abort();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn language_pack_recovery_changed_source_or_proxy_cannot_rebind_saved_entry() {
    let f = PackFixture::new().await;
    let (client, mut runtime, calls, server) = pack_provider().await;
    let constraints = EffectiveConstraints::resolve(None, None, None, 10_000, "none");
    let base = "http://127.0.0.1/site-a/wp-json/wptsall/v2/owned/client";
    translate_language_pack_entry(
        &client,
        &runtime,
        Some(&f.db),
        base,
        &f.relation,
        "plugin_i18n",
        "plugin",
        &f.source[0],
        &f.params,
        &constraints,
    )
    .await
    .unwrap();
    let mut changed = f.source[0].clone();
    changed.complete_data.msgid_plural = "A changed source".into();
    assert!(translate_language_pack_entry(
        &client,
        &runtime,
        Some(&f.db),
        base,
        &f.relation,
        "plugin_i18n",
        "plugin",
        &changed,
        &f.params,
        &constraints
    )
    .await
    .is_err());
    runtime.proxy_profile_id = Some("operator-changed-proxy".into());
    assert!(translate_language_pack_entry(
        &client,
        &runtime,
        Some(&f.db),
        base,
        &f.relation,
        "plugin_i18n",
        "plugin",
        &f.source[0],
        &f.params,
        &constraints
    )
    .await
    .is_err());
    server.abort();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn language_pack_recovery_route_rotation_cannot_escape_original_paid_entry() {
    let f = PackFixture::new().await;
    let (client, runtime, calls, server) = pack_provider().await;
    let constraints = EffectiveConstraints::resolve(None, None, None, 10_000, "none");
    translate_language_pack_entry(
        &client,
        &runtime,
        Some(&f.db),
        "http://127.0.0.1/site-a/wp-json/wptsall/v2/owned/client",
        &f.relation,
        "plugin_i18n",
        "plugin",
        &f.source[0],
        &f.params,
        &constraints,
    )
    .await
    .unwrap();
    let rotated = translate_language_pack_entry(
        &client,
        &runtime,
        Some(&f.db),
        "http://127.0.0.1/site-a/wp-json/wptsall/v2/rotated-route/client",
        &f.relation,
        "plugin_i18n",
        "plugin",
        &f.source[0],
        &f.params,
        &constraints,
    )
    .await;
    server.abort();
    assert!(
        rotated.is_err(),
        "route rotation cannot create a second fee for a retained physical entry"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn language_pack_recovery_i18n_and_site_string_numeric_ids_do_not_share_paid_authority() {
    let f = PackFixture::new().await;
    let (client, mut runtime, calls, server) = pack_provider().await;
    runtime
        .supported_business_lines
        .push("widget_strings".into());
    let constraints = EffectiveConstraints::resolve(None, None, None, 10_000, "none");
    for (line, subtype) in [("plugin_i18n", "plugin"), ("widget_strings", "widget")] {
        translate_language_pack_entry(
            &client,
            &runtime,
            Some(&f.db),
            "http://127.0.0.1/site-a/wp-json/wptsall/v2/owned/client",
            &f.relation,
            line,
            subtype,
            &f.source[0],
            &f.params,
            &constraints,
        )
        .await
        .expect("unrelated entry tables may legitimately reuse numeric IDs");
    }
    server.abort();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn language_pack_recovery_review_numeric_ids_do_not_collide_between_entry_tables() {
    let mut f = PackFixture::new().await;
    let a = review_pack(&f).await;
    f.payload.business_line = "widget_strings".into();
    f.payload.client_task_id = "owned-widget-callback".into();
    let b = persist_language_pack_batch_for_review(
        &f.db,
        f.job,
        f.root.to_str().unwrap(),
        "http://127.0.0.1/site-a/wp-json/wptsall/v2/owned/client",
        &f.relation,
        &f.relation,
        "widget_strings",
        "widget",
        &f.source,
        &f.payload.entries,
        &f.payload.client_task_id,
        None,
        &f.payload,
        "comp-selected",
        &f.params,
    )
    .await
    .expect("review identities must distinguish i18n and site-string entry tables");
    assert_ne!(a.item_id, b.item_id);
    assert_ne!(a.translated_path, b.translated_path);
}

#[tokio::test]
async fn language_pack_recovery_callback_projection_fault_keeps_intent_then_recovers_original() {
    let f = PackFixture::new().await;
    let (base, calls, server) = pack_callback_endpoint(true).await;
    let saved = f.persist_for(&base).await.unwrap();
    let original = std::fs::read(&saved.translated_path).unwrap();
    f.db.lock()
        .await
        .execute_batch(
            "CREATE TRIGGER deny_pack_receipt BEFORE INSERT ON system_config
         WHEN NEW.key LIKE 'language-pack-callback-v1:%'
         BEGIN SELECT RAISE(IGNORE); END;",
        )
        .unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let worker = discovery_test_worker_config();
    let sem = Arc::new(tokio::sync::Semaphore::new(1));
    assert!(crate::task_engine::pipeline::sync_i18n_item_to_wp(
        &f.db,
        &client,
        saved.item_id,
        &saved.translated_path,
        &base,
        "owned-pack-token",
        &worker,
        "/dev/null",
        &sem,
    )
    .await
    .is_err());
    let item = jobs::get_item_checked(&*f.db.lock().await, saved.item_id)
        .unwrap()
        .unwrap();
    assert_eq!(item.status, "translated");
    assert_eq!(item.client_task_id, f.payload.client_task_id);
    assert!(crate::db::system::get_system_config_checked(
        &*f.db.lock().await,
        &format!("language-pack-delivery-v1:{}", saved.item_id),
    )
    .unwrap()
    .is_some());
    assert_eq!(std::fs::read(&saved.translated_path).unwrap(), original);
    f.db.lock()
        .await
        .execute_batch("DROP TRIGGER deny_pack_receipt")
        .unwrap();
    assert_eq!(
        crate::task_engine::pipeline::sync_i18n_item_to_wp(
            &f.db,
            &client,
            saved.item_id,
            &saved.translated_path,
            &base,
            "owned-pack-token",
            &worker,
            "/dev/null",
            &sem,
        )
        .await
        .unwrap(),
        1
    );
    sem.close();
    assert_eq!(
        crate::task_engine::pipeline::sync_i18n_item_to_wp(
            &f.db,
            &client,
            saved.item_id,
            &saved.translated_path,
            &base,
            "owned-pack-token",
            &worker,
            "/dev/null",
            &sem,
        )
        .await
        .unwrap(),
        0,
        "saved acknowledgement precedes a new callback budget"
    );
    server.abort();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "projection recovery repeats only the original callback"
    );
}

#[tokio::test]
async fn language_pack_recovery_review_edit_keeps_encryption_and_bounded_identity() {
    let mut f = PackFixture::new().await;
    f.payload.client_task_id = "x".repeat(120);
    let saved = review_pack(&f).await;
    let original = std::fs::read(&saved.translated_path).unwrap();
    let harness = crate::web_ui::test_support::WebUiTestHarness::new("", None)
        .await
        .unwrap();
    harness.state.lock().await.db = f.db.clone();
    let before = harness
        .get_json(&format!("/api/items/{}/content", saved.item_id))
        .await
        .unwrap();
    assert_eq!(
        before.body["data"]["translated"]["payload"]["entries"][0]["entry_id"],
        101
    );
    let edited = harness
        .put_json(
            &format!("/api/items/{}/translated", saved.item_id),
            json!({"content":{"entries":[{"entry_id":101,"msgstr":"Owned reviewed text"}]}}),
        )
        .await
        .unwrap();
    assert!(
        edited.status_line.contains("200"),
        "valid pack review must save: {}",
        edited.status_line
    );
    let item = jobs::get_item_checked(&*f.db.lock().await, saved.item_id)
        .unwrap()
        .unwrap();
    assert!(
        std::fs::read(&item.translated_path)
            .unwrap()
            .starts_with(b"WPTC"),
        "review must not turn encrypted pack credentials into plaintext"
    );
    assert!(
        item.client_task_id.len() <= 128,
        "edited callback identity must satisfy the WP header contract"
    );
    assert_eq!(std::fs::read(&saved.translated_path).unwrap(), original);
}

#[tokio::test]
async fn language_pack_recovery_review_cannot_retarget_entry_ids() {
    let f = PackFixture::new().await;
    let saved = review_pack(&f).await;
    let before = jobs::get_item_checked(&*f.db.lock().await, saved.item_id)
        .unwrap()
        .unwrap();
    let bytes = std::fs::read(&before.translated_path).unwrap();
    let harness = crate::web_ui::test_support::WebUiTestHarness::new("", None)
        .await
        .unwrap();
    harness.state.lock().await.db = f.db.clone();
    for entry in [
        json!({"entry_id":999,"msgstr":"Wrong entry"}),
        json!({"entry_id":101,"msgstr":7}),
        json!({"entry_id":101,"msgstr":""}),
    ] {
        let response = harness
            .put_json(
                &format!("/api/items/{}/translated", saved.item_id),
                json!({"content":{"entries":[entry]}}),
            )
            .await
            .unwrap();
        assert!(
            response.status_line.contains("409") || response.status_line.contains("400"),
            "invalid pack review must refuse: {}",
            response.status_line
        );
        let after = jobs::get_item_checked(&*f.db.lock().await, saved.item_id)
            .unwrap()
            .unwrap();
        assert_eq!(after.translated_path, before.translated_path);
        assert_eq!(std::fs::read(&after.translated_path).unwrap(), bytes);
    }
}

#[tokio::test]
async fn language_pack_recovery_applied_callback_replay_is_zero_network_noop() {
    let f = PackFixture::new().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!(
        "http://{}/wp-json/wptsall/v2/owned/client",
        listener.local_addr().unwrap()
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 8192];
            socket.read(&mut request).await.unwrap();
            observed.fetch_add(1, Ordering::SeqCst);
            let body = r#"{"success":true,"result_id":81,"protocol":"v2","result_status":"synced","entries_updated":1,"entries_rejected":0}"#;
            let signature = crate::web_ui::test_support::sign_wp_plaintext_response(
                "owned-pack-token",
                body.as_bytes(),
            );
            socket.write_all(format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-WPTSALL-Response-Signature: {signature}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()
            ).as_bytes()).await.unwrap();
        }
    });
    let saved = f.persist_for(&base).await.unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let worker = discovery_test_worker_config();
    let sem = Arc::new(tokio::sync::Semaphore::new(1));
    let first = crate::task_engine::pipeline::sync_i18n_item_to_wp(
        &f.db,
        &client,
        saved.item_id,
        &saved.translated_path,
        &base,
        "owned-pack-token",
        &worker,
        "/dev/null",
        &sem,
    )
    .await
    .unwrap();
    assert_eq!(first, 1);
    let again = crate::task_engine::pipeline::sync_i18n_item_to_wp(
        &f.db,
        &client,
        saved.item_id,
        &saved.translated_path,
        &base,
        "owned-pack-token",
        &worker,
        "/dev/null",
        &sem,
    )
    .await
    .unwrap();
    server.abort();
    assert_eq!(again, 0, "an old applied receipt is not new success");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "applied receipt must replay before egress"
    );
}
