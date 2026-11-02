//! Embedded, forward-only migrations. Each runs in a transaction, after a `VACUUM INTO` backup of a database
//! that already has a schema.

use std::path::Path;

use rusqlite::Connection;

use super::{BACKUPS, Error, Result, io_error};

/// Applied in order; `PRAGMA user_version` counts those already applied.
pub(super) const MIGRATIONS: &[&str] = &[
    include_str!("migrations/0001_initial.sql"),
    include_str!("migrations/0002_jobs.sql"),
    include_str!("migrations/0003_selections.sql"),
    include_str!("migrations/0004_builds.sql"),
    include_str!("migrations/0005_folder_checks.sql"),
    include_str!("migrations/0006_kept_files.sql"),
    include_str!("migrations/0007_cleanup.sql"),
    include_str!("migrations/0008_incremental_audits.sql"),
    include_str!("migrations/0009_ingest_folders.sql"),
    include_str!("migrations/0010_source_repairs.sql"),
    // 0011 is Operator Additions (#19), developed alongside; its place in this list is settled when both merge.
    include_str!("migrations/0012_storage_policy.sql"),
];

pub(super) fn migrate(conn: &mut Connection, dir: &Path, migrations: &[&str]) -> Result<()> {
    let applied: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    let applied = usize::try_from(applied).unwrap_or(usize::MAX);
    if applied > migrations.len() {
        return Err(Error::Refused(format!(
            "{}: the store has schema version {applied}, newer than this Title Select's {}",
            dir.display(),
            migrations.len()
        )));
    }
    if applied == migrations.len() {
        return Ok(());
    }
    // A migration may rebuild a table others refer to, which SQLite allows only with foreign keys off; each checks
    // them before it commits instead. The pragma is a no-op inside a transaction, so it is set around them.
    conn.pragma_update(None, "foreign_keys", "OFF")?;
    let migrated = apply(conn, dir, migrations, applied);
    conn.pragma_update(None, "foreign_keys", "ON")?;
    migrated
}

fn apply(conn: &mut Connection, dir: &Path, migrations: &[&str], applied: usize) -> Result<()> {
    for (version, sql) in migrations.iter().enumerate().skip(applied) {
        // A store opened with no schema has nothing to protect.
        if applied > 0 {
            backup(conn, dir, version)?;
        }
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        let violations: i64 =
            tx.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })?;
        if violations > 0 {
            return Err(Error::Io(format!(
                "{}: migration {} would leave {violations} broken references",
                dir.display(),
                version + 1
            )));
        }
        tx.pragma_update(None, "user_version", version as i64 + 1)?;
        tx.commit()?;
    }
    Ok(())
}

/// Writes a copy of the database at schema `version` to `backups/`.
fn backup(conn: &Connection, dir: &Path, version: usize) -> Result<()> {
    let backups = dir.join(BACKUPS);
    std::fs::create_dir_all(&backups).map_err(|e| io_error(&backups, e))?;
    let stamp = jiff::Timestamp::now().strftime("%Y%m%dT%H%M%S%.fZ");
    let path = backups.join(format!("schema-{version}-{stamp}.sqlite"));
    conn.execute("VACUUM INTO ?1", [path.to_string_lossy()])?;
    Ok(())
}
