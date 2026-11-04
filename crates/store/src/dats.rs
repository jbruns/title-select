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

/// What orders a DAT's revisions by their header's version: its runs of digits, as numbers. It reads the versions
/// providers write the same way: No-Intro's `20240101-123456`, Redump's `2024-01-01 12-34-56`, MAME's
/// `0.265 (mame0265)` and its tags. A version with no digits orders by import alone.
fn version_key(version: &str) -> Vec<u128> {
    version
        .split(|c: char| !c.is_ascii_digit())
        .filter(|run| !run.is_empty())
        .map(|run| {
            let run = run.trim_start_matches('0');
            // A run too long for a u128 is past any real version; it orders as the largest.
            run.parse()
                .unwrap_or(if run.is_empty() { 0 } else { u128::MAX })
        })
        .collect()
}

/// Renumbers `header_order` within the DAT `dat_id` by [`version_key`], then by import where versions don't tell.
fn reorder_revisions(c: &Connection, dat_id: i64) -> Result<()> {
    let mut stmt =
        c.prepare("SELECT id, version FROM dat_revisions WHERE dat_id = ?1 ORDER BY id")?;
    let mut revisions = stmt
        .query_map([dat_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // Each revision, in import order, goes just before the first already placed whose version is higher; one whose
    // version has no digits, or that none is higher than, goes last, as the newest so far.
    let mut ordered: Vec<(i64, Vec<u128>)> = Vec::with_capacity(revisions.len());
    for (id, version) in revisions.drain(..) {
        let key = version_key(&version);
        let at = if key.is_empty() {
            None
        } else {
            ordered.iter().position(|(_, k)| !k.is_empty() && *k > key)
        };
        ordered.insert(at.unwrap_or(ordered.len()), (id, key));
    }
    for (order, (id, _)) in ordered.iter().enumerate() {
        c.execute(
            "UPDATE dat_revisions SET header_order = ?2 WHERE id = ?1",
            params![id, order as i64 + 1],
        )?;
    }
    Ok(())
}

/// Whether a revision at `version` of the DAT `<provider>/<name>`, not yet imported, would be its newest, as
/// [`reorder_revisions`] places it: true when the DAT has no revision yet.
pub(crate) fn would_be_newest(
    c: &Connection,
    provider: &str,
    name: &str,
    version: &str,
) -> Result<bool> {
    let key = version_key(version);
    if key.is_empty() {
        return Ok(true);
    }
    let mut stmt = c.prepare(
        "SELECT r.version FROM dats d JOIN dat_revisions r ON r.dat_id = d.id
         WHERE d.provider = ?1 AND d.name = ?2",
    )?;
    let versions = stmt
        .query_map(params![provider, name], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(!versions.iter().any(|v| {
        let k = version_key(v);
        !k.is_empty() && k > key
    }))
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
        self.import_parsed_dat(&dat, bytes, file, provider, source_url)
    }

    /// As [`Store::import_dat_bytes`], for `bytes` already parsed as `dat`.
    pub(crate) fn import_parsed_dat(
        &self,
        dat: &Dat,
        bytes: &[u8],
        file: &str,
        provider: &str,
        source_url: Option<&str>,
    ) -> Result<(DatRevisionRecord, ImportOutcome)> {
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
            reorder_revisions(&tx, dat_id)?;
            let record = revision_by_sha(&tx, &dat.sha256)?.expect("just inserted").1;
            crate::stale::note_followed_revisions(&tx)?;
            tx.commit()?;
            Ok((record, ImportOutcome::Added))
        })?;
        if outcome == ImportOutcome::Added {
            self.retain_blobs()?;
        }
        Ok((record, outcome))
    }

    /// Every DAT Revision, oldest first by header version within each DAT, the DATs in import order.
    pub fn dat_revisions(&self) -> Result<Vec<DatRevisionRecord>> {
        self.read(|c| {
            let sql = format!(
                "SELECT {REVISION_COLUMNS} FROM dat_revisions r JOIN dats d ON d.id = r.dat_id
                 ORDER BY d.id, r.header_order, r.id"
            );
            let mut stmt = c.prepare(&sql)?;
            let rows = stmt.query_map([], |r| revision_from_row(r).map(|(_, rev)| rev))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }

    /// Drops the blob of every DAT Revision that no retained or running Audit, no Queued or Running Job, and no
    /// Audit a DAT import from an Ingest Folder still owes refers to, that is not the newest revision of its DAT (ADR 0003), and that is not held as restored. The row
    /// stays. Cue packs are retained alike ([`Store::retain_cue_packs`]). Returns the SHA-256s whose DAT blobs were dropped.
    pub fn retain_blobs(&self) -> Result<Vec<String>> {
        self.with_writer(|conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            // An Audit row that is not pruned, including one still running, holds its DAT Revision; so does a
            // Selection that is not pruned.
            let shas = tx
                .prepare(
                    "SELECT r.sha256 FROM dat_revisions r
                     WHERE r.blob_dropped_at IS NULL AND r.restored_at IS NULL
                       AND r.id <> (SELECT n.id FROM dat_revisions n WHERE n.dat_id = r.dat_id
                                    ORDER BY n.header_order DESC LIMIT 1)
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
        .and_then(|dropped| {
            self.retain_cue_packs()?;
            self.retain_sbi_packs()?;
            Ok(dropped)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::version_key;

    #[test]
    fn versions_order_as_each_provider_writes_them() {
        let ordered = |older: &str, newer: &str| version_key(older) < version_key(newer);
        assert!(ordered("20231231-235959", "20240101-000000"), "No-Intro");
        assert!(
            ordered("2023-12-31 23-59-59", "2024-01-01 00-00-00"),
            "Redump"
        );
        assert!(
            ordered("0.99 (mame099)", "0.265 (mame0265)"),
            "MAME ListXML"
        );
        assert!(ordered("mame0099", "mame0265"), "a MAME tag");
        assert!(ordered("1.9", "1.10"));
        assert!(version_key("").is_empty());
        assert_eq!(version_key("v1.02"), version_key("1.2"));
    }
}
