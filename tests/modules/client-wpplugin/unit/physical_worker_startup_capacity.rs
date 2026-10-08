// catalog: WEBUI-MOD-worker-rs
// oracle: L2
use super::*;
use base64::Engine;

struct StartupFixture {
    _key: crate::db::TestEnvVarGuard,
    root: tempfile::TempDir,
    _root: crate::db::TestEnvVarGuard,
    conn: Connection,
}

impl StartupFixture {
    fn new() -> Self {
        let key = crate::db::owned_mock_bindings_key();
        let root = tempfile::tempdir().unwrap();
        let guard =
            crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
        let conn = crate::db::open_db(root.path().join("owned.sqlite").to_str().unwrap()).unwrap();
        Self {
            _key: key,
            root,
            _root: guard,
            conn,
        }
    }

    fn paths(&self) -> (String, String) {
        (
            self.root.path().join("bindings.json").display().to_string(),
            self.root
                .path()
                .join("components.json")
                .display()
                .to_string(),
        )
    }

    fn full(&self) {
        let busy: i64 = self
            .conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
            .unwrap();
        assert_eq!(busy, 0);
        std::fs::write(
            self.root.path().join(".wptsall-storage-v1.json"),
            br#"{"format":"wptsall-storage-v1","max_physical_bytes":1}"#,
        )
        .unwrap();
    }
}

struct SavedStartupDelivery {
    item: i64,
    base: String,
    path: std::path::PathBuf,
    bindings: DomainTokenBindingsDoc,
    envelope: Value,
}

impl StartupFixture {
    fn delivery(&self, i18n: bool) -> SavedStartupDelivery {
        let origin = "http://127.0.0.1:9";
        let base = build_wp_base_url(origin, "owned").unwrap();
        let job = crate::db::jobs::create_job(
            &self.conn,
            &crate::db::jobs::CreateJobRequest {
                domain: base.clone(),
                relation_id: 7,
                business_line: "discovery".into(),
                triggered_by: "auto".into(),
            },
        )
        .unwrap();
        let path = self.root.path().join("original-result.json");
        let item = crate::db::jobs::record_item_outcome(
            &self.conn,
            &crate::db::jobs::CreateItemRequest {
                job_id: job,
                domain: base.clone(),
                relation_id: 7,
                business_line: if i18n { "plugin_i18n" } else { "post_content" }.into(),
                object_type: if i18n { "language_pack" } else { "post_type" }.into(),
                wp_object_id: 42,
                wp_object_subtype: if i18n { "plugin" } else { "post" }.into(),
                task_type: "text".into(),
                source_lang: "en".into(),
                target_lang: "zh".into(),
                component_id: "owned".into(),
                component_ids: vec!["owned".into()],
                selected_component_id: None,
                effective_source_lang: None,
                effective_target_lang: None,
                editable_overrides: None,
                raw_path: String::new(),
                client_task_id: "owned-startup-delivery".into(),
                max_retries: 3,
            },
            path.to_str().unwrap(),
            "translated",
            None,
        )
        .unwrap();
        let payload = if i18n {
            serde_json::json!({
                "business_line":"plugin_i18n","relation_id":7,
                "client_task_id":"owned-startup-delivery","worker_id":"owned-worker",
                "source_lang":"en","target_lang":"zh",
                "entries":[{"entry_id":42,"msgstr":"Original paid result"}],
            })
        } else {
            serde_json::to_value(
                serde_json::from_value::<TranslationCallbackPayload>(serde_json::json!({
                    "relation_id":7,"business_line":"post_content","object_type":"post_type",
                    "post_type":"post","object_id":42,
                    "translated_fields":{"post_title":"Original paid result"},"translated_meta":{},
                    "media_mappings":[],"client_task_id":"owned-startup-delivery",
                    "worker_id":"owned-worker","source_lang":"en","target_lang":"zh",
                    "execution_time_ms":1,"source_revision":"owned-startup-r1",
                }))
                .unwrap(),
            )
            .unwrap()
        };
        let mut envelope = serde_json::json!({
            "idempotency_key":"owned-startup-delivery","route_secret":"owned","payload":payload,
        });
        if i18n {
            envelope["payload_type"] = serde_json::json!("i18n_language_pack");
            envelope["persisted_at"] = serde_json::json!(1);
            let key = format!("language-pack-delivery-v1:{item}");
            let intent = serde_json::json!({
                "format":"language-pack-delivery-v1","item_id":item,"site":base,
                "path":path.to_str().unwrap(),"idempotency_key":"owned-startup-delivery",
                "payload":payload,"max_retries":3,
            });
            crate::db::system::set_encrypted_config(&self.conn, &key, &intent.to_string()).unwrap();
            let association_key = format!(
                "language-pack-delivery-entry-v1:{}",
                crate::db::system::private_json_digest(&serde_json::json!({
                    "physical":crate::db::unit_lock::UnitLease::content_suffix(
                        &base, 7, "language_pack", 42).unwrap(),
                    "business_line":"plugin_i18n",
                }))
                .unwrap()
            );
            crate::db::system::set_encrypted_config(
                &self.conn,
                &association_key,
                &serde_json::json!({
                    "format":"language-pack-delivery-entry-v1","item_id":item,
                    "delivery_key":key,"idempotency_key":"owned-startup-delivery",
                })
                .to_string(),
            )
            .unwrap();
        } else {
            crate::db::pending_callbacks::add_pending_callback(
                &self.conn,
                &crate::db::pending_callbacks::PendingCallbackEntry {
                    api_base_url: base.clone(),
                    idempotency_key: "owned-startup-delivery".into(),
                    payload: serde_json::from_value(payload).unwrap(),
                    route_secret: Some("owned".into()),
                    created_at: 1,
                    retry_count: 0,
                    last_retry_at: 0,
                    relation_id: 7,
                    object_id: 42,
                    object_type: "post_type".into(),
                },
            )
            .unwrap();
        }
        crate::bindings::save_encrypted_file(&path, &envelope.to_string()).unwrap();
        SavedStartupDelivery {
            item,
            base,
            path,
            envelope,
            bindings: serde_json::from_value(serde_json::json!({
                "version":3,"domains":{origin:{
                    "wp_client_token":"owned-token","route_secret":"owned",
                    "plugin_identity":"wpmmcc_ats",
                    "identity_verified_at":crate::bindings::format_rfc3339_utc(now_unix()),
                }},
            }))
            .unwrap(),
        }
    }

    fn assert_refused_without_installing(&self, bindings: &DomainTokenBindingsDoc) {
        let (component, local) = self.paths();
        let booked = crate::storage_capacity::inventory_at(self.root.path())
            .unwrap()
            .root_booked_bytes;
        assert!(load_component_documents(&self.conn, bindings, &component, &local).is_err());
        assert!(!Path::new(&component).exists() && !Path::new(&local).exists());
        assert_eq!(
            crate::storage_capacity::inventory_at(self.root.path())
                .unwrap()
                .root_booked_bytes,
            booked
        );
        assert_eq!(
            self.root
                .path()
                .join("owned.sqlite-wal")
                .metadata()
                .unwrap()
                .len(),
            0
        );
    }
}

#[test]
fn physical_worker_startup_fresh_missing_configuration_refuses_ambient_credit() {
    let fixture = StartupFixture::new();
    let (bindings, local) = fixture.paths();
    let credit = crate::storage_capacity::database_recovery_credit(
        &fixture.root.path().join("owned.sqlite"),
        false,
    )
    .unwrap();
    fixture.full();
    let booked = crate::storage_capacity::inventory_at(fixture.root.path())
        .unwrap()
        .root_booked_bytes;
    let result = crate::storage_capacity::with_database_credit(credit, || {
        load_component_documents(
            &fixture.conn,
            &DomainTokenBindingsDoc::default(),
            &bindings,
            &local,
        )
    });
    assert!(format!("{:#}", result.unwrap_err()).contains("STORAGE_CAPACITY_EXHAUSTED"));
    assert!(!Path::new(&bindings).exists() && !Path::new(&local).exists());
    assert_eq!(
        fixture
            .root
            .path()
            .join("owned.sqlite-wal")
            .metadata()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        crate::storage_capacity::inventory_at(fixture.root.path())
            .unwrap()
            .root_booked_bytes,
        booked
    );
}

#[test]
fn physical_worker_startup_fresh_configuration_initializes_encrypted_without_replacement() {
    let fixture = StartupFixture::new();
    let (bindings, local) = fixture.paths();
    let loaded = load_component_documents(
        &fixture.conn,
        &DomainTokenBindingsDoc::default(),
        &bindings,
        &local,
    )
    .unwrap();
    assert!(loaded.0.components.is_empty() && loaded.1.components.is_empty());
    let before_bindings = std::fs::read(&bindings).unwrap();
    let before_local = std::fs::read(&local).unwrap();
    assert!(before_bindings.starts_with(b"WPTC") && before_local.starts_with(b"WPTC"));
    fixture.full();
    load_component_documents(
        &fixture.conn,
        &DomainTokenBindingsDoc::default(),
        &bindings,
        &local,
    )
    .unwrap();
    assert_eq!(std::fs::read(&bindings).unwrap(), before_bindings);
    assert_eq!(std::fs::read(&local).unwrap(), before_local);
    assert!(install_missing_document(Path::new(&bindings), &loaded.0).is_err());
    assert_eq!(std::fs::read(&bindings).unwrap(), before_bindings);
}

#[test]
fn physical_worker_startup_existing_binding_does_not_allow_missing_local_configuration() {
    let fixture = StartupFixture::new();
    let (bindings, local) = fixture.paths();
    let original = br#"{"version":1,"components":{}}"#;
    std::fs::write(&bindings, original).unwrap();
    fixture.full();
    let result = load_component_documents(
        &fixture.conn,
        &DomainTokenBindingsDoc::default(),
        &bindings,
        &local,
    );
    assert!(format!("{:#}", result.unwrap_err()).contains("STORAGE_CAPACITY_EXHAUSTED"));
    assert_eq!(std::fs::read(&bindings).unwrap(), original);
    assert!(!Path::new(&local).exists());
}

#[test]
fn physical_worker_startup_damaged_documents_are_not_missing_authority() {
    let fixture = StartupFixture::new();
    let (bindings, local) = fixture.paths();
    for raw in ["", "null", "[]", "{"] {
        std::fs::write(&bindings, raw).unwrap();
        std::fs::write(&local, raw).unwrap();
        assert!(load_component_documents(
            &fixture.conn,
            &DomainTokenBindingsDoc::default(),
            &bindings,
            &local
        )
        .is_err());
        assert_eq!(std::fs::read_to_string(&bindings).unwrap(), raw);
        assert_eq!(std::fs::read_to_string(&local).unwrap(), raw);
    }
}

#[test]
fn physical_worker_startup_original_delivery_inventory_is_read_only_for_both_lanes() {
    for i18n in [false, true] {
        let fixture = StartupFixture::new();
        let saved = fixture.delivery(i18n);
        let before = std::fs::read(&saved.path).unwrap();
        fixture.full();
        let booked = crate::storage_capacity::inventory_at(fixture.root.path())
            .unwrap()
            .root_booked_bytes;
        let (component, local) = fixture.paths();
        let loaded =
            load_component_documents(&fixture.conn, &saved.bindings, &component, &local).unwrap();
        assert!(loaded.0.components.is_empty() && loaded.1.components.is_empty());
        assert!(!Path::new(&component).exists() && !Path::new(&local).exists());
        assert_eq!(std::fs::read(&saved.path).unwrap(), before);
        assert_eq!(
            crate::storage_capacity::inventory_at(fixture.root.path())
                .unwrap()
                .root_booked_bytes,
            booked
        );
        assert_eq!(
            fixture
                .root
                .path()
                .join("owned.sqlite-wal")
                .metadata()
                .unwrap()
                .len(),
            0
        );
    }
}

#[test]
fn physical_worker_startup_unverified_foreign_or_missing_binding_cannot_skip_initialization() {
    for field in [
        "identity_verified_at",
        "plugin_identity",
        "route_secret",
        "wp_client_token",
    ] {
        let fixture = StartupFixture::new();
        let mut saved = fixture.delivery(false);
        let entry = saved.bindings.domains.values_mut().next().unwrap();
        match field {
            "identity_verified_at" => entry.identity_verified_at = None,
            "plugin_identity" => entry.plugin_identity = Some(PluginIdentity::Wpmmcc),
            "route_secret" => entry.route_secret = "changed-owned-route".into(),
            "wp_client_token" => entry.wp_client_token.clear(),
            _ => unreachable!(),
        }
        fixture.full();
        fixture.assert_refused_without_installing(&saved.bindings);
    }
}

#[test]
fn physical_worker_startup_missing_or_legacy_plain_intent_is_not_recovery_permission() {
    for i18n in [false, true] {
        for missing in [false, true] {
            let fixture = StartupFixture::new();
            let saved = fixture.delivery(i18n);
            if i18n {
                let key = format!("language-pack-delivery-v1:{}", saved.item);
                if missing {
                    fixture
                        .conn
                        .execute("DELETE FROM system_config WHERE key=?1", [&key])
                        .unwrap();
                } else {
                    let plain =
                        crate::db::system::get_decrypted_config_checked(&fixture.conn, &key)
                            .unwrap()
                            .unwrap();
                    crate::db::system::set_system_config(
                        &fixture.conn,
                        &key,
                        &base64::engine::general_purpose::STANDARD.encode(plain),
                    )
                    .unwrap();
                }
            } else if missing {
                crate::db::pending_callbacks::remove_pending_callback(
                    &fixture.conn,
                    &saved.base,
                    7,
                    "post_type",
                    42,
                )
                .unwrap();
            } else {
                fixture.conn.execute(
                    "UPDATE pending_callbacks SET api_base_url=?1,payload_json=?2,route_secret_enc=?3",
                    rusqlite::params![
                        saved.base,
                        base64::engine::general_purpose::STANDARD.encode(saved.envelope["payload"].to_string()),
                        "owned"
                    ],
                ).unwrap();
                assert!(crate::db::pending_callbacks::find_pending_callback(
                    &fixture.conn,
                    &saved.base,
                    7,
                    "post_type",
                    42
                )
                .unwrap()
                .is_some());
            }
            fixture.full();
            fixture.assert_refused_without_installing(&saved.bindings);
        }
    }
}

#[test]
fn physical_worker_startup_changed_or_plain_result_cannot_borrow_original_permission() {
    for i18n in [false, true] {
        for plain in [false, true] {
            let fixture = StartupFixture::new();
            let mut saved = fixture.delivery(i18n);
            if plain {
                std::fs::write(&saved.path, saved.envelope.to_string()).unwrap();
            } else {
                saved.envelope["payload"]["client_task_id"] = serde_json::json!("changed-owned");
                crate::bindings::save_encrypted_file(&saved.path, &saved.envelope.to_string())
                    .unwrap();
            }
            let before = std::fs::read(&saved.path).unwrap();
            fixture.full();
            fixture.assert_refused_without_installing(&saved.bindings);
            assert_eq!(std::fs::read(&saved.path).unwrap(), before);
        }
    }
}

#[test]
fn physical_worker_startup_missing_or_changed_i18n_entry_association_refuses() {
    for missing in [false, true] {
        let fixture = StartupFixture::new();
        let saved = fixture.delivery(true);
        let key: String = fixture
            .conn
            .query_row(
                "SELECT key FROM system_config WHERE key LIKE 'language-pack-delivery-entry-v1:%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        if missing {
            fixture
                .conn
                .execute("DELETE FROM system_config WHERE key=?1", [&key])
                .unwrap();
        } else {
            crate::db::system::set_encrypted_config(
                &fixture.conn, &key,
                &serde_json::json!({"format":"language-pack-delivery-entry-v1","item_id":saved.item+1,
                    "delivery_key":format!("language-pack-delivery-v1:{}",saved.item),
                    "idempotency_key":"owned-startup-delivery"}).to_string(),
            ).unwrap();
        }
        fixture.full();
        fixture.assert_refused_without_installing(&saved.bindings);
    }
}

#[test]
fn physical_worker_startup_terminal_or_closed_retry_budget_is_not_original_delivery() {
    for terminal in [false, true] {
        let fixture = StartupFixture::new();
        let saved = fixture.delivery(false);
        if terminal {
            fixture
                .conn
                .execute(
                    "UPDATE translation_items SET status='done' WHERE id=?1",
                    [saved.item],
                )
                .unwrap();
        } else {
            fixture
                .conn
                .execute(
                    "UPDATE translation_items SET retry_count=max_retries WHERE id=?1",
                    [saved.item],
                )
                .unwrap();
        }
        fixture.full();
        fixture.assert_refused_without_installing(&saved.bindings);
    }
}
