//! Redump cue packs: ZIPs of `.cue` sheets for one DAT, kept unmodified under `cue_packs/<sha256>` in the store with
//! where they came from. A repair fills or replaces a `.cue` ROM from one, matched by the DAT's checksums.

use std::fs::File;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Row, params};
use sha2::{Digest, Sha256};

use super::blobs::write_blob;
use super::dats::split_dat;
use super::repairs::planning::CueSheet;
use super::{Error, Result, Store, io_error, now};

use title_select_api::{CuePackOrigin, CuePackRecord, ImportOutcome};

/// The directory, in the store directory, that holds cue packs.
pub const CUE_PACKS: &str = "cue_packs";
/// The largest cue pack imported, in bytes.
pub const CUE_PACK_LIMIT: u64 = 256 << 20;
/// The largest `.cue` sheet read from a cue pack, in bytes.
const CUE_LIMIT: u64 = 1 << 20;

const COLUMNS: &str = "sha256, dat, file_name, source_url, origin, cues, imported_at";

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
        let (provider, name) = split_dat(dat)?;
        if bytes.len() as u64 > CUE_PACK_LIMIT {
            return Err(Error::Refused(format!(
                "{file}: a cue pack is at most {CUE_PACK_LIMIT} bytes"
            )));
        }
        let known: bool = self.read(|c| {
            Ok(c.query_row(
                "SELECT EXISTS (SELECT 1 FROM dats WHERE provider = ?1 AND name = ?2)",
                params![provider, name],
                |r| r.get(0),
            )?)
        })?;
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
                title_select_engine::build::cue_sheets(&f, CUE_LIMIT)
                    .map_err(|why| Error::Refused(format!("{file}: not a cue pack: {why}")))
            });
        let _ = std::fs::remove_file(&tmp);
        let sheets = sheets?;
        self.with_writer(|conn| {
            if let Some(existing) = pack_by_sha(conn, &sha)? {
                if existing.dat != dat {
                    return Err(Error::Refused(format!(
                        "{file}: this cue pack is already imported for {}",
                        existing.dat
                    )));
                }
                if self.cue_pack_path(&sha).is_file() {
                    return Ok((existing, ImportOutcome::AlreadyImported));
                }
                write_blob(&packs, &sha, bytes)?;
                return Ok((existing, ImportOutcome::Restored));
            }
            write_blob(&packs, &sha, bytes)?;
            conn.execute(
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
            let record = pack_by_sha(conn, &sha)?.expect("just inserted");
            Ok((record, ImportOutcome::Added))
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

    /// Every `.cue` sheet of the cue packs for `dat`, each read and hashed now. A pack whose file is missing or no
    /// longer reads is passed over.
    pub(crate) fn cue_sheets(&self, dat: &str) -> Result<Vec<CueSheet>> {
        let packs: Vec<CuePackRecord> = self
            .cue_packs()?
            .into_iter()
            .filter(|p| p.dat == dat)
            .collect();
        let mut sheets = Vec::new();
        for pack in packs {
            let Ok(file) = File::open(self.cue_pack_path(&pack.sha256)) else {
                continue;
            };
            let Ok(found) = title_select_engine::build::cue_sheets(&file, CUE_LIMIT) else {
                continue;
            };
            sheets.extend(found.into_iter().map(|(member, sums)| CueSheet {
                pack: pack.sha256.clone(),
                file: pack.file.clone(),
                member,
                sums,
            }));
        }
        Ok(sheets)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
