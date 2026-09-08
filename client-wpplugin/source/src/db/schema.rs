use anyhow::Result;
use rusqlite::Connection;

pub(crate) fn create_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "-- schema v1
        CREATE TABLE IF NOT EXISTS system_config (
            key        TEXT PRIMARY KEY NOT NULL,
            value      TEXT NOT NULL DEFAULT ''
        );

        CREATE TABLE IF NOT EXISTS task_queue_state (
            api_base_url    TEXT NOT NULL,
            task_id         INTEGER NOT NULL,
            task_json       TEXT NOT NULL DEFAULT '{}',
            checkpoint_json TEXT NOT NULL DEFAULT '{}',
            updated_at      INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (api_base_url, task_id)
        );
        CREATE INDEX IF NOT EXISTS idx_tqs_domain ON task_queue_state(api_base_url);

        CREATE TABLE IF NOT EXISTS pending_callbacks (
            api_base_url     TEXT NOT NULL,
            idempotency_key  TEXT PRIMARY KEY NOT NULL,
            payload_json     TEXT NOT NULL DEFAULT '{}',
            route_secret_enc TEXT,
            created_at       INTEGER NOT NULL DEFAULT 0,
            retry_count      INTEGER NOT NULL DEFAULT 0,
            last_retry_at    INTEGER NOT NULL DEFAULT 0,
            relation_id      INTEGER NOT NULL DEFAULT 0,
            object_id        INTEGER NOT NULL DEFAULT 0,
            object_type      TEXT NOT NULL DEFAULT ''
        );
        CREATE INDEX IF NOT EXISTS idx_pc_domain ON pending_callbacks(api_base_url);

        CREATE TABLE IF NOT EXISTS translation_records (
            id               INTEGER PRIMARY KEY AUTOINCREMENT,
            created_at       INTEGER NOT NULL DEFAULT 0,
            domain           TEXT NOT NULL DEFAULT '',
            relation_id      INTEGER,
            object_id        INTEGER,
            object_type      TEXT,
            business_line    TEXT,
            source_lang      TEXT NOT NULL DEFAULT '',
            target_lang      TEXT NOT NULL DEFAULT '',
            status           TEXT NOT NULL DEFAULT 'pending',
            execution_ms     INTEGER,
            worker_id        TEXT,
            idempotency_key  TEXT,
            callback_sent_at INTEGER,
            callback_retries INTEGER NOT NULL DEFAULT 0,
            fields_count     INTEGER NOT NULL DEFAULT 0,
            error_message    TEXT,
            component_ids_json TEXT NOT NULL DEFAULT '[]',
            media_mappings_count INTEGER NOT NULL DEFAULT 0,
            failed_fields_count INTEGER NOT NULL DEFAULT 0,
            primary_failure_reason TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_tr_domain_ts ON translation_records(domain, created_at);
        CREATE INDEX IF NOT EXISTS idx_tr_status ON translation_records(status, created_at);

        CREATE TABLE IF NOT EXISTS discovery_tasks (
            id           INTEGER PRIMARY KEY AUTOINCREMENT,
            domain       TEXT NOT NULL,
            relation_id  INTEGER NOT NULL,
            concurrency  INTEGER NOT NULL DEFAULT 20,
            batch_parallel INTEGER NOT NULL DEFAULT 1,
            per_page     INTEGER NOT NULL DEFAULT 20,
            retry_max    INTEGER NOT NULL DEFAULT 3,
            timeout_secs INTEGER NOT NULL DEFAULT 60,
            enabled      INTEGER NOT NULL DEFAULT 1,
            include_resync INTEGER NOT NULL DEFAULT 0,
            selected_component_id TEXT,
            effective_source_lang TEXT,
            effective_target_lang TEXT,
            editable_overrides_json TEXT,
            last_run_at  INTEGER NOT NULL DEFAULT 0,
            created_at   INTEGER NOT NULL DEFAULT 0,
            updated_at   INTEGER NOT NULL DEFAULT 0,
            UNIQUE(domain, relation_id)
        );

        CREATE TABLE IF NOT EXISTS translation_in_progress (
            domain      TEXT NOT NULL,
            relation_id INTEGER NOT NULL,
            object_type TEXT NOT NULL DEFAULT 'post_type',
            object_id   INTEGER NOT NULL,
            claimed_at  INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (domain, relation_id, object_type, object_id),
            UNIQUE(domain, relation_id, object_type, object_id)
        );

        CREATE TABLE IF NOT EXISTS translation_jobs (
            id             INTEGER PRIMARY KEY AUTOINCREMENT,
            domain         TEXT NOT NULL,
            relation_id    INTEGER NOT NULL,
            business_line  TEXT NOT NULL DEFAULT '',
            status         TEXT NOT NULL DEFAULT 'pending',
            total_items    INTEGER NOT NULL DEFAULT 0,
            done_items     INTEGER NOT NULL DEFAULT 0,
            failed_items   INTEGER NOT NULL DEFAULT 0,
            triggered_by   TEXT NOT NULL DEFAULT 'manual',
            started_at     INTEGER,
            completed_at   INTEGER,
            created_at     INTEGER NOT NULL DEFAULT 0,
            updated_at     INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_tj_domain ON translation_jobs(domain, created_at DESC);
        CREATE INDEX IF NOT EXISTS idx_tj_status ON translation_jobs(status, created_at DESC);

        CREATE TABLE IF NOT EXISTS translation_items (
            id                  INTEGER PRIMARY KEY AUTOINCREMENT,
            job_id              INTEGER NOT NULL REFERENCES translation_jobs(id),
            domain              TEXT NOT NULL,
            relation_id         INTEGER NOT NULL,
            business_line       TEXT NOT NULL DEFAULT '',
            object_type         TEXT NOT NULL DEFAULT 'post',
            wp_object_id        INTEGER NOT NULL,
            wp_object_subtype   TEXT NOT NULL DEFAULT '',
            task_type           TEXT NOT NULL DEFAULT 'text',
            source_lang         TEXT NOT NULL DEFAULT '',
            target_lang         TEXT NOT NULL DEFAULT '',
            component_id        TEXT NOT NULL DEFAULT '',
            component_ids_json  TEXT NOT NULL DEFAULT '[]',
            selected_component_id TEXT,
            effective_source_lang TEXT,
            effective_target_lang TEXT,
            editable_overrides_json TEXT,
            raw_path            TEXT NOT NULL DEFAULT '',
            translated_path     TEXT NOT NULL DEFAULT '',
            content_hash        TEXT NOT NULL DEFAULT '',
            status              TEXT NOT NULL DEFAULT 'pending',
            client_task_id      TEXT NOT NULL DEFAULT '',
            upload_id           TEXT,
            wp_attachment_id    INTEGER,
            sync_response_json  TEXT,
            error_message       TEXT,
            retry_count         INTEGER NOT NULL DEFAULT 0,
            max_retries         INTEGER NOT NULL DEFAULT 3,
            fetched_at          INTEGER,
            translated_at       INTEGER,
            synced_at           INTEGER,
            created_at          INTEGER NOT NULL DEFAULT 0,
            updated_at          INTEGER NOT NULL DEFAULT 0,
            UNIQUE(domain, relation_id, object_type, wp_object_id, task_type)
        );
        CREATE INDEX IF NOT EXISTS idx_ti_job ON translation_items(job_id, status);
        CREATE INDEX IF NOT EXISTS idx_ti_domain_obj ON translation_items(domain, relation_id, wp_object_id);
        CREATE INDEX IF NOT EXISTS idx_ti_status ON translation_items(status, created_at);

        CREATE TABLE IF NOT EXISTS object_sync_coordination (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            domain          TEXT NOT NULL,
            relation_id     INTEGER NOT NULL,
            wp_object_id    INTEGER NOT NULL,
            business_line   TEXT NOT NULL DEFAULT '',
            client_task_id  TEXT NOT NULL DEFAULT '',
            text_status     TEXT NOT NULL DEFAULT 'na',
            image_status    TEXT NOT NULL DEFAULT 'na',
            video_status    TEXT NOT NULL DEFAULT 'na',
            audio_status    TEXT NOT NULL DEFAULT 'na',
            document_status TEXT NOT NULL DEFAULT 'na',
            callback_at     INTEGER,
            UNIQUE(domain, relation_id, wp_object_id, business_line)
        );

        CREATE TABLE IF NOT EXISTS tasktype_concurrency (
            relation_id  INTEGER NOT NULL,
            task_type    TEXT NOT NULL,
            concurrency  INTEGER NOT NULL DEFAULT 20,
            PRIMARY KEY(relation_id, task_type)
        );

        CREATE TABLE IF NOT EXISTS retry_queue (
            id            INTEGER PRIMARY KEY AUTOINCREMENT,
            domain        TEXT NOT NULL,
            relation_id   INTEGER NOT NULL,
            object_id     INTEGER NOT NULL,
            object_type   TEXT NOT NULL DEFAULT 'post',
            business_line TEXT NOT NULL DEFAULT '',
            source_lang   TEXT NOT NULL DEFAULT '',
            target_lang   TEXT NOT NULL DEFAULT '',
            created_at    INTEGER NOT NULL DEFAULT 0,
            status        TEXT NOT NULL DEFAULT 'pending',
            UNIQUE(domain, relation_id, object_type, object_id)
        );",
    )?;

    // Idempotent schema upgrades for existing databases (columns added in later versions).
    // Using individual execute() calls with let _ = to ignore "duplicate column" errors.
    let _ = conn.execute(
        "ALTER TABLE pending_callbacks ADD COLUMN relation_id INTEGER NOT NULL DEFAULT 0",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pending_callbacks ADD COLUMN object_id INTEGER NOT NULL DEFAULT 0",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE pending_callbacks ADD COLUMN object_type TEXT NOT NULL DEFAULT ''",
        [],
    );
    let _ = conn.execute_batch(
        "UPDATE pending_callbacks
         SET object_type = CASE
            WHEN instr(payload_json, '\"object_type\":\"taxonomy\"') > 0 THEN 'taxonomy'
            WHEN instr(payload_json, '\"object_type\":\"term\"') > 0 THEN 'taxonomy'
            WHEN instr(payload_json, '\"object_type\":\"language_pack\"') > 0 THEN 'language_pack'
            ELSE 'post_type'
         END
         WHERE object_type = '';",
    );
    let _ = conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_pc_lookup ON pending_callbacks(api_base_url, relation_id, object_id);",
    );
    let _ = conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_pc_lookup_v2 ON pending_callbacks(api_base_url, relation_id, object_type, object_id);",
    );

    // Idempotent unique index for translation_in_progress (object_type-aware).
    // Drop the legacy index first to avoid reintroducing collisions across
    // post/taxonomy entries that share the same object_id.
    let _ = conn.execute_batch("DROP INDEX IF EXISTS uix_tip;");
    let _ = conn.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS uix_tip_v2 ON translation_in_progress(domain, relation_id, object_type, object_id);",
    );

    // Add include_resync column to discovery_tasks for existing databases.
    let _ = conn.execute(
        "ALTER TABLE translation_records ADD COLUMN component_ids_json TEXT NOT NULL DEFAULT '[]'",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE translation_records ADD COLUMN media_mappings_count INTEGER NOT NULL DEFAULT 0",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE translation_records ADD COLUMN failed_fields_count INTEGER NOT NULL DEFAULT 0",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE translation_records ADD COLUMN primary_failure_reason TEXT",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE discovery_tasks ADD COLUMN include_resync INTEGER NOT NULL DEFAULT 0",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE discovery_tasks ADD COLUMN selected_component_id TEXT",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE discovery_tasks ADD COLUMN effective_source_lang TEXT",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE discovery_tasks ADD COLUMN effective_target_lang TEXT",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE discovery_tasks ADD COLUMN editable_overrides_json TEXT",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE translation_items ADD COLUMN component_ids_json TEXT NOT NULL DEFAULT '[]'",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE translation_items ADD COLUMN selected_component_id TEXT",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE translation_items ADD COLUMN effective_source_lang TEXT",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE translation_items ADD COLUMN effective_target_lang TEXT",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE translation_items ADD COLUMN editable_overrides_json TEXT",
        [],
    );

    // Rebuild legacy tables that were created before object_type became part
    // of the key shape.
    migrate_translation_in_progress_schema(conn)?;
    migrate_retry_queue_schema(conn)?;

    Ok(())
}

fn table_sql(conn: &Connection, table_name: &str) -> Option<String> {
    conn.query_row(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table_name],
        |row| row.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
}

fn migrate_translation_in_progress_schema(conn: &Connection) -> Result<()> {
    let sql = table_sql(conn, "translation_in_progress")
        .unwrap_or_default()
        .to_lowercase();
    if sql.contains("object_type") {
        return Ok(());
    }

    conn.execute_batch(
        "ALTER TABLE translation_in_progress RENAME TO translation_in_progress_old;
         CREATE TABLE translation_in_progress (
             domain      TEXT NOT NULL,
             relation_id INTEGER NOT NULL,
             object_type TEXT NOT NULL DEFAULT 'post_type',
             object_id   INTEGER NOT NULL,
             claimed_at  INTEGER NOT NULL DEFAULT 0,
             PRIMARY KEY (domain, relation_id, object_type, object_id),
             UNIQUE(domain, relation_id, object_type, object_id)
         );
         INSERT INTO translation_in_progress (domain, relation_id, object_type, object_id, claimed_at)
         SELECT domain, relation_id, 'post_type', object_id, claimed_at
         FROM translation_in_progress_old;
         DROP TABLE translation_in_progress_old;",
    )?;

    Ok(())
}

fn migrate_retry_queue_schema(conn: &Connection) -> Result<()> {
    let sql = table_sql(conn, "retry_queue")
        .unwrap_or_default()
        .to_lowercase();
    if sql.contains("unique(domain, relation_id, object_type, object_id)") {
        return Ok(());
    }

    conn.execute_batch(
        "ALTER TABLE retry_queue RENAME TO retry_queue_old;
         CREATE TABLE retry_queue (
             id            INTEGER PRIMARY KEY AUTOINCREMENT,
             domain        TEXT NOT NULL,
             relation_id   INTEGER NOT NULL,
             object_id     INTEGER NOT NULL,
             object_type   TEXT NOT NULL DEFAULT 'post_type',
             business_line TEXT NOT NULL DEFAULT '',
             source_lang   TEXT NOT NULL DEFAULT '',
             target_lang   TEXT NOT NULL DEFAULT '',
             created_at    INTEGER NOT NULL DEFAULT 0,
             status        TEXT NOT NULL DEFAULT 'pending',
             UNIQUE(domain, relation_id, object_type, object_id)
         );
         INSERT OR REPLACE INTO retry_queue
             (domain, relation_id, object_id, object_type, business_line, source_lang, target_lang, created_at, status)
         SELECT
             domain,
             relation_id,
             object_id,
             CASE lower(coalesce(object_type, ''))
                 WHEN 'term' THEN 'taxonomy'
                 WHEN 'taxonomy' THEN 'taxonomy'
                 WHEN 'post_type' THEN 'post_type'
                 ELSE 'post_type'
             END AS object_type,
             business_line,
             source_lang,
             target_lang,
             created_at,
             status
         FROM retry_queue_old
         ORDER BY created_at ASC, id ASC;
         DROP TABLE retry_queue_old;",
    )?;

    Ok(())
}
