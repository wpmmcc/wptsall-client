use super::*;

fn make_db() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::db::schema::create_tables(&conn).unwrap();
    conn
}

const DOMAIN: &str = "https://example.com";
const RELATION: i64 = 1;
const OBJ_ID: i64 = 42;
const BIZ: &str = "post_content";
const TASK_ID: &str = "task-abc-123";

// ------------------------------------------------------------------
// ensure_coordination
// ------------------------------------------------------------------

#[test]
fn test_ensure_coordination_creates_row_with_correct_statuses() {
    let conn = make_db();

    // Only text and image are applicable for this object.
    ensure_coordination(
        &conn,
        DOMAIN,
        RELATION,
        OBJ_ID,
        BIZ,
        TASK_ID,
        &["text", "image"],
    )
    .unwrap();

    let (text, image, video, audio, document): (String, String, String, String, String) = conn
        .query_row(
            "SELECT text_status, image_status, video_status, audio_status, document_status
                 FROM object_sync_coordination
                 WHERE domain = ?1 AND relation_id = ?2
                   AND wp_object_id = ?3 AND business_line = ?4",
            params![DOMAIN, RELATION, OBJ_ID, BIZ],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();

    assert_eq!(text, "pending");
    assert_eq!(image, "pending");
    assert_eq!(video, "na");
    assert_eq!(audio, "na");
    assert_eq!(document, "na");
}

#[test]
fn test_ensure_coordination_only_text() {
    let conn = make_db();

    ensure_coordination(&conn, DOMAIN, RELATION, OBJ_ID, BIZ, TASK_ID, &["text"]).unwrap();

    let (text, image, video, audio, document): (String, String, String, String, String) = conn
        .query_row(
            "SELECT text_status, image_status, video_status, audio_status, document_status
                 FROM object_sync_coordination
                 WHERE domain = ?1 AND relation_id = ?2
                   AND wp_object_id = ?3 AND business_line = ?4",
            params![DOMAIN, RELATION, OBJ_ID, BIZ],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();

    assert_eq!(text, "pending");
    assert_eq!(image, "na");
    assert_eq!(video, "na");
    assert_eq!(audio, "na");
    assert_eq!(document, "na");
}

#[test]
fn test_ensure_coordination_is_idempotent() {
    let conn = make_db();

    // Call twice — second call must not reset anything.
    ensure_coordination(
        &conn,
        DOMAIN,
        RELATION,
        OBJ_ID,
        BIZ,
        TASK_ID,
        &["text", "image"],
    )
    .unwrap();
    ensure_coordination(
        &conn,
        DOMAIN,
        RELATION,
        OBJ_ID,
        BIZ,
        TASK_ID,
        &["text", "image"],
    )
    .unwrap();

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM object_sync_coordination
                 WHERE domain = ?1 AND relation_id = ?2
                   AND wp_object_id = ?3 AND business_line = ?4",
            params![DOMAIN, RELATION, OBJ_ID, BIZ],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn test_ensure_coordination_does_not_downgrade_status() {
    let conn = make_db();

    ensure_coordination(&conn, DOMAIN, RELATION, OBJ_ID, BIZ, TASK_ID, &["text"]).unwrap();
    mark_task_type_done(&conn, DOMAIN, RELATION, OBJ_ID, BIZ, "text").unwrap();

    // Call ensure again — should not revert 'done' back to 'pending'.
    ensure_coordination(&conn, DOMAIN, RELATION, OBJ_ID, BIZ, TASK_ID, &["text"]).unwrap();

    let text: String = conn
        .query_row(
            "SELECT text_status FROM object_sync_coordination
                 WHERE domain = ?1 AND relation_id = ?2
                   AND wp_object_id = ?3 AND business_line = ?4",
            params![DOMAIN, RELATION, OBJ_ID, BIZ],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(text, "done");
}

// ------------------------------------------------------------------
// mark_task_type_done + is_all_done
// ------------------------------------------------------------------

#[test]
fn test_mark_task_type_done_and_is_all_done_single() {
    let conn = make_db();

    ensure_coordination(&conn, DOMAIN, RELATION, OBJ_ID, BIZ, TASK_ID, &["text"]).unwrap();

    // Not all done yet.
    assert!(!is_all_done(&conn, DOMAIN, RELATION, OBJ_ID, BIZ));

    mark_task_type_done(&conn, DOMAIN, RELATION, OBJ_ID, BIZ, "text").unwrap();

    assert!(is_all_done(&conn, DOMAIN, RELATION, OBJ_ID, BIZ));
}

#[test]
fn test_mark_task_type_done_and_is_all_done_multiple() {
    let conn = make_db();

    ensure_coordination(
        &conn,
        DOMAIN,
        RELATION,
        OBJ_ID,
        BIZ,
        TASK_ID,
        &["text", "image", "video"],
    )
    .unwrap();

    assert!(!is_all_done(&conn, DOMAIN, RELATION, OBJ_ID, BIZ));

    mark_task_type_done(&conn, DOMAIN, RELATION, OBJ_ID, BIZ, "text").unwrap();
    assert!(!is_all_done(&conn, DOMAIN, RELATION, OBJ_ID, BIZ));

    mark_task_type_done(&conn, DOMAIN, RELATION, OBJ_ID, BIZ, "image").unwrap();
    assert!(!is_all_done(&conn, DOMAIN, RELATION, OBJ_ID, BIZ));

    mark_task_type_done(&conn, DOMAIN, RELATION, OBJ_ID, BIZ, "video").unwrap();
    assert!(is_all_done(&conn, DOMAIN, RELATION, OBJ_ID, BIZ));
}

#[test]
fn test_is_all_done_returns_false_for_missing_row() {
    let conn = make_db();
    assert!(!is_all_done(&conn, DOMAIN, RELATION, OBJ_ID, BIZ));
}

#[test]
fn test_is_all_done_returns_false_when_all_na() {
    let conn = make_db();

    // Insert with no task types — all statuses remain 'na'.
    ensure_coordination(&conn, DOMAIN, RELATION, OBJ_ID, BIZ, TASK_ID, &[]).unwrap();

    // An all-'na' row must not be treated as "done".
    assert!(!is_all_done(&conn, DOMAIN, RELATION, OBJ_ID, BIZ));
}

// ------------------------------------------------------------------
// mark_callback_sent
// ------------------------------------------------------------------

#[test]
fn test_mark_callback_sent_sets_callback_at() {
    let conn = make_db();

    ensure_coordination(&conn, DOMAIN, RELATION, OBJ_ID, BIZ, TASK_ID, &["text"]).unwrap();

    let before = now_secs();
    mark_callback_sent(&conn, DOMAIN, RELATION, OBJ_ID, BIZ).unwrap();
    let after = now_secs();

    let callback_at: i64 = conn
        .query_row(
            "SELECT callback_at FROM object_sync_coordination
                 WHERE domain = ?1 AND relation_id = ?2
                   AND wp_object_id = ?3 AND business_line = ?4",
            params![DOMAIN, RELATION, OBJ_ID, BIZ],
            |row| row.get(0),
        )
        .unwrap();

    assert!(
        callback_at >= before && callback_at <= after,
        "callback_at={callback_at} should be in [{before}, {after}]"
    );
}

#[test]
fn test_mark_callback_sent_is_idempotent() {
    let conn = make_db();

    ensure_coordination(&conn, DOMAIN, RELATION, OBJ_ID, BIZ, TASK_ID, &["text"]).unwrap();

    mark_callback_sent(&conn, DOMAIN, RELATION, OBJ_ID, BIZ).unwrap();
    let first_ts: i64 = conn
        .query_row(
            "SELECT callback_at FROM object_sync_coordination
                 WHERE domain = ?1 AND relation_id = ?2
                   AND wp_object_id = ?3 AND business_line = ?4",
            params![DOMAIN, RELATION, OBJ_ID, BIZ],
            |row| row.get(0),
        )
        .unwrap();

    // Calling again should update callback_at to the same or a later value.
    mark_callback_sent(&conn, DOMAIN, RELATION, OBJ_ID, BIZ).unwrap();
    let second_ts: i64 = conn
        .query_row(
            "SELECT callback_at FROM object_sync_coordination
                 WHERE domain = ?1 AND relation_id = ?2
                   AND wp_object_id = ?3 AND business_line = ?4",
            params![DOMAIN, RELATION, OBJ_ID, BIZ],
            |row| row.get(0),
        )
        .unwrap();

    assert!(second_ts >= first_ts);
}

// ------------------------------------------------------------------
// get_client_task_id
// ------------------------------------------------------------------

#[test]
fn test_get_client_task_id_returns_stored_value() {
    let conn = make_db();

    ensure_coordination(&conn, DOMAIN, RELATION, OBJ_ID, BIZ, TASK_ID, &["text"]).unwrap();

    let result = get_client_task_id(&conn, DOMAIN, RELATION, OBJ_ID, BIZ);
    assert_eq!(result, Some(TASK_ID.to_string()));
}

#[test]
fn test_get_client_task_id_returns_none_for_missing_row() {
    let conn = make_db();

    let result = get_client_task_id(&conn, DOMAIN, RELATION, OBJ_ID, BIZ);
    assert_eq!(result, None);
}

#[test]
fn test_multiple_objects_are_independent() {
    let conn = make_db();

    ensure_coordination(&conn, DOMAIN, RELATION, 10, BIZ, "task-10", &["text"]).unwrap();
    ensure_coordination(&conn, DOMAIN, RELATION, 20, BIZ, "task-20", &["image"]).unwrap();

    mark_task_type_done(&conn, DOMAIN, RELATION, 10, BIZ, "text").unwrap();

    assert!(is_all_done(&conn, DOMAIN, RELATION, 10, BIZ));
    assert!(!is_all_done(&conn, DOMAIN, RELATION, 20, BIZ));

    assert_eq!(
        get_client_task_id(&conn, DOMAIN, RELATION, 10, BIZ),
        Some("task-10".to_string())
    );
    assert_eq!(
        get_client_task_id(&conn, DOMAIN, RELATION, 20, BIZ),
        Some("task-20".to_string())
    );
}
