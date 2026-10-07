//! Incremental cross-site sync engine (WBS 4.2/4.3, real contract).
//!
//! One run of a pair executes the plugin's own reconciliation architecture
//! client-side:
//!
//!   1. `/sync/digest` (keyset cursor `last_seen_source_id`) → discover
//!      new/changed/trashed entities at the source.
//!   2. `/sync/pull` (≤50 UUIDs per batch) → full packets for changed ones.
//!   3. Optional translation cascade (`sync_and_translate`): title,
//!      content, excerpt through the pair's translate component.
//!   4. Media transfer: every manifest asset is downloaded from the source
//!      and chunk-uploaded to the target (`/sync/media-chunk`), then content
//!      and manifest URLs are rewritten to the target attachment URLs.
//!   5. `/sync/push` relay to the target (origin context preserved).
//!   6. `/sync/reconcile-digest` rotation over the known set → propagates
//!      source deletions (`missing` verdict → delete packet) and picks up
//!      out-of-band edits (`source_newer`).
//!
//! All calls are HMAC-signed with the per-site shared secret from the
//! pairing handshake; the sender UUID is the client origin UUID.
//!
//! Bookkeeping is honest by construction: `last_sync_count` only counts
//! packets the target actually acked; `last_error` carries the first
//! error; the digest cursor only advances over scanned items.

use reqwest::Client;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;

use super::credentials::{find_peer_credential, PeerCredentialsDoc};
use super::packet::SyncPacket;
use super::review::{
    has_pending_uuid, load_sync_review, sync_review_file, update_sync_review, upsert_pending_item,
    SyncReviewItem, SyncReviewStatus,
};
use super::shipper::{
    build_tombstone_packet, relay_packet_for_target, DigestItem, RelayTranslation, Shipper,
};
use super::state::{
    load_sync_state, remove_pair_state, update_pair_state, KnownEntity, PairSyncState, SyncStateDoc,
};
use super::storage::{
    delete_pair_in_doc, find_pair_in_doc, find_pair_in_doc_mut, load_sync_pairs, update_sync_pairs,
};
use super::types::{ConflictStrategy, SyncMode, SyncPair, SyncPairStatus};
use crate::component_rt::runner::translate_text_via_component_with_env;
use crate::config::{sync_pairs_file, sync_state_file};
use crate::logging::{log_event, unix_ts, unix_ts_ms};

/// Digest page size per source scan.
const DIGEST_PAGE_SIZE: u32 = 100;
/// Pull batch size (plugin caps `/sync/pull` at 50 UUIDs).
const PULL_BATCH_SIZE: usize = 50;

/// Handle to a ready translation component runtime (built by the caller —
/// worker lane loads it from component bindings, the Web UI run trigger
/// uses its state-backed registry). Opaque outside this module: callers
/// only pass it through to `sync_pair_run`.
pub struct TranslatorHandle {
    pub(crate) vendor_client: Client,
    pub(crate) runtime: Arc<crate::types::ComponentRuntime>,
}

/// Build a translator handle for one component id (local-mode registry
/// load, same path the worker uses for task translation).
pub async fn build_translator(
    client: &Client,
    component_id: &str,
    log_file: &str,
) -> anyhow::Result<TranslatorHandle> {
    let component_id = component_id.trim();
    if component_id.is_empty() {
        anyhow::bail!("component_id must not be empty");
    }
    let bindings_path = crate::config::component_bindings_file();
    // §67 recheck (tasks/cursor CURSOR-COMMERCIAL-USE-RECHECK-20260929):
    // the sync-pair run trigger used to load component credentials from the
    // file path only. Under the sqlite WebUI runtime the operator's bindings
    // live in the runtime store, so every sync_and_translate run failed at
    // the translator build ("no supported component runtime loaded") even
    // though the component was configured and the worker lane worked. Load
    // from the same runtime source the worker lane resolves (sqlite when
    // enabled, file otherwise) and keep the file path for loader save-backs.
    let sqlite_runtime = std::env::var("WPTSALL_WEB_UI")
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes"
            )
        })
        .unwrap_or(false);
    let mut bindings = if sqlite_runtime {
        crate::db::bindings::load_runtime_component_bindings_doc()?
    } else {
        crate::bindings::load_component_bindings(&bindings_path)?
    };
    let target_ids = std::collections::BTreeSet::from([component_id.to_string()]);
    let registry = crate::component_rt::loader::load_component_runtimes(
        client,
        "",
        "",
        log_file,
        &mut bindings,
        &bindings_path,
        Some(&target_ids),
        None,
    )
    .await?;
    let runtime = registry
        .runtimes
        .get(component_id)
        .cloned()
        .ok_or_else(|| {
            anyhow::anyhow!("翻译组件 '{component_id}' 没有可用的运行时（未启用或缺少凭证）")
        })?;
    let profiles = crate::bindings::load_proxy_profiles(&crate::config::proxy_profiles_file())?;
    let proxy_pool = crate::component_rt::proxy::ProxyClientPool::new(&profiles.profiles)?;
    Ok(TranslatorHandle {
        vendor_client: proxy_pool
            .get_client(runtime.proxy_profile_id.as_deref())?
            .clone(),
        runtime: Arc::new(runtime),
    })
}

#[derive(Debug, Clone, Default)]
pub struct SyncRunReport {
    pub pair_id: String,
    pub source_domain: String,
    pub target_domain: String,
    /// GAP-06 (批 C): run-level trace id — one per sync_pair_run call,
    /// carried by every sync_engine event for this run so its full
    /// timeline (started → packets → finished/failed) greps as one unit.
    /// Millisecond-suffixed so near-simultaneous runs of the same pair
    /// stay distinguishable (ts/ts_ms already give the shared timeline).
    pub trace_id: String,
    pub scanned_count: usize,
    pub pulled_count: usize,
    pub synced_count: usize,
    pub pending_review_count: usize,
    pub trashed_count: usize,
    pub deleted_count: usize,
    pub media_transferred: usize,
    pub skipped_count: usize,
    pub error_count: usize,
    pub last_error: Option<String>,
}

/// 批 C (tasks/5.3falsh2/12): total-failure semantics for the sync-pair
/// backoff domain — a run that synced NOTHING and errored at least once
/// is a pair-level failure (dead target site / revoked credentials), and
/// only that trips the pair cooldown. A run with any successful push
/// proves the lane works, so residual per-packet errors are packet-level
/// concerns; a clean park (manual_review) has zero errors and never
/// counts as failure either.
pub fn sync_run_is_total_failure(report: &SyncRunReport) -> bool {
    report.synced_count == 0 && report.error_count > 0
}

/// Load the pair, run one incremental pass, persist bookkeeping.
#[allow(clippy::too_many_arguments)]
pub async fn sync_pair_run(
    client: &Client,
    pair_id: &str,
    credentials: &PeerCredentialsDoc,
    translator: Option<&TranslatorHandle>,
    log_file: &str,
) -> anyhow::Result<SyncRunReport> {
    let pairs_path = sync_pairs_file();
    let state_path = sync_state_file();

    let pair = match find_pair_in_doc(&load_sync_pairs(&pairs_path)?, pair_id) {
        Some(p) => p.clone(),
        None => anyhow::bail!("Sync pair '{}' not found", pair_id),
    };
    let state_doc: SyncStateDoc = load_sync_state(&state_path)?;
    if parks_in_review_inbox(&pair) {
        load_sync_review(&sync_review_file())?;
    }

    let mut report = SyncRunReport {
        pair_id: pair.id.clone(),
        source_domain: pair.source_domain.clone(),
        target_domain: pair.target_domain.clone(),
        trace_id: format!("sync-{}-{}", pair.id, unix_ts_ms()),
        ..Default::default()
    };

    if pair.status == SyncPairStatus::Paused {
        return Ok(report);
    }

    // GAP-06 收尾: sync 泳道本就有 run 级 trace_id（批 C），此处将其装甲为
    // 出站 trace 上下文——本 pair 的全部 WP/供应商请求携带 X-WPTSALL-Trace-Id，
    // 与发现泳道的 disc- 前缀同构，三方日志可按同一 id 对齐。
    let _run_trace_guard = crate::auth::scoped_run_trace_id(report.trace_id.clone());

    let _ = log_event(
        log_file,
        "info",
        "sync_engine.pair_started",
        json!({
            "pair_id": pair.id,
            "trace_id": report.trace_id,
            "source": pair.source_domain,
            "target": pair.target_domain,
            "mode": format!("{:?}", pair.sync_mode),
        }),
    );

    // --- Credentials gate: both ends must be paired before anything else.
    let source_cred = match find_peer_credential(credentials, &pair.source_domain) {
        Some(c) => c.clone(),
        None => {
            return finish_with_error(
                &pairs_path,
                &pair,
                &mut report,
                &format!(
                    "源站点 {} 尚未完成 WPMMCC 配对（请先在站点管理中绑定并配对）",
                    pair.source_domain
                ),
                log_file,
            )
        }
    };
    let target_cred = match find_peer_credential(credentials, &pair.target_domain) {
        Some(c) => c.clone(),
        None => {
            return finish_with_error(
                &pairs_path,
                &pair,
                &mut report,
                &format!(
                    "目标站点 {} 尚未完成 WPMMCC 配对（请先在站点管理中绑定并配对）",
                    pair.target_domain
                ),
                log_file,
            )
        }
    };

    let client_uuid = credentials.client_origin_uuid.clone();
    let source_secret = source_cred.shared_secret_bytes()?;
    let target_secret = target_cred.shared_secret_bytes()?;
    let source_base = source_cred.rest_base_url();
    let target_base = target_cred.rest_base_url();
    // Real source-site identity for tombstone authority (no placeholders).
    let source_site_url = source_base
        .trim_end_matches('/')
        .strip_suffix("/wp-json/wpmmcc/v1")
        .unwrap_or(source_base.as_str())
        .to_string();

    // --- Translation cascade gate: mode requires a configured component
    // and a usable runtime; otherwise the run stops with an explicit error
    // instead of silently shipping untranslated content.
    let translator = match pair.sync_mode {
        SyncMode::SyncAndTranslate => {
            let component_id = pair.translate_component_id.as_deref().unwrap_or("").trim();
            if component_id.is_empty() {
                return finish_with_error(
                    &pairs_path,
                    &pair,
                    &mut report,
                    "sync_and_translate 需要配置翻译组件（translate_component_id）",
                    log_file,
                );
            }
            match translator {
                Some(t) => Some(t),
                None => {
                    return finish_with_error(
                        &pairs_path,
                        &pair,
                        &mut report,
                        &format!("翻译组件 '{component_id}' 未能加载运行时，无法执行级联翻译"),
                        log_file,
                    )
                }
            }
        }
        SyncMode::SyncOnly => None,
    };

    let shipper = Shipper::new(client.clone())?.with_source_confirmation(
        source_base.clone(), source_secret.clone(), target_cred.peer_uuid.clone(),
    );

    // Translation, media and target writes require a durable checkpoint
    // store. A broken store never authorizes a stateless external effect.
    let inflight_db: Option<Arc<tokio::sync::Mutex<rusqlite::Connection>>> =
        match crate::db::open_db(&crate::config::db_path()) {
            Ok(conn) => Some(Arc::new(tokio::sync::Mutex::new(conn))),
            Err(error) => {
                return finish_with_error(
                    &pairs_path,
                    &pair,
                    &mut report,
                    &format!("打开翻译快照数据库失败: {error:#}"),
                    log_file,
                );
            }
        };

    // --- Phase 1+2: digest scan (keyset pages) → pull → ship.
    let mut cursor = state_doc
        .pairs
        .get(&pair.id)
        .map(|s| s.last_seen_source_id)
        .unwrap_or(0);
    let pair_initial_cursor = cursor;
    // FL-11 (doc 24 §四.3, SIM-14): the digest is a pure keyset cursor on
    // source-local ids. Persisting the raw scan maximum BEFORE the
    // pull+ship meant a first-ship failure (transient 5xx/401) left the
    // entity unknown to BOTH later digests (its id ≤ cursor forever) and
    // the reconcile rotation (only asks known entities) — a silent
    // permanent loss. The safe cursor only advances over CONVERGED items
    // (skipped-as-stable, successfully shipped, or tombstoned) and freezes
    // at the first item error, so the next run rescans the unconverged
    // tail. Fingerprint stability keeps the rescan cheap (converged items
    // skip without wire traffic).
    // §68 amendment (contiguous prefix): convergence is decided per item,
    // and the cursor may only advance over an UNBROKEN run of converged
    // items from the digest page head — never max() over individually
    // converged ids. A fingerprint-stable skip at a HIGH id used to leap
    // the cursor over a lower-id ship failure in the same page (and the
    // pre-ship persist made that poisoned cursor durable before the ships
    // even ran), permanently hiding the failed item from every later
    // digest — reintroducing exactly the loss FL-11 exists to prevent.
    //
    // CLI-01 amendment (cross-page): the §68 rule judged convergence per
    // PAGE, but the safe cursor is shared across pages — a fully-converged
    // page 2 still leapfrogged a page-1 failure. Once any page fails to
    // fully converge AFTER its ship phase, the cursor freezes for the rest
    // of this run; the next run's digest rescans from the frozen cursor
    // (fingerprint-stable skips keep that cheap).
    let mut safe_cursor = pair_initial_cursor;
    let mut cursor_blocked = false;

    loop {
        let items = match shipper
            .fetch_digest(
                &pair.id,
                &source_base,
                &client_uuid,
                &source_secret,
                cursor,
                &pair.post_types,
                DIGEST_PAGE_SIZE,
            )
            .await
        {
            Ok(items) => items,
            Err(err) => {
                return finish_with_error(
                    &pairs_path,
                    &pair,
                    &mut report,
                    &format!("源站 digest 拉取失败: {err:#}"),
                    log_file,
                )
            }
        };
        if items.is_empty() {
            break;
        }
        report.scanned_count += items.len();

        let state_snapshot: HashMap<String, KnownEntity> =
            pair_state_snapshot(&state_path, &pair.id)?.known;

        let mut to_pull: Vec<(String, u64)> = Vec::new();
        let mut to_trash: Vec<DigestItem> = Vec::new();
        // §68: per-item convergence for the contiguous-prefix advance.
        // Absent = unconverged (to_pull / to_trash / pull-lost).
        let mut page_converged: HashMap<u64, bool> = HashMap::new();
        let review_doc = load_sync_review(&sync_review_file())?;
        for item in &items {
            cursor = cursor.max(item.local_id);
            if has_pending_uuid(&review_doc, &pair.id, &item.canonical_uuid) {
                report.skipped_count += 1;
                // Held for human review — the approve route ships it and
                // records the known state itself.
                page_converged.insert(item.local_id, true);
                continue;
            }
            match state_snapshot.get(&item.canonical_uuid) {
                Some(known) => {
                    if known.source_fingerprint == item.source_fingerprint {
                        report.skipped_count += 1;
                        // Fingerprint-stable: converged (advances the cursor
                        // only if every lower-id item converged too).
                        page_converged.insert(item.local_id, true);
                        continue;
                    }
                    if item.post_status == "trash" && known.post_status != "trash" {
                        if known.review_rejected && known.last_shipped_at == 0 {
                            report.skipped_count += 1;
                            page_converged.insert(item.local_id, true);
                            update_pair_state(&state_path, &pair.id, |s| {
                                s.known.remove(&item.canonical_uuid);
                            })?;
                            continue;
                        }
                        to_trash.push(item.clone());
                        continue;
                    }
                    to_pull.push((item.canonical_uuid.clone(), item.local_id));
                }
                None => {
                    if item.post_status == "trash" {
                        // Never shipped and already trashed at source —
                        // nothing to propagate.
                        report.skipped_count += 1;
                        page_converged.insert(item.local_id, true);
                    } else {
                        to_pull.push((item.canonical_uuid.clone(), item.local_id));
                    }
                }
            }
        }

        // Persist the SAFE cursor before the (possibly long) pull+ship so a
        // crash mid-batch does not rescan the whole source next run — while
        // never advancing past an item whose ship has not converged yet.
        // §68: contiguous-prefix advance (skips resolved so far — the first
        // to_pull/to_trash item blocks the cursor below it).
        // CLI-01: stopping early here is EXPECTED whenever the page has
        // items to pull/trash, so the pre-ship walk never sets
        // cursor_blocked — only the post-ship walk (below) may.
        if !cursor_blocked {
            advance_safe_cursor_contiguous(&mut safe_cursor, &items, &page_converged);
        }
        update_pair_state(&state_path, &pair.id, |s| {
            s.last_seen_source_id = safe_cursor;
        })?;

        if !to_pull.is_empty() {
            for batch in to_pull.chunks(PULL_BATCH_SIZE) {
                let uuids: Vec<String> = batch.iter().map(|(u, _)| u.clone()).collect();
                let id_by_uuid: HashMap<String, u64> =
                    batch.iter().cloned().collect::<HashMap<_, _>>();
                let packets = match shipper
                    .pull_packets(&pair.id, &source_base, &client_uuid, &source_secret, &uuids)
                    .await
                {
                    Ok(p) => p,
                    Err(err) => {
                        // §68: the un-pulled items stay absent from
                        // page_converged, so the contiguous prefix blocks
                        // the cursor below them — next run rescans.
                        record_item_error(&mut report, &format!("源站 pull 失败: {err:#}"));
                        break;
                    }
                };
                report.pulled_count += packets.len();
                for packet in packets {
                    let guid = packet.entity.guid.clone();
                    match ship_one_packet(
                        &shipper,
                        &pair,
                        log_file,
                        &packet,
                        translator,
                        &client_uuid,
                        &target_base,
                        &target_secret,
                        &mut report,
                        inflight_db.as_ref(),
                    )
                    .await
                    {
                        Ok(()) => {
                            if let Some(id) = id_by_uuid.get(&guid) {
                                // §68: converged — the contiguous prefix
                                // decides whether the cursor may advance (a
                                // lower-id failure still blocks it).
                                page_converged.insert(*id, true);
                            }
                        }
                        Err(err) => {
                            record_item_error(&mut report, &err);
                            // §68: explicitly unconverged — the contiguous
                            // prefix stops below this id so the next run
                            // rescans the tail (skips are free — fingerprint
                            // stability). Later successes still ship.
                            if let Some(id) = id_by_uuid.get(&guid) {
                                page_converged.insert(*id, false);
                            }
                        }
                    }
                }
            }
        }

        for item in to_trash {
            let tombstone = build_tombstone_packet(
                &item.canonical_uuid,
                &item.post_type,
                item.local_id,
                &source_cred.peer_uuid,
                item.vector_clock,
                "trash",
                &source_site_url,
                &pair.source_lang,
                &pair.target_lang,
            );
            match ship_tombstone_packet(
                &shipper,
                &pair,
                log_file,
                &tombstone,
                &client_uuid,
                &target_base,
                &target_secret,
                inflight_db.as_ref(),
            )
            .await
            {
                Ok(()) => {
                    report.trashed_count += 1;
                    page_converged.insert(item.local_id, true);
                    let uuid = item.canonical_uuid.clone();
                    let status = item.post_status.clone();
                    let fp = item.source_fingerprint.clone();
                    let clock = item.vector_clock;
                    update_pair_state(&state_path, &pair.id, |s| {
                        if let Some(entry) = s.known.get_mut(&uuid) {
                            entry.post_status = status;
                            entry.source_fingerprint = fp;
                            entry.vector_clock = clock;
                        }
                    })?;
                }
                Err(err) => {
                    record_item_error(
                        &mut report,
                        &format!("trash 推送失败 ({}): {}", item.canonical_uuid, err),
                    );
                }
            }
        }

        // Persist the post-ship safe cursor (crash mid-reconcile keeps the
        // ship progress; the unconverged tail is rescanned next run).
        // §68: contiguous-prefix advance over the fully-resolved page.
        // CLI-01: if THIS page still has an unconverged item after its
        // ships, freeze the cursor for the REST OF THE RUN — otherwise a
        // later, fully-converged page would leap the persisted cursor over
        // this page's failure and the item would never be re-reported.
        if !cursor_blocked
            && !advance_safe_cursor_contiguous(&mut safe_cursor, &items, &page_converged)
        {
            cursor_blocked = true;
        }
        update_pair_state(&state_path, &pair.id, |s| {
            s.last_seen_source_id = safe_cursor;
        })?;

        if (items.len() as u32) < DIGEST_PAGE_SIZE {
            break;
        }
    }

    // --- Phase 3: reconcile rotation — deletions and out-of-band drift.
    {
        let snapshot = pair_state_snapshot(&state_path, &pair.id)?;
        let (batch_refs, next_cursor) = super::state::reconcile_batch(&snapshot);
        let batch: Vec<KnownEntity> = batch_refs.into_iter().cloned().collect();
        if !batch.is_empty() {
            let fingerprints: Vec<(String, String, u64)> = batch
                .iter()
                .map(|k| {
                    (
                        k.canonical_uuid.clone(),
                        k.source_fingerprint.clone(),
                        k.vector_clock,
                    )
                })
                .collect();
            match shipper
                .reconcile_fingerprints(
                    &pair.id,
                    &source_base,
                    &client_uuid,
                    &source_secret,
                    &fingerprints,
                )
                .await
            {
                Ok(verdicts) => {
                    for entry in &batch {
                        match verdicts.get(&entry.canonical_uuid).map(String::as_str) {
                            Some("missing") => {
                                if entry.review_rejected && entry.last_shipped_at == 0 {
                                    report.skipped_count += 1;
                                    update_pair_state(&state_path, &pair.id, |s| {
                                        s.known.remove(&entry.canonical_uuid);
                                    })?;
                                    continue;
                                }
                                let tombstone = build_tombstone_packet(
                                    &entry.canonical_uuid,
                                    &entry.post_type,
                                    entry.target_post_id.unwrap_or(0),
                                    &source_cred.peer_uuid,
                                    entry.vector_clock,
                                    "delete",
                                    &source_site_url,
                                    &pair.source_lang,
                                    &pair.target_lang,
                                );
                                match ship_tombstone_packet(
                                    &shipper,
                                    &pair,
                                    log_file,
                                    &tombstone,
                                    &client_uuid,
                                    &target_base,
                                    &target_secret,
                                    inflight_db.as_ref(),
                                )
                                .await
                                {
                                    Ok(()) => {
                                        report.deleted_count += 1;
                                        let uuid = entry.canonical_uuid.clone();
                                        update_pair_state(&state_path, &pair.id, |s| {
                                            s.known.remove(&uuid);
                                        })?;
                                    }
                                    Err(err) => record_item_error(
                                        &mut report,
                                        &format!(
                                            "delete 推送失败 ({}): {}",
                                            entry.canonical_uuid,
                                            err
                                        ),
                                    ),
                                }
                            }
                            Some("source_newer") => {
                                let review = load_sync_review(&sync_review_file())?;
                                if has_pending_uuid(&review, &pair.id, &entry.canonical_uuid) {
                                    report.skipped_count += 1;
                                    continue;
                                }
                                // FL-10 (doc 24 §四.3, SIM-14): the source's
                                // content changed after this client last
                                // shipped the entity. The digest CANNOT
                                // re-report it — the plugin's digest is a
                                // pure keyset cursor on local ids, so an
                                // already-scanned post's id never exceeds
                                // last_seen_id again — which makes the
                                // reconcile rotation the ONLY update-
                                // propagation path. Re-pull the entity and
                                // ship the new version; ship_one_packet
                                // refreshes the known fingerprint on
                                // success, so the next rotation converges
                                // to in_sync.
                                match shipper
                                    .pull_packets(
                                        &pair.id,
                                        &source_base,
                                        &client_uuid,
                                        &source_secret,
                                        &[entry.canonical_uuid.clone()],
                                    )
                                    .await
                                {
                                    Ok(packets) if !packets.is_empty() => {
                                        for packet in &packets {
                                            if let Err(err) = ship_one_packet(
                                                &shipper,
                                                &pair,
                                                log_file,
                                                packet,
                                                translator,
                                                &client_uuid,
                                                &target_base,
                                                &target_secret,
                                                &mut report,
                                                inflight_db.as_ref(),
                                            )
                                            .await
                                            {
                                                record_item_error(
                                                    &mut report,
                                                    &format!(
                                                        "source_newer 重推失败 ({}): {err}",
                                                        entry.canonical_uuid
                                                    ),
                                                );
                                            }
                                        }
                                    }
                                    Ok(_) => record_item_error(
                                        &mut report,
                                        &format!(
                                            "source_newer 重拉为空 ({})",
                                            entry.canonical_uuid
                                        ),
                                    ),
                                    Err(err) => record_item_error(
                                        &mut report,
                                        &format!(
                                            "source_newer 重拉失败 ({}): {err:#}",
                                            entry.canonical_uuid
                                        ),
                                    ),
                                }
                            }
                            // in_sync / local_edited: nothing to do from
                            // the client side (local_edited means the
                            // TARGET diverged — a later source change or
                            // manual fix resolves it).
                            _ => {}
                        }
                    }
                }
                Err(err) => record_item_error(&mut report, &format!("reconcile 校验失败: {err:#}")),
            }
            update_pair_state(&state_path, &pair.id, |s| {
                s.reconcile_cursor = next_cursor;
            })?;
        }
    }

    // 批 R: in-transit observability — shipping rows that survived this run
    // without converging. Deliberately NO synthetic re-push here: a stored
    // packet whose source has moved on (edited/deleted) would push stale
    // content; unconverged units ride the next run's recovery path (paid
    // snapshot reuse + same-packet_id idempotent re-push). Age never
    // authorizes deletion of an unresolved effect.
    if let Some(db) = inflight_db.as_ref() {
        match crate::db::sync_inflight::list_pair_shipping(db, &pair.id).await {
            Ok(rows) => {
                if !rows.is_empty() {
                    let _ = log_event(
                        log_file,
                        "warning",
                        "sync_engine.inflight_unresolved",
                        json!({
                            "pair_id": pair.id,
                            "trace_id": report.trace_id,
                            "units": rows.len(),
                            "sample": rows
                                .iter()
                                .take(5)
                                .map(|(u, r)| json!({
                                    "uuid": u,
                                    "attempts": r.attempts,
                                    "has_relayed_packet": !r.relayed_json.is_empty(),
                                }))
                                .collect::<Vec<_>>(),
                        }),
                    );
                }
            }
            Err(error) => {
                record_item_error(&mut report, &format!("读取保留的同步快照失败: {error:#}"))
            }
        }
    }

    // --- Finalize pair bookkeeping.
    let now = unix_ts();
    let error_summary = report.last_error.clone();
    let synced = report.synced_count as u32;
    // FL-11: the pairs doc mirrors the STATE's safe cursor — the scan
    // maximum is NOT persisted when an item failed, so the unconverged
    // tail is rescanned next run instead of being silently lost.
    let cursor_final = safe_cursor;
    update_sync_pairs(&pairs_path, |doc| {
        if let Some(p) = find_pair_in_doc_mut(doc, &pair.id) {
            p.last_sync_at = Some(now);
            p.last_sync_count = Some(p.last_sync_count.unwrap_or(0) + synced);
            p.last_error = error_summary;
            p.last_seen_source_id = Some(cursor_final);
            p.updated_at = now;
            doc.updated_at = now;
        }
        Ok(())
    })?;

    let _ = log_event(
        log_file,
        if report.error_count > 0 {
            "warning"
        } else {
            "info"
        },
        "sync_engine.pair_finished",
        json!({
            "pair_id": pair.id,
            "trace_id": report.trace_id,
            "scanned": report.scanned_count,
            "pulled": report.pulled_count,
            "synced": report.synced_count,
            "trashed": report.trashed_count,
            "deleted": report.deleted_count,
            "media": report.media_transferred,
            "skipped": report.skipped_count,
            "errors": report.error_count,
            "cursor": cursor_final,
        }),
    );

    Ok(report)
}

/// X-1 (tasks/5.3falsh2/12 批 B P3): does this pair park packets in the
/// client-side sync-review inbox instead of pushing directly?
///
/// Two ways in: the per-push opt-in flag (`review_before_push`) or the
/// pair's conflict strategy being `manual_review`. Client-side parking is
/// a whole-flow gate — the relay cannot know the target's local state
/// before pushing, so every packet waits for a human approve/reject.
/// Conflict-time arbitration on the target site (shadow draft) remains the
/// server-side manual_review behavior via the handshake-carried strategy;
/// both review surfaces coexist by design.
fn parks_in_review_inbox(pair: &SyncPair) -> bool {
    pair.review_before_push || pair.conflict_strategy == ConflictStrategy::ManualReview
}

fn paid_snapshot_scope(
    packet: &SyncPacket,
    pair: &SyncPair,
    translator: Option<&TranslatorHandle>,
) -> Result<String, String> {
    let scope = json!({
        "action":packet.action,
        "sync_mode":pair.sync_mode,
        "manifest":packet.multimodal_manifest,
        "source_fingerprint": packet.source_fingerprint,
        "entity": packet.entity,
        "source_site_uuid": packet.origin_context.origin_site_uuid,
        "origin_permalink": packet.origin_context.origin_permalink,
        "origin_site_url": packet.origin_context.origin_site_url,
        "origin_blog_id": packet.origin_context.origin_blog_id,
        "origin_lang": packet.origin_context.origin_lang,
        "source_domain": pair.source_domain,
        "target_domain": pair.target_domain,
        "source_lang": pair.source_lang,
        "target_lang": pair.target_lang,
        "component_id": pair.translate_component_id,
        "runtime":translator.map(|translator| {
            let runtime = &translator.runtime;
            json!({
                "template": runtime.template,
                "auth": runtime.auth_values.iter().collect::<std::collections::BTreeMap<_, _>>(),
                "language_map": runtime.language_map.iter().collect::<std::collections::BTreeMap<_, _>>(),
                "key_pool":runtime.key_pool.as_ref().map(|pool| pool.snapshot()),
                "oauth_pool":runtime.oauth_pool.as_ref().map(|pool| pool.snapshot()),
                "proxy":runtime.proxy_profile_id,
                "max_concurrent":runtime.runtime_max_concurrent_requests,
                "min_interval":runtime.runtime_min_interval_ms,
            })
        }),
    });
    let bytes =
        serde_json::to_vec(&scope).map_err(|error| format!("翻译快照上下文序列化失败: {error}"))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

/// Pull → (translate) → (media) → relay-push one packet; update state on ack.
///
/// 批 R wrapper around the relay state machine: opens the durable in-flight
/// row (open-or-reopen — a re-begin preserves paid snapshots), routes
/// Ok → close / Err → note-error (row stays shipping for the next run's
/// recovery). Paid field lookup/begin/checkpoint failures stop before any
/// next paid phase or target write.
async fn ship_one_packet(
    shipper: &Shipper,
    pair: &SyncPair,
    log_file: &str,
    packet: &SyncPacket,
    translator: Option<&TranslatorHandle>,
    client_uuid: &str,
    target_base: &str,
    target_secret: &[u8],
    report: &mut SyncRunReport,
    inflight_db: Option<&Arc<tokio::sync::Mutex<rusqlite::Connection>>>,
) -> Result<(), String> {
    let uuid = packet.entity.guid.clone();
    let db = inflight_db.ok_or_else(|| "同步副作用缺少持久化权限；未执行".to_string())?;
    let lease = {
        let conn = db.lock().await;
        let suffix = format!(
            "relay-{}",
            crate::db::system::private_json_digest(&json!({"pair":pair.id,"uuid":uuid}),)
                .map_err(|error| format!("同步单元权限失败: {error:#}"))?
        );
        crate::db::unit_lock::UnitLease::acquire(
            &conn,
            db,
            &suffix,
            "RELAY_UNIT_BUSY: original relay unit is active",
        )
        .map_err(|error| format!("同步单元权限失败 ({uuid}): {error:#}"))?
    };
    let scope = paid_snapshot_scope(packet, pair, translator)?;

    // A changed action/source/configuration cannot erase an unfinished
    // paid or target effect and authorize a different one.
    let inflight = match inflight_db {
        Some(db) => match crate::db::sync_inflight::find_shipping(db, &pair.id, &uuid).await {
            Ok(Some(row))
                if (row.ctx.action.is_empty() || row.ctx.action == packet.action)
                    && row.ctx.translation_scope == scope
                    && (translator.is_some() || row.ctx.translation.is_none()) =>
            {
                Some(row)
            }
            Ok(Some(_)) => {
                return Err(format!(
                    "同步快照范围已变更 ({uuid})；原始副作用未确认，保留证据，未执行新操作"
                ))
            }
            Err(error) => {
                return Err(format!("读取翻译快照失败 ({uuid}): {error:#}"));
            }
            Ok(None) => None,
        },
        None => None,
    };
    if let Some(row) = inflight.as_ref() {
        // 批 R 取证痛点「零在途可观测」的对账面:每次恢复可从日志确认
        // 哪些已付费阶段被复用、重试到第几次。
        let _ = log_event(
            log_file,
            "info",
            "sync_engine.inflight_resumed",
            json!({
                "pair_id": pair.id,
                "canonical_uuid": uuid,
                "attempts": row.attempts,
                "reuses_translation": row.ctx.translation.is_some(),
                "media_done": row.ctx.media_url_map.len(),
                "reuses_relayed_packet": !row.relayed_json.is_empty(),
            }),
        );
    }
    if let Some(db) = inflight_db {
        crate::db::sync_inflight::begin_shipping_scoped(db, &pair.id, &uuid, &scope)
            .await
            .map_err(|error| format!("建立翻译快照失败 ({uuid}): {error:#}"))?;
    }

    let result = ship_one_packet_inner(
        shipper,
        pair,
        log_file,
        packet,
        translator,
        client_uuid,
        target_base,
        target_secret,
        report,
        inflight_db,
        inflight.as_ref(),
        &lease,
    )
    .await;

    lease.assert_native_owner().map_err(|error| format!("同步执行权已变更；原快照保留: {error:#}"))?;
    if let Some(db) = inflight_db {
        match &result {
            Ok(()) => {
                // Durable projection reached. Archive the original encrypted
                // packet and paid/receipt evidence before releasing the row.
                crate::db::sync_inflight::close_shipping(db, &pair.id, &uuid)
                    .await
                    .map_err(|error| format!("同步已接收但关闭快照失败 ({uuid}): {error:#}"))?;
            }
            Err(msg) => {
                // Keep the row shipping so the next run resumes with the
                // paid snapshots; the snippet bounds the error column.
                let snippet: String = msg.chars().take(300).collect();
                crate::db::sync_inflight::note_error(db, &pair.id, &uuid, &snippet)
                    .await
                    .map_err(|error| format!("{msg}; 保存快照错误失败 ({uuid}): {error:#}"))?;
            }
        }
    }
    result
}

/// Durable tombstone delivery. Trash/delete used to push a fresh packet
/// directly, outside any persisted authority: a crash after the wire write
/// left an unknown target effect and the next run invented a NEW packet_id
/// for the same deletion. This path applies the same frozen-authority
/// discipline as upserts (unit lease, persisted packet bytes, submit marker
/// BEFORE the wire, receipt-first recovery). Source prepare/confirm is
/// upsert-only on the PHP side, so tombstones skip it by contract.
async fn ship_tombstone_packet(
    shipper: &Shipper,
    pair: &SyncPair,
    log_file: &str,
    packet: &SyncPacket,
    client_uuid: &str,
    target_base: &str,
    target_secret: &[u8],
    inflight_db: Option<&Arc<tokio::sync::Mutex<rusqlite::Connection>>>,
) -> Result<(), String> {
    let uuid = packet.entity.guid.clone();
    if packet.action != "trash" && packet.action != "delete" {
        return Err(format!("非 tombstone 动作进入删除通道 ({uuid})；未执行"));
    }
    let db = inflight_db.ok_or_else(|| "同步副作用缺少持久化权限；未执行".to_string())?;
    let lease = {
        let conn = db.lock().await;
        let suffix = format!(
            "relay-{}",
            crate::db::system::private_json_digest(&json!({"pair":pair.id,"uuid":uuid}),)
                .map_err(|error| format!("同步单元权限失败: {error:#}"))?,
        );
        crate::db::unit_lock::UnitLease::acquire(
            &conn,
            db,
            &suffix,
            "RELAY_UNIT_BUSY: original relay unit is active",
        )
        .map_err(|error| format!("同步单元权限失败 ({uuid}): {error:#}"))?
    };
    let scope = paid_snapshot_scope(packet, pair, None)?;

    let inflight = match crate::db::sync_inflight::find_shipping(db, &pair.id, &uuid).await {
        Ok(Some(row))
            if (row.ctx.action.is_empty() || row.ctx.action == packet.action)
                && row.ctx.translation_scope == scope
                && row.ctx.translation.is_none() =>
        {
            Some(row)
        }
        Ok(Some(_)) => {
            return Err(format!(
                "同步快照范围已变更 ({uuid})；原始副作用未确认，保留证据，未执行新操作"
            ))
        }
        Ok(None) => None,
        Err(error) => return Err(format!("读取同步快照失败 ({uuid}): {error:#}")),
    };
    if let Some(row) = inflight.as_ref() {
        let _ = log_event(
            log_file,
            "info",
            "sync_engine.inflight_resumed",
            json!({
                "pair_id": pair.id,
                "canonical_uuid": uuid,
                "attempts": row.attempts,
                "tombstone": true,
                "reuses_relayed_packet": !row.relayed_json.is_empty(),
            }),
        );
    }
    crate::db::sync_inflight::begin_shipping_scoped(db, &pair.id, &uuid, &scope)
        .await
        .map_err(|error| format!("建立同步快照失败 ({uuid}): {error:#}"))?;

    let result = ship_tombstone_inner(
        shipper,
        pair,
        packet,
        client_uuid,
        target_base,
        target_secret,
        db,
        inflight.as_ref(),
        &lease,
    )
    .await;

    lease
        .assert_native_owner()
        .map_err(|error| format!("同步执行权已变更；原快照保留: {error:#}"))?;
    match &result {
        Ok(()) => {
            crate::db::sync_inflight::close_shipping(db, &pair.id, &uuid)
                .await
                .map_err(|error| format!("同步已接收但关闭快照失败 ({uuid}): {error:#}"))?;
        }
        Err(msg) => {
            let snippet: String = msg.chars().take(300).collect();
            crate::db::sync_inflight::note_error(db, &pair.id, &uuid, &snippet)
                .await
                .map_err(|error| format!("{msg}; 保存快照错误失败 ({uuid}): {error:#}"))?;
        }
    }
    result
}

async fn ship_tombstone_inner(
    shipper: &Shipper,
    pair: &SyncPair,
    packet: &SyncPacket,
    client_uuid: &str,
    target_base: &str,
    target_secret: &[u8],
    db: &Arc<tokio::sync::Mutex<rusqlite::Connection>>,
    inflight: Option<&crate::db::sync_inflight::InflightRow>,
    lease: &crate::db::unit_lock::UnitLease,
) -> Result<(), String> {
    let uuid = packet.entity.guid.clone();
    // Freeze the tombstone bytes BEFORE the first wire write; a resumed run
    // replays the stored packet (same packet_id) so the target's dedup can
    // recognize the original effect.
    let relayed: SyncPacket =
        if let Some(row) = inflight.filter(|row| !row.relayed_json.is_empty()) {
            serde_json::from_str(&row.relayed_json)
                .map_err(|e| format!("在途包反序列化失败 ({uuid}): {e:#}"))?
        } else {
            let relayed_json = serde_json::to_string(packet)
                .map_err(|e| format!("在途包序列化失败 ({uuid}): {e:#}"))?;
            crate::db::sync_inflight::store_relayed_packet(
                db,
                &pair.id,
                &uuid,
                &packet.action,
                &relayed_json,
            )
            .await
            .map_err(|error| format!("保存目标投递快照失败 ({uuid}): {error:#}"))?;
            packet.clone()
        };
    if relayed.action != packet.action || relayed.entity.guid != uuid {
        return Err("冻结 tombstone 范围已变更；保留证据，未执行新操作".into());
    }

    let result = match inflight.and_then(|row| row.ctx.target_receipt.clone()) {
        Some(receipt) => {
            if receipt.packet_id != relayed.packet_id {
                return Err("原目标回执不属于冻结同步包；保留原快照".into());
            }
            shipper.validate_saved_result(target_base, &relayed, &receipt)
                .map_err(|error| format!("原目标回执范围已变更；保留冻结同步包: {error:#}"))?;
            receipt
        }
        None => {
            lease
                .assert_native_owner()
                .map_err(|error| format!("同步执行权已变更: {error:#}"))?;
            let was_submitted = inflight.is_some_and(|row| {
                !row.relayed_json.is_empty()
                    && row.ctx.delivery_phase.as_deref() != Some("prepared")
            });
            let result = if was_submitted {
                // Unknown original effect: ask the target for the original
                // receipt first, never blindly re-push a new packet.
                shipper
                    .resume_submitted_packet(&pair.id, target_base, client_uuid, target_secret, &relayed)
                    .await
            } else {
                crate::db::sync_inflight::mark_submitted(db, &pair.id, &uuid)
                    .await
                    .map_err(|error| format!("保存同步投递意图失败；未推送: {error:#}"))?;
                lease
                    .assert_native_owner()
                    .map_err(|error| format!("同步执行权已变更: {error:#}"))?;
                shipper
                    .push_packet(&pair.id, target_base, client_uuid, target_secret, &relayed)
                    .await
            }
            .map_err(|e| format!("目标站原投递未确认 ({uuid}): {e:#}"))?;
            if result.success {
                crate::db::sync_inflight::store_target_receipt(db, &pair.id, &uuid, &result)
                    .await
                    .map_err(|error| format!("保存原目标回执失败；保留冻结同步包: {error:#}"))?;
            }
            result
        }
    };

    if !result.success {
        return Err(format!(
            "目标站拒绝包 ({uuid}, {}): {}",
            result.status,
            result.error.unwrap_or_default()
        ));
    }
    Ok(())
}

async fn ship_one_packet_inner(
    shipper: &Shipper,
    pair: &SyncPair,
    log_file: &str,
    packet: &SyncPacket,
    translator: Option<&TranslatorHandle>,
    client_uuid: &str,
    target_base: &str,
    target_secret: &[u8],
    report: &mut SyncRunReport,
    inflight_db: Option<&Arc<tokio::sync::Mutex<rusqlite::Connection>>>,
    inflight: Option<&crate::db::sync_inflight::InflightRow>,
    lease: &crate::db::unit_lock::UnitLease,
) -> Result<(), String> {
    let uuid = packet.entity.guid.clone();

    // Each successful field is durable before the next vendor call. A
    // partial snapshot reuses only completed fields, not the whole cascade.
    let mut translation: Option<RelayTranslation> =
        inflight.and_then(|row| row.ctx.translation.clone());
    if let Some(t) = translator {
        let db = inflight_db.ok_or_else(|| "收费翻译缺少持久化权限".to_string())?;
        let scope = paid_snapshot_scope(packet, pair, Some(t))?;
        let source_id = i64::try_from(packet.entity.source_id)
            .map_err(|_| "同步对象 ID 超出安全范围；未执行收费操作".to_string())?;
        let env = |field: &str| crate::db::async_jobs::AsyncJobEnv {
            db: db.clone(),
            domain: pair.source_domain.clone(),
            relation_id: 0,
            object_type: format!("wpmmcc-relay:{}", packet.entity.object_type),
            object_id: source_id,
            field_name: format!("{field}@relay-{}", pair.id),
            chunk_index: 0,
            lane: "text",
            source_snapshot: Some(json!({"pair":pair.id,"uuid":uuid,"scope":scope})),
            resume_binding: None,
        };
        let field = |name: &str| -> String {
            packet
                .entity
                .core_fields
                .get(name)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let title = field("post_title");
        let content = field("post_content");
        let excerpt = field("post_excerpt");
        let mut relay = translation.take().unwrap_or_default();
        if !title.trim().is_empty() && relay.title.is_none() {
            relay.title = Some(
                translate_text_via_component_with_env(
                    &t.vendor_client,
                    &t.runtime,
                    &title,
                    &pair.source_lang,
                    &pair.target_lang,
                    Some(env("post_title")),
                )
                .await
                .map_err(|e| format!("翻译标题失败 ({uuid}): {e:#}"))?,
            );
            if let Some(db) = inflight_db {
                crate::db::sync_inflight::store_translation(db, &pair.id, &uuid, &relay)
                    .await
                    .map_err(|error| format!("保存标题翻译快照失败 ({uuid}): {error:#}"))?;
            }
        }
        if !content.trim().is_empty() && relay.content.is_none() {
            relay.content = Some(
                translate_text_via_component_with_env(
                    &t.vendor_client,
                    &t.runtime,
                    &content,
                    &pair.source_lang,
                    &pair.target_lang,
                    Some(env("post_content")),
                )
                .await
                .map_err(|e| format!("翻译正文失败 ({uuid}): {e:#}"))?,
            );
            if let Some(db) = inflight_db {
                crate::db::sync_inflight::store_translation(db, &pair.id, &uuid, &relay)
                    .await
                    .map_err(|error| format!("保存正文翻译快照失败 ({uuid}): {error:#}"))?;
            }
        }
        if !excerpt.trim().is_empty() && relay.excerpt.is_none() {
            relay.excerpt = Some(
                translate_text_via_component_with_env(
                    &t.vendor_client,
                    &t.runtime,
                    &excerpt,
                    &pair.source_lang,
                    &pair.target_lang,
                    Some(env("post_excerpt")),
                )
                .await
                .map_err(|e| format!("翻译摘要失败 ({uuid}): {e:#}"))?,
            );
            if let Some(db) = inflight_db {
                crate::db::sync_inflight::store_translation(db, &pair.id, &uuid, &relay)
                    .await
                    .map_err(|error| format!("保存摘要翻译快照失败 ({uuid}): {error:#}"))?;
            }
        }
        translation = Some(relay);
    }

    // Media transfer: pull each manifest asset to the target library and
    // collect the URL map for the relay rewrite. 批 R: a resumed run skips
    // assets whose transfer is already recorded (per-asset paid side
    // effect — a crash mid-manifest only re-transfers the REMAINING assets).
    let mut media_url_map = inflight
        .map(|row| row.ctx.media_url_map.clone())
        .unwrap_or_default();
    for entry in &packet.multimodal_manifest {
        if let Some(remote_url) = entry.get("remote_url").and_then(|v| v.as_str()) {
            if remote_url.trim().is_empty() {
                continue;
            }
            if media_url_map.contains_key(remote_url) {
                continue;
            }
            match shipper
                .transfer_media_for_operation(
                    &crate::db::system::private_json_digest(&json!({
                        "pair":pair.id, "uuid":uuid, "scope":paid_snapshot_scope(packet,pair,translator)?,
                        "remote":remote_url,
                    })).map_err(|error| format!("媒体操作身份失败: {error:#}"))?,
                    &pair.id,
                    remote_url,
                    target_base,
                    client_uuid,
                    target_secret,
                )
                .await
            {
                Ok(result) if !result.target_url.is_empty() => {
                    report.media_transferred += 1;
                    // 批 R: persist per-asset BEFORE the map insert moves it.
                    if let Some(db) = inflight_db {
                        crate::db::sync_inflight::store_media_entry(
                            db,
                            &pair.id,
                            &uuid,
                            remote_url,
                            &result.target_url,
                        )
                        .await
                        .map_err(|error| format!("保存媒体快照失败 ({uuid}): {error:#}"))?;
                    }
                    media_url_map.insert(remote_url.to_string(), result.target_url);
                }
                Ok(_) => return Err("媒体转存完成但目标站未返回附件地址；原资产保留".into()),
                Err(err) => return Err(format!("媒体转存失败；原资产保留: {err:#}")),
            }
        }
    }

    // Relay transform + push (or park for human review). 批 R: a resumed
    // run replays the STORED relayed packet — same packet_id, and the
    // target's fingerprint ack makes the re-push idempotent; a fresh
    // transform is persisted BEFORE the push so a replay is byte-identical.
    let relayed = if let Some(row) = inflight.filter(|row| !row.relayed_json.is_empty()) {
        serde_json::from_str(&row.relayed_json)
            .map_err(|e| format!("在途包反序列化失败 ({uuid}): {e:#}"))?
    } else {
        let relayed = relay_packet_for_target(packet, pair, translation.clone(), &media_url_map);
        if let Some(db) = inflight_db {
            let relayed_json = serde_json::to_string(&relayed)
                .map_err(|e| format!("在途包序列化失败 ({uuid}): {e:#}"))?;
            crate::db::sync_inflight::store_relayed_packet(
                db,
                &pair.id,
                &uuid,
                &packet.action,
                &relayed_json,
            )
            .await
            .map_err(|error| format!("保存目标投递快照失败 ({uuid}): {error:#}"))?;
        }
        relayed
    };

    let field = |name: &str| -> String {
        packet
            .entity
            .core_fields
            .get(name)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let source_title = field("post_title");
    let source_content = field("post_content");
    let source_excerpt = field("post_excerpt");
    let proposed_title = translation
        .as_ref()
        .and_then(|t| t.title.clone())
        .unwrap_or_else(|| source_title.clone());
    let proposed_content = translation
        .as_ref()
        .and_then(|t| t.content.clone())
        .unwrap_or_else(|| source_content.clone());
    let proposed_excerpt = translation
        .as_ref()
        .and_then(|t| t.excerpt.clone())
        .unwrap_or_else(|| source_excerpt.clone());

    if parks_in_review_inbox(pair) {
        let fingerprint = packet.source_fingerprint.clone();
        let clock = packet
            .origin_context
            .vector_clock
            .get(&packet.origin_context.origin_site_uuid)
            .cloned()
            .unwrap_or(1);
        let now = unix_ts();
        let item = SyncReviewItem {
            id: format!("sr_{}", uuid::Uuid::new_v4().simple()),
            pair_id: pair.id.clone(),
            canonical_uuid: uuid.clone(),
            status: SyncReviewStatus::PendingReview,
            source_title,
            source_content,
            source_excerpt,
            proposed_title,
            proposed_content,
            proposed_excerpt,
            relayed_packet: relayed,
            source_fingerprint: fingerprint,
            vector_clock: clock,
            post_type: packet.entity.subtype.clone(),
            post_status: packet.entity.status.clone(),
            error_message: None,
            delivery: None,
            created_at: now,
            updated_at: now,
        };
        let path = sync_review_file();
        update_sync_review(&path, |doc| {
            upsert_pending_item(doc, item);
            Ok(())
        })
        .map_err(|e| format!("保存同步待审失败: {e:#}"))?;
        // FO-2 (Wave-2): parking an item in the sync-review inbox used to
        // be SILENT — a reviewer could not tell from the log that a new
        // item was waiting (sim-11 window evidence: zero events between
        // pair_started and the inbox contents). Emit the park event with
        // the pair + entity keys.
        let _ = log_event(
            log_file,
            "info",
            "sync_review.item_parked",
            json!({
                "pair_id": pair.id,
                "canonical_uuid": uuid,
                "post_type": packet.entity.subtype,
            }),
        );
        report.pending_review_count += 1;
        return Ok(());
    }

    let result = match inflight.and_then(|row| row.ctx.target_receipt.clone()) {
        Some(receipt) => {
            if receipt.packet_id != relayed.packet_id {
                return Err("原目标回执不属于冻结同步包；保留原快照".into());
            }
            receipt
        }
        None => {
            lease.assert_native_owner().map_err(|error| format!("同步执行权已变更: {error:#}"))?;
            let was_submitted = inflight.is_some_and(|row| !row.relayed_json.is_empty()
                && row.ctx.delivery_phase.as_deref() != Some("prepared"));
            let result = if was_submitted {
                shipper.resume_submitted_packet(&pair.id, target_base, client_uuid, target_secret, &relayed).await
            } else {
                shipper.prepare_configured_source(client_uuid, target_base, &relayed).await
                    .map_err(|error| format!("冻结源站同步权威失败；未推送: {error:#}"))?;
                let db = inflight_db.ok_or_else(|| "同步投递缺少持久意图".to_string())?;
                crate::db::sync_inflight::mark_submitted(db, &pair.id, &uuid).await
                    .map_err(|error| format!("保存同步投递意图失败；未推送: {error:#}"))?;
                lease.assert_native_owner().map_err(|error| format!("同步执行权已变更: {error:#}"))?;
                shipper.push_packet(&pair.id, target_base, client_uuid, target_secret, &relayed).await
            }.map_err(|e| format!("目标站原投递未确认 ({uuid}): {e:#}"))?;
            if result.success {
                if let Some(db) = inflight_db {
                    crate::db::sync_inflight::store_target_receipt(db, &pair.id, &uuid, &result)
                        .await.map_err(|error| format!("保存原目标回执失败；保留冻结同步包: {error:#}"))?;
                }
            }
            result
        }
    };

    if !result.success {
        return Err(format!(
            "目标站拒绝包 ({uuid}, {}): {}",
            result.status,
            result.error.unwrap_or_default()
        ));
    }
    lease.assert_native_owner().map_err(|error| format!("同步执行权已变更: {error:#}"))?;
    shipper.confirm_configured_source(client_uuid, &relayed, &result)
        .await.map_err(|error| format!("源站互惠映射未确认；原目标回执保留: {error:#}"))?;

    report.synced_count += 1;

    // State update: remember the digest-reported fingerprint + clock so the
    // next run skips this entity unless the source reports a change.
    let fingerprint = packet.source_fingerprint.clone();
    let clock = packet
        .origin_context
        .vector_clock
        .get(&packet.origin_context.origin_site_uuid)
        .cloned()
        .unwrap_or(1);
    let subtype = packet.entity.subtype.clone();
    let status = packet.entity.status.clone();
    let target_post_id = result.target_id;
    let now = unix_ts();
    update_pair_state(&sync_state_file(), &pair.id, |s| {
        s.known.insert(
            uuid.clone(),
            KnownEntity {
                canonical_uuid: uuid.clone(),
                source_fingerprint: fingerprint,
                vector_clock: clock,
                post_type: subtype,
                post_status: status,
                target_post_id,
                last_shipped_at: now,
                review_rejected: false,
            },
        );
    })
    .map_err(|e| format!("目标已接收但保存同步状态失败 ({uuid}): {e:#}"))?;

    Ok(())
}

fn record_item_error(report: &mut SyncRunReport, message: &str) {
    report.error_count += 1;
    if report.last_error.is_none() {
        report.last_error = Some(message.to_string());
    }
}

/// §68 (FL-11 amendment): the safe cursor may only advance over an
/// UNBROKEN run of converged items from the digest page head. The page
/// arrives ordered by source-local id (the plugin digest contract:
/// `WHERE ID > cursor ORDER BY ID ASC`), so stopping at the first
/// unconverged item guarantees every later digest (ID > cursor) still
/// re-reports it — while a `max()` over individually converged ids let a
/// high-id fingerprint-stable skip leap the cursor over a lower-id ship
/// failure, silently losing that item forever.
/// Advance `safe_cursor` over the unbroken converged prefix of one digest
/// page. Returns `true` when EVERY item on the page converged (the walk
/// never stopped early) — callers use that to freeze the cursor for the
/// rest of the run once any page retains a post-ship failure (CLI-01).
fn advance_safe_cursor_contiguous(
    safe_cursor: &mut u64,
    items: &[DigestItem],
    converged: &HashMap<u64, bool>,
) -> bool {
    for item in items {
        if converged.get(&item.local_id).copied().unwrap_or(false) {
            *safe_cursor = (*safe_cursor).max(item.local_id);
        } else {
            return false;
        }
    }
    true
}

fn finish_with_error(
    pairs_path: &str,
    pair: &SyncPair,
    report: &mut SyncRunReport,
    message: &str,
    log_file: &str,
) -> anyhow::Result<SyncRunReport> {
    report.error_count += 1;
    report.last_error = Some(message.to_string());
    let now = unix_ts();
    let error = report.last_error.clone();
    update_sync_pairs(pairs_path, |doc| {
        if let Some(p) = find_pair_in_doc_mut(doc, &pair.id) {
            p.last_sync_at = Some(now);
            p.last_error = error;
            p.updated_at = now;
            doc.updated_at = now;
        }
        Ok(())
    })?;
    let _ = log_event(
        log_file,
        "error",
        "sync_engine.pair_failed",
        json!({ "pair_id": pair.id, "trace_id": report.trace_id, "error": message }),
    );
    Ok(report.clone())
}

/// Read-only state snapshot for one pair (avoids holding the doc borrow).
fn pair_state_snapshot(state_path: &str, pair_id: &str) -> anyhow::Result<PairSyncState> {
    Ok(load_sync_state(state_path)?
        .pairs
        .get(pair_id)
        .cloned()
        .unwrap_or_default())
}

/// Delete a pair and its incremental state.
pub fn delete_pair_everywhere(pair_id: &str) -> anyhow::Result<bool> {
    let pairs_path = sync_pairs_file();
    let state_path = sync_state_file();
    update_sync_pairs(&pairs_path, |doc| {
        if find_pair_in_doc(doc, pair_id).is_none() {
            return Ok(false);
        }
        // Refuse damaged state before changing either document. This is a
        // retryable two-file operation, not a crash-atomic cross-file commit.
        remove_pair_state(&state_path, pair_id)?;
        Ok(delete_pair_in_doc(doc, pair_id))
    })
}
