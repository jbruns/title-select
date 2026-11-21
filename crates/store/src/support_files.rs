//! MAME Support Files (ADR 0012's amendment): progetto-SNAPS's `catver.ini`, `languages.ini` and `bestgames.ini`,
//! kept unmodified under `support_files/<sha256>` in the store, like DAT Revisions, with the MAME version each header
//! names. A fetch brings them, or the Operator drops them in. None is deleted: a Selection records the ones it read.

use rusqlite::{Connection, OptionalExtension, Row, params};
use sha2::{Digest, Sha256};

use super::blobs::write_blob;
use super::{Error, Result, Store, io_error, now};

use title_select_api::ImportOutcome;
use title_select_api::support::{
    SupportFileKind, SupportFileOrigin, SupportFileRecord, read_header,
};

/// The directory, in the store directory, that holds Support Files.
pub const SUPPORT_FILES: &str = "support_files";
/// The largest Support File kept, well past catver's few MiB.
pub const SUPPORT_FILE_LIMIT: u64 = 64 << 20;

const COLUMNS: &str =
    "sha256, kind, file_name, mame_version, commit_sha, origin, source_url, imported_at";

fn record_from_row(row: &Row) -> rusqlite::Result<SupportFileRecord> {
    let kind: String = row.get(1)?;
    let origin: String = row.get(5)?;
    Ok(SupportFileRecord {
        sha256: row.get(0)?,
        kind: SupportFileKind::parse(&kind).unwrap_or(SupportFileKind::Catver),
        file: row.get(2)?,
        mame_version: row.get(3)?,
        commit: row.get(4)?,
        origin: match origin.as_str() {
            "fetch" => SupportFileOrigin::Fetch,
            _ => SupportFileOrigin::Import,
        },
        source_url: row.get(6)?,
        imported_at: row.get(7)?,
    })
}

fn by_sha(c: &Connection, sha256: &str) -> Result<Option<SupportFileRecord>> {
    let sql = format!("SELECT {COLUMNS} FROM support_files WHERE sha256 = ?1");
    Ok(c.query_row(&sql, [sha256], record_from_row).optional()?)
}

/// Whether the store keeps the Support File whose SHA-256 is `sha256`.
pub(crate) fn is_kept(c: &Connection, sha256: &str) -> Result<bool> {
    Ok(by_sha(c, sha256)?.is_some())
}

impl Store {
    /// Imports the unmodified bytes of a Support File the Operator drops in, whose original filename is `file`: its header
    /// says which file it is and the MAME version it is for. Importing bytes already kept returns the file as it was
    /// recorded, restoring it if it is missing. Never reads the network.
    pub fn import_support_file(
        &self,
        bytes: &[u8],
        file: &str,
        source_url: Option<&str>,
    ) -> Result<(SupportFileRecord, ImportOutcome)> {
        self.keep_support_file(bytes, file, source_url, SupportFileOrigin::Import, None)
    }

    /// Keeps a Support File, as [`Store::import_support_file`], recording how it came and, if fetched, the commit it
    /// was fetched at.
    pub(crate) fn keep_support_file(
        &self,
        bytes: &[u8],
        file: &str,
        source_url: Option<&str>,
        origin: SupportFileOrigin,
        commit: Option<&str>,
    ) -> Result<(SupportFileRecord, ImportOutcome)> {
        if bytes.len() as u64 > SUPPORT_FILE_LIMIT {
            return Err(Error::Refused(format!(
                "{file}: a support file is at most {SUPPORT_FILE_LIMIT} bytes"
            )));
        }
        let (kind, version) = read_header(bytes, file).map_err(Error::Refused)?;
        let sha = super::builds::hex(&Sha256::digest(bytes));
        let dir = self.dir.join(SUPPORT_FILES);
        let path = dir.join(&sha);
        self.with_writer(|conn| {
            std::fs::create_dir_all(&dir).map_err(|e| io_error(&dir, e))?;
            let outcome = match by_sha(conn, &sha)? {
                Some(_) if path.is_file() => ImportOutcome::AlreadyImported,
                Some(_) => {
                    write_blob(&dir, &sha, bytes)?;
                    ImportOutcome::Restored
                }
                None => {
                    write_blob(&dir, &sha, bytes)?;
                    conn.execute(
                        "INSERT INTO support_files
                             (sha256, kind, file_name, mame_version, commit_sha, origin, source_url, imported_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                        params![
                            sha,
                            kind.as_str(),
                            file,
                            version,
                            commit,
                            origin.as_str(),
                            source_url,
                            now()
                        ],
                    )?;
                    ImportOutcome::Added
                }
            };
            let record = by_sha(conn, &sha)?.ok_or_else(|| Error::Io(format!("{sha}: not kept")))?;
            Ok((record, outcome))
        })
    }

    /// The Support Files a Selection reads: of each kind, the one for the newest MAME version, and of those the last
    /// to come, as paths to the files the store keeps.
    pub fn selection_support_files(&self) -> Result<Vec<std::path::PathBuf>> {
        let version = |v: &str| -> (u32, u32) {
            let (major, minor) = v.split_once('.').unwrap_or_default();
            (major.parse().unwrap_or(0), minor.parse().unwrap_or(0))
        };
        let mut newest: std::collections::BTreeMap<SupportFileKind, SupportFileRecord> =
            std::collections::BTreeMap::new();
        for file in self.support_files()? {
            match newest.get(&file.kind) {
                Some(kept) if version(&kept.mame_version) > version(&file.mame_version) => {}
                _ => {
                    newest.insert(file.kind, file);
                }
            }
        }
        let dir = self.dir.join(SUPPORT_FILES);
        Ok(newest.into_values().map(|f| dir.join(f.sha256)).collect())
    }

    /// Every Support File the store keeps, in the order they came.
    pub fn support_files(&self) -> Result<Vec<SupportFileRecord>> {
        self.read(|c| {
            let sql = format!("SELECT {COLUMNS} FROM support_files ORDER BY id");
            let mut stmt = c.prepare(&sql)?;
            Ok(stmt
                .query_map([], record_from_row)?
                .collect::<rusqlite::Result<_>>()?)
        })
    }
}
