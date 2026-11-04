//! The lifecycle of registrations. Unregistering a Source Set, Library Folder, Additions Set or Ingest Folder keeps its
//! row and history, read-only, and touches no game file: only Title Select's own marker is removed, once no Kept File
//! of it is left to delete. It is refused while anything holds the registration: another registration that depends
//! on it, a Job Queued or Running for it, or work on it not finished. Forgetting an Unregistered registration then
//! deletes its history from the store, touching no file.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use rustix::fs::FileType;
use title_select_api::build::MARKER;
use title_select_api::lifecycle::{
    Forgotten, Holds, RegistrationKind, UnregisteredRecord, Unregistration,
};

use super::folder_dir::{FolderDir, Identity};
use super::folders::{MARKER_LIMIT, folder_by_id, open_folder, store_id};
use super::{Error, Result, Store, now};
use crate::config::{ADDITIONS_ROOT, Config};

/// The table each kind is registered in.
fn table(kind: RegistrationKind) -> &'static str {
    match kind {
        RegistrationKind::SourceSet => "source_sets",
        RegistrationKind::LibraryFolder => "library_folders",
        RegistrationKind::AdditionsSet => "additions_sets",
        RegistrationKind::IngestFolder => "ingest_folders",
    }
}

/// The column of `jobs` naming a registration of each kind.
fn job_column(kind: RegistrationKind) -> &'static str {
    match kind {
        RegistrationKind::SourceSet => "source_set_id",
        RegistrationKind::LibraryFolder => "library_folder_id",
        RegistrationKind::AdditionsSet => "additions_set_id",
        RegistrationKind::IngestFolder => "ingest_folder_id",
    }
}

/// The field of each kind's marker that names it.
fn marker_field(kind: RegistrationKind) -> &'static str {
    match kind {
        RegistrationKind::SourceSet => "source_set",
        RegistrationKind::LibraryFolder => "folder",
        RegistrationKind::AdditionsSet => "additions_set",
        RegistrationKind::IngestFolder => "ingest_folder",
    }
}

/// A registration's row: its name as you read it, where it is, and when it was registered and Unregistered.
struct Registration {
    name: String,
    root: String,
    path: String,
    registered_at: String,
    unregistered_at: Option<String>,
}

fn registration(c: &Connection, kind: RegistrationKind, id: i64) -> Result<Registration> {
    let sql = match kind {
        RegistrationKind::SourceSet => {
            "SELECT name, root, path, added_at, unregistered_at FROM source_sets WHERE id = ?1"
        }
        RegistrationKind::LibraryFolder => {
            "SELECT root || '/' || path, root, path, created_at, unregistered_at FROM library_folders WHERE id = ?1"
        }
        RegistrationKind::AdditionsSet => {
            "SELECT d.provider || '/' || d.name, s.root, s.path, s.created_at, s.unregistered_at
             FROM additions_sets s JOIN dats d ON d.id = s.dat_id WHERE s.id = ?1"
        }
        RegistrationKind::IngestFolder => {
            "SELECT root, root, '', created_at, unregistered_at FROM ingest_folders WHERE id = ?1"
        }
    };
    c.query_row(sql, [id], |r| {
        Ok(Registration {
            name: r.get(0)?,
            root: r.get(1)?,
            path: r.get(2)?,
            registered_at: r.get(3)?,
            unregistered_at: r.get(4)?,
        })
    })
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("no {} {id}", kind.label())))
}

/// Every string the first column of `sql`, run with `params`, returns.
fn strings(c: &Connection, sql: &str, params: impl rusqlite::Params) -> Result<Vec<String>> {
    let mut stmt = c.prepare(sql)?;
    let rows = stmt.query_map(params, |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The Jobs Queued or Running for registration `id` of `kind`, as holds.
fn active_jobs(c: &Connection, kind: RegistrationKind, id: i64) -> Result<Vec<String>> {
    strings(
        c,
        &format!(
            "SELECT replace(kind, '_', ' ') || ' job ' || id || ' is queued or running for it'
             FROM jobs WHERE {} = ?1 AND state IN ('queued', 'running') ORDER BY id",
            job_column(kind)
        ),
        [id],
    )
}

fn source_set_holds(c: &Connection, id: i64) -> Result<Vec<String>> {
    let mut holds = strings(
        c,
        "SELECT 'play library folder ' || root || '/' || path || ' is bound to it; unregister that folder, or bind \
                 it to another source set, first'
         FROM library_folders WHERE source_set_id = ?1 AND unregistered_at IS NULL ORDER BY root, path",
        [id],
    )?;
    holds.extend(active_jobs(c, RegistrationKind::SourceSet, id)?);
    if let Some(why) = crate::repairs::repair_active(c, id)? {
        holds.push(why);
    }
    holds.extend(strings(
        c,
        "SELECT 'the incremental audit repair ' || id || ' owes is not queued yet'
         FROM repair_plans WHERE source_set_id = ?1 AND audit_owed = 1",
        [id],
    )?);
    holds.extend(strings(
        c,
        "SELECT 'audit ' || id || ' of it is running' FROM audits WHERE source_set_id = ?1 AND status = 'running'",
        [id],
    )?);
    holds.extend(strings(
        c,
        "SELECT 'an audit ingest plan ' || plan_id || ' owes it is not queued yet'
         FROM ingest_reaudits WHERE source_set_id = ?1 AND fulfilled_at IS NULL",
        [id],
    )?);
    holds.extend(strings(
        c,
        "SELECT DISTINCT 'ingest plan ' || m.plan_id || ' is still moving files into it'
         FROM ingest_moves m JOIN source_sets s ON s.root = m.dest_root AND s.path = m.dest_rel
         WHERE s.id = ?1 AND m.dest_kind = 'path' AND m.state NOT IN ('done', 'failed')",
        [id],
    )?);
    holds.extend(strings(
        c,
        "SELECT 'the addition ' || a.name || ' is still being added from one of its kept files'
         FROM additions a JOIN repair_plans p ON p.id = a.repair_plan_id
         WHERE p.source_set_id = ?1 AND a.state IN ('pending', 'staging')",
        [id],
    )?);
    Ok(holds)
}

fn library_folder_holds(c: &Connection, id: i64) -> Result<Vec<String>> {
    let mut holds = library_folder_work(c, id)?;
    holds.extend(strings(
        c,
        "SELECT 'its move to ' || to_path || ' is not finished; the service finishes it when it next starts'
         FROM library_folder_moves WHERE folder_id = ?1 AND approved_at IS NOT NULL AND finished_at IS NULL",
        [id],
    )?);
    Ok(holds)
}

/// What holds Library Folder `id` other than its own move: the work that changes the folder where it is.
pub(super) fn library_folder_work(c: &Connection, id: i64) -> Result<Vec<String>> {
    let mut holds = active_jobs(c, RegistrationKind::LibraryFolder, id)?;
    holds.extend(strings(
        c,
        "SELECT 'build ' || id || ' stopped before it finished, and the folder is not settled yet; the service \
                 settles it when it next starts'
         FROM builds WHERE folder_id = ?1
           AND (state = 'running' OR (state IN ('failed', 'interrupted') AND cleaned_at IS NULL))
           AND NOT EXISTS (SELECT 1 FROM jobs j WHERE j.id = builds.job_id AND j.state IN ('queued', 'running'))",
        [id],
    )?);
    holds.extend(strings(
        c,
        "SELECT 'deletion ' || id || ' of its kept files is not finished; the service finishes it when it next starts'
         FROM kept_deletions WHERE folder_id = ?1 AND approved_at IS NOT NULL AND finished_at IS NULL",
        [id],
    )?);
    holds.extend(strings(
        c,
        "SELECT 'its kept file ' || name || ' is being ' || CASE state WHEN 'keeping' THEN 'kept' ELSE 'restored' END
         FROM kept_files WHERE folder_id = ?1 AND state IN ('keeping', 'restoring')",
        [id],
    )?);
    Ok(holds)
}

fn additions_set_holds(c: &Connection, id: i64) -> Result<Vec<String>> {
    let mut holds = active_jobs(c, RegistrationKind::AdditionsSet, id)?;
    holds.extend(strings(
        c,
        "SELECT 'the addition ' || name || ' is ' || CASE state
                    WHEN 'retiring' THEN 'being retired'
                    WHEN 'restoring' THEN 'being restored'
                    ELSE 'being added' END
         FROM additions WHERE set_id = ?1 AND state IN ('pending', 'staging', 'retiring', 'restoring')
         ORDER BY name",
        [id],
    )?);
    holds.extend(strings(
        c,
        "SELECT 'deletion ' || id || ' of its kept files is not finished; the service finishes it when it next starts'
         FROM additions_kept_deletions WHERE additions_set_id = ?1 AND approved_at IS NOT NULL AND finished_at IS NULL",
        [id],
    )?);
    if crate::additions::build_reading(c, id)?.is_some() {
        holds.extend(strings(
            c,
            "SELECT 'build job ' || j.id || ' may read this system''s additions'
             FROM jobs j JOIN library_folders f ON f.id = j.library_folder_id
             JOIN source_sets ss ON ss.id = f.source_set_id
             WHERE j.kind = 'build' AND j.state IN ('queued', 'running')
               AND ss.dat_id = (SELECT dat_id FROM additions_sets WHERE id = ?1)",
            [id],
        )?);
    }
    Ok(holds)
}

fn ingest_folder_holds(c: &Connection, id: i64) -> Result<Vec<String>> {
    let mut holds = active_jobs(c, RegistrationKind::IngestFolder, id)?;
    holds.extend(strings(
        c,
        "SELECT 'ingest plan ' || id || ' is approved but not finished'
         FROM ingest_plans WHERE folder_id = ?1 AND approved_at IS NOT NULL AND finished_at IS NULL",
        [id],
    )?);
    holds.extend(strings(
        c,
        "SELECT DISTINCT 'a move of ' || name || ' out of it is not finished'
         FROM ingest_moves WHERE folder_id = ?1 AND state NOT IN ('done', 'failed')",
        [id],
    )?);
    Ok(holds)
}

pub(crate) fn holds_of(c: &Connection, kind: RegistrationKind, id: i64) -> Result<Vec<String>> {
    match kind {
        RegistrationKind::SourceSet => source_set_holds(c, id),
        RegistrationKind::LibraryFolder => library_folder_holds(c, id),
        RegistrationKind::AdditionsSet => additions_set_holds(c, id),
        RegistrationKind::IngestFolder => ingest_folder_holds(c, id),
    }
}

/// The Kept Files of a registration still in its folder, which only its marker lets you delete. A kept directory is
/// never deleted by Title Select, so it does not count.
fn live_kept(c: &Connection, kind: RegistrationKind, id: i64) -> Result<u64> {
    let sql = match kind {
        RegistrationKind::SourceSet => {
            "SELECT count(*) FROM source_kept WHERE source_set_id = ?1 AND state IN ('kept', 'deleting')"
        }
        RegistrationKind::LibraryFolder => {
            "SELECT count(*) FROM kept_files WHERE folder_id = ?1 AND directory = 0
             AND state IN ('keeping', 'kept', 'deleting', 'quarantined', 'restoring')"
        }
        RegistrationKind::AdditionsSet => {
            "SELECT count(*) FROM additions_kept WHERE additions_set_id = ?1
             AND state IN ('keeping', 'kept', 'deleting')"
        }
        RegistrationKind::IngestFolder => return Ok(0),
    };
    Ok(c.query_row(sql, [id], |r| r.get::<_, i64>(0))? as u64)
}

/// Unregisters registration `id` of `kind` in `tx`: refused, saying what holds it, while anything does.
pub(crate) fn unregister_in(tx: &Transaction, kind: RegistrationKind, id: i64) -> Result<String> {
    let reg = registration(tx, kind, id)?;
    if reg.unregistered_at.is_some() {
        return Err(Error::Refused(format!(
            "{} {} is already unregistered",
            kind.label(),
            reg.name
        )));
    }
    let holds = holds_of(tx, kind, id)?;
    if !holds.is_empty() {
        return Err(Error::Conflict(format!(
            "{} {} can't be unregistered yet: {}",
            kind.label(),
            reg.name,
            holds.join("; ")
        )));
    }
    let at = now();
    tx.execute(
        &format!(
            "UPDATE {} SET unregistered_at = ?2 WHERE id = ?1",
            table(kind)
        ),
        params![id, at],
    )?;
    Ok(at)
}

/// Removes the marker `dir` holds when it reads as this store's marker of registration `id` of `kind`. Returns why
/// it was left, if it was; nothing at all is not a reason.
fn remove_own_marker(
    dir: &FolderDir,
    store: &str,
    kind: RegistrationKind,
    id: i64,
) -> std::result::Result<(), String> {
    let left = |why: String| Err(format!("its .title-select marker was left in place: {why}"));
    let before = match dir.stat(MARKER) {
        Ok(None) => return Ok(()),
        Ok(Some(st)) => st,
        Err(e) => return left(e.to_string()),
    };
    if FolderDir::kind(&before) != FileType::RegularFile {
        return left("it is not a file".into());
    }
    let bytes = match dir.read(MARKER, MARKER_LIMIT) {
        Ok(bytes) => bytes,
        Err(e) => return left(e.to_string()),
    };
    let ours = serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .is_some_and(|v| {
            v.get("store").and_then(|s| s.as_str()) == Some(store)
                && v.get(marker_field(kind)).and_then(|i| i.as_i64()) == Some(id)
        });
    if !ours {
        return left(format!("it does not read as this {}'s", kind.label()));
    }
    match dir.stat(MARKER) {
        Ok(Some(st)) if Identity::of(&st) == Identity::of(&before) => {}
        _ => return left("it changed while it was read".into()),
    }
    dir.unlink(MARKER)
        .and_then(|()| dir.sync())
        .or_else(|e| left(e.to_string()))
}

impl Store {
    /// The id of the registered Source Set `name`.
    pub fn source_set_id(&self, name: &str) -> Result<i64> {
        self.read(|c| Ok(crate::sets::set_by_name(c, name)?.id))
    }

    fn additions_set_id(c: &Connection, system: &str) -> Result<i64> {
        Ok(crate::additions::set_of_system(c, system)?.id)
    }

    fn ingest_folder_id(c: &Connection, root: &str) -> Result<i64> {
        c.query_row(
            "SELECT id FROM ingest_folders WHERE root = ?1 AND unregistered_at IS NULL",
            [root],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("ingest folder {root} is not registered")))
    }

    fn registered_library_folder(c: &Connection, id: i64) -> Result<i64> {
        let row = folder_by_id(c, id)?;
        if row.unregistered_at.is_some() {
            return Err(Error::NotFound(format!(
                "play library folder {}/{} is already unregistered",
                row.root, row.path
            )));
        }
        Ok(id)
    }

    /// What holds Source Set `name` registered.
    pub fn source_set_holds(&self, name: &str) -> Result<Holds> {
        self.read(|c| {
            let id = crate::sets::set_by_name(c, name)?.id;
            Ok(Holds {
                holds: source_set_holds(c, id)?,
            })
        })
    }

    /// What holds Library Folder `id` registered.
    pub fn library_folder_holds(&self, id: i64) -> Result<Holds> {
        self.read(|c| {
            Self::registered_library_folder(c, id)?;
            Ok(Holds {
                holds: library_folder_holds(c, id)?,
            })
        })
    }

    /// What holds the Additions Set of `system` registered.
    pub fn additions_set_holds(&self, system: &str) -> Result<Holds> {
        self.read(|c| {
            let id = Self::additions_set_id(c, system)?;
            Ok(Holds {
                holds: additions_set_holds(c, id)?,
            })
        })
    }

    /// What holds the Ingest Folder of Root `root` registered.
    pub fn ingest_folder_holds(&self, root: &str) -> Result<Holds> {
        self.read(|c| {
            let id = Self::ingest_folder_id(c, root)?;
            Ok(Holds {
                holds: ingest_folder_holds(c, id)?,
            })
        })
    }

    /// What holds the Root `name` in the configuration: each registration still registered in it. A Root lives in
    /// the configuration only, so once nothing holds it you remove it there.
    pub fn root_holds(&self, config: &Config, name: &str) -> Result<Holds> {
        if config.root(name).is_none() {
            return Err(Error::NotFound(format!(
                "there is no root {name} in the configuration"
            )));
        }
        self.read(|c| {
            let mut holds = strings(
                c,
                "SELECT 'source set ' || name || ' is registered in it'
                 FROM source_sets WHERE root = ?1 AND unregistered_at IS NULL ORDER BY name",
                [name],
            )?;
            holds.extend(strings(
                c,
                "SELECT 'play library folder ' || root || '/' || path || ' is registered in it'
                 FROM library_folders WHERE root = ?1 AND unregistered_at IS NULL ORDER BY path",
                [name],
            )?);
            holds.extend(strings(
                c,
                "SELECT 'the additions set of ' || d.provider || '/' || d.name || ' is registered in it'
                 FROM additions_sets s JOIN dats d ON d.id = s.dat_id
                 WHERE s.root = ?1 AND s.unregistered_at IS NULL ORDER BY 1",
                [name],
            )?);
            holds.extend(strings(
                c,
                "SELECT 'it is registered as an ingest folder'
                 FROM ingest_folders WHERE root = ?1 AND unregistered_at IS NULL",
                [name],
            )?);
            Ok(Holds { holds })
        })
    }

    /// Unregisters Source Set `name`, touching no game file.
    pub fn unregister_source_set(&self, config: &Config, name: &str) -> Result<Unregistration> {
        let id = self.source_set_id(name)?;
        self.unregister(config, RegistrationKind::SourceSet, id)
    }

    /// Unregisters Library Folder `id`. Its built files are left in place, as files Title Select no longer manages.
    pub fn unregister_library_folder(&self, config: &Config, id: i64) -> Result<Unregistration> {
        self.read(|c| Self::registered_library_folder(c, id))?;
        self.unregister(config, RegistrationKind::LibraryFolder, id)
    }

    /// Unregisters the Additions Set of `system`. Its additions are left in its folder, as files Title Select no
    /// longer manages.
    pub fn unregister_additions_set(
        &self,
        config: &Config,
        system: &str,
    ) -> Result<Unregistration> {
        let id = self.read(|c| Self::additions_set_id(c, system))?;
        self.unregister(config, RegistrationKind::AdditionsSet, id)
    }

    /// Unregisters the Ingest Folder of Root `root`. What is in it is left as it is.
    pub fn unregister_ingest_folder(&self, config: &Config, root: &str) -> Result<Unregistration> {
        let id = self.read(|c| Self::ingest_folder_id(c, root))?;
        self.unregister(config, RegistrationKind::IngestFolder, id)
    }

    pub(crate) fn unregister(
        &self,
        config: &Config,
        kind: RegistrationKind,
        id: i64,
    ) -> Result<Unregistration> {
        let at = self.write(|tx| unregister_in(tx, kind, id))?;
        let reg = self.read(|c| registration(c, kind, id))?;
        let mut notes = Vec::new();
        if let Some(note) = self.settle_marker(config, kind, id)? {
            notes.push(note);
        }
        if kind == RegistrationKind::IngestFolder {
            let kept: i64 = self.read(|c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM ingest_kept WHERE folder_id = ?1",
                    [id],
                    |r| r.get(0),
                )?)
            })?;
            if kept > 0 {
                notes.push(format!(
                    "{kept} kept files are left in its kept files folder, which Title Select no longer manages"
                ));
            }
        }
        Ok(Unregistration {
            kind,
            id,
            name: reg.name,
            unregistered_at: at,
            notes,
        })
    }

    /// Removes the marker of Unregistered registration `id` of `kind`, unless Kept Files of it are left, which its
    /// marker lets you delete. Returns a note when the marker stays, or could not be removed.
    pub(crate) fn settle_marker(
        &self,
        config: &Config,
        kind: RegistrationKind,
        id: i64,
    ) -> Result<Option<String>> {
        let (reg, kept, store) = self.read(|c| {
            Ok((
                registration(c, kind, id)?,
                live_kept(c, kind, id)?,
                store_id(c)?,
            ))
        })?;
        if reg.unregistered_at.is_none() {
            return Ok(None);
        }
        if kept > 0 {
            return Ok(Some(format!(
                "{kept} kept files are left in its folder; its .title-select marker stays until you delete them"
            )));
        }
        let dir = match kind {
            RegistrationKind::SourceSet => match config.root(&reg.root) {
                Some(root) => FolderDir::open(&root.path, &reg.path).map_err(|e| e.to_string()),
                None => Err(format!(
                    "there is no root {} in the configuration",
                    reg.root
                )),
            },
            RegistrationKind::LibraryFolder => open_folder(config, &reg.root, &reg.path),
            RegistrationKind::AdditionsSet => {
                let created: bool = self.read(|c| {
                    Ok(c.query_row(
                        "SELECT dev IS NOT NULL FROM additions_sets WHERE id = ?1",
                        [id],
                        |r| r.get(0),
                    )?)
                })?;
                if !created {
                    return Ok(None);
                }
                debug_assert_eq!(reg.root, ADDITIONS_ROOT);
                self.open_additions(config, id)?
            }
            RegistrationKind::IngestFolder => crate::ingest::open_ingest(config, &reg.root),
        };
        let removed = dir.and_then(|dir| remove_own_marker(&dir, &store, kind, id));
        Ok(removed.err())
    }

    /// Every Unregistered registration, newest first, with what of its history the store still holds.
    pub fn unregistered(&self) -> Result<Vec<UnregisteredRecord>> {
        self.read(|c| {
            let mut found = Vec::new();
            for kind in [
                RegistrationKind::SourceSet,
                RegistrationKind::LibraryFolder,
                RegistrationKind::AdditionsSet,
                RegistrationKind::IngestFolder,
            ] {
                let ids: Vec<i64> = {
                    let mut stmt = c.prepare(&format!(
                        "SELECT id FROM {} WHERE unregistered_at IS NOT NULL",
                        table(kind)
                    ))?;
                    stmt.query_map([], |r| r.get(0))?
                        .collect::<Result<_, _>>()?
                };
                for id in ids {
                    found.push(unregistered_record(c, kind, id)?);
                }
            }
            found.sort_by(|a, b| b.unregistered_at.cmp(&a.unregistered_at));
            Ok(found)
        })
    }

    /// Forgets Unregistered registration `id` of `kind`: deletes its row and its history (Audits, Selections,
    /// Builds, Jobs, plans and what they recorded) from the store, touching no file, then drops the blob of each DAT
    /// Revision nothing holds any more. Refused while Kept Files of it are left to delete, and while another
    /// registration's history refers to it.
    pub fn forget(&self, kind: RegistrationKind, id: i64) -> Result<Forgotten> {
        let mut forgotten = self.write(|tx| {
            let reg = registration(tx, kind, id)?;
            if reg.unregistered_at.is_none() {
                return Err(Error::Refused(format!(
                    "{} {} is registered; unregister it before you forget it",
                    kind.label(),
                    reg.name
                )));
            }
            let record = unregistered_record(tx, kind, id)?;
            if record.kept_files > 0 {
                return Err(Error::Refused(format!(
                    "{} kept files of {} {} are left in its folder; delete them before you forget it",
                    record.kept_files,
                    kind.label(),
                    reg.name
                )));
            }
            let blocking = match kind {
                RegistrationKind::SourceSet => strings(
                    tx,
                    "SELECT 'play library folder ' || root || '/' || path || ' was bound to it; forget that folder \
                             first'
                     FROM library_folders WHERE source_set_id = ?1 ORDER BY root, path",
                    [id],
                )?,
                RegistrationKind::AdditionsSet => strings(
                    tx,
                    "SELECT DISTINCT 'play library folder ' || f.root || '/' || f.path || ' has files built from \
                             its additions'
                     FROM built_files b JOIN additions a ON a.id = b.addition_id
                     JOIN library_folders f ON f.id = b.folder_id
                     WHERE a.set_id = ?1",
                    [id],
                )?,
                _ => Vec::new(),
            };
            if !blocking.is_empty() {
                return Err(Error::Refused(format!(
                    "{} {} can't be forgotten yet: {}",
                    kind.label(),
                    reg.name,
                    blocking.join("; ")
                )));
            }
            let graph = ForeignKeys::read(tx)?;
            let jobs: Vec<i64> = {
                let mut stmt =
                    tx.prepare(&format!("SELECT id FROM jobs WHERE {} = ?1", job_column(kind)))?;
                stmt.query_map([id], |r| r.get(0))?
                    .collect::<Result<_, _>>()?
            };
            graph.purge(tx, "jobs", &jobs, table(kind))?;
            if kind == RegistrationKind::SourceSet {
                tx.execute("DELETE FROM source_folders WHERE source_set_id = ?1", [id])?;
            }
            graph.purge(tx, table(kind), &[id], table(kind))?;
            Ok(Forgotten {
                kind,
                id,
                name: reg.name,
                audits: record.audits,
                selections: record.selections,
                builds: record.builds,
                jobs: record.jobs,
                dat_revisions: 0,
            })
        })?;
        forgotten.dat_revisions = self.retain_blobs()?.len() as u64;
        self.collect_garbage()?;
        Ok(forgotten)
    }
}

fn unregistered_record(
    c: &Connection,
    kind: RegistrationKind,
    id: i64,
) -> Result<UnregisteredRecord> {
    let reg = registration(c, kind, id)?;
    let count =
        |sql: &str| -> Result<u64> { Ok(c.query_row(sql, [id], |r| r.get::<_, i64>(0))? as u64) };
    let (audits, selections, builds) = match kind {
        RegistrationKind::SourceSet => (
            count("SELECT count(*) FROM audits WHERE source_set_id = ?1 AND status = 'finished'")?,
            count("SELECT count(*) FROM selections WHERE source_set_id = ?1")?,
            0,
        ),
        RegistrationKind::LibraryFolder => (
            0,
            0,
            count("SELECT count(*) FROM builds WHERE folder_id = ?1")?,
        ),
        _ => (0, 0, 0),
    };
    Ok(UnregisteredRecord {
        kind,
        id,
        name: reg.name,
        root: reg.root,
        path: reg.path,
        registered_at: reg.registered_at,
        unregistered_at: reg.unregistered_at.unwrap_or_default(),
        audits,
        selections,
        builds,
        jobs: count(&format!(
            "SELECT count(*) FROM jobs WHERE {} = ?1",
            job_column(kind)
        ))?,
        kept_files: live_kept(c, kind, id)?,
    })
}

/// The tables a forgotten registration's history may never reach into: other registrations, and what they share.
const SHARED: [&str; 9] = [
    "source_sets",
    "library_folders",
    "additions_sets",
    "ingest_folders",
    "profiles",
    "profile_versions",
    "dats",
    "dat_revisions",
    "meta",
];

/// The schema's foreign keys, by the table they refer to: each referring table and column, and whether the column
/// must hold a value (NOT NULL, or part of the primary key).
struct ForeignKeys(BTreeMap<String, Vec<(String, String, bool)>>);

impl ForeignKeys {
    fn read(c: &Connection) -> Result<ForeignKeys> {
        let mut stmt = c.prepare(
            "SELECT m.name, p.\"from\", p.\"table\", c.\"notnull\" OR c.pk > 0
             FROM sqlite_master m
             JOIN pragma_foreign_key_list(m.name) p
             JOIN pragma_table_info(m.name) c ON c.name = p.\"from\"
             WHERE m.type = 'table'",
        )?;
        let mut graph: BTreeMap<String, Vec<(String, String, bool)>> = BTreeMap::new();
        for row in stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, bool>(3)?,
            ))
        })? {
            let (child, column, parent, not_null) = row?;
            graph
                .entry(parent)
                .or_default()
                .push((child, column, not_null));
        }
        Ok(ForeignKeys(graph))
    }

    /// Deletes rows `ids` of `table`, first deleting every row that must refer to one of them and clearing every
    /// reference that may be empty. Refused if that would reach a table in [`SHARED`] other than `root`.
    fn purge(&self, tx: &Transaction, table: &str, ids: &[i64], root: &str) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        if table != root && SHARED.contains(&table) {
            return Err(Error::Refused(format!(
                "what it left in the store is still referred to by {table}; nothing was forgotten"
            )));
        }
        let list = serde_json::to_string(ids).expect("ids serialize");
        let mut seen = BTreeSet::new();
        for (child, column, not_null) in self.0.get(table).into_iter().flatten() {
            if !seen.insert((child.clone(), column.clone())) {
                continue;
            }
            if *not_null {
                let rows: Vec<i64> = {
                    let mut stmt = tx.prepare(&format!(
                        "SELECT rowid FROM \"{child}\" WHERE \"{column}\" IN (SELECT value FROM json_each(?1))"
                    ))?;
                    stmt.query_map([&list], |r| r.get(0))?
                        .collect::<Result<_, _>>()?
                };
                self.purge(tx, child, &rows, root)?;
            } else {
                tx.execute(
                    &format!(
                        "UPDATE \"{child}\" SET \"{column}\" = NULL
                         WHERE \"{column}\" IN (SELECT value FROM json_each(?1))"
                    ),
                    [&list],
                )?;
            }
        }
        tx.execute(
            &format!("DELETE FROM \"{table}\" WHERE rowid IN (SELECT value FROM json_each(?1))"),
            [&list],
        )?;
        Ok(())
    }
}
