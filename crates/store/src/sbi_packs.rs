//! Redump SBI packs: ZIPs of subchannel files for one DAT, kept unmodified under `sbi_packs/<sha256>` in the store
//! with where they were fetched from. Only a fetch brings one (ADR 0012). A pack's file is kept while a repair plan
//! refers to it or it is what the latest successful fetch of its system's SBI pack brought; then it is deleted, and
//! its row stays.

use std::io::Cursor;
use std::path::PathBuf;

use rusqlite::{TransactionBehavior, params};
use sha2::{Digest, Sha256};

use super::blobs::{remove_blob, write_blob};
use super::{Error, Result, Store, io_error, now};

use title_select_api::ImportOutcome;

/// The directory, in the store directory, that holds SBI packs.
pub const SBI_PACKS: &str = "sbi_packs";

impl Store {
    /// Keeps the unmodified bytes of an SBI pack fetched from `source_url` as `file`, for `dat`, named
    /// `<provider>/<name>`, once they are found to be a ZIP holding at least one `.sbi` file. Returns its SHA-256.
    /// Bytes already kept are not written again, unless their file was deleted.
    pub(crate) fn import_sbi_pack_bytes(
        &self,
        bytes: &[u8],
        file: &str,
        dat: &str,
        source_url: &str,
    ) -> Result<(String, ImportOutcome)> {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
            .map_err(|e| Error::Refused(format!("{file}: not a ZIP: {e}")))?;
        let holds_sbi = (0..archive.len()).any(|i| {
            archive.by_index_raw(i).is_ok_and(|m| {
                m.is_file()
                    && m.name()
                        .is_ok_and(|n| n.to_ascii_lowercase().ends_with(".sbi"))
            })
        });
        if !holds_sbi {
            return Err(Error::Refused(format!(
                "{file}: not an SBI pack: it holds no .sbi file"
            )));
        }
        let sha: String = Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let packs = self.dir.join(SBI_PACKS);
        let outcome = self.with_writer(|conn| {
            std::fs::create_dir_all(&packs).map_err(|e| io_error(&packs, e))?;
            let known: Option<bool> = conn
                .query_row(
                    "SELECT blob_dropped_at IS NULL FROM sbi_packs WHERE sha256 = ?1",
                    [&sha],
                    |r| r.get(0),
                )
                .ok();
            match known {
                Some(true) if self.sbi_pack_path(&sha).is_file() => {
                    Ok(ImportOutcome::AlreadyImported)
                }
                Some(_) => {
                    write_blob(&packs, &sha, bytes)?;
                    conn.execute(
                        "UPDATE sbi_packs SET blob_dropped_at = NULL WHERE sha256 = ?1",
                        [&sha],
                    )?;
                    Ok(ImportOutcome::Restored)
                }
                None => {
                    write_blob(&packs, &sha, bytes)?;
                    conn.execute(
                        "INSERT INTO sbi_packs (sha256, dat, file_name, source_url, fetched_at)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![sha, dat, file, source_url, now()],
                    )?;
                    Ok(ImportOutcome::Added)
                }
            }
        })?;
        Ok((sha, outcome))
    }

    /// Deletes the file of every SBI pack that no repair plan or step refers to and that the latest successful fetch
    /// of its system's SBI pack did not bring. A pack no finished fetch names yet is being fetched, and is kept. The
    /// row stays. Returns the SHA-256s whose files were deleted.
    pub fn retain_sbi_packs(&self) -> Result<Vec<String>> {
        self.with_writer(|conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let shas = tx
                .prepare(
                    "SELECT p.sha256 FROM sbi_packs p
                     WHERE p.blob_dropped_at IS NULL
                       AND EXISTS (SELECT 1 FROM fetches f WHERE f.pack_sha256 = p.sha256)
                       AND p.sha256 NOT IN (
                           SELECT f.pack_sha256 FROM fetches f
                           WHERE f.artifact = 'sbi' AND f.pack_sha256 IS NOT NULL
                             AND f.id = (SELECT max(g.id) FROM fetches g
                                         WHERE g.provider = f.provider AND g.system = f.system
                                           AND g.artifact = 'sbi' AND g.outcome IN ('added', 'unchanged')))
                       AND NOT EXISTS (SELECT 1 FROM repair_plans r WHERE instr(r.content, p.sha256) > 0)
                       AND NOT EXISTS (SELECT 1 FROM repair_steps s WHERE instr(s.detail, p.sha256) > 0)
                     ORDER BY p.id",
                )?
                .query_map([], |r| r.get(0))?
                .collect::<Result<Vec<String>, _>>()?;
            let at = now();
            for sha in &shas {
                tx.execute(
                    "UPDATE sbi_packs SET blob_dropped_at = ?1 WHERE sha256 = ?2",
                    params![at, sha],
                )?;
            }
            tx.commit()?;
            for sha in &shas {
                remove_blob(&self.sbi_pack_path(sha))?;
            }
            Ok(shas)
        })
    }

    /// Where the SBI pack `sha256` is kept in the store.
    pub fn sbi_pack_path(&self, sha256: &str) -> PathBuf {
        self.dir.join(SBI_PACKS).join(sha256)
    }
}
