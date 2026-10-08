// catalog: WEBUI-MOD-sync-engine-discoverer-rs
// catalog: WEBUI-MOD-db-sync-inflight-rs
// oracle: L2
use super::*;
use crate::types::{ComponentRuntime, ComponentTemplate};
use crate::web_ui::test_support::WebUiTestHarness;
use rusqlite::OptionalExtension;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

struct MockVendor {
    url: String,
    calls: Shared<Vec<String>>,
    faults: Shared<Vec<String>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl MockVendor {
    fn spawn() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let faults = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (recorded, failures, stopped) = (calls.clone(), faults.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        socket
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let (head, body) = read_request(&mut socket).expect("vendor HTTP request");
                        assert!(
                            head.starts_with("POST "),
                            "translation must use the configured request"
                        );
                        let body: Value = serde_json::from_slice(&body).unwrap();
                        let text = body["text"].as_str().unwrap().to_owned();
                        recorded.lock().unwrap().push(text.clone());
                        let mut failures = failures.lock().unwrap();
                        let fail = failures.iter().position(|candidate| candidate == &text);
                        if let Some(index) = fail {
                            failures.remove(index);
                            write_json_response(
                                &mut socket,
                                500,
                                &json!({"error":"Injected vendor failure"}),
                            );
                        } else {
                            write_json_response(
                                &mut socket,
                                200,
                                &json!({"text":format!("Translated {text}")}),
                            );
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("vendor accept failed: {error}"),
                }
            }
        });
        Self {
            url,
            calls,
            faults,
            stop,
            thread: Some(thread),
        }
    }

    fn fail_once(&self, text: &str) {
        self.faults.lock().unwrap().push(text.into());
    }

    fn count(&self, text: &str) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|value| *value == text)
            .count()
    }

    fn translator(&self) -> super::super::TranslatorHandle {
        Self::translator_at(&self.url)
    }

    fn translator_at(url: &str) -> super::super::TranslatorHandle {
        let template: ComponentTemplate = serde_json::from_value(json!({
            "id":"cli13-mock-vendor", "name":"Owned CLI13 mock", "version":"1",
            "type":"text", "auth":null,
            "request":{"method":"POST","url":url,"body_type":"json","body":{"text":"{{input.text}}"}},
            "response":{"translated_text_path":"text"}
        })).unwrap();
        let runtime = ComponentRuntime {
            template,
            auth_values: HashMap::new(),
            supported_business_lines: vec![],
            language_map: HashMap::new(),
            supported_content_formats: vec!["plain_text".into()],
            supported_formats: vec![],
            key_pool: None,
            oauth_pool: None,
            oauth_manager: None,
            proxy_profile_id: None,
            runtime_max_concurrent_requests: 0,
            runtime_min_interval_ms: 0,
            runtime_concurrency_sem: None,
            runtime_last_request_at: None,
        };
        super::super::TranslatorHandle {
            vendor_client: reqwest::Client::builder().no_proxy().build().unwrap(),
            runtime: Arc::new(runtime),
        }
    }
}

impl Drop for MockVendor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("vendor fixture panic must propagate");
        }
    }
}

fn paid_source_packet(base: &str) -> Value {
    let mut packet = sample_source_packet(base, "uuid-paid", 1, "Paid title");
    packet["entity"]["core_fields"]["post_content"] = json!("Paid content");
    packet["entity"]["core_fields"]["post_excerpt"] = json!("Paid excerpt");
    packet["multimodal_manifest"] = json!([]);
    packet
}

#[tokio::test]
async fn cli13_successful_fields_are_not_retranslated_after_later_field_failure() {
    for failing_field in ["Paid content", "Paid excerpt"] {
        let harness = WebUiTestHarness::new("", None).await.unwrap();
        let source = MockSite::spawn(SOURCE_UUID, |base| vec![paid_source_packet(base)]);
        let target = MockSite::spawn(TARGET_UUID, |_| vec![]);
        let vendor = MockVendor::spawn();
        vendor.fail_once(failing_field);
        let mut pair = make_pair(&source.base_url, &target.base_url);
        pair.sync_mode = crate::sync_engine::SyncMode::SyncAndTranslate;
        pair.translate_component_id = Some("cli13-mock-vendor".into());
        persist_pair(&crate::config::sync_pairs_file(), &pair);
        let creds = credentials_doc(&source, &target);
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let translator = vendor.translator();
        let log = harness.log_file.to_string_lossy().into_owned();
        let first = super::super::sync_pair_run(&client, &pair.id, &creds, Some(&translator), &log)
            .await
            .unwrap();
        assert_eq!((first.error_count, first.synced_count), (1, 0), "{first:?}");
        assert!(target.received_packets().is_empty());
        let stored = {
            let db = Arc::new(tokio::sync::Mutex::new(
                crate::db::open_db(&harness.db_path.to_string_lossy()).unwrap(),
            ));
            crate::db::sync_inflight::find_shipping(&db, &pair.id, "uuid-paid")
                .await
                .unwrap()
                .unwrap()
        };
        // A new run opens a new SQLite connection; no in-memory checkpoint
        // can satisfy the request-count and stored-row assertions.
        let retry_translator = vendor.translator();
        let next =
            super::super::sync_pair_run(&client, &pair.id, &creds, Some(&retry_translator), &log)
                .await
                .unwrap();
        assert_eq!((next.error_count, next.synced_count), (1, 0), "{next:?}");
        assert!(next.last_error.as_deref().unwrap().contains("unknown"));
        assert_eq!(
            vendor.count("Paid title"),
            1,
            "a successful title must not be requested twice after {failing_field} fails"
        );
        assert_eq!(
            vendor.count(failing_field),
            1,
            "HTTP 500 is not evidence that an accepted fee can be reissued"
        );
        if failing_field == "Paid excerpt" {
            assert_eq!(
                vendor.count("Paid content"),
                1,
                "successful content must be reused"
            );
        }
        let paid = stored
            .ctx
            .translation
            .expect("successful fields must already be durable before the later call fails");
        assert_eq!(paid.title.as_deref(), Some("Translated Paid title"));
        assert_eq!(paid.excerpt, None);
        if failing_field == "Paid excerpt" {
            assert_eq!(paid.content.as_deref(), Some("Translated Paid content"));
        } else {
            assert_eq!(paid.content, None);
        }
        assert!(target.received_packets().is_empty());
        let db = Arc::new(tokio::sync::Mutex::new(
            crate::db::open_db(&harness.db_path.to_string_lossy()).unwrap(),
        ));
        assert!(
            crate::db::sync_inflight::find_shipping(&db, &pair.id, "uuid-paid")
                .await
                .unwrap()
                .is_some()
        );
        let third =
            super::super::sync_pair_run(&client, &pair.id, &creds, Some(&retry_translator), &log)
                .await
                .unwrap();
        assert_eq!((third.error_count, third.synced_count), (1, 0));
        assert_eq!(vendor.count("Paid title"), 1);
    }
}

#[tokio::test]
async fn cli13_changed_translation_scope_never_reuses_old_partial_output() {
    for change in [
        "source",
        "fingerprint",
        "body",
        "target_lang",
        "source_lang",
        "component",
        "pair_component",
        "template",
        "auth",
        "language_map",
        "mode",
        "legacy",
        "legacy_sync_only",
    ] {
        let harness = WebUiTestHarness::new("", None).await.unwrap();
        let source = MockSite::spawn(SOURCE_UUID, |base| vec![paid_source_packet(base)]);
        let target = MockSite::spawn(TARGET_UUID, |_| vec![]);
        let vendor = MockVendor::spawn();
        vendor.fail_once("Paid content");
        let mut pair = make_pair(&source.base_url, &target.base_url);
        pair.sync_mode = crate::sync_engine::SyncMode::SyncAndTranslate;
        pair.translate_component_id = Some("cli13-mock-vendor".into());
        persist_pair(&crate::config::sync_pairs_file(), &pair);
        let conn = crate::db::open_db(&harness.db_path.to_string_lossy()).unwrap();
        let creds = credentials_doc(&source, &target);
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let translator = vendor.translator();
        let log = harness.log_file.to_string_lossy().into_owned();
        let first = super::super::sync_pair_run(&client, &pair.id, &creds, Some(&translator), &log)
            .await
            .unwrap();
        assert_eq!(first.error_count, 1);
        let mut next_translator = vendor.translator();
        match change {
            "source" => source.mutate_post("uuid-paid", "New title"),
            "fingerprint" => {
                source.posts.lock().unwrap()[0]["source_fingerprint"] =
                    json!("fp-paid-new-revision")
            }
            "body" => {
                source.posts.lock().unwrap()[0]["entity"]["core_fields"]["post_content"] =
                    json!("New content")
            }
            "target_lang" => pair.target_lang = "fr_FR".into(),
            "source_lang" => pair.source_lang = "de_DE".into(),
            "component" => {
                pair.translate_component_id = Some("other-component".into());
                Arc::get_mut(&mut next_translator.runtime)
                    .unwrap()
                    .template
                    .id = "other-component".into();
            }
            "pair_component" => pair.translate_component_id = Some("other-component".into()),
            "template" => {
                Arc::get_mut(&mut next_translator.runtime)
                    .unwrap()
                    .template
                    .version = "2".into()
            }
            "auth" => {
                Arc::get_mut(&mut next_translator.runtime)
                    .unwrap()
                    .auth_values
                    .insert("fixture-profile".into(), "profile-2".into());
            }
            "language_map" => {
                Arc::get_mut(&mut next_translator.runtime)
                    .unwrap()
                    .language_map
                    .insert("zh_CN".into(), "zh-Hans".into());
            }
            "mode" => pair.sync_mode = crate::sync_engine::SyncMode::SyncOnly,
            "legacy" | "legacy_sync_only" => {
                let db = crate::db::open_db(&harness.db_path.to_string_lossy()).unwrap();
                let stored: String = db
                    .query_row("SELECT ctx_json FROM sync_inflight", [], |row| row.get(0))
                    .unwrap();
                let envelope: Value = serde_json::from_str(
                    &crate::db::system::decrypt_config_value(&stored).unwrap(),
                )
                .unwrap();
                let mut legacy = envelope["value"].clone();
                legacy.as_object_mut().unwrap().remove("translation_scope");
                db.execute(
                    "UPDATE sync_inflight SET ctx_json=?1",
                    [serde_json::to_string(&legacy).unwrap()],
                )
                .unwrap();
                if change == "legacy_sync_only" {
                    pair.sync_mode = crate::sync_engine::SyncMode::SyncOnly;
                }
            }
            _ => unreachable!(),
        }
        persist_pair(&crate::config::sync_pairs_file(), &pair);
        let before: String = conn
            .query_row("SELECT ctx_json FROM sync_inflight", [], |row| row.get(0))
            .unwrap();
        let next =
            super::super::sync_pair_run(&client, &pair.id, &creds, Some(&next_translator), &log)
                .await
                .unwrap();
        assert_eq!(
            (next.error_count, next.synced_count),
            (1, 0),
            "{change}: {next:?}"
        );
        assert!(next.last_error.as_deref().unwrap().contains("快照"));
        assert!(target.received_packets().is_empty());
        assert_eq!(
            vendor.count("Paid title"),
            1,
            "{change}: an unfinished scope must not authorize new fees"
        );
        assert_eq!(vendor.count("Paid content"), 1);
        assert_eq!(vendor.count("Paid excerpt"), 0);
        assert_eq!(vendor.count("New title"), 0);
        let after: String = conn
            .query_row("SELECT ctx_json FROM sync_inflight", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            after, before,
            "{change}: original evidence must be retained"
        );
    }
}

#[tokio::test]
async fn cli13_shipping_row_insert_failure_prevents_any_vendor_call() {
    let harness = WebUiTestHarness::new("", None).await.unwrap();
    let source = MockSite::spawn(SOURCE_UUID, |base| vec![paid_source_packet(base)]);
    let target = MockSite::spawn(TARGET_UUID, |_| vec![]);
    let vendor = MockVendor::spawn();
    let mut pair = make_pair(&source.base_url, &target.base_url);
    pair.sync_mode = crate::sync_engine::SyncMode::SyncAndTranslate;
    pair.translate_component_id = Some("cli13-mock-vendor".into());
    persist_pair(&crate::config::sync_pairs_file(), &pair);
    let db = crate::db::open_db(&harness.db_path.to_string_lossy()).unwrap();
    db.execute_batch("CREATE TRIGGER deny_paid_row BEFORE INSERT ON sync_inflight BEGIN SELECT RAISE(ABORT,'injected begin failure'); END;").unwrap();
    let creds = credentials_doc(&source, &target);
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let translator = vendor.translator();
    let log = harness.log_file.to_string_lossy().into_owned();
    let failed = super::super::sync_pair_run(&client, &pair.id, &creds, Some(&translator), &log)
        .await
        .unwrap();
    assert_eq!(
        (failed.error_count, failed.synced_count),
        (1, 0),
        "failed durable begin must stop the paid cascade"
    );
    assert!(vendor.calls.lock().unwrap().is_empty());
    assert!(target.received_packets().is_empty());
    db.execute_batch("DROP TRIGGER deny_paid_row;").unwrap();
    let next = super::super::sync_pair_run(&client, &pair.id, &creds, Some(&translator), &log)
        .await
        .unwrap();
    assert_eq!((next.error_count, next.synced_count), (0, 1));
    assert_eq!(vendor.count("Paid title"), 1);
}

#[tokio::test]
async fn cli13_checkpoint_write_failure_stops_before_the_next_paid_stage() {
    for field in ["scope", "title", "content", "excerpt"] {
        let harness = WebUiTestHarness::new("", None).await.unwrap();
        let source = MockSite::spawn(SOURCE_UUID, |base| vec![paid_source_packet(base)]);
        let target = MockSite::spawn(TARGET_UUID, |_| vec![]);
        let vendor = MockVendor::spawn();
        let mut pair = make_pair(&source.base_url, &target.base_url);
        pair.sync_mode = crate::sync_engine::SyncMode::SyncAndTranslate;
        pair.translate_component_id = Some("cli13-mock-vendor".into());
        persist_pair(&crate::config::sync_pairs_file(), &pair);
        let db = crate::db::open_db(&harness.db_path.to_string_lossy()).unwrap();
        if field == "scope" {
            db.execute_batch(
                "CREATE TRIGGER deny_paid_checkpoint BEFORE INSERT ON sync_inflight
                 BEGIN SELECT RAISE(ABORT,'injected checkpoint failure'); END;",
            )
            .unwrap();
        } else {
            let ordinal = match field {
                "title" => 1,
                "content" => 2,
                "excerpt" => 3,
                _ => unreachable!(),
            };
            // The checkpoint is ciphertext. Inject at its actual write boundary,
            // not with JSON operators that cannot inspect encrypted values.
            db.execute_batch(&format!(
                "CREATE TABLE owned_checkpoint_writes (n INTEGER NOT NULL);
                 INSERT INTO owned_checkpoint_writes VALUES (0);
                 CREATE TRIGGER deny_paid_checkpoint BEFORE UPDATE OF ctx_json ON sync_inflight
                 WHEN (SELECT n FROM owned_checkpoint_writes)+1={ordinal}
                 BEGIN SELECT RAISE(ABORT,'injected checkpoint failure'); END;
                 CREATE TRIGGER count_owned_checkpoint_writes AFTER UPDATE OF ctx_json ON sync_inflight
                 BEGIN UPDATE owned_checkpoint_writes SET n=n+1; END;"
            ))
            .unwrap();
        }
        let creds = credentials_doc(&source, &target);
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let translator = vendor.translator();
        let log = harness.log_file.to_string_lossy().into_owned();
        let failed =
            super::super::sync_pair_run(&client, &pair.id, &creds, Some(&translator), &log)
                .await
                .unwrap();
        assert_eq!(
            (failed.error_count, failed.synced_count),
            (1, 0),
            "{field}: {failed:?}"
        );
        assert!(failed.last_error.as_deref().unwrap().contains("快照"));
        assert!(target.received_packets().is_empty());
        let calls = vendor.calls.lock().unwrap().clone();
        let expected = match field {
            "scope" => vec![],
            "title" => vec!["Paid title"],
            "content" => vec!["Paid title", "Paid content"],
            "excerpt" => vec!["Paid title", "Paid content", "Paid excerpt"],
            _ => unreachable!(),
        };
        assert_eq!(
            calls, expected,
            "no later vendor call after {field} persistence fails"
        );
        let stored: Option<String> = db
            .query_row("SELECT ctx_json FROM sync_inflight", [], |row| row.get(0))
            .optional()
            .unwrap();
        assert_eq!(
            stored.is_none(),
            field == "scope",
            "admission and scope are one atomic snapshot"
        );
        let context: crate::db::sync_inflight::InflightCtx = stored
            .map(|stored| {
                let envelope: Value = serde_json::from_str(
                    &crate::db::system::decrypt_config_value(&stored).unwrap(),
                )
                .unwrap();
                serde_json::from_value(envelope["value"].clone()).unwrap()
            })
            .unwrap_or_default();
        if field == "content" || field == "excerpt" {
            assert_eq!(
                context.translation.as_ref().unwrap().title.as_deref(),
                Some("Translated Paid title")
            );
        }
        if field == "excerpt" {
            assert_eq!(
                context.translation.as_ref().unwrap().content.as_deref(),
                Some("Translated Paid content")
            );
        }
        assert!(context
            .translation
            .as_ref()
            .and_then(|paid| paid.excerpt.as_ref())
            .is_none());
        db.execute_batch("DROP TRIGGER deny_paid_checkpoint;")
            .unwrap();
        let retry = vendor.translator();
        let next = super::super::sync_pair_run(&client, &pair.id, &creds, Some(&retry), &log)
            .await
            .unwrap();
        assert_eq!((next.error_count, next.synced_count), (0, 1));
        assert_eq!(vendor.count("Paid title"), 1);
        assert_eq!(vendor.count("Paid content"), 1);
        assert_eq!(vendor.count("Paid excerpt"), 1);
        assert_eq!(target.received_packets().len(), 1);
    }
}

#[tokio::test]
async fn cli13_corrupt_shipping_context_blocks_rebilling_and_preserves_bytes() {
    let harness = WebUiTestHarness::new("", None).await.unwrap();
    let source = MockSite::spawn(SOURCE_UUID, |base| vec![paid_source_packet(base)]);
    let target = MockSite::spawn(TARGET_UUID, |_| vec![]);
    let vendor = MockVendor::spawn();
    let mut pair = make_pair(&source.base_url, &target.base_url);
    pair.sync_mode = crate::sync_engine::SyncMode::SyncAndTranslate;
    pair.translate_component_id = Some("cli13-mock-vendor".into());
    persist_pair(&crate::config::sync_pairs_file(), &pair);
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(&harness.db_path.to_string_lossy()).unwrap(),
    ));
    crate::db::sync_inflight::begin_shipping(&db, &pair.id, "uuid-paid")
        .await
        .unwrap();
    db.lock()
        .await
        .execute(
            "UPDATE sync_inflight SET ctx_json=?1",
            ["{broken paid snapshot"],
        )
        .unwrap();
    let creds = credentials_doc(&source, &target);
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let translator = vendor.translator();
    let log = harness.log_file.to_string_lossy().into_owned();
    let failed = super::super::sync_pair_run(&client, &pair.id, &creds, Some(&translator), &log)
        .await
        .unwrap();
    assert_eq!(
        (failed.error_count, failed.synced_count),
        (2, 0),
        "both the direct lookup and final inventory must report damaged evidence"
    );
    assert!(failed
        .last_error
        .as_deref()
        .unwrap()
        .contains("读取翻译快照"));
    assert!(vendor.calls.lock().unwrap().is_empty());
    assert!(target.received_packets().is_empty());
    let bytes: String = db
        .lock()
        .await
        .query_row("SELECT ctx_json FROM sync_inflight", [], |row| row.get(0))
        .unwrap();
    assert_eq!(bytes, "{broken paid snapshot");
}

#[tokio::test]
async fn cli13_database_open_failure_prevents_stateless_paid_fallback() {
    let harness = WebUiTestHarness::new("", None).await.unwrap();
    let source = MockSite::spawn(SOURCE_UUID, |base| vec![paid_source_packet(base)]);
    let target = MockSite::spawn(TARGET_UUID, |_| vec![]);
    let vendor = MockVendor::spawn();
    let mut pair = make_pair(&source.base_url, &target.base_url);
    pair.sync_mode = crate::sync_engine::SyncMode::SyncAndTranslate;
    pair.translate_component_id = Some("cli13-mock-vendor".into());
    persist_pair(&crate::config::sync_pairs_file(), &pair);
    let creds = credentials_doc(&source, &target);
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let translator = vendor.translator();
    let log = harness.log_file.to_string_lossy().into_owned();
    let original = crate::config::db_path();
    std::env::set_var("WPTSALL_DB_PATH", harness.db_path.parent().unwrap());
    let failed = super::super::sync_pair_run(&client, &pair.id, &creds, Some(&translator), &log)
        .await
        .unwrap();
    std::env::set_var("WPTSALL_DB_PATH", &original);
    assert_eq!((failed.error_count, failed.synced_count), (1, 0));
    assert!(vendor.calls.lock().unwrap().is_empty());
    assert!(target.received_packets().is_empty());
    let next = super::super::sync_pair_run(&client, &pair.id, &creds, Some(&translator), &log)
        .await
        .unwrap();
    assert_eq!((next.error_count, next.synced_count), (0, 1));
    assert_eq!(vendor.count("Paid title"), 1);
}

#[tokio::test]
async fn cli13_empty_fields_are_not_paid_and_nonempty_fields_are_completed() {
    for empty in ["title", "content", "excerpt", "all"] {
        let harness = WebUiTestHarness::new("", None).await.unwrap();
        let source = MockSite::spawn(SOURCE_UUID, |base| {
            let mut packet = paid_source_packet(base);
            for field in ["title", "content", "excerpt"] {
                if empty == field || empty == "all" {
                    packet["entity"]["core_fields"][format!("post_{field}")] = json!(" \n ");
                }
            }
            packet["entity"]["meta_fields"] = json!({"fixture_meta":"Must remain untranslated"});
            vec![packet]
        });
        let target = MockSite::spawn(TARGET_UUID, |_| vec![]);
        let vendor = MockVendor::spawn();
        let mut pair = make_pair(&source.base_url, &target.base_url);
        pair.sync_mode = crate::sync_engine::SyncMode::SyncAndTranslate;
        pair.translate_component_id = Some("cli13-mock-vendor".into());
        persist_pair(&crate::config::sync_pairs_file(), &pair);
        let creds = credentials_doc(&source, &target);
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let translator = vendor.translator();
        let log = harness.log_file.to_string_lossy().into_owned();
        let report =
            super::super::sync_pair_run(&client, &pair.id, &creds, Some(&translator), &log)
                .await
                .unwrap();
        assert_eq!((report.error_count, report.synced_count), (0, 1));
        assert!(!vendor
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|text| text.trim().is_empty()));
        let received = target.received_packets();
        assert_eq!(received.len(), 1);
        for field in ["title", "content", "excerpt"] {
            let expected = if empty == field || empty == "all" {
                " \n ".to_owned()
            } else {
                format!("Translated Paid {field}")
            };
            assert_eq!(
                received[0]["entity"]["core_fields"][format!("post_{field}")],
                expected,
                "{empty}/{field}"
            );
            assert_eq!(
                vendor.count(&format!("Paid {field}")),
                usize::from(empty != field && empty != "all")
            );
        }
        assert_eq!(
            received[0]["entity"]["meta_fields"]["fixture_meta"],
            "Must remain untranslated"
        );
    }
}

#[tokio::test]
async fn cli13_paid_fields_survive_later_media_push_and_review_failures() {
    for stage in ["media", "push", "review"] {
        let harness = WebUiTestHarness::new("", None).await.unwrap();
        let source = MockSite::spawn(SOURCE_UUID, |base| {
            let mut packet = paid_source_packet(base);
            packet["multimodal_manifest"] =
                sample_source_packet(base, "uuid-paid", 1, "unused")["multimodal_manifest"].clone();
            vec![packet]
        });
        let target = MockSite::spawn(TARGET_UUID, |_| vec![]);
        let vendor = MockVendor::spawn();
        let mut pair = make_pair(&source.base_url, &target.base_url);
        pair.sync_mode = crate::sync_engine::SyncMode::SyncAndTranslate;
        pair.translate_component_id = Some("cli13-mock-vendor".into());
        let review_path = std::path::PathBuf::from(crate::config::sync_review_file());
        let blocked = review_path.with_file_name(".sync-review.json.lock");
        match stage {
            "media" => source
                .records
                .lock()
                .unwrap()
                .media_faults
                .push("uuid-paid".into()),
            "push" => target.inject_push_faults(1, 503),
            "review" => {
                pair.review_before_push = true;
                std::fs::create_dir_all(review_path.parent().unwrap()).unwrap();
                std::fs::write(
                    &review_path,
                    serde_json::to_vec(&crate::sync_engine::SyncReviewDoc::default()).unwrap(),
                )
                .unwrap();
                // Keep reads healthy, then fail the actual writer's lock
                // boundary AFTER paid fields have been checkpointed.
                std::fs::create_dir(&blocked).unwrap();
            }
            _ => unreachable!(),
        }
        persist_pair(&crate::config::sync_pairs_file(), &pair);
        let creds = credentials_doc(&source, &target);
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let translator = vendor.translator();
        let log = harness.log_file.to_string_lossy().into_owned();
        let first = super::super::sync_pair_run(&client, &pair.id, &creds, Some(&translator), &log)
            .await
            .unwrap();
        assert_eq!(
            (
                first.error_count,
                first.synced_count,
                first.pending_review_count
            ),
            (1, 0, 0),
            "{stage}: {first:?}"
        );
        assert!(target.received_packets().is_empty());
        let db = Arc::new(tokio::sync::Mutex::new(
            crate::db::open_db(&harness.db_path.to_string_lossy()).unwrap(),
        ));
        let stored = crate::db::sync_inflight::find_shipping(&db, &pair.id, "uuid-paid")
            .await
            .unwrap()
            .unwrap();
        let paid = stored.ctx.translation.as_ref().unwrap();
        assert_eq!(paid.title.as_deref(), Some("Translated Paid title"));
        assert_eq!(paid.content.as_deref(), Some("Translated Paid content"));
        assert_eq!(paid.excerpt.as_deref(), Some("Translated Paid excerpt"));
        let chunks = target.received_chunks();
        if stage == "media" {
            source.records.lock().unwrap().media_faults.clear();
        } else if stage == "review" {
            // This empty directory is an explicitly owned fault fixture.
            std::fs::remove_dir(&blocked).unwrap();
        }
        let retry = vendor.translator();
        let next = super::super::sync_pair_run(&client, &pair.id, &creds, Some(&retry), &log)
            .await
            .unwrap();
        assert_eq!(next.error_count, 0, "{stage}: {next:?}");
        for field in ["title", "content", "excerpt"] {
            assert_eq!(vendor.count(&format!("Paid {field}")), 1, "{stage}/{field}");
        }
        if stage == "review" {
            assert_eq!((next.synced_count, next.pending_review_count), (0, 1));
            let inbox =
                crate::sync_engine::load_sync_review(&crate::config::sync_review_file()).unwrap();
            assert_eq!(inbox.items.len(), 1);
            assert_eq!(
                inbox.items[0].relayed_packet.entity.core_fields["post_title"],
                "Translated Paid title"
            );
        } else {
            assert_eq!((next.synced_count, next.pending_review_count), (1, 0));
            let received = target.received_packets();
            assert_eq!(received.len(), 1);
            assert_eq!(
                received[0]["entity"]["core_fields"]["post_content"],
                "Translated Paid content"
            );
            if stage == "push" {
                let prior: Value = serde_json::from_str(&stored.relayed_json).unwrap();
                assert_eq!(received[0]["packet_id"], prior["packet_id"]);
            }
        }
        assert_eq!(
            target.received_chunks(),
            if stage == "media" { chunks + 1 } else { chunks }
        );
        assert!(
            crate::db::sync_inflight::find_shipping(&db, &pair.id, "uuid-paid")
                .await
                .unwrap()
                .is_none()
        );
    }
}

#[tokio::test]
async fn cli13_new_owned_test_process_resumes_only_missing_paid_fields() {
    const CHILD: &str = "WPTSALL_TEST_CLI13_RESUME_CHILD";
    const TEST: &str = "sync_engine::discoverer::tests::paid_snapshots::cli13_new_owned_test_process_resumes_only_missing_paid_fields";
    if let Ok(raw) = std::env::var(CHILD) {
        let payload: Value = serde_json::from_str(&raw).unwrap();
        let credentials = serde_json::from_value(payload["credentials"].clone()).unwrap();
        let translator = MockVendor::translator_at(payload["vendor_url"].as_str().unwrap());
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let report = super::super::sync_pair_run(
            &client,
            payload["pair_id"].as_str().unwrap(),
            &credentials,
            Some(&translator),
            payload["log"].as_str().unwrap(),
        )
        .await
        .unwrap();
        assert_eq!((report.error_count, report.synced_count), (0, 1));
        return;
    }
    let harness = WebUiTestHarness::new("", None).await.unwrap();
    let source = MockSite::spawn(SOURCE_UUID, |base| vec![paid_source_packet(base)]);
    let target = MockSite::spawn(TARGET_UUID, |_| vec![]);
    let vendor = MockVendor::spawn();
    let mut pair = make_pair(&source.base_url, &target.base_url);
    pair.sync_mode = crate::sync_engine::SyncMode::SyncAndTranslate;
    pair.translate_component_id = Some("cli13-mock-vendor".into());
    persist_pair(&crate::config::sync_pairs_file(), &pair);
    let conn = crate::db::open_db(&harness.db_path.to_string_lossy()).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER refuse_projection BEFORE UPDATE OF ctx_json ON sync_inflight
         WHEN json_extract(NEW.ctx_json,'$.translation.title') IS NOT NULL
         BEGIN SELECT RAISE(ABORT,'owned durable result projection refusal'); END;",
    )
    .unwrap();
    let credentials = credentials_doc(&source, &target);
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let log = harness.log_file.to_string_lossy().into_owned();
    let first_translator = vendor.translator();
    let first = super::super::sync_pair_run(
        &client,
        &pair.id,
        &credentials,
        Some(&first_translator),
        &log,
    )
    .await
    .unwrap();
    assert_eq!((first.error_count, first.synced_count), (1, 0));
    conn.execute_batch("DROP TRIGGER refuse_projection;")
        .unwrap();
    drop(first_translator);
    let payload =
        json!({"pair_id":pair.id, "credentials":credentials, "vendor_url":vendor.url, "log":log})
            .to_string();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST, "--nocapture"])
        .env(CHILD, payload)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "owned recovery child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("test result: ok. 1 passed;"),
        "child must run a nonempty exact test"
    );
    assert_eq!(
        vendor.count("Paid title"),
        1,
        "a separate process must reuse SQLite, not parent memory"
    );
    assert_eq!(vendor.count("Paid content"), 1);
    assert_eq!(vendor.count("Paid excerpt"), 1);
    let packets = target.received_packets();
    assert_eq!(packets.len(), 1);
    assert_eq!(
        packets[0]["entity"]["core_fields"]["post_title"],
        "Translated Paid title"
    );
}

#[tokio::test]
async fn cli13_reopen_or_stale_scope_cleanup_failure_preserves_paid_fields() {
    for operation in ["reopen", "cleanup"] {
        let harness = WebUiTestHarness::new("", None).await.unwrap();
        let source = MockSite::spawn(SOURCE_UUID, |base| vec![paid_source_packet(base)]);
        let target = MockSite::spawn(TARGET_UUID, |_| vec![]);
        let vendor = MockVendor::spawn();
        vendor.fail_once("Paid content");
        let mut pair = make_pair(&source.base_url, &target.base_url);
        pair.sync_mode = crate::sync_engine::SyncMode::SyncAndTranslate;
        pair.translate_component_id = Some("cli13-mock-vendor".into());
        persist_pair(&crate::config::sync_pairs_file(), &pair);
        let creds = credentials_doc(&source, &target);
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let translator = vendor.translator();
        let log = harness.log_file.to_string_lossy().into_owned();
        let first = super::super::sync_pair_run(&client, &pair.id, &creds, Some(&translator), &log)
            .await
            .unwrap();
        assert_eq!(first.error_count, 1);
        let db = crate::db::open_db(&harness.db_path.to_string_lossy()).unwrap();
        let before: String = db
            .query_row("SELECT ctx_json FROM sync_inflight", [], |row| row.get(0))
            .unwrap();
        let sql = if operation == "reopen" {
            "CREATE TRIGGER deny_retry BEFORE UPDATE OF attempts ON sync_inflight BEGIN SELECT RAISE(ABORT,'injected reopen failure'); END;"
        } else {
            pair.target_lang = "fr_FR".into();
            persist_pair(&crate::config::sync_pairs_file(), &pair);
            "CREATE TRIGGER deny_retry BEFORE DELETE ON sync_inflight BEGIN SELECT RAISE(ABORT,'injected cleanup failure'); END;"
        };
        db.execute_batch(sql).unwrap();
        let failed =
            super::super::sync_pair_run(&client, &pair.id, &creds, Some(&translator), &log)
                .await
                .unwrap();
        assert_eq!(
            (failed.error_count, failed.synced_count),
            (1, 0),
            "{operation}: {failed:?}"
        );
        assert!(failed.last_error.as_deref().unwrap().contains("快照"));
        assert_eq!(vendor.count("Paid title"), 1);
        assert_eq!(vendor.count("Paid content"), 1);
        assert_eq!(vendor.count("Paid excerpt"), 0);
        assert!(target.received_packets().is_empty());
        let after: String = db
            .query_row("SELECT ctx_json FROM sync_inflight", [], |row| row.get(0))
            .unwrap();
        assert_eq!(after, before);
        db.execute_batch("DROP TRIGGER deny_retry;").unwrap();
        let next = super::super::sync_pair_run(&client, &pair.id, &creds, Some(&translator), &log)
            .await
            .unwrap();
        assert_eq!(
            (next.error_count, next.synced_count),
            (1, 0),
            "SQL recovery cannot authorize repeating an unknown HTTP 500 submit"
        );
        assert_eq!(vendor.count("Paid title"), 1);
        assert_eq!(vendor.count("Paid content"), 1);
        assert_eq!(vendor.count("Paid excerpt"), 0);
        assert!(target.received_packets().is_empty());
    }
}

#[test]
fn cli13_scope_digest_is_order_stable_and_contains_no_plain_auth_values() {
    let vendor = MockVendor::spawn();
    let packet: crate::sync_engine::packet::SyncPacket =
        serde_json::from_value(paid_source_packet("http://127.0.0.1:1")).unwrap();
    let pair = make_pair("http://127.0.0.1:1", "http://127.0.0.1:2");
    let mut first = vendor.translator();
    let auth = &mut Arc::get_mut(&mut first.runtime).unwrap().auth_values;
    auth.insert("b".into(), "mock-profile-b".into());
    auth.insert("a".into(), "mock-profile-a".into());
    let mut second = vendor.translator();
    let auth = &mut Arc::get_mut(&mut second.runtime).unwrap().auth_values;
    auth.insert("a".into(), "mock-profile-a".into());
    auth.insert("b".into(), "mock-profile-b".into());
    let scope = super::super::paid_snapshot_scope(&packet, &pair, Some(&first)).unwrap();
    assert_eq!(
        scope,
        super::super::paid_snapshot_scope(&packet, &pair, Some(&second)).unwrap()
    );
    assert_eq!(scope.len(), 64);
    assert!(scope.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert!(!scope.contains("mock-profile"));
}

#[tokio::test]
async fn relay_authority_changed_scope_retains_original_paid_evidence_without_new_fee() {
    let harness = WebUiTestHarness::new("", None).await.unwrap();
    let source = MockSite::spawn(SOURCE_UUID, |base| vec![paid_source_packet(base)]);
    let target = MockSite::spawn(TARGET_UUID, |_| vec![]);
    let vendor = MockVendor::spawn();
    vendor.fail_once("Paid content");
    let mut pair = make_pair(&source.base_url, &target.base_url);
    pair.sync_mode = crate::sync_engine::SyncMode::SyncAndTranslate;
    pair.translate_component_id = Some("cli13-mock-vendor".into());
    persist_pair(&crate::config::sync_pairs_file(), &pair);
    let creds = credentials_doc(&source, &target);
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let translator = vendor.translator();
    let log = harness.log_file.to_string_lossy().into_owned();
    let first = super::super::sync_pair_run(&client, &pair.id, &creds, Some(&translator), &log)
        .await
        .unwrap();
    assert_eq!(first.error_count, 1);
    let conn = crate::db::open_db(&harness.db_path.to_string_lossy()).unwrap();
    let before: String = conn
        .query_row("SELECT ctx_json FROM sync_inflight", [], |row| row.get(0))
        .unwrap();
    pair.target_lang = "fr_FR".into();
    persist_pair(&crate::config::sync_pairs_file(), &pair);
    let next = super::super::sync_pair_run(&client, &pair.id, &creds, Some(&translator), &log)
        .await
        .unwrap();
    assert!(
        next.error_count > 0 && next.synced_count == 0,
        "unresolved scope must stay parked: {next:?}"
    );
    assert_eq!(vendor.count("Paid title"), 1);
    assert_eq!(vendor.count("Paid content"), 1);
    assert!(target.received_packets().is_empty());
    let after: String = conn
        .query_row("SELECT ctx_json FROM sync_inflight", [], |row| row.get(0))
        .unwrap();
    assert_eq!(after, before);
}

#[tokio::test]
async fn relay_authority_paid_reply_projection_refusal_replays_without_resubmission() {
    let harness = WebUiTestHarness::new("", None).await.unwrap();
    let source = MockSite::spawn(SOURCE_UUID, |base| vec![paid_source_packet(base)]);
    let target = MockSite::spawn(TARGET_UUID, |_| vec![]);
    let vendor = MockVendor::spawn();
    let mut pair = make_pair(&source.base_url, &target.base_url);
    pair.sync_mode = crate::sync_engine::SyncMode::SyncAndTranslate;
    pair.translate_component_id = Some("cli13-mock-vendor".into());
    persist_pair(&crate::config::sync_pairs_file(), &pair);
    let conn = crate::db::open_db(&harness.db_path.to_string_lossy()).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER refuse_paid_projection BEFORE UPDATE OF ctx_json ON sync_inflight
         WHEN json_extract(NEW.ctx_json,'$.translation.title') IS NOT NULL
         BEGIN SELECT RAISE(ABORT,'owned paid projection refusal'); END;",
    )
    .unwrap();
    let creds = credentials_doc(&source, &target);
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let translator = vendor.translator();
    let log = harness.log_file.to_string_lossy().into_owned();
    let first = super::super::sync_pair_run(&client, &pair.id, &creds, Some(&translator), &log)
        .await
        .unwrap();
    assert_eq!((first.error_count, first.synced_count), (1, 0));
    assert_eq!(vendor.count("Paid title"), 1);
    conn.execute_batch("DROP TRIGGER refuse_paid_projection;")
        .unwrap();
    let next = super::super::sync_pair_run(&client, &pair.id, &creds, Some(&translator), &log)
        .await
        .unwrap();
    assert_eq!((next.error_count, next.synced_count), (0, 1));
    assert_eq!(
        vendor.count("Paid title"),
        1,
        "a durable paid reply must replay after a projection failure"
    );
    assert_eq!(vendor.count("Paid content"), 1);
    assert_eq!(vendor.count("Paid excerpt"), 1);
}

#[tokio::test]
async fn relay_authority_sync_only_admission_refusal_prevents_target_write() {
    let harness = WebUiTestHarness::new("", None).await.unwrap();
    let source = MockSite::spawn(SOURCE_UUID, |base| vec![paid_source_packet(base)]);
    let target = MockSite::spawn(TARGET_UUID, |_| vec![]);
    let pair = make_pair(&source.base_url, &target.base_url);
    persist_pair(&crate::config::sync_pairs_file(), &pair);
    let conn = crate::db::open_db(&harness.db_path.to_string_lossy()).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER refuse_admission BEFORE INSERT ON sync_inflight
         BEGIN SELECT RAISE(IGNORE); END;",
    )
    .unwrap();
    let creds = credentials_doc(&source, &target);
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let log = harness.log_file.to_string_lossy().into_owned();
    let report = super::super::sync_pair_run(&client, &pair.id, &creds, None, &log)
        .await
        .unwrap();
    assert!(
        report.error_count > 0 && report.synced_count == 0,
        "uncommitted sync unit must refuse target effects: {report:?}"
    );
    assert!(target.received_packets().is_empty());
}
