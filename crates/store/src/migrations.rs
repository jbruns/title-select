//! Embedded, forward-only migrations. Each runs in a transaction, after a `VACUUM INTO` backup of a database
//! that already has a schema.

use std::path::Path;

use rusqlite::Connection;

use super::{BACKUPS, DATS, Error, Result, io_error};

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
    include_str!("migrations/0011_storage_policy.sql"),
    include_str!("migrations/0012_additions.sql"),
    include_str!("migrations/0013_library_folders.sql"),
    include_str!("migrations/0014_audit_content.sql"),
    include_str!("migrations/0015_item_moves.sql"),
    include_str!("migrations/0016_stale_source_sets.sql"),
    include_str!("migrations/0017_lifecycle.sql"),
    include_str!("migrations/0018_disc_sets.sql"),
    include_str!("migrations/0019_fetches.sql"),
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
        after(&tx, dir, version)?;
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

/// The migration, counted from 0, that orders each DAT's revisions by their headers.
const HEADER_ORDER: usize = 18;
/// How much of a DAT file is read for its header's date.
const HEADER_BYTES: u64 = 1 << 20;

/// What a migration needs that SQL can't express, in its transaction. [`HEADER_ORDER`] reads the header date of every
/// DAT Revision whose file is kept, then numbers each DAT's revisions as an import does.
fn after(tx: &rusqlite::Transaction, dir: &Path, version: usize) -> Result<()> {
    use std::io::Read;
    if version != HEADER_ORDER {
        return Ok(());
    }
    let kept = tx
        .prepare("SELECT id, sha256 FROM dat_revisions WHERE blob_dropped_at IS NULL")?
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, sha) in kept {
        let path = dir.join(DATS).join(&sha);
        let Ok(file) = std::fs::File::open(&path) else {
            continue;
        };
        let mut head = Vec::new();
        file.take(HEADER_BYTES)
            .read_to_end(&mut head)
            .map_err(|e| io_error(&path, e))?;
        let date = title_select_engine::dat::Dat::header_date(&head);
        tx.execute(
            "UPDATE dat_revisions SET header_date = ?2 WHERE id = ?1",
            rusqlite::params![id, date],
        )?;
    }
    let dats = tx
        .prepare("SELECT id FROM dats")?
        .query_map([], |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for dat in dats {
        crate::dats::reorder_revisions(tx, dat)?;
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
