//! Redump cue packs: ZIPs of `.cue` sheets for one DAT, kept unmodified under `cue_packs/<sha256>` in the store with
//! where they came from. A repair fills or replaces a `.cue` ROM from one, matched by the DAT's checksums. Each sheet is
//! hashed once, when its pack is imported, and indexed, so planning reads no pack; a repair reads only the sheet it
//! writes. A pack no repair plan refers to has its file deleted, as an unheld DAT blob is, once it is no longer the
//! latest imported for its DAT or, if it was fetched, the one the latest fetch of its system brought.

use std::fs::File;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use sha2::{Digest, Sha256};

use super::blobs::{remove_blob, write_blob};
use super::dats::split_dat;
use super::fetch::{NOT_HELD, hold_pack};
use super::repairs::planning::CueSheet;
use super::{Error, Result, Store, io_error, now};

use title_select_api::{CuePackOrigin, CuePackRecord, ImportOutcome};
use title_select_engine::build::CueLimits;
use title_select_engine::payload::Checksums;

/// The directory, in the store directory, that holds cue packs.
pub const CUE_PACKS: &str = "cue_packs";
/// The largest cue pack imported, in bytes.
pub const CUE_PACK_LIMIT: u64 = 256 << 20;
/// The bounds a cue pack is read within: a sheet of at most 1 MiB, at most 64 MiB of sheets together, and at most
/// 32768 members, well past any Redump cue pack.
pub const CUE_LIMITS: CueLimits = CueLimits {
    sheet: 1 << 20,
    total: 64 << 20,
    entries: 32768,
};

const COLUMNS: &str =
    "sha256, dat, file_name, source_url, origin, cues, imported_at, blob_dropped_at IS NULL";

fn record_from_row(row: &Row) -> rusqlite::Result<CuePackRecord> {
    let origin: String = row.get(4)?;
    Ok(CuePackRecord {
        sha256: row.get(0)?,
        dat: row.get(1)?,
        file: row.get(2)?,
        source_url: row.get(3)?,
        origin: match origin.as_str() {
            "fetch" => CuePackOrigin::Fetch,
            _ => CuePackOrigin::Import,
        },
        cues: row.get::<_, i64>(5)? as u64,
        imported_at: row.get(6)?,
        blob_present: row.get(7)?,
    })
}

fn pack_by_sha(c: &Connection, sha256: &str) -> Result<Option<CuePackRecord>> {
    let sql = format!("SELECT {COLUMNS} FROM cue_packs WHERE sha256 = ?1");
    Ok(c.query_row(&sql, [sha256], record_from_row).optional()?)
}

impl Store {
    /// Imports the cue pack at `path` for `dat`, named `<provider>/<name>`, as [`Store::import_cue_pack_bytes`].
    pub fn import_cue_pack(
        &self,
        path: &Path,
        dat: &str,
        source_url: Option<&str>,
    ) -> Result<(CuePackRecord, ImportOutcome)> {
        let size = std::fs::metadata(path)
            .map_err(|e| io_error(path, e))?
            .len();
        if size > CUE_PACK_LIMIT {
            return Err(Error::Refused(format!(
                "{}: a cue pack is at most {CUE_PACK_LIMIT} bytes",
                path.display()
            )));
        }
        let bytes = std::fs::read(path).map_err(|e| io_error(path, e))?;
        let file = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.import_cue_pack_bytes(&bytes, &file, dat, source_url, CuePackOrigin::Import)
    }

    /// Imports the unmodified bytes of a cue pack whose original filename is `file`, for `dat`, a DAT already
    /// imported, named `<provider>/<name>`: a ZIP holding `.cue` sheets. Importing bytes already imported returns the
    /// pack as it was recorded, restoring its file if it is missing. Never reads the network: a pack fetched elsewhere
    /// is imported here with origin [`CuePackOrigin::Fetch`] and where it was fetched from.
    pub fn import_cue_pack_bytes(
        &self,
        bytes: &[u8],
        file: &str,
        dat: &str,
        source_url: Option<&str>,
        origin: CuePackOrigin,
    ) -> Result<(CuePackRecord, ImportOutcome)> {
        self.import_cue_pack_held(bytes, file, dat, source_url, origin, None)
    }

    /// As [`Store::import_cue_pack_bytes`], recording the unfinished fetch `fetch`, if given, as bringing the pack as
    /// it is kept, so that no retention between deletes it before that fetch finishes.
    pub(crate) fn import_cue_pack_held(
        &self,
        bytes: &[u8],
        file: &str,
        dat: &str,
        source_url: Option<&str>,
        origin: CuePackOrigin,
        fetch: Option<i64>,
    ) -> Result<(CuePackRecord, ImportOutcome)> {
        let (provider, name) = split_dat(dat)?;
        if bytes.len() as u64 > CUE_PACK_LIMIT {
            return Err(Error::Refused(format!(
                "{file}: a cue pack is at most {CUE_PACK_LIMIT} bytes"
            )));
        }
        let known: bool = self.read(|c| Ok(super::dats::find_dat(c, provider, name)?.is_some()))?;
        if !known {
            return Err(Error::Refused(format!(
                "{dat}: import the DAT before a cue pack for it"
            )));
        }
        let sha = hex(&Sha256::digest(bytes));
        let packs = self.dir.join(CUE_PACKS);
        std::fs::create_dir_all(&packs).map_err(|e| io_error(&packs, e))?;
        let tmp = packs.join(format!(".{sha}.check"));
        std::fs::write(&tmp, bytes).map_err(|e| io_error(&tmp, e))?;
        let sheets = File::open(&tmp)
            .map_err(|e| io_error(&tmp, e))
            .and_then(|f| {
                title_select_engine::build::cue_sheets(&f, &CUE_LIMITS)
                    .map_err(|why| Error::Refused(format!("{file}: not a cue pack: {why}")))
            });
        let _ = std::fs::remove_file(&tmp);
        let sheets = sheets?;
        let imported = self.with_writer(|conn| {
            if let Some(fetch) = fetch {
                hold_pack(conn, fetch, &sha)?;
            }
            if let Some(existing) = pack_by_sha(conn, &sha)? {
                if existing.dat != dat {
                    return Err(Error::Refused(format!(
                        "{file}: this cue pack is already imported for {}",
                        existing.dat
                    )));
                }
                if existing.blob_present && self.cue_pack_path(&sha).is_file() {
                    return Ok((existing, ImportOutcome::AlreadyImported));
                }
                write_blob(&packs, &sha, bytes)?;
                conn.execute(
                    "UPDATE cue_packs SET blob_dropped_at = NULL, restored_at = ?2 WHERE sha256 = ?1",
                    params![sha, now()],
                )?;
                let restored = pack_by_sha(conn, &sha)?.expect("just restored");
                return Ok((restored, ImportOutcome::Restored));
            }
            write_blob(&packs, &sha, bytes)?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            // A later pack releases any restored pack of the same DAT.
            tx.execute(
                "UPDATE cue_packs SET restored_at = NULL WHERE dat = ?1",
                [dat],
            )?;
            tx.execute(
                "INSERT INTO cue_packs (sha256, dat, file_name, source_url, origin, cues, imported_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    sha,
                    dat,
                    file,
                    source_url,
                    origin.as_str(),
                    sheets.len() as i64,
                    now()
                ],
            )?;
            let id = tx.last_insert_rowid();
            for (member, sums) in &sheets {
                tx.execute(
                    "INSERT INTO cue_sheets (pack_id, member, size, crc, md5, sha1, sha256)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        id,
                        member,
                        sums.size as i64,
                        sums.crc,
                        sums.md5,
                        sums.sha1,
                        sums.sha256
                    ],
                )?;
            }
            let record = pack_by_sha(&tx, &sha)?.expect("just inserted");
            tx.commit()?;
            Ok((record, ImportOutcome::Added))
        })?;
        if imported.1 == ImportOutcome::Added {
            self.retain_cue_packs()?;
        }
        Ok(imported)
    }

    /// Deletes the file of every cue pack that no repair plan or step refers to and that its origin no longer holds.
    /// An imported pack is held while it is the latest added for its DAT or held as restored. A fetched pack is held
    /// while it is the one the latest successful fetch of its system's cue pack brought, or while no fetch names it
    /// yet, as it is being fetched. The row stays, and importing the pack again restores its file. Returns the
    /// SHA-256s whose files were deleted.
    pub fn retain_cue_packs(&self) -> Result<Vec<String>> {
        self.with_writer(|conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let shas = tx
                .prepare(
                    &format!(
                    "SELECT c.sha256 FROM cue_packs c
                     WHERE c.blob_dropped_at IS NULL
                       AND CASE c.origin
                           WHEN 'fetch' THEN
                               EXISTS (SELECT 1 FROM fetches f
                                       WHERE f.artifact = 'cues' AND f.pack_sha256 = c.sha256)
                               AND c.sha256 NOT IN (
                                   SELECT f.pack_sha256 FROM fetches f
                                   WHERE f.artifact = 'cues' AND f.pack_sha256 IS NOT NULL
                                     AND f.id = (SELECT max(g.id) FROM fetches g
                                                 WHERE g.provider = f.provider AND g.system = f.system
                                                   AND g.artifact = 'cues'
                                                   AND g.outcome IN ('added', 'unchanged')))
                               AND {not_held}
                           ELSE c.restored_at IS NULL
                               AND c.id <> (SELECT max(n.id) FROM cue_packs n WHERE n.dat = c.dat)
                           END
                       AND NOT EXISTS (SELECT 1 FROM repair_plans p WHERE instr(p.content, c.sha256) > 0)
                       AND NOT EXISTS (SELECT 1 FROM repair_steps s WHERE instr(s.detail, c.sha256) > 0)
                     ORDER BY c.id",
                    not_held = NOT_HELD.replace("{pack}", "c.sha256")
                ),
                )?
                .query_map([], |r| r.get(0))?
                .collect::<Result<Vec<String>, _>>()?;
            let at = now();
            for sha in &shas {
                tx.execute(
                    "UPDATE cue_packs SET blob_dropped_at = ?1 WHERE sha256 = ?2",
                    params![at, sha],
                )?;
            }
            tx.commit()?;
            // The writer is still held, so no import restores one of these in between.
            for sha in &shas {
                remove_blob(&self.cue_pack_path(sha))?;
            }
            Ok(shas)
        })
    }

    /// Every cue pack, oldest import first.
    pub fn cue_packs(&self) -> Result<Vec<CuePackRecord>> {
        self.read(|c| {
            let sql = format!("SELECT {COLUMNS} FROM cue_packs ORDER BY id");
            let mut stmt = c.prepare(&sql)?;
            let rows = stmt.query_map([], record_from_row)?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }

    /// Where the cue pack `sha256` is kept in the store.
    pub fn cue_pack_path(&self, sha256: &str) -> PathBuf {
        self.dir.join(CUE_PACKS).join(sha256)
    }

    /// Every `.cue` sheet of the cue packs for `dat` whose file is kept, as indexed when each was imported. No pack is
    /// read.
    pub(crate) fn cue_sheets(&self, dat: &str) -> Result<Vec<CueSheet>> {
        self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT p.sha256, p.file_name, s.member, s.size, s.crc, s.md5, s.sha1, s.sha256
                 FROM cue_sheets s JOIN cue_packs p ON p.id = s.pack_id
                 WHERE p.dat = ?1 AND p.blob_dropped_at IS NULL
                 ORDER BY p.id, s.member",
            )?;
            let rows = stmt.query_map([dat], |r| {
                Ok(CueSheet {
                    pack: r.get(0)?,
                    file: r.get(1)?,
                    member: r.get(2)?,
                    sums: Checksums {
                        size: r.get::<_, i64>(3)? as u64,
                        crc: r.get(4)?,
                        md5: r.get(5)?,
                        sha1: r.get(6)?,
                        sha256: r.get(7)?,
                    },
                })
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
