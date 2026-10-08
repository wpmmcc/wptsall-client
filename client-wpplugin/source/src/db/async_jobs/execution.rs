//! One live runner owns a unit. Native locks prove liveness, not record age.
//! The retained encrypted claim fences every write in its SQLite transaction.
use super::{AsyncJobEnv, AsyncUnit};
use anyhow::{ensure, Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::ops::Deref;
#[cfg(test)]
use std::{collections::HashMap, sync::Arc};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedClaim {
    format: String,
    unit: AsyncUnit,
    binding: Option<String>,
    owner: String,
}

#[cfg(test)]
#[path = "execution/tests.rs"]
mod tests;

/// This is intentionally not Clone. Callers pass this authority to persistence
/// APIs, rather than reconstructing a writer from a unit or an owner string.
pub(crate) struct ProviderExecution {
    env: AsyncJobEnv,
    key: String,
    raw: String,
    _native: crate::db::unit_lock::UnitLease,
}

impl Deref for ProviderExecution {
    type Target = AsyncJobEnv;

    fn deref(&self) -> &Self::Target {
        &self.env
    }
}

fn unit_key(unit: &AsyncUnit) -> Result<String> {
    // A new source revision/runtime must not acquire a second poller for the
    // same physical async projection while its original runner is active.
    let mut projection_unit = unit.clone();
    projection_unit.source_snapshot = None;
    Ok(format!(
        "provider-execution-claim-v1:{}",
        crate::db::system::private_json_digest(&serde_json::to_value(projection_unit)?)?
    ))
}

impl ProviderExecution {
    /// Acquires before credential selection, prepare/upload, submit or poll.
    /// A prior valid record is orphaned only because this native lock is free.
    /// Never unlink the lock on release, and never take over by timestamp.
    pub(crate) async fn acquire(env: AsyncJobEnv) -> Result<Self> {
        let key = unit_key(&AsyncUnit::from_env(&env))?;
        let mut connection = env.db.lock().await;
        let native = crate::db::unit_lock::UnitLease::acquire(
            &connection,
            &env.db,
            &format!("{}.poller", key.rsplit(':').next().unwrap()),
            "PROVIDER_UNIT_ALREADY_RUNNING: this unit has an active runner",
        )?;
        let tx = connection.savepoint()?;
        let prior = crate::db::system::get_system_config_checked(&tx, &key)?;
        if let Some(raw) = &prior {
            ensure!(
                raw.starts_with("V1BUQw"),
                "provider execution claim is not encrypted; retained"
            );
            let saved: SavedClaim =
                serde_json::from_str(&crate::db::system::decrypt_config_value(raw)?)
                    .context("damaged provider execution claim; retained")?;
            ensure!(
                saved.format == "provider-execution-claim-v1"
                    && unit_key(&saved.unit)? == key
                    && uuid::Uuid::parse_str(&saved.owner)
                        .is_ok_and(|owner| !owner.is_nil() && owner.to_string() == saved.owner),
                "provider execution claim scope mismatch; retained"
            );
        }
        let claim = SavedClaim {
            format: "provider-execution-claim-v1".into(),
            unit: AsyncUnit::from_env(&env),
            binding: env.resume_binding.clone(),
            owner: uuid::Uuid::new_v4().to_string(),
        };
        let raw = crate::db::system::encrypt_config_value(&serde_json::to_string(&claim)?)?;
        let credit = super::ledger::continuation_credit(&tx, &env)?;
        crate::storage_capacity::with_database_credit(credit, || {
            let affected = match &prior {
                Some(prior) => tx.execute(
                    "UPDATE system_config SET value=?1 WHERE key=?2 AND value=?3",
                    params![raw, key, prior],
                )?,
                None => tx.execute(
                    "INSERT INTO system_config (key,value) VALUES (?1,?2)",
                    params![key, raw],
                )?,
            };
            ensure!(
                affected == 1,
                "provider execution claim was not committed; retained"
            );
            tx.commit()?;
            Ok::<(), anyhow::Error>(())
        })?;
        drop(connection);
        Ok(Self {
            env,
            key,
            raw,
            _native: native,
        })
    }

    /// Must run inside the same transaction as the mutation. A missing,
    /// changed, damaged or superseded owner cannot touch any projection/result.
    pub(super) fn assert_owner(&self, conn: &Connection) -> Result<()> {
        ensure!(
            crate::db::system::get_system_config_checked(conn, &self.key)?.as_deref()
                == Some(self.raw.as_str()),
            "provider execution owner changed; original state retained"
        );
        Ok(())
    }
}
