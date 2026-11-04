//! Redump SBI packs: ZIPs of subchannel files for one DAT, kept unmodified under `sbi_packs/<sha256>` in the store
//! with where they were fetched from. Only a fetch brings one (ADR 0012). A pack's file is kept while a repair plan
//! refers to it or it is what the latest successful fetch of its system's SBI pack brought; then it is deleted, and
//! its row stays. A Build puts a disc's SBI file from its system's latest pack beside the disc when the system opts in
//! (ADR 0014), so a pack a plan refers to is kept too.

use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::path::PathBuf;

use rusqlite::{OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};

use super::blobs::{remove_blob, write_blob};
use super::fetch::{NOT_HELD, hold_pack};
use super::{Error, Result, Store, io_error, now};

use title_select_api::ImportOutcome;

/// The directory, in the store directory, that holds SBI packs.
pub const SBI_PACKS: &str = "sbi_packs";

/// The most an SBI file may hold: one lists a disc's few altered subchannel sectors.
const SBI_LIMIT: u64 = 1 << 20;

/// A system's latest SBI pack, as a plan reads it.
pub(crate) struct SbiPack {
    pub sha256: String,
    /// Each `.sbi` file in it, by its name's lowercased stem.
    pub members: HashMap<String, SbiMember>,
}

pub(crate) struct SbiMember {
    /// Its name in the pack.
    pub name: String,
    pub sha256: String,
    pub size: u64,
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

impl Store {
    /// Keeps the unmodified bytes of an SBI pack fetched from `source_url` as `file`, for `dat`, named
    /// `<provider>/<name>`, once they are found to be a ZIP holding at least one `.sbi` file. Returns its SHA-256.
    /// Bytes already kept are not written again, unless their file was deleted. The unfinished fetch `fetch` is
    /// recorded as bringing it as it is kept, so that no retention between deletes it before that fetch finishes.
    pub(crate) fn import_sbi_pack_bytes(
        &self,
        bytes: &[u8],
        file: &str,
        dat: &str,
        source_url: &str,
        fetch: i64,
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
            hold_pack(conn, fetch, &sha)?;
            let dat = super::dats::canonical_dat(conn, dat)?;
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
                        params![sha, &dat, file, source_url, now()],
                    )?;
                    Ok(ImportOutcome::Added)
                }
            }
        })?;
        Ok((sha, outcome))
    }

    /// Deletes the file of every SBI pack that no repair plan or step, or Change Plan, refers to and that the latest successful fetch
    /// of its system's SBI pack did not bring. A pack no finished fetch names yet is being fetched, and is kept. The
    /// row stays. Returns the SHA-256s whose files were deleted.
    pub fn retain_sbi_packs(&self) -> Result<Vec<String>> {
        self.with_writer(|conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let shas = tx
                .prepare(
                    &format!(
                    "SELECT p.sha256 FROM sbi_packs p
                     WHERE p.blob_dropped_at IS NULL
                       AND EXISTS (SELECT 1 FROM fetches f WHERE f.pack_sha256 = p.sha256)
                       AND p.sha256 NOT IN (
                           SELECT f.pack_sha256 FROM fetches f
                           WHERE f.artifact = 'sbi' AND f.pack_sha256 IS NOT NULL
                             AND f.id = (SELECT max(g.id) FROM fetches g
                                         WHERE g.provider = f.provider AND g.system = f.system
                                           AND g.artifact = 'sbi' AND g.outcome IN ('added', 'unchanged')))
                       AND {not_held}
                       AND NOT EXISTS (SELECT 1 FROM repair_plans r WHERE instr(r.content, p.sha256) > 0)
                       AND NOT EXISTS (SELECT 1 FROM plans b WHERE instr(b.content, p.sha256) > 0)
                       AND NOT EXISTS (SELECT 1 FROM repair_steps s WHERE instr(s.detail, p.sha256) > 0)
                     ORDER BY p.id",
                    not_held = NOT_HELD.replace("{pack}", "p.sha256")
                ),
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

    /// Keeps `bytes`, an SBI pack for `dat`, as a fetch would, without one.
    #[doc(hidden)]
    pub fn keep_sbi_pack(&self, bytes: &[u8], dat: &str) -> Result<String> {
        let sha = sha256_hex(bytes);
        let packs = self.dir.join(SBI_PACKS);
        self.with_writer(|conn| {
            std::fs::create_dir_all(&packs).map_err(|e| io_error(&packs, e))?;
            write_blob(&packs, &sha, bytes)?;
            conn.execute(
                "INSERT OR IGNORE INTO sbi_packs (sha256, dat, file_name, fetched_at)
                 VALUES (?1, ?2, 'sbi.zip', ?3)",
                params![sha, dat, now()],
            )?;
            Ok(sha)
        })
    }

    /// The latest SBI pack whose file is kept for `dat`, named `<provider>/<name>`: the one the latest successful
    /// fetch of its SBI pack brought, else the one kept last; `None` if there is none.
    pub(crate) fn latest_sbi_pack(&self, dat: &str) -> Result<Option<SbiPack>> {
        let sha: Option<String> = self.read(|c| {
            Ok(c.query_row(
                "SELECT p.sha256 FROM sbi_packs p
                 WHERE p.dat = ?1 AND p.blob_dropped_at IS NULL
                 ORDER BY coalesce((SELECT max(f.id) FROM fetches f
                                    WHERE f.pack_sha256 = p.sha256 AND f.artifact = 'sbi'
                                      AND f.outcome IN ('added', 'unchanged')), 0) DESC,
                          p.id DESC
                 LIMIT 1",
                [dat],
                |r| r.get(0),
            )
            .optional()?)
        })?;
        let Some(sha) = sha else {
            return Ok(None);
        };
        let path = self.sbi_pack_path(&sha);
        let file = std::fs::File::open(&path).map_err(|e| io_error(&path, e))?;
        let mut archive = zip::ZipArchive::new(file)
            .map_err(|e| Error::Io(format!("{}: not a ZIP: {e}", path.display())))?;
        let mut members = HashMap::new();
        for i in 0..archive.len() {
            let Ok(mut member) = archive.by_index(i) else {
                continue;
            };
            let name = member.name().unwrap_or_default().to_string();
            let leaf = name.rsplit('/').next().unwrap_or_default().to_owned();
            let Some((stem, ext)) = leaf.rsplit_once('.') else {
                continue;
            };
            if !member.is_file() || !ext.eq_ignore_ascii_case("sbi") || member.size() > SBI_LIMIT {
                continue;
            }
            let mut bytes = Vec::new();
            if member.read_to_end(&mut bytes).is_err() {
                continue;
            }
            members.insert(
                stem.to_lowercase(),
                SbiMember {
                    name,
                    sha256: sha256_hex(&bytes),
                    size: bytes.len() as u64,
                },
            );
        }
        Ok(Some(SbiPack {
            sha256: sha,
            members,
        }))
    }

    /// The bytes of `member` in the kept SBI pack `pack`, when its SHA-256 is `sha256`.
    pub(crate) fn sbi_member(&self, pack: &str, member: &str, sha256: &str) -> Result<Vec<u8>> {
        let path = self.sbi_pack_path(pack);
        let file = std::fs::File::open(&path).map_err(|e| io_error(&path, e))?;
        let mut archive = zip::ZipArchive::new(file)
            .map_err(|e| Error::Io(format!("{}: not a ZIP: {e}", path.display())))?;
        let mut found = archive
            .by_name(member)
            .map_err(|e| Error::Io(format!("{}: {member}: {e}", path.display())))?;
        let mut bytes = Vec::new();
        (&mut found)
            .take(SBI_LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| io_error(&path, e))?;
        if sha256_hex(&bytes) != sha256 {
            return Err(Error::Io(format!(
                "{}: {member} is not the SBI file the plan was made with",
                path.display()
            )));
        }
        Ok(bytes)
    }
}
