//! Deleting kept files of a Source Set, once you approve it: as for a Managed Folder, each file is moved, by one
//! rename that never replaces anything, into the private folder kept files are deleted from, and deleted there only
//! once it is found to be the file that was kept. Anything else moved there is moved back.

use std::collections::BTreeSet;

use rusqlite::{Connection, OptionalExtension, Row, params};
use rustix::fs::FileType;
use sha2::{Digest, Sha256};
use title_select_api::build::{Approval, KeptDeletionOutcome, PlanKeptDeletion};
use title_select_api::repair::{
    KeptFileReviewed, SourceKeptDeletion, SourceKeptDeletionContent, SourceKeptFile,
};

use super::{MarkerRule, open_repairable, repair_active};
use crate::builds::still_sha256;
use crate::config::Config;
use crate::folder_dir::{FolderDir, Identity, ctime_ns, file_stat};
use crate::kept::{Scope, open_area_deleting, open_retention};
use crate::plans::{json, to_json};
use crate::sets::set_by_name;
use crate::{Error, Result, Store, now};

const KEPT_QUERY: &str = "SELECT id, source_set_id, plan_id, generation, name, kept_name, reason, sha256, size, \
     mtime_ns, dev, ino, kept_at FROM source_kept";

fn kept_from_row(r: &Row) -> rusqlite::Result<SourceKeptFile> {
    let mtime: String = r.get(9)?;
    Ok(SourceKeptFile {
        id: r.get(0)?,
        source_set: r.get(1)?,
        plan: r.get(2)?,
        generation: r.get::<_, i64>(3)? as u64,
        name: r.get(4)?,
        kept_name: r.get(5)?,
        reason: r.get(6)?,
        sha256: r.get(7)?,
        file: title_select_api::build::FileStat {
            size: r.get::<_, i64>(8)? as u64,
            mtime_ns: mtime.parse().unwrap_or_default(),
        },
        identity: Identity::from_sql(r.get(10)?, r.get(11)?).into(),
        kept_at: r.get(12)?,
    })
}

fn kept_of(c: &Connection, set: i64, state: &str) -> Result<Vec<SourceKeptFile>> {
    let mut stmt = c.prepare(&format!(
        "{KEPT_QUERY} WHERE source_set_id = ?1 AND state = ?2 ORDER BY generation DESC, id"
    ))?;
    Ok(stmt
        .query_map(params![set, state], kept_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

const DELETION_QUERY: &str = "SELECT id, digest, created_at, content, approved_digest, approved_by, approved_at, \
     finished_at, outcomes, recovery_error FROM source_kept_deletions";

fn deletion_from_row(r: &Row) -> rusqlite::Result<SourceKeptDeletion> {
    let outcomes: Option<String> = r.get(8)?;
    Ok(SourceKeptDeletion {
        id: r.get(0)?,
        digest: r.get(1)?,
        created_at: r.get(2)?,
        content: json(r, 3)?,
        approval: match r.get::<_, Option<String>>(4)? {
            Some(digest) => Some(Approval {
                digest,
                actor: r.get(5)?,
                at: r.get(6)?,
            }),
            None => None,
        },
        finished_at: r.get(7)?,
        outcomes: match outcomes {
            Some(_) => json(r, 8)?,
            None => Vec::new(),
        },
        recovery_error: r.get(9)?,
    })
}

fn deletion_by_id(c: &Connection, id: i64) -> Result<SourceKeptDeletion> {
    c.query_row(
        &format!("{DELETION_QUERY} WHERE id = ?1"),
        [id],
        deletion_from_row,
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("no deletion {id} of kept files of a source set")))
}

fn digest(content: &SourceKeptDeletionContent) -> String {
    let bytes = serde_json::to_vec(content).expect("a deletion always serializes");
    Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn outcome(file: &SourceKeptFile, deleted: bool, detail: Option<String>) -> KeptDeletionOutcome {
    KeptDeletionOutcome {
        id: file.id,
        kept_name: file.kept_name.clone(),
        generation: file.generation,
        deleted,
        detail,
    }
}

/// Whether `name` in `dir` is, without following a symlink, the regular file kept as `file`.
fn is_kept(dir: &FolderDir, name: &str, file: &SourceKeptFile) -> Result<bool> {
    let found = dir.stat(name).map_err(|e| Error::Io(e.to_string()))?;
    Ok(found.is_some_and(|st| {
        FolderDir::kind(&st) == FileType::RegularFile
            && Identity::of(&st) == Identity::from(file.identity)
            && file_stat(&st) == file.file
    }))
}

/// Kept file `file` as read now through `held`, opened on it: whether it is still the file kept, has ctime `ctime`
/// and, read while it holds still, hashes as it did when you reviewed its deletion. The ctime is the one you
/// reviewed until it is renamed, which moves it, and the one read through `held` just after that rename once it is.
fn reads_as_reviewed(
    held: &std::fs::File,
    file: &SourceKeptFile,
    reviewed: Option<&KeptFileReviewed>,
    ctime: Option<i128>,
) -> Result<bool> {
    let io = |e: std::io::Error| Error::Io(e.to_string());
    let (Some(reviewed), Some(ctime)) = (reviewed, ctime) else {
        return Ok(false);
    };
    let st = rustix::fs::fstat(held).map_err(|e| io(e.into()))?;
    if Identity::of(&st) != Identity::from(file.identity) || file_stat(&st) != file.file {
        return Ok(false);
    }
    Ok(still_sha256(held, ctime).map_err(io)?.as_deref() == Some(reviewed.sha256.as_str()))
}

/// Reads kept file `file` of Source Set folder `dir` through a descriptor, for you to review its deletion. `Err`
/// says why it cannot be: it is no longer the file that was kept, or cannot be read.
fn review(
    store: &Store,
    dir: &FolderDir,
    file: &SourceKeptFile,
) -> Result<std::result::Result<KeptFileReviewed, String>> {
    let kept = &file.kept_name;
    let generation = match open_retention(
        store,
        Scope::Source(file.source_set),
        dir,
        file.generation,
        false,
    )? {
        Ok(Some(retention)) => retention.generation,
        Ok(None) => {
            return Ok(Err(format!(
                "the kept files folder holding {kept} is missing"
            )));
        }
        Err(why) => return Ok(Err(why)),
    };
    let held = match generation.open_file(kept) {
        Ok(held) => held,
        Err(e) => return Ok(Err(format!("{kept} could not be read: {e}"))),
    };
    let st = rustix::fs::fstat(&held).map_err(|e| Error::Io(e.to_string()))?;
    if Identity::of(&st) != Identity::from(file.identity) || file_stat(&st) != file.file {
        return Ok(Err(format!("{kept} is no longer the file that was kept")));
    }
    let Some(sha256) = still_sha256(&held, ctime_ns(&st)).map_err(|e| Error::Io(e.to_string()))?
    else {
        return Ok(Err(format!("{kept} changed while it was read")));
    };
    Ok(Ok(KeptFileReviewed {
        id: file.id,
        ctime_ns: ctime_ns(&st),
        sha256,
    }))
}

const CHANGED: &str = "it changed since you reviewed its deletion";

fn slot(file: &SourceKeptFile) -> String {
    format!("source-kept-{}", file.id)
}

impl Store {
    /// The kept files of Source Set `name`, newest generation first.
    pub fn source_kept_files(&self, name: &str) -> Result<Vec<SourceKeptFile>> {
        self.read(|c| kept_of(c, set_by_name(c, name)?.id, "kept"))
    }

    /// Plans deleting the kept files of Source Set `name` that `request` picks, each read through a descriptor so
    /// the plan binds what it holds, and stores the plan for review. Nothing is deleted until you approve it. A kept
    /// file that is no longer the file kept is left out of "all", and refuses a plan that picks it.
    pub fn plan_source_kept_deletion(
        &self,
        config: &Config,
        name: &str,
        request: &PlanKeptDeletion,
    ) -> Result<SourceKeptDeletion> {
        let (set, picked) = self.read(|c| {
            let set = set_by_name(c, name)?;
            let kept = kept_of(c, set.id, "kept")?;
            let files: Vec<SourceKeptFile> = if request.all {
                kept
            } else {
                let mut files = Vec::new();
                for id in request.ids.iter().collect::<BTreeSet<_>>() {
                    match kept.iter().find(|k| k.id == *id) {
                        Some(k) => files.push(k.clone()),
                        None => {
                            return Err(Error::Refused(format!(
                                "source set {name} has no kept file {id}"
                            )));
                        }
                    }
                }
                files
            };
            Ok((set.id, files))
        })?;
        let dir =
            open_repairable(self, config, set, MarkerRule::Required)?.map_err(Error::Refused)?;
        let (mut files, mut reviewed) = (Vec::new(), Vec::new());
        for file in picked {
            match review(self, &dir, &file)? {
                Ok(read) => {
                    files.push(file);
                    reviewed.push(read);
                }
                Err(_) if request.all => {}
                Err(why) => return Err(Error::Refused(why)),
            }
        }
        if files.is_empty() {
            return Err(Error::Refused(
                "pick at least one kept file to delete".into(),
            ));
        }
        self.write(|tx| {
            let now_kept = kept_of(tx, set, "kept")?;
            if files.iter().any(|f| !now_kept.contains(f)) {
                return Err(Error::Conflict(
                    "the kept files changed while the deletion was planned; plan it again".into(),
                ));
            }
            let content = SourceKeptDeletionContent {
                source_set: set,
                files,
                reviewed,
            };
            tx.execute(
                "INSERT INTO source_kept_deletions (source_set_id, content, digest, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![set, to_json(&content), digest(&content), now()],
            )?;
            deletion_by_id(tx, tx.last_insert_rowid())
        })
    }

    pub fn source_kept_deletion(&self, id: i64) -> Result<SourceKeptDeletion> {
        self.read(|c| deletion_by_id(c, id))
    }

    /// Approves deleting kept files of a Source Set as reviewed with `digest`, and carries it out. Each file is
    /// deleted only while its kept entry is the file that was kept; anything else is left, and its outcome says why.
    /// Refused while a repair or another deletion of the Source Set is unfinished, and when its kept files changed
    /// since the plan.
    pub fn approve_source_kept_deletion(
        &self,
        config: &Config,
        id: i64,
        digest_reviewed: &str,
        actor: &str,
    ) -> Result<SourceKeptDeletion> {
        let plan = self.source_kept_deletion(id)?;
        if plan.approval.is_some() {
            return Err(Error::Conflict(format!(
                "deletion {id} is already approved"
            )));
        }
        if digest_reviewed != plan.digest {
            return Err(Error::Conflict(format!(
                "you reviewed a different version of deletion {id}; review it again"
            )));
        }
        let content = &plan.content;
        let set = content.source_set;
        open_repairable(self, config, set, MarkerRule::Required)?.map_err(Error::Refused)?;
        self.write(|tx| {
            if let Some(busy) = repair_active(tx, set)? {
                return Err(Error::Conflict(busy));
            }
            let now_kept = kept_of(tx, set, "kept")?;
            if content
                .files
                .iter()
                .any(|f| !now_kept.iter().any(|k| k == f))
            {
                return Err(Error::Conflict(format!(
                    "the kept files changed since you reviewed deletion {id}; plan it again"
                )));
            }
            tx.execute(
                "UPDATE source_kept_deletions SET approved_digest = ?2, approved_by = ?3, approved_at = ?4
                 WHERE id = ?1 AND approved_at IS NULL",
                params![id, plan.digest, actor, now()],
            )?;
            for file in &content.files {
                tx.execute(
                    "UPDATE source_kept SET state = 'deleting', deletion_id = ?2 WHERE id = ?1 AND state = 'kept'",
                    params![file.id, id],
                )?;
            }
            Ok(())
        })?;
        if let Err(why) = self.carry_out_source_deletion(config, id)? {
            return Err(Error::Refused(why));
        }
        self.source_kept_deletion(id)
    }

    /// Finishes each approved deletion of kept files of a Source Set that a stop interrupted. Returns each left
    /// unfinished, with why.
    pub fn recover_source_kept_deletions(&self, config: &Config) -> Result<Vec<(i64, String)>> {
        let unfinished: Vec<i64> = self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT id FROM source_kept_deletions WHERE approved_at IS NOT NULL AND finished_at IS NULL
                 ORDER BY id",
            )?;
            Ok(stmt
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })?;
        let mut left = Vec::new();
        for id in unfinished {
            if let Err(why) = self.carry_out_source_deletion(config, id)? {
                left.push((id, why));
            }
        }
        Ok(left)
    }

    /// Deletes each file of deletion `id` still recorded as being deleted, then records it finished. `Err` says why
    /// it was left unfinished.
    fn carry_out_source_deletion(
        &self,
        config: &Config,
        id: i64,
    ) -> Result<std::result::Result<(), String>> {
        let plan = self.source_kept_deletion(id)?;
        let set = plan.content.source_set;
        let unfinished = |why: String| -> Result<std::result::Result<(), String>> {
            self.write(|tx| {
                tx.execute(
                    "UPDATE source_kept_deletions SET recovery_error = ?2 WHERE id = ?1",
                    params![id, why],
                )?;
                Ok(())
            })?;
            Ok(Err(why))
        };
        let dir = match open_repairable(self, config, set, MarkerRule::Required)? {
            Ok(dir) => dir,
            Err(why) => return unfinished(why),
        };
        let deleting = match open_area_deleting(self, Scope::Source(set), &dir, true)? {
            Ok(Some(deleting)) => deleting,
            Ok(None) => {
                return unfinished("the folder kept files are deleted from is missing".into());
            }
            Err(why) => return unfinished(why),
        };
        let mut outcomes = Vec::new();
        for file in &plan.content.files {
            let state: String = self.read(|c| {
                Ok(c.query_row(
                    "SELECT state FROM source_kept WHERE id = ?1",
                    [file.id],
                    |r| r.get(0),
                )?)
            })?;
            let done = match state.as_str() {
                "deleting" => {
                    let reviewed = plan.content.reviewed.iter().find(|r| r.id == file.id);
                    self.delete_source_kept(&dir, &deleting, file, reviewed)?
                }
                "deleted" => outcome(file, true, None),
                "gone" => outcome(
                    file,
                    true,
                    Some("it was gone when its deletion was carried out".into()),
                ),
                _ => outcome(file, false, Some("it was left kept".into())),
            };
            outcomes.push(done);
        }
        self.write(|tx| {
            tx.execute(
                "UPDATE source_kept_deletions SET finished_at = ?2, outcomes = ?3, recovery_error = NULL
                 WHERE id = ?1",
                params![id, now(), to_json(&outcomes)],
            )?;
            Ok(())
        })?;
        Ok(Ok(()))
    }

    /// Deletes the original a conversion kept aside as kept file `id`, recorded as being deleted, as an approved
    /// deletion does: only while it still reads as `read`, the hash the conversion read it as and the ctime read
    /// through the descriptor held across the rename that kept it. Otherwise it stays kept.
    pub(super) fn delete_converted(
        &self,
        dir: &FolderDir,
        id: i64,
        read: &KeptFileReviewed,
    ) -> Result<KeptDeletionOutcome> {
        let file = self.read(|c| {
            Ok(c.query_row(&format!("{KEPT_QUERY} WHERE id = ?1"), [id], kept_from_row)?)
        })?;
        let deleting = match open_area_deleting(self, Scope::Source(file.source_set), dir, true)? {
            Ok(Some(deleting)) => deleting,
            Ok(None) => {
                return Err(Error::Io(
                    "the folder kept files are deleted from is missing".into(),
                ));
            }
            Err(why) => return Err(Error::Io(why)),
        };
        self.delete_source_kept(dir, &deleting, &file, Some(read))
    }

    fn set_slot_ctime(&self, file: &SourceKeptFile, ctime: Option<i128>) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE source_kept SET slot_ctime_ns = ?2 WHERE id = ?1",
                params![file.id, ctime.map(|c| c.to_string())],
            )?;
            Ok(())
        })
    }

    fn slot_ctime(&self, file: &SourceKeptFile) -> Result<Option<i128>> {
        self.read(|c| {
            let at: Option<String> = c.query_row(
                "SELECT slot_ctime_ns FROM source_kept WHERE id = ?1",
                [file.id],
                |r| r.get(0),
            )?;
            Ok(at.and_then(|at| at.parse().ok()))
        })
    }

    fn settle_source_kept(
        &self,
        file: &SourceKeptFile,
        state: &str,
        error: Option<&str>,
    ) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE source_kept SET state = ?2, error = ?3, slot_ctime_ns = NULL,
                        deleted_at = CASE WHEN ?2 = 'kept' THEN NULL ELSE ?4 END,
                        deletion_id = CASE WHEN ?2 = 'kept' THEN NULL ELSE deletion_id END
                 WHERE id = ?1",
                params![file.id, state, error, now()],
            )?;
            Ok(())
        })
    }

    /// Deletes kept file `file`, recorded as being deleted, from wherever a stop left it: its kept entry or its slot
    /// in the private folder kept files are deleted from.
    fn delete_source_kept(
        &self,
        dir: &FolderDir,
        deleting: &FolderDir,
        file: &SourceKeptFile,
        reviewed: Option<&KeptFileReviewed>,
    ) -> Result<KeptDeletionOutcome> {
        let io = |e: std::io::Error| Error::Io(e.to_string());
        let slot = slot(file);
        let generation = match open_retention(
            self,
            Scope::Source(file.source_set),
            dir,
            file.generation,
            false,
        )? {
            Ok(found) => found.map(|r| r.generation),
            Err(why) => return Ok(outcome(file, false, Some(why))),
        };
        // A stop may have left a rename unsynced: nothing is decided or recorded until both folders are synced.
        if let Some(generation) = &generation {
            generation.sync().map_err(io)?;
        }
        deleting.sync().map_err(io)?;
        self.fault_point(crate::Fault::SourceKeptSynced(file.id));
        let in_slot = deleting.stat(&slot).map_err(io)?.is_some();
        let mut held = None;
        if !in_slot {
            let Some(generation) = &generation else {
                self.settle_source_kept(
                    file,
                    "gone",
                    Some("it was gone when its deletion was carried out"),
                )?;
                return Ok(outcome(file, true, Some("it was already gone".into())));
            };
            if generation.stat(&file.kept_name).map_err(io)?.is_none() {
                self.settle_source_kept(
                    file,
                    "gone",
                    Some("it was gone when its deletion was carried out"),
                )?;
                return Ok(outcome(file, true, Some("it was already gone".into())));
            }
            if !is_kept(generation, &file.kept_name, file)? {
                let why = "it is no longer the file that was kept, so it is never deleted";
                self.settle_source_kept(file, "kept", Some(why))?;
                return Ok(outcome(file, false, Some(why.into())));
            }
            // Read as reviewed before the rename, which moves its ctime, and held through it.
            let read = match generation.open_file(&file.kept_name) {
                Ok(read)
                    if reads_as_reviewed(&read, file, reviewed, reviewed.map(|r| r.ctime_ns))? =>
                {
                    read
                }
                _ => {
                    let why = format!("{CHANGED}, so it was left kept");
                    self.settle_source_kept(file, "kept", Some(&why))?;
                    return Ok(outcome(file, false, Some(why)));
                }
            };
            self.set_slot_ctime(file, None)?;
            if let Err(e) = generation.rename_new(&file.kept_name, deleting, &slot) {
                let why =
                    format!("it could not be moved to be deleted ({e}), so it is never deleted");
                self.settle_source_kept(file, "kept", Some(&why))?;
                return Ok(outcome(file, false, Some(why)));
            }
            let moved = rustix::fs::fstat(&read).map_err(|e| io(e.into()))?;
            if Identity::of(&moved) == Identity::from(file.identity) {
                self.set_slot_ctime(file, Some(ctime_ns(&moved)))?;
            }
            held = Some(read);
            self.fault_point(crate::Fault::SourceKeptMoved(file.id));
            generation.sync().map_err(io)?;
            deleting.sync().map_err(io)?;
        }
        let kept = is_kept(deleting, &slot, file)?;
        let held = match held {
            Some(held) => Some(held),
            None if kept => deleting.open_file(&slot).ok(),
            None => None,
        };
        let unchanged = match &held {
            Some(held) => kept && reads_as_reviewed(held, file, reviewed, self.slot_ctime(file)?)?,
            None => false,
        };
        if unchanged && is_kept(deleting, &slot, file)? {
            deleting.unlink(&slot).map_err(io)?;
            deleting.sync().map_err(io)?;
            self.settle_source_kept(file, "deleted", None)?;
            return Ok(outcome(file, true, None));
        }
        // Not the file that was kept, or changed since you reviewed it: put back, never deleted.
        let back = match &generation {
            Some(generation) => deleting
                .rename_new(&slot, generation, &file.kept_name)
                .and_then(|()| generation.sync())
                .and_then(|()| deleting.sync()),
            None => Err(std::io::Error::other("its kept files folder is gone")),
        };
        match back {
            Ok(()) => {
                let why = if kept {
                    format!("{CHANGED}, so it was moved back and left kept")
                } else {
                    "what was moved to be deleted was not the file that was kept, so it was moved back".into()
                };
                self.settle_source_kept(file, "kept", Some(&why))?;
                Ok(outcome(file, false, Some(why)))
            }
            Err(e) => Err(Error::Io(format!(
                "what was moved to be deleted, at .title-select-retained/.deleting/{slot}, is not kept file {} and \
                 could not be moved back ({e})",
                file.id
            ))),
        }
    }
}
