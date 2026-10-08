//! Engine integration tests against a loopback mock WPMMCC site that
//! implements the plugin REST contract faithfully — including full HMAC
//! verification of every authenticated call (mirroring
//! `class-wpmmcc-rest-middleware.php`). A passing test proves the client's
//! signatures, packet shapes, and flow ordering are wire-correct.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use base64::Engine;

use serde_json::{json, Value};

use crate::sync_engine::credentials::{
    derive_peer_shared_secret, PeerCredential, PeerCredentialsDoc,
};
use crate::sync_engine::hmac::{compute_hmac_signature, sha256_hex};
use crate::sync_engine::types::{
    ConflictStrategy, SyncDirection, SyncFrequency, SyncMode, SyncPair, SyncPairStatus,
};

use super::super::review::{count_pending_for_pair, load_sync_review, SyncReviewStatus};
use super::super::state::load_sync_state;
use super::super::storage::{
    find_pair_in_doc, load_sync_pairs, save_sync_pairs, upsert_pair_in_doc,
};

const CLIENT_UUID: &str = "11111111-1111-1111-1111-111111111111";
const SOURCE_UUID: &str = "22222222-2222-2222-2222-222222222222";
const TARGET_UUID: &str = "33333333-3333-3333-3333-333333333333";

type Shared<T> = Arc<Mutex<T>>;

/// Records one mock site keeps for assertions.
#[derive(Default)]
struct SiteRecords {
    received_packets: Vec<Value>,
    received_chunks: usize,
    /// Packets seen by /sync/push but rejected by an injected fault.
    rejected_packets: usize,
    /// Remaining push rejections to serve (fault injection lever).
    push_faults_remaining: usize,
    /// HTTP status served for injected push faults.
    push_fault_status: u16,
    media_faults: Vec<String>,
    receipts: HashMap<String, Value>,
    prepared: HashMap<String, Value>,
    media: HashMap<String, MockMedia>,
}

#[derive(Default)]
struct MockMedia {
    scope: Value,
    parts: std::collections::BTreeMap<u64, Vec<u8>>,
    receipt: Option<Value>,
}

struct ResponseAuth {
    nonce: String, timestamp: String, sender: String,
    method: String, uri: String, secret: Vec<u8>,
}

thread_local! {
    static RESPONSE_AUTH: std::cell::RefCell<Option<ResponseAuth>> = const { std::cell::RefCell::new(None) };
}

/// One mock WPMMCC site: serves digest/pull/push/media-chunk/reconcile
/// with HMAC enforcement, plus public media GETs, and records everything.
pub(crate) struct MockSite {
    pub(crate) base_url: String,
    records: Shared<SiteRecords>,
    posts: Shared<Vec<Value>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl MockSite {
    /// Spin the site up on an ephemeral loopback port. The posts builder
    /// receives the live base URL — media URLs inside packets must point at
    /// this server so the engine's media download hits a real socket.
    pub(crate) fn spawn(site_uuid: &'static str, build_posts: impl FnOnce(&str) -> Vec<Value>) -> Self {
        // Deterministic shared secret, mirroring the handshake derivation
        // with pairing_secret = "secret-<site-uuid>".
        let pairing_secret = format!("secret-{site_uuid}");
        let shared_secret =
            derive_peer_shared_secret(&pairing_secret, CLIENT_UUID, site_uuid).to_vec();

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock site");
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let base_url = format!("http://127.0.0.1:{port}");

        let records: Shared<SiteRecords> = Arc::new(Mutex::new(SiteRecords::default()));
        let thread_records = Arc::clone(&records);
        let posts: Shared<Vec<Value>> = Arc::new(Mutex::new(build_posts(&base_url)));
        let thread_posts = Arc::clone(&posts);
        let uuid = site_uuid.to_string();

        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = std::thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    },
                    Err(error) => panic!("mock site accept: {error}"),
                };
                stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                let records = Arc::clone(&thread_records);
                let posts = Arc::clone(&thread_posts);
                let secret = shared_secret.clone();
                let uuid = uuid.clone();
                std::thread::spawn(move || {
                    handle_conn(&mut stream, &secret, &uuid, &posts, &records);
                });
            }
        });

        MockSite {
            base_url,
            records,
            posts,
            stop, thread: Some(thread),
        }
    }

    pub(crate) fn received_packets(&self) -> Vec<Value> {
        self.records.lock().unwrap().received_packets.clone()
    }

    fn received_chunks(&self) -> usize {
        self.records.lock().unwrap().received_chunks
    }

    fn rejected_packets(&self) -> usize {
        self.records.lock().unwrap().rejected_packets
    }

    /// SIM-14 lever: reject the next `times` pushes with `status`.
    pub(crate) fn inject_push_faults(&self, times: usize, status: u16) {
        let mut rec = self.records.lock().unwrap();
        // status is stored per-injection; keep it simple: only one active
        // status at a time (sufficient for the recovery journeys).
        rec.push_faults_remaining = times;
        rec.push_fault_status = status;
    }

    /// SIM-14 lever: mutate one post's title + fingerprint (the real
    /// plugin re-hashes content on save; the local id stays put).
    fn mutate_post(&self, guid: &str, title: &str) {
        let mut posts = self.posts.lock().unwrap();
        let post = posts
            .iter_mut()
            .find(|p| p["entity"]["guid"].as_str() == Some(guid))
            .expect("post exists");
        post["entity"]["core_fields"]["post_title"] = json!(title);
        post["source_fingerprint"] = json!(format!("fp-{guid}-r1"));
    }

    /// SIM-14 lever: trash one post (status → trash + fingerprint bump).
    fn trash_post(&self, guid: &str) {
        let mut posts = self.posts.lock().unwrap();
        let post = posts
            .iter_mut()
            .find(|p| p["entity"]["guid"].as_str() == Some(guid))
            .expect("post exists");
        post["entity"]["status"] = json!("trash");
        post["source_fingerprint"] = json!(format!("fp-{guid}-trash"));
    }

    /// SIM-14 lever: hard-delete one post (vanishes from digest + pull +
    /// the reconcile-known set → the client's rotation reports 'missing').
    fn delete_post(&self, guid: &str) {
        let mut posts = self.posts.lock().unwrap();
        let len_before = posts.len();
        posts.retain(|p| p["entity"]["guid"].as_str() != Some(guid));
        assert_eq!(posts.len(), len_before - 1, "post existed");
    }
}

impl Drop for MockSite {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() { thread.join().unwrap(); }
    }
}

/// Minimal HTTP/1.1 handling: one request per connection.
fn handle_conn(
    stream: &mut std::net::TcpStream,
    secret: &[u8],
    site_uuid: &str,
    posts: &Shared<Vec<Value>>,
    records: &Shared<SiteRecords>,
) {
    let Some((head, body)) = read_request(stream) else {
        return;
    };

    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let raw_path = parts.next().unwrap_or("/").to_string();

    let mut headers: HashMap<String, String> = HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_lowercase(), v.trim().to_string());
        }
    }

    // Public media GET (the manifest's remote_url) — no HMAC.
    if method == "GET" {
        if records
            .lock()
            .unwrap()
            .media_faults
            .iter()
            .any(|uuid| raw_path.contains(uuid))
        {
            write_json_response(
                stream,
                503,
                &json!({"message": "fixture media unavailable"}),
            );
            return;
        }
        write_bytes_response(stream, "image/png", &[1u8, 2, 3, 4]);
        return;
    }

    // ---- HMAC verification (mirror of the plugin middleware).
    let sig = headers
        .get("x-wpmmcc-signature")
        .cloned()
        .unwrap_or_default();
    let ts = headers
        .get("x-wpmmcc-timestamp")
        .cloned()
        .unwrap_or_default();
    let nonce = headers.get("x-wpmmcc-nonce").cloned().unwrap_or_default();
    let sender = headers
        .get("x-wpmmcc-site-uuid")
        .cloned()
        .unwrap_or_default();
    let canonical_uri = raw_path
        .strip_prefix("/wp-json")
        .unwrap_or(&raw_path)
        .to_string();
    let body_hash = sha256_hex(&body);
    let string_to_sign = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        method.to_uppercase(),
        canonical_uri,
        ts,
        nonce,
        sender,
        body_hash
    );
    let expected = compute_hmac_signature(&string_to_sign, secret).unwrap_or_default();

    let authenticated = !sig.is_empty()
        && !ts.is_empty()
        && !nonce.is_empty()
        && sender == CLIENT_UUID
        && sig == expected;

    if !authenticated {
        write_json_response(
            stream,
            401,
            &json!({
                "code": "wpmmcc_signature_mismatch",
                "message": "HMAC verification failed",
                "data": { "status": 401 }
            }),
        );
        return;
    }

    let path = canonical_uri.as_str();
    RESPONSE_AUTH.with(|authority| {
        *authority.borrow_mut() = Some(ResponseAuth {
            nonce, timestamp: ts, sender, method: method.clone(),
            uri: canonical_uri.clone(), secret: secret.to_vec(),
        });
    });
    let base = format!("http://{}", stream.local_addr().unwrap());
    match (method.as_str(), path) {
        ("POST", "/wpmmcc/v1/sync/digest") => {
            // REAL plugin contract (class-wpmmcc-reconciler.php): pure
            // keyset cursor on local ids — only items with ID >
            // last_seen_id are (re)reported. Updates to already-scanned
            // posts therefore never reappear here; the reconcile rotation
            // is the update-propagation path.
            let req: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
            let last_seen_id = req["last_seen_id"].as_u64().unwrap_or(0);
            let limit = req["limit"].as_u64().unwrap_or(u64::MAX) as usize;
            let items: Vec<Value> = posts
                .lock()
                .unwrap()
                .iter()
                .filter(|p| p["entity"]["source_id"].as_u64().unwrap_or(0) > last_seen_id)
                .take(limit)
                .map(|p| {
                    json!({
                        "canonical_uuid": p["entity"]["guid"],
                        "source_fingerprint": p["source_fingerprint"],
                        "vector_clock": p["origin_context"]["vector_clock"][site_uuid],
                        "local_id": p["entity"]["source_id"],
                        "post_status": p["entity"]["status"],
                        "post_type": p["entity"]["subtype"],
                        "last_modified": "2026-01-01 00:00:00"
                    })
                })
                .collect();
            write_json_response(
                stream,
                200,
                &json!({ "success": true, "data": { "items": items, "server_time": 1 } }),
            );
        }
        ("POST", "/wpmmcc/v1/sync/pull") => {
            let req: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
            let wanted: Vec<String> = req["canonical_uuids"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let packets: Vec<Value> = posts
                .lock()
                .unwrap()
                .iter()
                .filter(|p| {
                    wanted.contains(&p["entity"]["guid"].as_str().unwrap_or("").to_string())
                })
                .cloned()
                .collect();
            write_json_response(
                stream,
                200,
                &json!({ "success": true, "data": { "packets": packets } }),
            );
        }
        ("POST", "/wpmmcc/v1/sync/prepare") => {
            let request: Value = serde_json::from_slice(&body).unwrap();
            let packet = &request["packet"];
            let id = packet["packet_id"].as_str().unwrap();
            let mut recorded = records.lock().unwrap();
            if let Some(prior) = recorded.prepared.get(id) {
                assert_eq!(prior, &request, "source authority is immutable");
            }
            recorded.prepared.insert(id.into(), request.clone());
            write_json_response(stream, 200, &json!({"success":true,"data":{
                "prepared":true,"packet_id":id,"canonical_uuid":packet["entity"]["guid"],
                "target_site_uuid":request["target_site_uuid"],
            }}));
        }
        ("POST", "/wpmmcc/v1/sync/confirm") => {
            let request: Value = serde_json::from_slice(&body).unwrap();
            let packet = &request["packet"];
            let id = packet["packet_id"].as_str().unwrap();
            let recorded = records.lock().unwrap();
            let prepared = recorded.prepared.get(id).expect("source prepare precedes effect confirmation");
            assert_eq!(prepared["packet"], *packet);
            assert_eq!(request["receipt"]["target_site_uuid"], prepared["target_site_uuid"]);
            assert_eq!(request["receipt"]["packet_id"], id);
            assert_eq!(request["receipt"]["committed"], true);
            write_json_response(stream, 200, &json!({"success":true,"data":{
                "confirmed":true,"packet_id":id,"canonical_uuid":packet["entity"]["guid"],
                "target_site_uuid":prepared["target_site_uuid"],
            }}));
        }
        ("POST", "/wpmmcc/v1/sync/receipt") => {
            let packet: Value = serde_json::from_slice(&body).unwrap();
            let recorded = records.lock().unwrap();
            let receipt = recorded.receipts.get(packet["packet_id"].as_str().unwrap())
                .cloned().unwrap_or_else(|| json!({
                    "committed":false, "phase":"absent", "packet_id":packet["packet_id"],
                    "canonical_uuid":packet["entity"]["guid"],
                    "origin_site_uuid":packet["origin_context"]["origin_site_uuid"],
                    "target_site_uuid":site_uuid, "target_lang":packet["target_lang"],
                    "fingerprint_ack":packet["source_fingerprint"],
                }));
            write_json_response(stream, 200, &json!({"success":true,"data":receipt}));
        }
        ("POST", "/wpmmcc/v1/sync/push") => {
            let packet: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
            let fingerprint = packet["source_fingerprint"]
                .as_str()
                .unwrap_or("")
                .to_string();
            // SIM-14 fault injection: transient rejections (the self-heal
            // journey). Rejected packets are counted separately — they are
            // NOT accepted deliveries.
            let fault = {
                let mut rec = records.lock().unwrap();
                if rec.push_faults_remaining > 0 {
                    rec.push_faults_remaining -= 1;
                    rec.rejected_packets += 1;
                    Some(rec.push_fault_status)
                } else {
                    None
                }
            };
            if let Some(status) = fault {
                write_json_response(
                    stream,
                    status,
                    &json!({
                        "success": false,
                        "code": if status == 401 { "wpmmcc_signature_mismatch" } else { "wpmmcc_error" },
                        "message": "injected push fault (sim-14)",
                        "data": { "status": status }
                    }),
                );
                return;
            }
            let receipt = {
                let mut rec = records.lock().unwrap();
                let id = packet["packet_id"].as_str().unwrap();
                if let Some(receipt) = rec.receipts.get(id) {
                    receipt.clone()
                } else {
                    rec.received_packets.push(packet.clone());
                    let target_id = if packet["action"] == "upsert" { rec.received_packets.len() as u64 + 100 } else { 0 };
                    let receipt = json!({
                        "committed":true, "status":"success", "packet_id":packet["packet_id"],
                        "canonical_uuid":packet["entity"]["guid"],
                        "origin_site_uuid":packet["origin_context"]["origin_site_uuid"],
                        "target_site_uuid":site_uuid, "target_lang":packet["target_lang"],
                        "target_id":target_id,
                        "target_url":if target_id > 0 {format!("{base}/?p={target_id}")} else {String::new()},
                        "fingerprint_ack":fingerprint,
                    });
                    rec.receipts.insert(id.into(), receipt.clone());
                    receipt
                }
            };
            write_json_response(
                stream,
                200,
                &json!({
                    "success": true,
                    "data": receipt
                }),
            );
        }
        ("POST", "/wpmmcc/v1/sync/media-chunk") => {
            let request: Value = serde_json::from_slice(&body).unwrap();
            let mut scope = request.clone();
            for key in ["chunk_index", "bytes"] { scope.as_object_mut().unwrap().remove(key); }
            let bytes = base64::engine::general_purpose::STANDARD.decode(request["bytes"].as_str().unwrap()).unwrap();
            let index = request["chunk_index"].as_u64().unwrap();
            let total = scope["total"].as_u64().unwrap();
            let mut recorded = records.lock().unwrap();
            recorded.received_chunks += 1;
            let chunks = recorded.received_chunks;
            let media = recorded.media.entry(scope["file_uuid"].as_str().unwrap().into()).or_default();
            if !media.scope.is_null() { assert_eq!(media.scope, scope); }
            media.scope = scope;
            if let Some(prior) = media.parts.get(&index) { assert_eq!(prior, &bytes); }
            media.parts.insert(index, bytes);
            let mut ack = json!({
                "chunk_acked":true, "file_uuid":request["file_uuid"], "chunk_index":index,
                "total":total, "completed":false,
            });
            if media.parts.len() == total as usize {
                let joined: Vec<u8> = media.parts.values().flatten().copied().collect();
                assert_eq!(sha256_hex(&joined), request["expected_sha256"].as_str().unwrap());
                assert_eq!(joined.len() as u64, request["expected_size"].as_u64().unwrap());
                let receipt = json!({
                    "completed":true,"assembled_sha256":sha256_hex(&joined),"attachment_id":55,
                    "attachment_url":format!("{base}/wp-content/uploads/mock/{chunks}.png"),"reused":false,
                });
                ack.as_object_mut().unwrap().extend(receipt.as_object().unwrap().clone());
                media.receipt = Some(receipt);
            }
            write_json_response(stream, 200, &json!({"success":true,"data":ack}));
        }
        ("POST", "/wpmmcc/v1/sync/media-receipt") => {
            let scope: Value = serde_json::from_slice(&body).unwrap();
            let recorded = records.lock().unwrap();
            let mut proof = scope.clone();
            if let Some(media) = recorded.media.get(scope["file_uuid"].as_str().unwrap()) {
                assert_eq!(media.scope, scope);
                if let Some(receipt) = &media.receipt {
                    proof.as_object_mut().unwrap().extend(receipt.as_object().unwrap().clone());
                    proof["phase"] = json!("applied");
                } else {
                    proof["phase"] = json!("receiving");
                    proof["completed"] = json!(false);
                    proof["missing_chunks"] = json!((0..scope["total"].as_u64().unwrap())
                        .filter(|index| !media.parts.contains_key(index)).collect::<Vec<_>>());
                }
            } else {
                proof["phase"] = json!("absent");
                proof["completed"] = json!(false);
                proof["missing_chunks"] = json!((0..scope["total"].as_u64().unwrap()).collect::<Vec<_>>());
            }
            write_json_response(stream, 200, &json!({"success":true,"data":proof}));
        }
        ("POST", "/wpmmcc/v1/sync/reconcile-digest") => {
            // REAL verdict fidelity (class-wpmmcc-reconciler.php): the
            // submitted fingerprint is compared against the SOURCE's
            // current one — equal → in_sync, unknown uuid → missing,
            // drifted → source_newer (the update-propagation verdict).
            let req: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
            let mut verdicts = serde_json::Map::new();
            if let Some(fps) = req["fingerprints"].as_array() {
                let current: HashMap<String, String> = posts
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|p| {
                        (
                            p["entity"]["guid"].as_str().unwrap_or("").to_string(),
                            p["source_fingerprint"].as_str().unwrap_or("").to_string(),
                        )
                    })
                    .collect();
                for item in fps {
                    let uuid = item["canonical_uuid"].as_str().unwrap_or("");
                    let remote_fp = item["source_fingerprint"].as_str().unwrap_or("");
                    let verdict = match current.get(uuid) {
                        None => "missing",
                        Some(cur) if cur == remote_fp => "in_sync",
                        Some(_) => "source_newer",
                    };
                    verdicts.insert(uuid.to_string(), json!(verdict));
                }
            }
            write_json_response(
                stream,
                200,
                &json!({ "success": true, "data": { "verdicts": verdicts, "server_time": 1 } }),
            );
        }
        _ => {
            write_json_response(
                stream,
                404,
                &json!({ "code": "rest_no_route", "message": "no route" }),
            );
        }
    }
}

#[path = "tests/paid_snapshots.rs"]
mod paid_snapshots;
#[path = "tests/review_decisions.rs"]
mod review_decisions;

#[tokio::test]
async fn batch41_signed_absence_resends_only_the_same_packet_once() {
    let _harness = crate::web_ui::test_support::WebUiTestHarness::new("", None).await.unwrap();
    let target = MockSite::spawn(TARGET_UUID, |_| vec![]);
    let base = format!("{}/wp-json/wpmmcc/v1", target.base_url);
    let secret = derive_peer_shared_secret(&format!("secret-{TARGET_UUID}"), CLIENT_UUID, TARGET_UUID);
    let mut packet: crate::sync_engine::packet::SyncPacket =
        serde_json::from_value(sample_source_packet("https://source.example", "absent-unit", 1, "Original")).unwrap();
    packet.action = "delete".into();
    packet.target_lang = "zh_CN".into();
    let shipper = super::super::shipper::Shipper::new(reqwest::Client::new()).unwrap()
        .with_source_confirmation("https://source.example/wp-json/wpmmcc/v1".into(), vec![], TARGET_UUID.into());
    let result = shipper.resume_submitted_packet("pair-absent", &base, CLIENT_UUID, &secret, &packet).await.unwrap();
    assert!(result.success);
    assert_eq!(target.received_packets(), vec![serde_json::to_value(&packet).unwrap()]);
    let replay = shipper.resume_submitted_packet("pair-absent", &base, CLIENT_UUID, &secret, &packet).await.unwrap();
    assert_eq!(serde_json::to_value(replay).unwrap(), serde_json::to_value(result).unwrap());
    assert_eq!(target.received_packets().len(), 1, "a committed receipt never authorizes another target effect");
}

#[tokio::test]
async fn batch41_signed_foreign_or_unknown_proof_cannot_reapply() {
    let _harness = crate::web_ui::test_support::WebUiTestHarness::new("", None).await.unwrap();
    let target = MockSite::spawn(TARGET_UUID, |_| vec![]);
    let base = format!("{}/wp-json/wpmmcc/v1", target.base_url);
    let secret = derive_peer_shared_secret(&format!("secret-{TARGET_UUID}"), CLIENT_UUID, TARGET_UUID);
    let mut packet: crate::sync_engine::packet::SyncPacket =
        serde_json::from_value(sample_source_packet("https://source.example", "foreign-unit", 1, "Original")).unwrap();
    packet.action = "delete".into();
    packet.target_lang = "zh_CN".into();
    let shipper = super::super::shipper::Shipper::new(reqwest::Client::new()).unwrap()
        .with_source_confirmation("https://source.example/wp-json/wpmmcc/v1".into(), vec![], TARGET_UUID.into());
    for field in ["packet_id", "canonical_uuid", "origin_site_uuid", "target_site_uuid", "target_lang", "fingerprint_ack", "phase"] {
        let mut proof = json!({
            "committed":false, "phase":"absent", "packet_id":packet.packet_id,
            "canonical_uuid":packet.entity.guid, "origin_site_uuid":packet.origin_context.origin_site_uuid,
            "target_site_uuid":TARGET_UUID, "target_lang":packet.target_lang, "fingerprint_ack":packet.source_fingerprint,
        });
        proof[field] = json!("foreign-or-applying");
        target.records.lock().unwrap().receipts.insert(packet.packet_id.clone(), proof);
        let error = shipper.resume_submitted_packet("pair-foreign", &base, CLIENT_UUID, &secret, &packet).await.unwrap_err();
        assert!(error.to_string().contains("proof scope differs") || error.to_string().contains("matching committed receipt"), "{field}: {error:#}");
        assert!(target.received_packets().is_empty(), "{field} must refuse before the target push");
    }
}

fn read_request(stream: &mut std::net::TcpStream) -> Option<(String, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let header_end = loop {
        match stream.read(&mut tmp) {
            Ok(0) => return None,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break pos + 4;
                }
            }
            Err(_) => return None,
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let content_length: usize = head
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            if k.trim().eq_ignore_ascii_case("content-length") {
                v.trim().parse().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);
    while buf.len() < header_end + content_length {
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(_) => return None,
        }
    }
    let body = buf[header_end..(header_end + content_length).min(buf.len())].to_vec();
    Some((head, body))
}

fn write_json_response(stream: &mut std::net::TcpStream, status: u16, body: &Value) {
    let payload = serde_json::to_vec(body).unwrap_or_default();
    let signed = RESPONSE_AUTH.with(|authority| authority.borrow().as_ref().map(|authority| {
        let message = crate::sync_engine::hmac::response_string_to_sign(
            &authority.nonce, &authority.timestamp, &authority.sender, &authority.method,
            &authority.uri, status, &payload,
        );
        format!("X-WPMMCC-Response-Signature: {}\r\n",
            compute_hmac_signature(&message, &authority.secret).unwrap())
    })).unwrap_or_default();
    let reason = match status {
        200 => "OK",
        401 => "Unauthorized",
        404 => "Not Found",
        _ => "Error",
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{}Connection: close\r\n\r\n",
        status,
        reason,
        payload.len(),
        signed
    );
    let _ = stream.write_all(&payload);
    let _ = stream.flush();
}

fn write_bytes_response(stream: &mut std::net::TcpStream, content_type: &str, body: &[u8]) {
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        content_type,
        body.len()
    );
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

/// A source-site packet whose media remote_url points at the live mock
/// source server (so the engine's media download hits a real socket).
fn sample_source_packet(source_base: &str, guid: &str, source_id: u64, title: &str) -> Value {
    json!({
        "schema_version": "wpmmcc-sync-v1.0",
        "packet_id": format!("pkt_{guid}"),
        "origin_context": {
            "origin_site_uuid": SOURCE_UUID,
            "origin_site_url": source_base,
            "origin_blog_id": 1,
            "origin_lang": "en_US",
            "vector_clock": { SOURCE_UUID: 1 },
            "hop_count": 1,
            "dispatch_timestamp": 100
        },
        "action": "upsert",
        "sync_mode": "sync_only",
        "target_lang": "zh_CN",
        "source_fingerprint": format!("fp-{guid}"),
        "entity": {
            "guid": guid,
            "object_type": "post",
            "subtype": "post",
            "source_id": source_id,
            "slug": "hello",
            "status": "publish",
            "author_hint": { "display_name": "Sync Engine" },
            "core_fields": {
                "post_title": title,
                "post_content": format!(
                    "<p>{title} <img src=\"{source_base}/wp-content/uploads/{guid}.png\"/></p>"
                ),
                "post_excerpt": ""
            },
            "taxonomies": {},
            "meta_fields": {},
            "plugin_specific": {}
        },
        "multimodal_manifest": [
            {
                "asset_ref": format!("asset_{guid}"),
                "type": "image",
                "mime_type": "image/png",
                "original_filename": format!("{guid}.png"),
                "remote_url": format!("{source_base}/wp-content/uploads/{guid}.png"),
                "sha256": "x",
                "size_bytes": 4,
                "metadata": {}
            }
        ]
    })
}

fn make_pair(source: &str, target: &str) -> SyncPair {
    SyncPair {
        id: "pair-test-1".to_string(),
        name: "test pair".to_string(),
        source_domain: source.to_string(),
        target_domain: target.to_string(),
        direction: SyncDirection::Unidirectional,
        sync_mode: SyncMode::SyncOnly,
        source_lang: "en_US".to_string(),
        target_lang: "zh_CN".to_string(),
        conflict_strategy: ConflictStrategy::Lww,
        sync_frequency: SyncFrequency::Manual,
        post_types: vec!["post".to_string()],
        status: SyncPairStatus::Active,
        last_sync_at: None,
        last_seen_source_id: None,
        last_sync_count: None,
        last_error: None,
        translate_component_id: None,
        field_actions: Vec::new(),
        review_before_push: false,
        created_at: 1,
        updated_at: 1,
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

fn persist_pair(path: &str, pair: &SyncPair) {
    let mut doc = load_sync_pairs(path).unwrap();
    upsert_pair_in_doc(&mut doc, pair.clone());
    save_sync_pairs(path, &doc).unwrap();
}

pub(crate) fn credentials_doc(source: &MockSite, target: &MockSite) -> PeerCredentialsDoc {
    let source_secret =
        derive_peer_shared_secret(&format!("secret-{SOURCE_UUID}"), CLIENT_UUID, SOURCE_UUID);
    let target_secret =
        derive_peer_shared_secret(&format!("secret-{TARGET_UUID}"), CLIENT_UUID, TARGET_UUID);
    PeerCredentialsDoc {
        schema_version: "wpmmcc-peer-credentials.v1".to_string(),
        client_origin_uuid: CLIENT_UUID.to_string(),
        client_origin_url: "https://wpmmcc-ats-client.local/xx".to_string(),
        client_origin_name: "WPMMCC ATS Client".to_string(),
        peers: HashMap::from([
            (
                source.base_url.clone(),
                PeerCredential {
                    domain: source.base_url.clone(),
                    peer_uuid: SOURCE_UUID.to_string(),
                    peer_name: "Source".to_string(),
                    shared_secret_hex: hex(&source_secret),
                    key_scheme: "hmac_v1".to_string(),
                    negotiated_direction: "push_only".to_string(),
                    install_signature: String::new(),
                    paired_at: 1,
                    paired_as: "source".to_string(),
                },
            ),
            (
                target.base_url.clone(),
                PeerCredential {
                    domain: target.base_url.clone(),
                    peer_uuid: TARGET_UUID.to_string(),
                    peer_name: "Target".to_string(),
                    shared_secret_hex: hex(&target_secret),
                    key_scheme: "hmac_v1".to_string(),
                    negotiated_direction: "pull_only".to_string(),
                    install_signature: String::new(),
                    paired_at: 1,
                    paired_as: "target".to_string(),
                },
            ),
        ]),
        updated_at: 1,
    }
}

#[tokio::test]
async fn engine_run_pulls_ships_media_and_tracks_state() {
    let state_root = std::env::temp_dir().join(format!(
        "sync_engine_test_{}_{}",
        std::process::id(),
        crate::logging::unix_ts()
    ));
    std::fs::create_dir_all(&state_root).unwrap();
    let _owned_key = crate::db::owned_mock_bindings_key();
    let _owned_data = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", state_root.display().to_string());
    let pairs_path = state_root.join("sync-pairs.json");
    let sync_state_path = state_root.join("sync-state.json");
    let creds_path = state_root.join("peer-creds.json");

    let _g1 = crate::db::TestEnvVarGuard::set(
        "WPTSALL_SYNC_PAIRS_FILE",
        pairs_path.to_string_lossy().as_ref(),
    );
    let _g2 = crate::db::TestEnvVarGuard::set(
        "WPTSALL_SYNC_STATE_FILE",
        sync_state_path.to_string_lossy().as_ref(),
    );
    let _g3 = crate::db::TestEnvVarGuard::set(
        "WPTSALL_SYNC_PEER_CREDENTIALS_FILE",
        creds_path.to_string_lossy().as_ref(),
    );

    let source = MockSite::spawn("22222222-2222-2222-2222-222222222222", |base| {
        vec![
            sample_source_packet(base, "uuid-alpha", 1, "Alpha"),
            sample_source_packet(base, "uuid-beta", 2, "Beta"),
        ]
    });
    let target = MockSite::spawn("33333333-3333-3333-3333-333333333333", |_| vec![]);

    let creds = credentials_doc(&source, &target);
    let pair = make_pair(&source.base_url, &target.base_url);
    persist_pair(&pairs_path.to_string_lossy(), &pair);

    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let log_file = state_root.join("test.log").to_string_lossy().to_string();

    let report = super::sync_pair_run(&client, &pair.id, &creds, None, &log_file)
        .await
        .expect("run should succeed");

    // Both posts shipped to the target.
    assert_eq!(report.synced_count, 2, "report: {report:?}");
    assert_eq!(report.pulled_count, 2);
    assert_eq!(report.media_transferred, 2, "manifest assets transferred");
    assert_eq!(report.error_count, 0, "no errors: {:?}", report.last_error);

    // The target received two packets, HMAC-verified, with rewritten URLs.
    let received = target.received_packets();
    assert_eq!(received.len(), 2);
    let alpha = received
        .iter()
        .find(|p| p["entity"]["guid"] == "uuid-alpha")
        .expect("alpha packet received");
    let content = alpha["entity"]["core_fields"]["post_content"]
        .as_str()
        .unwrap();
    assert!(
        !content.contains(&source.base_url),
        "source URLs must not survive the relay: {content}"
    );
    // The transferred asset points at the target attachment URL.
    assert!(content.contains(&format!("{}/wp-content/uploads/mock/", target.base_url)));
    // Media chunks arrived at the target.
    assert_eq!(target.received_chunks(), 2);
    // Origin context preserved (the source site stays the origin).
    assert_eq!(alpha["origin_context"]["origin_site_uuid"], SOURCE_UUID);
    assert_eq!(
        alpha["multimodal_manifest"][0]["remote_url"]
            .as_str()
            .unwrap()
            .starts_with(&target.base_url),
        true
    );

    // State: both entities known with fingerprints + target ids.
    let state = load_sync_state(&sync_state_path.to_string_lossy()).unwrap();
    let pair_state = state.pairs.get(&pair.id).unwrap();
    assert_eq!(pair_state.known.len(), 2);
    assert!(pair_state.known.contains_key("uuid-alpha"));
    assert!(pair_state.known.contains_key("uuid-beta"));
    assert!(pair_state.known["uuid-alpha"].target_post_id.is_some());
    assert_eq!(pair_state.last_seen_source_id, 2);

    // Pair bookkeeping: count + cursor persisted, no error.
    let pairs_doc = load_sync_pairs(&pairs_path.to_string_lossy()).unwrap();
    let stored = find_pair_in_doc(&pairs_doc, &pair.id).unwrap();
    assert_eq!(stored.last_sync_count, Some(2));
    assert_eq!(stored.last_seen_source_id, Some(2));
    assert!(stored.last_error.is_none());

    // ---- Second run: nothing changed at source → the keyset digest
    // returns NOTHING past the cursor (real plugin contract: ID >
    // last_seen_id), so the run scans zero items and pushes nothing. (The
    // pre-SIM-14 mock re-reported every post, making skipped_count the
    // observable; under the real contract the zero-re-push proof is the
    // wire: no new packets.)
    let report2 = super::sync_pair_run(&client, &pair.id, &creds, None, &log_file)
        .await
        .expect("second run");
    assert_eq!(report2.synced_count, 0);
    assert_eq!(report2.scanned_count, 0);
    assert_eq!(
        target.received_packets().len(),
        2,
        "no duplicate pushes on the second run"
    );
    let state_after = load_sync_state(&sync_state_path.to_string_lossy()).unwrap();
    assert_eq!(
        state_after.pairs.get(&pair.id).unwrap().last_seen_source_id,
        2,
        "cursor stays converged"
    );
}

#[tokio::test]
async fn engine_run_reports_missing_credentials_clearly() {
    let state_root = std::env::temp_dir().join(format!(
        "sync_engine_nocred_{}_{}",
        std::process::id(),
        crate::logging::unix_ts()
    ));
    std::fs::create_dir_all(&state_root).unwrap();
    let _owned_key = crate::db::owned_mock_bindings_key();
    let _owned_data = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", state_root.display().to_string());
    let pairs_path = state_root.join("sync-pairs.json");
    let sync_state_path = state_root.join("sync-state.json");
    let creds_path = state_root.join("peer-creds.json");

    let _g1 = crate::db::TestEnvVarGuard::set(
        "WPTSALL_SYNC_PAIRS_FILE",
        pairs_path.to_string_lossy().as_ref(),
    );
    let _g2 = crate::db::TestEnvVarGuard::set(
        "WPTSALL_SYNC_STATE_FILE",
        sync_state_path.to_string_lossy().as_ref(),
    );
    let _g3 = crate::db::TestEnvVarGuard::set(
        "WPTSALL_SYNC_PEER_CREDENTIALS_FILE",
        creds_path.to_string_lossy().as_ref(),
    );

    let pair = make_pair("https://unpaired-a.example", "https://unpaired-b.example");
    persist_pair(&pairs_path.to_string_lossy(), &pair);

    let creds = PeerCredentialsDoc {
        schema_version: "wpmmcc-peer-credentials.v1".to_string(),
        client_origin_uuid: CLIENT_UUID.to_string(),
        client_origin_url: "https://wpmmcc-ats-client.local/xx".to_string(),
        client_origin_name: "WPMMCC ATS Client".to_string(),
        peers: HashMap::new(),
        updated_at: 1,
    };

    let client = reqwest::Client::new();
    let log_file = state_root.join("test.log").to_string_lossy().to_string();
    let report = super::sync_pair_run(&client, &pair.id, &creds, None, &log_file)
        .await
        .expect("run returns a report, not an error");

    assert_eq!(report.error_count, 1);
    assert!(report.last_error.as_deref().unwrap_or("").contains("配对"));
    assert_eq!(report.synced_count, 0);

    let pairs_doc = load_sync_pairs(&pairs_path.to_string_lossy()).unwrap();
    let stored = find_pair_in_doc(&pairs_doc, &pair.id).unwrap();
    assert!(stored.last_error.is_some());
}

/// FL-10 regression (doc 24 §四.3, SIM-14): an update to an ALREADY-SYNCED
/// post must propagate. The digest is a pure keyset cursor (the post's
/// local id never exceeds last_seen_id again), so the reconcile rotation's
/// `source_newer` verdict is the only update path — the client must re-pull
/// and re-ship, then converge (next rotation in_sync, no duplicate push).
#[tokio::test]
async fn source_newer_verdict_repulls_and_reships_updated_entity() {
    let state_root = std::env::temp_dir().join(format!(
        "sync_engine_srcnewer_{}_{}",
        std::process::id(),
        crate::logging::unix_ts()
    ));
    std::fs::create_dir_all(&state_root).unwrap();
    let _owned_key = crate::db::owned_mock_bindings_key();
    let _owned_data = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", state_root.display().to_string());
    let pairs_path = state_root.join("sync-pairs.json");
    let sync_state_path = state_root.join("sync-state.json");
    let creds_path = state_root.join("peer-creds.json");

    let _g1 = crate::db::TestEnvVarGuard::set(
        "WPTSALL_SYNC_PAIRS_FILE",
        pairs_path.to_string_lossy().as_ref(),
    );
    let _g2 = crate::db::TestEnvVarGuard::set(
        "WPTSALL_SYNC_STATE_FILE",
        sync_state_path.to_string_lossy().as_ref(),
    );
    let _g3 = crate::db::TestEnvVarGuard::set(
        "WPTSALL_SYNC_PEER_CREDENTIALS_FILE",
        creds_path.to_string_lossy().as_ref(),
    );

    let source = MockSite::spawn("22222222-2222-2222-2222-222222222222", |base| {
        vec![
            sample_source_packet(base, "uuid-alpha", 1, "Alpha v0"),
            sample_source_packet(base, "uuid-beta", 2, "Beta v0"),
        ]
    });
    let target = MockSite::spawn("33333333-3333-3333-3333-333333333333", |_| vec![]);

    let creds = credentials_doc(&source, &target);
    let pair = make_pair(&source.base_url, &target.base_url);
    persist_pair(&pairs_path.to_string_lossy(), &pair);

    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let log_file = state_root.join("test.log").to_string_lossy().to_string();

    // Run 1: both ship, cursor advances to 2.
    let report1 = super::sync_pair_run(&client, &pair.id, &creds, None, &log_file)
        .await
        .expect("first run");
    assert_eq!(report1.synced_count, 2);
    assert_eq!(report1.error_count, 0, "{:?}", report1.last_error);
    assert_eq!(target.received_packets().len(), 2);

    // The source post is UPDATED after the client already scanned past it.
    source.mutate_post("uuid-alpha", "Alpha v1");

    // Run 2: the digest cannot re-report alpha (id 1 ≤ cursor 2) — the
    // reconcile rotation must report source_newer and the client must
    // re-pull + re-ship the new version.
    let report2 = super::sync_pair_run(&client, &pair.id, &creds, None, &log_file)
        .await
        .expect("second run");
    assert_eq!(report2.error_count, 0, "{:?}", report2.last_error);
    assert_eq!(
        report2.synced_count, 1,
        "the updated entity re-ships via source_newer: {report2:?}"
    );
    assert_eq!(report2.scanned_count, 0, "keyset digest sees nothing new");

    let received = target.received_packets();
    assert_eq!(received.len(), 3, "exactly one re-ship, beta untouched");
    let alpha_v1 = received
        .iter()
        .filter(|p| p["entity"]["guid"] == "uuid-alpha")
        .last()
        .expect("alpha re-shipped")
        .clone();
    assert_eq!(
        alpha_v1["entity"]["core_fields"]["post_title"], "Alpha v1",
        "the relayed packet carries the UPDATED field verbatim"
    );

    // State converged to the new fingerprint.
    let state = load_sync_state(&sync_state_path.to_string_lossy()).unwrap();
    let known = state.pairs.get(&pair.id).unwrap();
    assert_eq!(
        known.known["uuid-alpha"].source_fingerprint,
        "fp-uuid-alpha-r1"
    );

    // Run 3: converged — rotation reports in_sync, nothing re-ships.
    let report3 = super::sync_pair_run(&client, &pair.id, &creds, None, &log_file)
        .await
        .expect("third run");
    assert_eq!(report3.error_count, 0, "{:?}", report3.last_error);
    assert_eq!(report3.synced_count, 0);
    assert_eq!(target.received_packets().len(), 3);
}

/// FL-11 regression (doc 24 §四.3, SIM-14): a transient push failure on a
/// FIRST ship must not lose the item. The safe cursor freezes below the
/// failed item, so the next run rescans it (and everything after), while
/// fingerprint stability keeps already-shipped items duplicate-free.
#[tokio::test]
async fn first_ship_failure_freezes_cursor_and_next_run_retries_without_duplicates() {
    let state_root = std::env::temp_dir().join(format!(
        "sync_engine_safecursor_{}_{}",
        std::process::id(),
        crate::logging::unix_ts()
    ));
    std::fs::create_dir_all(&state_root).unwrap();
    let _owned_key = crate::db::owned_mock_bindings_key();
    let _owned_data = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", state_root.display().to_string());
    let pairs_path = state_root.join("sync-pairs.json");
    let sync_state_path = state_root.join("sync-state.json");
    let creds_path = state_root.join("peer-creds.json");

    let _g1 = crate::db::TestEnvVarGuard::set(
        "WPTSALL_SYNC_PAIRS_FILE",
        pairs_path.to_string_lossy().as_ref(),
    );
    let _g2 = crate::db::TestEnvVarGuard::set(
        "WPTSALL_SYNC_STATE_FILE",
        sync_state_path.to_string_lossy().as_ref(),
    );
    let _g3 = crate::db::TestEnvVarGuard::set(
        "WPTSALL_SYNC_PEER_CREDENTIALS_FILE",
        creds_path.to_string_lossy().as_ref(),
    );

    let source = MockSite::spawn("22222222-2222-2222-2222-222222222222", |base| {
        vec![
            sample_source_packet(base, "uuid-alpha", 1, "Alpha"),
            sample_source_packet(base, "uuid-beta", 2, "Beta"),
        ]
    });
    let target = MockSite::spawn("33333333-3333-3333-3333-333333333333", |_| vec![]);

    let creds = credentials_doc(&source, &target);
    let pair = make_pair(&source.base_url, &target.base_url);
    persist_pair(&pairs_path.to_string_lossy(), &pair);

    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let log_file = state_root.join("test.log").to_string_lossy().to_string();

    // Fault: the FIRST push is rejected with a transient 500. Both items
    // are new (first ship) — alpha fails, beta still ships.
    target.inject_push_faults(1, 500);

    let report1 = super::sync_pair_run(&client, &pair.id, &creds, None, &log_file)
        .await
        .expect("faulted run returns a report");
    assert_eq!(report1.error_count, 1, "the rejected push is visible");
    assert_eq!(report1.synced_count, 1, "beta still shipped");
    assert_eq!(target.rejected_packets(), 1);
    assert_eq!(target.received_packets().len(), 1, "only beta accepted");

    // The cursor must NOT have advanced past the failed item: nothing was
    // known before this run, so the safe cursor stays at 0 even though
    // beta (higher id) shipped. Pre-FL-11 the scan maximum (2) was
    // persisted — alpha became permanently invisible.
    let state = load_sync_state(&sync_state_path.to_string_lossy()).unwrap();
    let pair_state = state.pairs.get(&pair.id).unwrap();
    assert_eq!(
        pair_state.last_seen_source_id, 0,
        "safe cursor froze below the failed first ship"
    );
    assert!(!pair_state.known.contains_key("uuid-alpha"));

    // Run 2 (clean): the digest rescans from 0 — alpha ships at last, and
    // beta skips via fingerprint stability (no duplicate).
    let report2 = super::sync_pair_run(&client, &pair.id, &creds, None, &log_file)
        .await
        .expect("recovery run");
    assert_eq!(report2.error_count, 0, "{:?}", report2.last_error);
    assert_eq!(report2.synced_count, 1, "alpha recovered");
    assert_eq!(report2.skipped_count, 1, "beta skipped, not re-pushed");
    assert_eq!(target.received_packets().len(), 2, "no duplicates");

    let state2 = load_sync_state(&sync_state_path.to_string_lossy()).unwrap();
    let pair_state2 = state2.pairs.get(&pair.id).unwrap();
    assert_eq!(pair_state2.last_seen_source_id, 2, "converged");
    assert!(pair_state2.known.contains_key("uuid-alpha"));
}

/// §68 (FL-11 contiguous-prefix amendment): a fingerprint-stable skip at a
/// HIGHER id must not advance the safe cursor over a lower-id ship failure
/// in the same digest page. Production shape: 23 first-ship media failures
/// sat BELOW 22 fingerprint-stable skips; the old max()-over-converged
/// advance (plus the pre-ship persist) wrote the stable tail's max id as
/// the cursor — every later digest (ID > cursor) came back empty and the
/// run finished clean with the 23 silently lost.
#[tokio::test]
async fn stable_skip_above_ship_failure_does_not_leap_cursor_over_it() {
    let state_root = std::env::temp_dir().join(format!(
        "sync_engine_prefix_{}_{}",
        std::process::id(),
        crate::logging::unix_ts()
    ));
    std::fs::create_dir_all(&state_root).unwrap();
    let _owned_key = crate::db::owned_mock_bindings_key();
    let _owned_data = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", state_root.display().to_string());
    let pairs_path = state_root.join("sync-pairs.json");
    let sync_state_path = state_root.join("sync-state.json");
    let creds_path = state_root.join("peer-creds.json");

    let _g1 = crate::db::TestEnvVarGuard::set(
        "WPTSALL_SYNC_PAIRS_FILE",
        pairs_path.to_string_lossy().as_ref(),
    );
    let _g2 = crate::db::TestEnvVarGuard::set(
        "WPTSALL_SYNC_STATE_FILE",
        sync_state_path.to_string_lossy().as_ref(),
    );
    let _g3 = crate::db::TestEnvVarGuard::set(
        "WPTSALL_SYNC_PEER_CREDENTIALS_FILE",
        creds_path.to_string_lossy().as_ref(),
    );

    // Page order (digest contract: ORDER BY ID ASC): alpha (id 1) NEW,
    // beta (id 2) already shipped and fingerprint-stable.
    let source = MockSite::spawn("22222222-2222-2222-2222-222222222222", |base| {
        vec![
            sample_source_packet(base, "uuid-alpha", 1, "Alpha"),
            sample_source_packet(base, "uuid-beta", 2, "Beta"),
        ]
    });
    let target = MockSite::spawn("33333333-3333-3333-3333-333333333333", |_| vec![]);

    let creds = credentials_doc(&source, &target);
    let pair = make_pair(&source.base_url, &target.base_url);
    persist_pair(&pairs_path.to_string_lossy(), &pair);

    // Seed the client state: beta known with the fingerprint the digest
    // re-reports (fp-uuid-beta) → a stable skip at id 2; cursor 0 so the
    // digest re-reports both items.
    {
        use crate::sync_engine::state::{
            save_sync_state, KnownEntity, PairSyncState, SyncStateDoc,
        };
        let mut doc = SyncStateDoc::default();
        doc.pairs.insert(
            pair.id.clone(),
            PairSyncState {
                pair_id: pair.id.clone(),
                last_seen_source_id: 0,
                reconcile_cursor: 0,
                known: HashMap::from([(
                    "uuid-beta".to_string(),
                    KnownEntity {
                        canonical_uuid: "uuid-beta".to_string(),
                        source_fingerprint: "fp-uuid-beta".to_string(),
                        vector_clock: 1,
                        post_type: "post".to_string(),
                        post_status: "publish".to_string(),
                        target_post_id: Some(202),
                        last_shipped_at: 1,
                        review_rejected: false,
                    },
                )]),
                updated_at: 1,
            },
        );
        save_sync_state(&sync_state_path.to_string_lossy(), &doc).unwrap();
    }

    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let log_file = state_root.join("test.log").to_string_lossy().to_string();

    // Fault: alpha's (the only) push is rejected with a transient 500.
    target.inject_push_faults(1, 500);

    let report1 = super::sync_pair_run(&client, &pair.id, &creds, None, &log_file)
        .await
        .expect("faulted run returns a report");
    assert_eq!(report1.error_count, 1, "the rejected push is visible");
    assert_eq!(report1.synced_count, 0, "nothing synced");
    assert_eq!(
        report1.skipped_count, 1,
        "beta skipped as fingerprint-stable"
    );
    assert_eq!(report1.pulled_count, 1, "only alpha pulled");
    assert_eq!(target.rejected_packets(), 1);
    assert_eq!(
        target.received_packets().len(),
        0,
        "alpha's push was rejected"
    );

    // §68: the cursor must NOT have advanced over alpha. Beta converged
    // (stable skip) at a HIGHER id, but alpha below it failed — the
    // contiguous prefix stops at alpha. Pre-§68 the skip's max() leapt the
    // cursor to 2 (and the pre-ship persist made it durable before the
    // ships even ran); every later digest (ID > 2) returned empty and the
    // failure vanished silently behind a clean run.
    let state = load_sync_state(&sync_state_path.to_string_lossy()).unwrap();
    let pair_state = state.pairs.get(&pair.id).unwrap();
    assert_eq!(
        pair_state.last_seen_source_id, 0,
        "a stable skip above a failure must not advance the cursor over it"
    );
    assert!(!pair_state.known.contains_key("uuid-alpha"));

    // Run 2 (clean): the digest rescans from 0 — alpha ships at last,
    // beta skips again (no duplicate).
    let report2 = super::sync_pair_run(&client, &pair.id, &creds, None, &log_file)
        .await
        .expect("recovery run");
    assert_eq!(report2.error_count, 0, "{:?}", report2.last_error);
    assert_eq!(report2.synced_count, 1, "alpha recovered");
    assert_eq!(report2.skipped_count, 1, "beta skipped, not re-pushed");
    assert_eq!(
        target.received_packets().len(),
        1,
        "only alpha, no duplicates"
    );

    let state2 = load_sync_state(&sync_state_path.to_string_lossy()).unwrap();
    let pair_state2 = state2.pairs.get(&pair.id).unwrap();
    assert_eq!(pair_state2.last_seen_source_id, 2, "converged");
    assert!(pair_state2.known.contains_key("uuid-alpha"));
    assert!(pair_state2.known.contains_key("uuid-beta"));
}

/// CLI-01 regression: the safe cursor is shared across digest pages but
/// convergence used to be judged per page. Page 1 (ids 1..=100) holds a
/// ship failure at id 1; page 2 (ids 101..=102) converges fully — the
/// persisted cursor must stay below id 1 and the next run must recover it.
#[tokio::test]
async fn cli01_two_page_digest_later_page_must_not_leap_cursor_over_page1_failure() {
    let state_root = std::env::temp_dir().join(format!(
        "sync_engine_twopage_{}_{}",
        std::process::id(),
        crate::logging::unix_ts()
    ));
    std::fs::create_dir_all(&state_root).unwrap();
    let _owned_key = crate::db::owned_mock_bindings_key();
    let _owned_data = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", state_root.display().to_string());
    let pairs_path = state_root.join("sync-pairs.json");
    let sync_state_path = state_root.join("sync-state.json");
    let creds_path = state_root.join("peer-creds.json");

    let _g1 = crate::db::TestEnvVarGuard::set(
        "WPTSALL_SYNC_PAIRS_FILE",
        pairs_path.to_string_lossy().as_ref(),
    );
    let _g2 = crate::db::TestEnvVarGuard::set(
        "WPTSALL_SYNC_STATE_FILE",
        sync_state_path.to_string_lossy().as_ref(),
    );
    let _g3 = crate::db::TestEnvVarGuard::set(
        "WPTSALL_SYNC_PEER_CREDENTIALS_FILE",
        creds_path.to_string_lossy().as_ref(),
    );

    let source = MockSite::spawn("22222222-2222-2222-2222-222222222222", |base| {
        (1..=102u64)
            .map(|id| sample_source_packet(base, &format!("uuid-{id:03}"), id, &format!("P{id}")))
            .collect()
    });
    let target = MockSite::spawn("33333333-3333-3333-3333-333333333333", |_| vec![]);

    let creds = credentials_doc(&source, &target);
    let pair = make_pair(&source.base_url, &target.base_url);
    persist_pair(&pairs_path.to_string_lossy(), &pair);

    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let log_file = state_root.join("test.log").to_string_lossy().to_string();

    // The first push (id 1, first of page 1) is rejected with a transient 500.
    target.inject_push_faults(1, 500);

    let report1 = super::sync_pair_run(&client, &pair.id, &creds, None, &log_file)
        .await
        .expect("faulted run returns a report");
    assert_eq!(report1.error_count, 1, "the rejected push is visible");
    assert_eq!(report1.synced_count, 101, "everything except id 1 shipped");
    assert_eq!(report1.scanned_count, 102, "both digest pages were scanned");

    let state = load_sync_state(&sync_state_path.to_string_lossy()).unwrap();
    let pair_state = state.pairs.get(&pair.id).unwrap();
    assert!(!pair_state.known.contains_key("uuid-001"));
    let cursor_after_run1 = pair_state.last_seen_source_id;
    eprintln!("CLI01 cursor_after_run1={cursor_after_run1}");

    let report2 = super::sync_pair_run(&client, &pair.id, &creds, None, &log_file)
        .await
        .expect("recovery run");
    eprintln!(
        "CLI01 run2: scanned={} synced={} skipped={} errors={} target_received={}",
        report2.scanned_count,
        report2.synced_count,
        report2.skipped_count,
        report2.error_count,
        target.received_packets().len()
    );
    assert_eq!(
        cursor_after_run1, 0,
        "page 2 converged but id 1 (page 1) did not: cursor must stay below it"
    );
    assert_eq!(report2.error_count, 0, "{:?}", report2.last_error);
    assert_eq!(report2.synced_count, 1, "id 1 recovered");
    assert_eq!(
        target.received_packets().len(),
        102,
        "no loss, no duplicates"
    );
}

/// R1 / TST-03: keep the already-fixed CLI-01 behavior at the page boundary,
/// boundary+1 and two boundaries+1, independently of the browser mock.
#[tokio::test]
async fn cli01_r1_scale_matrix_first_failure_recovers_without_duplicates() {
    for count in [100u64, 101, 201] {
        let root = std::env::temp_dir().join(format!(
            "sync_engine_r1_{}_{}_{}",
            std::process::id(),
            crate::logging::unix_ts(),
            count
        ));
        std::fs::create_dir_all(&root).unwrap();
        let _owned_key = crate::db::owned_mock_bindings_key();
        let _owned_data = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.display().to_string());
        let pairs_path = root.join("sync-pairs.json");
        let state_path = root.join("sync-state.json");
        let creds_path = root.join("peer-creds.json");
        let _g1 = crate::db::TestEnvVarGuard::set(
            "WPTSALL_SYNC_PAIRS_FILE",
            pairs_path.to_string_lossy().as_ref(),
        );
        let _g2 = crate::db::TestEnvVarGuard::set(
            "WPTSALL_SYNC_STATE_FILE",
            state_path.to_string_lossy().as_ref(),
        );
        let _g3 = crate::db::TestEnvVarGuard::set(
            "WPTSALL_SYNC_PEER_CREDENTIALS_FILE",
            creds_path.to_string_lossy().as_ref(),
        );
        let source = MockSite::spawn("22222222-2222-2222-2222-222222222222", |base| {
            (1..=count)
                .map(|id| sample_source_packet(base, &format!("r1-{id:03}"), id, &format!("P{id}")))
                .collect()
        });
        let target = MockSite::spawn("33333333-3333-3333-3333-333333333333", |_| vec![]);
        let creds = credentials_doc(&source, &target);
        let pair = make_pair(&source.base_url, &target.base_url);
        persist_pair(&pairs_path.to_string_lossy(), &pair);
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let log = root.join("test.log").to_string_lossy().to_string();
        target.inject_push_faults(1, 500);
        let failed = super::sync_pair_run(&client, &pair.id, &creds, None, &log)
            .await
            .unwrap();
        assert_eq!(failed.scanned_count, count as usize, "scale={count}");
        assert_eq!(failed.synced_count, count as usize - 1, "scale={count}");
        assert_eq!(failed.error_count, 1, "scale={count}");
        let state = load_sync_state(&state_path.to_string_lossy()).unwrap();
        assert_eq!(
            state.pairs[&pair.id].last_seen_source_id, 0,
            "scale={count}"
        );
        assert!(!state.pairs[&pair.id].known.contains_key("r1-001"));
        let recovered = super::sync_pair_run(&client, &pair.id, &creds, None, &log)
            .await
            .unwrap();
        assert_eq!(recovered.error_count, 0, "{:?}", recovered.last_error);
        assert_eq!(recovered.synced_count, 1, "scale={count}");
        assert_eq!(
            target.received_packets().len(),
            count as usize,
            "scale={count}"
        );
        let state = load_sync_state(&state_path.to_string_lossy()).unwrap();
        assert_eq!(
            state.pairs[&pair.id].last_seen_source_id, count,
            "scale={count}"
        );
        let noop = super::sync_pair_run(&client, &pair.id, &creds, None, &log)
            .await
            .unwrap();
        assert_eq!(noop.synced_count, 0, "scale={count}");
        assert_eq!(
            target.received_packets().len(),
            count as usize,
            "scale={count}"
        );
    }
}

/// The digest trash arm: a KNOWN entity whose digest item reports
/// post_status=trash (with a fingerprint bump) relays an action=trash
/// tombstone instead of re-pulling content. Reachable in production via
/// the safe-cursor rescan window (an item trashed between a failed ship
/// and its retry); driven here by seeding state directly.
#[tokio::test]
async fn digest_trash_arm_relays_trash_tombstone() {
    let state_root = std::env::temp_dir().join(format!(
        "sync_engine_trash_{}_{}",
        std::process::id(),
        crate::logging::unix_ts()
    ));
    std::fs::create_dir_all(&state_root).unwrap();
    let _owned_key = crate::db::owned_mock_bindings_key();
    let _owned_data = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", state_root.display().to_string());
    let pairs_path = state_root.join("sync-pairs.json");
    let sync_state_path = state_root.join("sync-state.json");
    let creds_path = state_root.join("peer-creds.json");

    let _g1 = crate::db::TestEnvVarGuard::set(
        "WPTSALL_SYNC_PAIRS_FILE",
        pairs_path.to_string_lossy().as_ref(),
    );
    let _g2 = crate::db::TestEnvVarGuard::set(
        "WPTSALL_SYNC_STATE_FILE",
        sync_state_path.to_string_lossy().as_ref(),
    );
    let _g3 = crate::db::TestEnvVarGuard::set(
        "WPTSALL_SYNC_PEER_CREDENTIALS_FILE",
        creds_path.to_string_lossy().as_ref(),
    );

    // Source: the post is ALREADY trashed (fingerprint bumped), but the
    // client's state still knows it as a shipped publish with a cursor
    // BELOW its id — exactly the rescan-window shape.
    let source = MockSite::spawn("22222222-2222-2222-2222-222222222222", |base| {
        let mut packet = sample_source_packet(base, "uuid-alpha", 1, "Alpha");
        packet["entity"]["status"] = json!("trash");
        packet["source_fingerprint"] = json!("fp-uuid-alpha-trash");
        vec![packet]
    });
    let target = MockSite::spawn("33333333-3333-3333-3333-333333333333", |_| vec![]);

    let creds = credentials_doc(&source, &target);
    let pair = make_pair(&source.base_url, &target.base_url);
    persist_pair(&pairs_path.to_string_lossy(), &pair);

    // Seed the client state: alpha known as publish + old fingerprint,
    // cursor 0 so the digest (ID > 0) re-reports the trashed item.
    {
        use crate::sync_engine::state::{
            save_sync_state, KnownEntity, PairSyncState, SyncStateDoc,
        };
        let mut doc = SyncStateDoc::default();
        doc.pairs.insert(
            pair.id.clone(),
            PairSyncState {
                pair_id: pair.id.clone(),
                last_seen_source_id: 0,
                reconcile_cursor: 0,
                known: HashMap::from([(
                    "uuid-alpha".to_string(),
                    KnownEntity {
                        canonical_uuid: "uuid-alpha".to_string(),
                        source_fingerprint: "fp-uuid-alpha".to_string(),
                        vector_clock: 1,
                        post_type: "post".to_string(),
                        post_status: "publish".to_string(),
                        target_post_id: Some(101),
                        last_shipped_at: 1,
                        review_rejected: false,
                    },
                )]),
                updated_at: 1,
            },
        );
        save_sync_state(&sync_state_path.to_string_lossy(), &doc).unwrap();
    }

    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let log_file = state_root.join("test.log").to_string_lossy().to_string();

    let report = super::sync_pair_run(&client, &pair.id, &creds, None, &log_file)
        .await
        .expect("run");
    assert_eq!(report.error_count, 0, "{:?}", report.last_error);
    assert_eq!(report.trashed_count, 1, "the trash tombstone shipped");
    assert_eq!(report.synced_count, 0, "no content re-pull for a trash");

    let received = target.received_packets();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0]["action"], "trash");
    assert_eq!(received[0]["entity"]["guid"], "uuid-alpha");
    assert_eq!(received[0]["entity"]["status"], "trash");

    // State records the trash (no repeat tombstone on the next run).
    let state = load_sync_state(&sync_state_path.to_string_lossy()).unwrap();
    let known = state.pairs.get(&pair.id).unwrap();
    assert_eq!(known.known["uuid-alpha"].post_status, "trash");
    let report2 = super::sync_pair_run(&client, &pair.id, &creds, None, &log_file)
        .await
        .expect("second run");
    assert_eq!(report2.trashed_count, 0);
    assert_eq!(target.received_packets().len(), 1, "no repeat tombstone");
}

#[test]
fn parks_in_review_inbox_matrix_over_strategies_and_flag() {
    // X-1 (tasks/5.3falsh2/12 批 B P3): only manual_review of the five
    // canonical values parks via strategy; the flag is an independent
    // OR-condition on top of every strategy.
    let strategies = [
        (ConflictStrategy::Lww, false),
        (ConflictStrategy::SourceWins, false),
        (ConflictStrategy::TargetWins, false),
        (ConflictStrategy::Merge, false),
        (ConflictStrategy::ManualReview, true),
    ];
    for (strategy, expected) in strategies {
        let mut pair = make_pair("https://a.example", "https://b.example");
        pair.conflict_strategy = strategy;
        pair.review_before_push = false;
        assert_eq!(
            super::parks_in_review_inbox(&pair),
            expected,
            "strategy {strategy:?} with flag off"
        );
        pair.review_before_push = true;
        assert!(
            super::parks_in_review_inbox(&pair),
            "flag on must always park ({strategy:?})"
        );
    }
}

#[tokio::test]
async fn manual_review_strategy_parks_packets_in_client_review_inbox() {
    // X-1 (tasks/5.3falsh2/12 批 B P3): a pair whose conflict strategy is
    // manual_review (review_before_push stays false) parks every pulled
    // packet in the client-side review inbox instead of pushing; the
    // target receives nothing, the cursor still converges, and a second
    // run does not duplicate the inbox.
    let state_root = std::env::temp_dir().join(format!(
        "sync_engine_manual_review_{}_{}",
        std::process::id(),
        crate::logging::unix_ts()
    ));
    std::fs::create_dir_all(&state_root).unwrap();
    let _owned_key = crate::db::owned_mock_bindings_key();
    let _owned_data = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", state_root.display().to_string());
    let pairs_path = state_root.join("sync-pairs.json");
    let sync_state_path = state_root.join("sync-state.json");
    let creds_path = state_root.join("peer-creds.json");
    let review_path = state_root.join("sync-review.json");

    let _g1 = crate::db::TestEnvVarGuard::set(
        "WPTSALL_SYNC_PAIRS_FILE",
        pairs_path.to_string_lossy().as_ref(),
    );
    let _g2 = crate::db::TestEnvVarGuard::set(
        "WPTSALL_SYNC_STATE_FILE",
        sync_state_path.to_string_lossy().as_ref(),
    );
    let _g3 = crate::db::TestEnvVarGuard::set(
        "WPTSALL_SYNC_PEER_CREDENTIALS_FILE",
        creds_path.to_string_lossy().as_ref(),
    );
    let _g4 = crate::db::TestEnvVarGuard::set(
        "WPTSALL_SYNC_REVIEW_FILE",
        review_path.to_string_lossy().as_ref(),
    );

    let source = MockSite::spawn("22222222-2222-2222-2222-222222222222", |base| {
        vec![
            sample_source_packet(base, "uuid-alpha", 1, "Alpha"),
            sample_source_packet(base, "uuid-beta", 2, "Beta"),
        ]
    });
    let target = MockSite::spawn("33333333-3333-3333-3333-333333333333", |_| vec![]);

    let creds = credentials_doc(&source, &target);
    let mut pair = make_pair(&source.base_url, &target.base_url);
    pair.conflict_strategy = ConflictStrategy::ManualReview;
    persist_pair(&pairs_path.to_string_lossy(), &pair);

    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let log_file = state_root.join("test.log").to_string_lossy().to_string();

    let report = super::sync_pair_run(&client, &pair.id, &creds, None, &log_file)
        .await
        .expect("run should succeed");

    // Nothing shipped; both packets parked for human review.
    assert_eq!(report.synced_count, 0, "report: {report:?}");
    assert_eq!(report.pending_review_count, 2);
    // Media transfers while BUILDING the push-ready relayed packet (the
    // parked item must carry target-rewritten URLs); the content push
    // itself is what waits for human approve.
    assert_eq!(
        report.media_transferred, 2,
        "media lands before parking; packets do not"
    );
    assert_eq!(report.error_count, 0, "no errors: {:?}", report.last_error);
    assert_eq!(
        target.received_packets().len(),
        0,
        "manual_review must not push directly"
    );

    // Both packets wait in the inbox with the pair id, source fields, and
    // a fully prepared relay packet for the approve flow.
    let review_doc = load_sync_review(&review_path.to_string_lossy()).unwrap();
    assert_eq!(count_pending_for_pair(&review_doc, &pair.id), 2);
    let alpha = review_doc
        .items
        .iter()
        .find(|i| i.canonical_uuid == "uuid-alpha")
        .expect("alpha parked");
    assert_eq!(alpha.status, SyncReviewStatus::PendingReview);
    assert_eq!(alpha.pair_id, pair.id);
    assert_eq!(alpha.source_title, "Alpha");
    assert_eq!(
        alpha.proposed_title, "Alpha",
        "no translator on this pair: proposed = source"
    );
    assert_eq!(alpha.relayed_packet.entity.guid, "uuid-alpha");

    // Parked items are handled (not failed): the cursor converges, so the
    // second run scans zero and does not duplicate the inbox or push.
    let report2 = super::sync_pair_run(&client, &pair.id, &creds, None, &log_file)
        .await
        .expect("second run");
    assert_eq!(report2.scanned_count, 0);
    assert_eq!(report2.pending_review_count, 0);
    assert_eq!(target.received_packets().len(), 0);
    let review_doc2 = load_sync_review(&review_path.to_string_lossy()).unwrap();
    assert_eq!(
        count_pending_for_pair(&review_doc2, &pair.id),
        2,
        "second run must not duplicate parked items"
    );
}

/// X-1 (tasks/5.3falsh2/12 批 B P4): five-value e2e journeys — one full
/// flow per canonical conflict strategy. The four push-through values
/// share this harness; the fifth value (manual_review) is the parking
/// journey above. Value semantics live at the executor — the WP-side
/// arbiter, covered by the plugin's relation-driven unit cases — while
/// the engine journey pins the other half of the contract end to end:
/// the canonical wire word persists through the pair store (serde
/// snake_case rename == wire vocabulary, the same form the pairing
/// handshake relays), then drives the full relay chain (pull →
/// translate → media transfer → HMAC-signed push) without parking, and
/// cursors converge so a second run re-ships nothing.
async fn run_push_through_strategy_journey(strategy: ConflictStrategy, wire_word: &str) {
    let state_root = std::env::temp_dir().join(format!(
        "sync_engine_strategy_{wire_word}_{}_{}",
        std::process::id(),
        crate::logging::unix_ts()
    ));
    std::fs::create_dir_all(&state_root).unwrap();
    let _owned_key = crate::db::owned_mock_bindings_key();
    let _owned_data = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", state_root.display().to_string());
    let pairs_path = state_root.join("sync-pairs.json");
    let sync_state_path = state_root.join("sync-state.json");
    let creds_path = state_root.join("peer-creds.json");
    let review_path = state_root.join("sync-review.json");

    let _g1 = crate::db::TestEnvVarGuard::set(
        "WPTSALL_SYNC_PAIRS_FILE",
        pairs_path.to_string_lossy().as_ref(),
    );
    let _g2 = crate::db::TestEnvVarGuard::set(
        "WPTSALL_SYNC_STATE_FILE",
        sync_state_path.to_string_lossy().as_ref(),
    );
    let _g3 = crate::db::TestEnvVarGuard::set(
        "WPTSALL_SYNC_PEER_CREDENTIALS_FILE",
        creds_path.to_string_lossy().as_ref(),
    );
    let _g4 = crate::db::TestEnvVarGuard::set(
        "WPTSALL_SYNC_REVIEW_FILE",
        review_path.to_string_lossy().as_ref(),
    );

    let source = MockSite::spawn("22222222-2222-2222-2222-222222222222", |base| {
        vec![
            sample_source_packet(base, "uuid-alpha", 1, "Alpha"),
            sample_source_packet(base, "uuid-beta", 2, "Beta"),
        ]
    });
    let target = MockSite::spawn("33333333-3333-3333-3333-333333333333", |_| vec![]);

    let creds = credentials_doc(&source, &target);
    let mut pair = make_pair(&source.base_url, &target.base_url);
    pair.conflict_strategy = strategy;
    persist_pair(&pairs_path.to_string_lossy(), &pair);

    // The persisted pair carries the canonical wire word — exactly what
    // every store reader (and the pairing handshake that relays this
    // field upstream) will see on the wire.
    let encoded = std::fs::read(&pairs_path).unwrap();
    let persisted = crate::bindings::decrypt_from_bytes(&encoded).unwrap();
    let persisted_doc: Value = serde_json::from_str(&persisted).unwrap();
    assert_eq!(
        persisted_doc["pairs"][0]["conflict_strategy"], wire_word,
        "persisted pair must carry the canonical wire word for {wire_word}"
    );

    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let log_file = state_root.join("test.log").to_string_lossy().to_string();

    let report = super::sync_pair_run(&client, &pair.id, &creds, None, &log_file)
        .await
        .expect("run should succeed");

    // Push-through: both packets ship on the signed push channel and
    // nothing parks in the review inbox.
    assert_eq!(report.synced_count, 2, "report: {report:?}");
    assert_eq!(report.pending_review_count, 0);
    assert_eq!(report.media_transferred, 2);
    assert_eq!(report.error_count, 0, "no errors: {:?}", report.last_error);
    let received = target.received_packets();
    let mut pushed_guids: Vec<&str> = received
        .iter()
        .map(|p| p["entity"]["guid"].as_str().unwrap_or_default())
        .collect();
    pushed_guids.sort_unstable();
    assert_eq!(
        pushed_guids,
        vec!["uuid-alpha", "uuid-beta"],
        "both packets must land on the target via the push channel"
    );

    // Cursors converge: the second run scans nothing and re-ships nothing.
    let report2 = super::sync_pair_run(&client, &pair.id, &creds, None, &log_file)
        .await
        .expect("second run");
    assert_eq!(report2.scanned_count, 0);
    assert_eq!(report2.synced_count, 0);
    assert_eq!(
        target.received_packets().len(),
        2,
        "second run must not duplicate pushes"
    );
}

#[tokio::test]
async fn lww_strategy_journey_pushes_through_end_to_end() {
    run_push_through_strategy_journey(ConflictStrategy::Lww, "lww").await;
}

#[tokio::test]
async fn source_wins_strategy_journey_pushes_through_end_to_end() {
    run_push_through_strategy_journey(ConflictStrategy::SourceWins, "source_wins").await;
}

#[tokio::test]
async fn target_wins_strategy_journey_pushes_through_end_to_end() {
    run_push_through_strategy_journey(ConflictStrategy::TargetWins, "target_wins").await;
}

#[tokio::test]
async fn merge_strategy_journey_pushes_through_end_to_end() {
    run_push_through_strategy_journey(ConflictStrategy::Merge, "merge").await;
}

/// 批 C (X-5): total-failure semantics for the sync-pair backoff domain.
/// Only zero-synced + ≥1-error runs count as pair-level failures.
#[test]
fn sync_run_total_failure_semantics() {
    let base = || crate::sync_engine::SyncRunReport {
        pair_id: "p".into(),
        source_domain: "s".into(),
        target_domain: "t".into(),
        trace_id: "sync-p-1".into(),
        ..Default::default()
    };

    // Zero synced + errors = total failure (dead endpoint / bad creds).
    let mut r = base();
    r.error_count = 3;
    assert!(crate::sync_engine::sync_run_is_total_failure(&r));

    // Any successful push proves the lane works: residual packet errors
    // are packet-level, never pair-level.
    let mut r = base();
    r.synced_count = 1;
    r.error_count = 2;
    assert!(!crate::sync_engine::sync_run_is_total_failure(&r));

    // A clean manual_review park (zero synced, zero errors) is not failure.
    let mut r = base();
    r.pending_review_count = 5;
    assert!(!crate::sync_engine::sync_run_is_total_failure(&r));

    // A fully clean, empty run is not failure.
    let r = base();
    assert!(!crate::sync_engine::sync_run_is_total_failure(&r));
}

/// 批 R (P2P pair 状态机, 冻结表销账): crash-recovery journey 1 — a
/// mid-flight shipping row (translation/media/relayed already paid and
/// persisted, crash before the push ack) must RESUME on the next pair run
/// without re-paying any stage: zero media re-transfers, a byte-identical
/// re-push of the STORED relayed packet (same relay-minted packet_id; the
/// target's fingerprint ack makes the re-push idempotent), URL-rewritten
/// content preserved from the stored packet, and the row closes once the
/// push acks.
#[tokio::test]
async fn crashed_inflight_row_resumes_without_repaying_paid_stages() {
    let state_root = std::env::temp_dir().join(format!(
        "sync_engine_inflight_resume_{}_{}",
        std::process::id(),
        crate::logging::unix_ts()
    ));
    std::fs::create_dir_all(&state_root).unwrap();
    let _owned_key = crate::db::owned_mock_bindings_key();
    let _owned_data = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", state_root.display().to_string());
    let pairs_path = state_root.join("sync-pairs.json");
    let sync_state_path = state_root.join("sync-state.json");
    let creds_path = state_root.join("peer-creds.json");
    let db_path = state_root.join("wptsall.db");

    let _g1 = crate::db::TestEnvVarGuard::set(
        "WPTSALL_SYNC_PAIRS_FILE",
        pairs_path.to_string_lossy().as_ref(),
    );
    let _g2 = crate::db::TestEnvVarGuard::set(
        "WPTSALL_SYNC_STATE_FILE",
        sync_state_path.to_string_lossy().as_ref(),
    );
    let _g3 = crate::db::TestEnvVarGuard::set(
        "WPTSALL_SYNC_PEER_CREDENTIALS_FILE",
        creds_path.to_string_lossy().as_ref(),
    );
    let _g4 =
        crate::db::TestEnvVarGuard::set("WPTSALL_DB_PATH", db_path.to_string_lossy().as_ref());

    let source = MockSite::spawn("22222222-2222-2222-2222-222222222222", |base| {
        vec![sample_source_packet(base, "uuid-alpha", 1, "Alpha")]
    });
    let target = MockSite::spawn("33333333-3333-3333-3333-333333333333", |_| vec![]);

    let creds = credentials_doc(&source, &target);
    let pair = make_pair(&source.base_url, &target.base_url);
    persist_pair(&pairs_path.to_string_lossy(), &pair);

    // Seed the mid-flight row exactly as a crash after the media transfer +
    // relay transform (but before the push ack) would have left it: the
    // per-asset media map complete, and the STORED relayed packet already
    // URL-rewritten via the real relay transform (same packet_id the
    // source packet carried).
    let replay_packet_id = {
        let conn = crate::db::open_db(&db_path.to_string_lossy()).unwrap();
        let db = std::sync::Arc::new(tokio::sync::Mutex::new(conn));
        crate::db::sync_inflight::begin_shipping(&db, &pair.id, "uuid-alpha")
            .await
            .unwrap();
        let remote_url = format!("{}/wp-content/uploads/uuid-alpha.png", source.base_url);
        let target_url = format!("{}/wp-content/uploads/mock/1.png", target.base_url);
        crate::db::sync_inflight::store_media_entry(
            &db,
            &pair.id,
            "uuid-alpha",
            &remote_url,
            &target_url,
        )
        .await
        .unwrap();
        let pulled: crate::sync_engine::packet::SyncPacket = serde_json::from_value(
            sample_source_packet(&source.base_url, "uuid-alpha", 1, "Alpha"),
        )
        .unwrap();
        crate::db::sync_inflight::store_translation_scope(
            &db,
            &pair.id,
            "uuid-alpha",
            &super::paid_snapshot_scope(&pulled, &pair, None).unwrap(),
        )
        .await
        .unwrap();
        let relayed = crate::sync_engine::shipper::relay_packet_for_target(
            &pulled,
            &pair,
            None,
            &HashMap::from([(remote_url, target_url)]),
            &[],
        );
        // The relay transform mints a fresh pkt_<uuid> per packet — the
        // resume invariant is a byte-identical replay of the STORED packet
        // (this relay-minted id), not the source pull id.
        let replay_packet_id = relayed.packet_id.clone();
        let relayed_json = serde_json::to_string(&relayed).unwrap();
        crate::db::sync_inflight::store_relayed_packet(
            &db,
            &pair.id,
            "uuid-alpha",
            "upsert",
            &relayed_json,
        )
        .await
        .unwrap();
        replay_packet_id
    };

    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let log_file = state_root.join("test.log").to_string_lossy().to_string();

    let report = super::sync_pair_run(&client, &pair.id, &creds, None, &log_file)
        .await
        .expect("recovery run");

    // The stored packet shipped: no errors, one synced.
    assert_eq!(report.error_count, 0, "{:?}", report.last_error);
    assert_eq!(report.synced_count, 1, "report: {report:?}");
    // THE paid-side-effect proof: the manifest asset was already recorded
    // in the stored map — the resume must transfer ZERO media chunks.
    assert_eq!(
        target.received_chunks(),
        0,
        "resume must not re-pay the media transfer"
    );
    // Same packet_id replay (target-side fingerprint ack contract).
    let received = target.received_packets();
    assert_eq!(received.len(), 1);
    assert_eq!(
        received[0]["packet_id"], replay_packet_id,
        "the replayed push must be the STORED relayed packet byte-identically"
    );
    assert_eq!(received[0]["entity"]["guid"], "uuid-alpha");
    // The STORED transform's URL rewrite survives the replay verbatim.
    let content = received[0]["entity"]["core_fields"]["post_content"]
        .as_str()
        .unwrap();
    assert!(
        !content.contains(&source.base_url),
        "stored relay rewrite must survive: {content}"
    );
    assert!(content.contains(&format!("{}/wp-content/uploads/mock/1.png", target.base_url)));
    // State converged: the entity is known with its target post id.
    let state = load_sync_state(&sync_state_path.to_string_lossy()).unwrap();
    let pair_state = state.pairs.get(&pair.id).unwrap();
    assert!(pair_state.known.contains_key("uuid-alpha"));
    assert!(pair_state.known["uuid-alpha"].target_post_id.is_some());
    // The shipping row closed (durable home reached).
    {
        let conn = crate::db::open_db(&db_path.to_string_lossy()).unwrap();
        let db = std::sync::Arc::new(tokio::sync::Mutex::new(conn));
        let row = crate::db::sync_inflight::find_shipping(&db, &pair.id, "uuid-alpha")
            .await
            .unwrap();
        assert!(row.is_none(), "row must close after the push ack");
    }

    // A follow-up clean run converges: nothing re-ships, no chunks.
    let report2 = super::sync_pair_run(&client, &pair.id, &creds, None, &log_file)
        .await
        .expect("second run");
    assert_eq!(report2.error_count, 0, "{:?}", report2.last_error);
    assert_eq!(report2.synced_count, 0);
    assert_eq!(target.received_packets().len(), 1);
    assert_eq!(target.received_chunks(), 0);
}

/// 批 R crash-recovery journey 2 — black-box (no row seeding): a push
/// failure mid-flight keeps the unit's shipping row alive with the paid
/// stages persisted (row stays resumable with an error note); the retry
/// run replays the STORED relayed packet byte-identically with ZERO new
/// media chunks and closes the row on ack.
#[tokio::test]
async fn push_failure_keeps_row_shipping_and_retry_reuses_paid_stages() {
    let state_root = std::env::temp_dir().join(format!(
        "sync_engine_inflight_retry_{}_{}",
        std::process::id(),
        crate::logging::unix_ts()
    ));
    std::fs::create_dir_all(&state_root).unwrap();
    let _owned_key = crate::db::owned_mock_bindings_key();
    let _owned_data = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", state_root.display().to_string());
    let pairs_path = state_root.join("sync-pairs.json");
    let sync_state_path = state_root.join("sync-state.json");
    let creds_path = state_root.join("peer-creds.json");
    let db_path = state_root.join("wptsall.db");

    let _g1 = crate::db::TestEnvVarGuard::set(
        "WPTSALL_SYNC_PAIRS_FILE",
        pairs_path.to_string_lossy().as_ref(),
    );
    let _g2 = crate::db::TestEnvVarGuard::set(
        "WPTSALL_SYNC_STATE_FILE",
        sync_state_path.to_string_lossy().as_ref(),
    );
    let _g3 = crate::db::TestEnvVarGuard::set(
        "WPTSALL_SYNC_PEER_CREDENTIALS_FILE",
        creds_path.to_string_lossy().as_ref(),
    );
    let _g4 =
        crate::db::TestEnvVarGuard::set("WPTSALL_DB_PATH", db_path.to_string_lossy().as_ref());

    let source = MockSite::spawn("22222222-2222-2222-2222-222222222222", |base| {
        vec![sample_source_packet(base, "uuid-alpha", 1, "Alpha")]
    });
    let target = MockSite::spawn("33333333-3333-3333-3333-333333333333", |_| vec![]);

    let creds = credentials_doc(&source, &target);
    let pair = make_pair(&source.base_url, &target.base_url);
    persist_pair(&pairs_path.to_string_lossy(), &pair);

    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let log_file = state_root.join("test.log").to_string_lossy().to_string();

    // Run 1: the push is rejected with a transient 500 AFTER the media
    // transfer + relay transform were paid and persisted.
    target.inject_push_faults(1, 500);
    let report1 = super::sync_pair_run(&client, &pair.id, &creds, None, &log_file)
        .await
        .expect("faulted run returns a report");
    assert_eq!(report1.error_count, 1, "the rejected push is visible");
    assert_eq!(report1.synced_count, 0);
    assert_eq!(report1.media_transferred, 1, "media was paid in run 1");
    assert_eq!(target.rejected_packets(), 1);
    assert_eq!(target.received_packets().len(), 0);

    // The unit's shipping row is alive with the paid stages + error note.
    let stored_packet_id = {
        let conn = crate::db::open_db(&db_path.to_string_lossy()).unwrap();
        let db = std::sync::Arc::new(tokio::sync::Mutex::new(conn));
        let row = crate::db::sync_inflight::find_shipping(&db, &pair.id, "uuid-alpha")
            .await
            .unwrap()
            .expect("row stays shipping after a push failure");
        assert!(row.ctx.translation.is_none(), "sync_only pair: no cascade");
        assert_eq!(row.ctx.media_url_map.len(), 1, "media map persisted");
        assert!(
            !row.relayed_json.is_empty(),
            "the relayed packet was persisted pre-push"
        );
        assert_eq!(row.ctx.action, "upsert");
        assert_eq!(row.attempts, 1);
        // The retry must replay THIS stored relayed packet — the relay
        // transform mints a fresh pkt_<uuid> per packet, so the invariant
        // is the stored packet_id, not the source pull id.
        let stored_packet_id =
            serde_json::from_str::<crate::sync_engine::packet::SyncPacket>(&row.relayed_json)
                .unwrap()
                .packet_id;
        let expected_remote = format!("{}/wp-content/uploads/uuid-alpha.png", source.base_url);
        assert!(
            row.ctx.media_url_map.contains_key(&expected_remote),
            "map keyed by the source remote url: {:?}",
            row.ctx.media_url_map.keys().collect::<Vec<_>>()
        );
        stored_packet_id
    };

    // Run 2 (clean): the digest rescans the unconverged item (safe cursor
    // froze below it), the stored relayed packet replays with the SAME
    // packet_id, ZERO new media chunks, and the row closes on ack.
    let report2 = super::sync_pair_run(&client, &pair.id, &creds, None, &log_file)
        .await
        .expect("recovery run");
    assert_eq!(report2.error_count, 0, "{:?}", report2.last_error);
    assert_eq!(report2.synced_count, 1, "report: {report2:?}");
    assert_eq!(
        report2.media_transferred, 0,
        "retry must not re-pay the media transfer"
    );
    assert_eq!(
        target.received_chunks(),
        1,
        "total chunks stay at run-1's count — no re-transfer"
    );
    let received = target.received_packets();
    assert_eq!(received.len(), 1);
    assert_eq!(
        received[0]["packet_id"], stored_packet_id,
        "the retry must replay the stored relayed packet byte-identically"
    );
    assert_eq!(received[0]["entity"]["guid"], "uuid-alpha");

    // State converged + row closed.
    let state = load_sync_state(&sync_state_path.to_string_lossy()).unwrap();
    let pair_state = state.pairs.get(&pair.id).unwrap();
    assert!(pair_state.known.contains_key("uuid-alpha"));
    {
        let conn = crate::db::open_db(&db_path.to_string_lossy()).unwrap();
        let db = std::sync::Arc::new(tokio::sync::Mutex::new(conn));
        let row = crate::db::sync_inflight::find_shipping(&db, &pair.id, "uuid-alpha")
            .await
            .unwrap();
        assert!(row.is_none(), "row must close after the retry ack");
    }
}
