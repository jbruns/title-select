//! Content-addressed DAT blobs under `dats/<sha256>` (ADR 0003).

use std::collections::HashSet;
use std::fs::File;
use std::io::Write;
use std::path::Path;

use super::{CUE_PACKS, DATS, Result, SBI_PACKS, Store, io_error};

/// Writes a blob to a temp file in the blob directory, fsyncs it, renames it into place and fsyncs the
/// directory, so it is durable before any row refers to it.
pub(super) fn write_blob(dats: &Path, sha256: &str, bytes: &[u8]) -> Result<()> {
    let tmp = dats.join(format!(".{sha256}.tmp"));
    let mut file = File::create(&tmp).map_err(|e| io_error(&tmp, e))?;
    file.write_all(bytes).map_err(|e| io_error(&tmp, e))?;
    file.sync_all().map_err(|e| io_error(&tmp, e))?;
    drop(file);
    let path = dats.join(sha256);
    std::fs::rename(&tmp, &path).map_err(|e| io_error(&path, e))?;
    sync_dir(dats)
}

#[cfg(unix)]
pub(super) fn sync_dir(dir: &Path) -> Result<()> {
    File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(|e| io_error(dir, e))
}

#[cfg(not(unix))]
pub(super) fn sync_dir(_dir: &Path) -> Result<()> {
    Ok(())
}

pub(super) fn remove_blob(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(io_error(path, e)),
        _ => Ok(()),
    }
}

impl Store {
    /// Deletes every file in the blob directory that is not the blob of a DAT Revision whose row holds it,
    /// including temp files left by an interrupted write, and every file in the cue pack directory that is not the
    /// file of a cue pack whose row holds it, including one whose deletion was recorded but not finished, and SBI packs
    /// alike. Returns the names it deleted, a cue pack's under `cue_packs/` and an SBI pack's under `sbi_packs/`.
    ///
    /// It holds the writer throughout, so no import in this process is between writing a blob and inserting its
    /// row; the store's lock excludes every other process.
    pub fn collect_garbage(&self) -> Result<Vec<String>> {
        self.with_writer(|conn| {
            let held = |sql: &str| -> Result<HashSet<String>> {
                let mut stmt = conn.prepare(sql)?;
                Ok(stmt
                    .query_map([], |r| r.get(0))?
                    .collect::<Result<HashSet<String>, _>>()?)
            };
            let mut removed = Vec::new();
            for (dir, sql, prefix) in [
                (
                    DATS,
                    "SELECT sha256 FROM dat_revisions WHERE blob_dropped_at IS NULL",
                    "",
                ),
                (
                    CUE_PACKS,
                    "SELECT sha256 FROM cue_packs WHERE blob_dropped_at IS NULL",
                    "cue_packs/",
                ),
                (
                    SBI_PACKS,
                    "SELECT sha256 FROM sbi_packs WHERE blob_dropped_at IS NULL",
                    "sbi_packs/",
                ),
            ] {
                let dir = self.dir.join(dir);
                // The cue pack directory is made by the first import.
                let items = match std::fs::read_dir(&dir) {
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound && !prefix.is_empty() => {
                        continue;
                    }
                    items => items.map_err(|e| io_error(&dir, e))?,
                };
                let held = held(sql)?;
                let mut gone = Vec::new();
                for item in items {
                    let item = item.map_err(|e| io_error(&dir, e))?;
                    let name = item.file_name().to_string_lossy().into_owned();
                    let is_file = item.file_type().is_ok_and(|t| t.is_file());
                    if is_file && !held.contains(&name) {
                        remove_blob(&item.path())?;
                        gone.push(format!("{prefix}{name}"));
                    }
                }
                gone.sort();
                removed.extend(gone);
            }
            Ok(removed)
        })
    }
}
