//! DAT import and DAT blob retention.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};

use super::blobs::{remove_blob, write_blob};
use super::{DATS, Error, Result, Store, io_error, now};
use title_select_engine::dat::Dat;

pub use title_select_api::{DatRevisionRecord, ImportOutcome};

pub(super) const REVISION_COLUMNS: &str = "r.id, r.sha256, d.provider, d.name, r.version, r.file_name, \
     r.imported_at, r.source_url, r.blob_dropped_at IS NULL";

pub(super) fn revision_from_row(row: &Row) -> rusqlite::Result<(i64, DatRevisionRecord)> {
    Ok((
        row.get(0)?,
        DatRevisionRecord {
            sha256: row.get(1)?,
            provider: row.get(2)?,
            name: row.get(3)?,
            version: row.get(4)?,
            file: row.get(5)?,
            imported_at: row.get(6)?,
            source_url: row.get(7)?,
            blob_present: row.get(8)?,
        },
    ))
}

pub(super) fn revision_by_id(c: &Connection, id: i64) -> Result<(i64, DatRevisionRecord)> {
    let sql = format!(
        "SELECT {REVISION_COLUMNS} FROM dat_revisions r JOIN dats d ON d.id = r.dat_id WHERE r.id = ?1"
    );
    c.query_row(&sql, [id], revision_from_row)
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("no DAT revision {id}")))
}

fn revision_by_sha(c: &Connection, sha256: &str) -> Result<Option<(i64, DatRevisionRecord)>> {
    let sql = format!(
        "SELECT {REVISION_COLUMNS} FROM dat_revisions r JOIN dats d ON d.id = r.dat_id WHERE r.sha256 = ?1"
    );
    Ok(c.query_row(&sql, [sha256], revision_from_row).optional()?)
}

/// Splits `<provider>/<name>`. A provider never contains `/`; a DAT's name may.
pub(super) fn split_dat(dat: &str) -> Result<(&str, &str)> {
    dat.split_once('/')
        .filter(|(p, n)| !p.is_empty() && !n.is_empty())
        .ok_or_else(|| Error::Refused(format!("{dat}: a DAT is named as <provider>/<name>")))
}

impl Store {
    /// Imports the DAT file at `path` as a DAT Revision of the DAT named by `provider` and its header name.
    /// Importing bytes already imported returns the existing DAT Revision, restoring its blob if it was dropped.
    pub fn import_dat(
        &self,
        path: &Path,
        provider: &str,
        source_url: Option<&str>,
    ) -> Result<(DatRevisionRecord, ImportOutcome)> {
        if provider.is_empty() || provider.contains('/') {
            return Err(Error::Refused(format!(
                "{provider:?}: a provider is a non-empty name without `/`"
            )));
        }
        let bytes = std::fs::read(path).map_err(|e| io_error(path, e))?;
        let file = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.import_dat_bytes(&bytes, &file, provider, source_url)
    }

    /// Imports the unmodified bytes of a DAT file whose original filename is `file`, like [`Store::import_dat`].
    pub fn import_dat_bytes(
        &self,
        bytes: &[u8],
        file: &str,
        provider: &str,
        source_url: Option<&str>,
    ) -> Result<(DatRevisionRecord, ImportOutcome)> {
        if provider.is_empty() || provider.contains('/') {
            return Err(Error::Refused(format!(
                "{provider:?}: a provider is a non-empty name without `/`"
            )));
        }
        let dat = Dat::parse(bytes, Path::new(file))?;
        if dat.name.is_empty() {
            return Err(Error::Refused(format!(
                "{file}: the DAT header has no name"
            )));
        }
        let dats = self.dir.join(DATS);
        // Holding the writer keeps the blob and its row consistent against retention and garbage collection.
        let (record, outcome) = self.with_writer(|conn| {
            if let Some((id, existing)) = revision_by_sha(conn, &dat.sha256)? {
                let blob = self.blob_path(&existing.sha256);
                if existing.blob_present && blob.is_file() {
                    return Ok((existing, ImportOutcome::AlreadyImported));
                }
                write_blob(&dats, &dat.sha256, bytes)?;
                conn.execute(
                    "UPDATE dat_revisions SET blob_dropped_at = NULL, restored_at = ?2 WHERE id = ?1",
                    params![id, now()],
                )?;
                let restored = DatRevisionRecord {
                    blob_present: true,
                    ..existing
                };
                return Ok((restored, ImportOutcome::Restored));
            }
            write_blob(&dats, &dat.sha256, bytes)?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute(
                "INSERT INTO dats (provider, name) VALUES (?1, ?2) ON CONFLICT DO NOTHING",
                params![provider, dat.name],
            )?;
            let dat_id: i64 = tx.query_row(
                "SELECT id FROM dats WHERE provider = ?1 AND name = ?2",
                params![provider, dat.name],
                |r| r.get(0),
            )?;
            // A newer revision releases any restored revision of the same DAT.
            tx.execute(
                "UPDATE dat_revisions SET restored_at = NULL WHERE dat_id = ?1",
                [dat_id],
            )?;
            tx.execute(
                "INSERT INTO dat_revisions (dat_id, sha256, version, file_name, imported_at, source_url)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![dat_id, dat.sha256, dat.version, file, now(), source_url],
            )?;
            let record = revision_by_sha(&tx, &dat.sha256)?.expect("just inserted").1;
            tx.commit()?;
            Ok((record, ImportOutcome::Added))
        })?;
        if outcome == ImportOutcome::Added {
            self.retain_blobs()?;
        }
        Ok((record, outcome))
    }

    /// Every DAT Revision, oldest import first.
    pub fn dat_revisions(&self) -> Result<Vec<DatRevisionRecord>> {
        self.read(|c| {
            let sql = format!(
                "SELECT {REVISION_COLUMNS} FROM dat_revisions r JOIN dats d ON d.id = r.dat_id ORDER BY r.id"
            );
            let mut stmt = c.prepare(&sql)?;
            let rows = stmt.query_map([], |r| revision_from_row(r).map(|(_, rev)| rev))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }

    /// Drops the blob of every DAT Revision that no retained or running Audit, no Queued or Running Job, and no
    /// Audit a DAT import from an Ingest Folder still owes refers to, that is not the newest imported revision of its DAT (ADR 0003), and that is not held as restored. The row
    /// stays. Returns the SHA-256s whose blobs were dropped.
    pub fn retain_blobs(&self) -> Result<Vec<String>> {
        self.with_writer(|conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            // An Audit row that is not pruned, including one still running, holds its DAT Revision; so does a
            // Selection that is not pruned.
            let shas = tx
                .prepare(
                    "SELECT r.sha256 FROM dat_revisions r
                     WHERE r.blob_dropped_at IS NULL AND r.restored_at IS NULL
                       AND r.id <> (SELECT max(n.id) FROM dat_revisions n WHERE n.dat_id = r.dat_id)
                       AND NOT EXISTS (
                           SELECT 1 FROM audits a WHERE a.dat_revision_id = r.id AND a.pruned_at IS NULL)
                       AND NOT EXISTS (
                           SELECT 1 FROM jobs j WHERE j.dat_revision_id = r.id AND j.state IN ('queued', 'running'))
                       AND NOT EXISTS (
                           SELECT 1 FROM selections s WHERE s.dat_revision_id = r.id AND s.pruned_at IS NULL)
                       AND NOT EXISTS (
                           SELECT 1 FROM ingest_reaudits o WHERE o.sha256 = r.sha256 AND o.fulfilled_at IS NULL)
                     ORDER BY r.id",
                )?
                .query_map([], |r| r.get(0))?
                .collect::<Result<Vec<String>, _>>()?;
            let at = now();
            for sha in &shas {
                tx.execute(
                    "UPDATE dat_revisions SET blob_dropped_at = ?1 WHERE sha256 = ?2",
                    params![at, sha],
                )?;
            }
            tx.commit()?;
            // The rows no longer hold these blobs, so a crash before this point leaves only garbage. The
            // writer is still held, so no restore can clear a marker in between; check it anyway before deleting.
            let mut dropped = Vec::new();
            for sha in shas {
                let still_dropped: bool = conn.query_row(
                    "SELECT blob_dropped_at IS NOT NULL FROM dat_revisions WHERE sha256 = ?1",
                    [&sha],
                    |r| r.get(0),
                )?;
                if still_dropped {
                    remove_blob(&self.blob_path(&sha))?;
                    dropped.push(sha);
                }
            }
            Ok(dropped)
        })
    }
}
