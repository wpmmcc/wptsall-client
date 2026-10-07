//! Retained capacity is reserved before a new external operation. Completion
//! does not release it; limits never authorize deletion of paid evidence.
use anyhow::{ensure, Context, Result};
use rusqlite::{params, Connection};
use serde::Serialize;

pub(crate) const DEFAULT_MAX_RETAINED_UNITS: u64 = 10_000;
pub(crate) const DEFAULT_MAX_RESERVED_BYTES: u64 = 32 * 1024 * 1024 * 1024;
pub(crate) const TEXT_RESERVATION_BYTES: u64 = 256 * 1024;
pub(crate) const MEDIA_RESERVATION_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub(crate) const MAX_CONFIG_LIMIT: u64 = 9_007_199_254_740_991;

#[derive(Debug, Serialize)]
pub(crate) struct CapacityInventory {
    pub(crate) retained_units: u64,
    pub(crate) reserved_bytes: u64,
    pub(crate) legacy_units: u64,
    pub(crate) max_retained_units: u64,
    pub(crate) max_reserved_bytes: u64,
    #[serde(flatten)]
    pub(crate) physical: crate::storage_capacity::PhysicalInventory,
}

pub(crate) fn limit(conn: &Connection, key: &str, default: u64) -> Result<u64> {
    let Some(raw) = super::system::get_system_config_checked(conn, key)? else {
        return Ok(default);
    };
    let value: u64 = raw
        .parse()
        .context("STORAGE_CAPACITY_INVALID: damaged retained-capacity limit")?;
    ensure!(
        (1..=MAX_CONFIG_LIMIT).contains(&value),
        "STORAGE_CAPACITY_INVALID: capacity limits must be positive safe integers"
    );
    Ok(value)
}

pub(crate) fn inventory(conn: &Connection) -> Result<CapacityInventory> {
    let (units, bytes): (i64, i64) = conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(reserved_bytes),0) FROM retained_capacity",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let legacy: i64 = conn.query_row(
        "SELECT COUNT(*) FROM system_config s
         WHERE s.key LIKE 'provider-operation-v1:%'
           AND NOT EXISTS (SELECT 1 FROM retained_capacity c WHERE c.unit_key=s.key)",
        [],
        |row| row.get(0),
    )?;
    let legacy_units = u64::try_from(legacy)?;
    // Old operations have no reservation. Count them conservatively without
    // changing or deleting their original authority.
    let reserved_bytes = u64::try_from(bytes)?
        .checked_add(
            legacy_units
                .checked_mul(MEDIA_RESERVATION_BYTES)
                .context("STORAGE_CAPACITY_INVALID: legacy capacity overflow")?,
        )
        .context("STORAGE_CAPACITY_INVALID: retained capacity overflow")?;
    Ok(CapacityInventory {
        retained_units: u64::try_from(units)?
            .checked_add(legacy_units)
            .context("STORAGE_CAPACITY_INVALID: retained unit count overflow")?,
        reserved_bytes,
        legacy_units,
        max_retained_units: limit(
            conn,
            "storage_max_retained_units",
            DEFAULT_MAX_RETAINED_UNITS,
        )?,
        max_reserved_bytes: limit(
            conn,
            "storage_max_reserved_bytes",
            DEFAULT_MAX_RESERVED_BYTES,
        )?,
        physical: crate::storage_capacity::inventory()?,
    })
}

/// The caller must reserve and create the operation in the SAME transaction.
/// A busy snapshot, ignored insert or damaged read refuses new admission.
pub(crate) fn reserve_new(conn: &Connection, unit_key: &str, bytes: u64) -> Result<()> {
    ensure!(
        !conn.is_autocommit(),
        "retained capacity requires the operation's authority transaction"
    );
    ensure!(
        bytes > 0 && bytes <= MAX_CONFIG_LIMIT,
        "invalid capacity reservation"
    );
    let pending: i64 = conn.query_row(
        "SELECT COUNT(*) FROM system_config WHERE key LIKE 'integration-save-v1:%'",
        [],
        |row| row.get(0),
    )?;
    ensure!(
        pending == 0,
        "INTEGRATION_CONFIG_PENDING: credential/proxy projection is unresolved; new work stopped"
    );
    let current = inventory(conn)?;
    ensure!(
        current.retained_units < current.max_retained_units
            && current
                .reserved_bytes
                .checked_add(bytes)
                .is_some_and(|total| total <= current.max_reserved_bytes),
        "STORAGE_CAPACITY_EXHAUSTED: retained storage is full; new work stopped, existing evidence retained"
    );
    let savepoint = format!("retained_capacity_{}", uuid::Uuid::new_v4().simple());
    conn.execute_batch(&format!("SAVEPOINT {savepoint}"))?;
    let result = (|| {
        record(conn, unit_key, bytes)?;
        crate::storage_capacity::reserve(conn, unit_key, bytes)
    })();
    if result.is_err() {
        conn.execute_batch(&format!("ROLLBACK TO {savepoint}; RELEASE {savepoint}"))?;
    } else {
        conn.execute_batch(&format!("RELEASE {savepoint}"))?;
    }
    result
}

/// Only use after validating an existing paid projection in this transaction.
/// Recovery records its already-consumed capacity even above a new limit.
pub(crate) fn record_existing(conn: &Connection, unit_key: &str, bytes: u64) -> Result<()> {
    ensure!(
        !conn.is_autocommit() && bytes > 0 && bytes <= MAX_CONFIG_LIMIT,
        "existing capacity requires a valid authority transaction"
    );
    record(conn, unit_key, bytes)
}

fn record(conn: &Connection, unit_key: &str, bytes: u64) -> Result<()> {
    let changed = conn.execute(
        "INSERT INTO retained_capacity(unit_key,reserved_bytes,created_at)
         VALUES (?1,?2,?3)",
        params![
            unit_key,
            i64::try_from(bytes)?,
            crate::logging::unix_ts() as i64
        ],
    )?;
    ensure!(
        changed == 1
            && conn.query_row(
                "SELECT reserved_bytes FROM retained_capacity WHERE unit_key=?1",
                [unit_key],
                |row| row.get::<_, i64>(0),
            )? == i64::try_from(bytes)?,
        "STORAGE_CAPACITY_NOT_COMMITTED: new work refused"
    );
    Ok(())
}
