pub mod credentials;
pub mod discoverer;
pub(crate) mod field_plan;
pub mod hmac;
mod json_store;
mod media_delivery;
pub mod packet;
pub mod review;
pub mod shipper;
pub mod state;
pub mod storage;
pub mod types;

pub use credentials::{
    credential_status, derive_peer_shared_secret, find_peer_credential, load_peer_credentials,
    pair_with_site, remove_peer_credential, save_peer_credentials, PairingRole, PeerCredential,
    PeerCredentialsDoc, HKDF_INFO, PEER_CREDENTIALS_SCHEMA_VERSION,
};
pub use discoverer::{
    build_translator, delete_pair_everywhere, sync_pair_run, sync_run_is_total_failure,
    SyncRunReport, TranslatorHandle,
};
pub use hmac::{build_hmac_headers, compute_hmac_signature, compute_string_to_sign};
pub use packet::{
    compute_content_fingerprint, extract_image_urls, rewrite_content_urls, SyncPacket,
    PACKET_SCHEMA_VERSION,
};
pub use review::{
    count_pending_for_pair, find_review_item, find_review_item_mut, has_pending_uuid, list_pending,
    load_sync_review, save_sync_review, sync_review_file, update_sync_review, upsert_pending_item,
    SyncReviewDoc, SyncReviewItem, SyncReviewStatus, SYNC_REVIEW_SCHEMA_VERSION,
};
pub use shipper::{
    build_tombstone_packet, relay_packet_for_target, DigestItem, MediaChunkAck, RelayTranslation,
    ShipResult, Shipper, TransferMediaResult, MEDIA_CHUNK_BYTES,
};
pub use state::{
    load_sync_state, reconcile_batch, remove_pair_state, save_sync_state, update_pair_state,
    KnownEntity, PairSyncState, SyncStateDoc, SYNC_STATE_SCHEMA_VERSION,
};
pub use storage::{
    delete_pair_in_doc, find_pair_in_doc, find_pair_in_doc_mut, load_sync_pairs, save_sync_pairs,
    update_sync_pairs, upsert_pair_in_doc,
};
pub use types::{
    ConflictStrategy, SyncDirection, SyncFrequency, SyncMode, SyncPair, SyncPairStatus,
    SyncPairsDoc, SYNC_PAIRS_SCHEMA_VERSION,
};
