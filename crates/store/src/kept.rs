//! Kept files: files a Build moved out of a Managed Folder into the folder's retention area,
//! `.title-select-retained/<generation>/`, rather than deleting them. The retention area and each folder in it are
//! created under a temporary name recorded by identity, made durable, renamed into place without replacing anything,
//! and recorded by identity before anything is moved into them; one found with another identity, or not recorded at
//! all, is never used. A file is kept aside by one rename within the folder that never replaces anything, only while
//! it is still the file the approved plan names, with the intent recorded first. It is deleted only by its own
//! approved plan: moved, with the intent recorded first, into the private folder `.deleting` in the retention area,
//! and deleted there only once what was moved is found to be the file that was kept.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use rustix::fs::FileType;
use sha2::{Digest, Sha256};
use title_select_api::build::{
    Approval, FileStat, KeepAside, KeepReason, KeptDeletion, KeptDeletionContent,
    KeptDeletionOutcome, KeptFile, Marker, PlanKeptDeletion, RETENTION, STAGING, kept_name,
};

use super::builds::{sha256_of, temps};
use super::checks::Seen;
use super::folder_dir::{FolderDir, Identity, file_stat};
use super::folders::{check_marker, folder_by_id, open_writable_folder, store_id};
use super::plans::{json, to_json};
use super::{Error, Fault, Result, Store, now};
use crate::config::Config;

/// What a kept file cites, in `audit_citations` and `selection_citations`: the provenance of the file a Build wrote,
/// so that pruning never drops what its record needs.
fn cited_by(kept: i64) -> String {
    format!("kept-file:{kept}")
}

/// Whose bookkeeping a directory Title Select creates is recorded in: a Managed Folder's (`folder_dirs`,
/// `folder_temps`, `folder_temp_names`) or an Ingest Folder's (`ingest_dirs`, `ingest_temps`, `ingest_temp_names`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Scope {
    Managed(i64),
    Ingest(i64),
}

impl From<i64> for Scope {
    fn from(folder: i64) -> Scope {
        Scope::Managed(folder)
    }
}

impl Scope {
    pub fn id(self) -> i64 {
        match self {
            Scope::Managed(id) | Scope::Ingest(id) => id,
        }
    }

    /// `sql` with `{dirs}`, `{temps}` and `{temp_names}` replaced by this scope's tables.
    fn sql(self, sql: &str) -> String {
        let prefix = match self {
            Scope::Managed(_) => "folder_",
            Scope::Ingest(_) => "ingest_",
        };
        sql.replace("{dirs}", &format!("{prefix}dirs"))
            .replace("{temps}", &format!("{prefix}temps"))
            .replace("{temp_names}", &format!("{prefix}temp_names"))
    }
}

/// The temporary entries recorded by identity in `folder`'s scope, by name.
fn scoped_temps(c: &Connection, folder: Scope) -> Result<HashMap<String, Identity>> {
    match folder {
        Scope::Managed(id) => temps(c, id),
        Scope::Ingest(id) => {
            let mut stmt = c.prepare("SELECT name, dev, ino FROM ingest_temps WHERE folder_id = ?1")?;
            let rows = stmt.query_map([id], |r| {
                Ok((r.get(0)?, Identity::from_sql(r.get(1)?, r.get(2)?)))
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        }
    }
}

/// The directories Builds created in Managed Folder `folder` that outlive them, by name.
pub(super) fn folder_dirs(c: &Connection, folder: impl Into<Scope>) -> Result<HashMap<String, Identity>> {
    let folder: Scope = folder.into();
    let mut stmt = c.prepare(&folder.sql("SELECT name, dev, ino FROM {dirs} WHERE folder_id = ?1"))?;
    let rows = stmt.query_map([folder.id()], |r| {
        Ok((r.get(0)?, Identity::from_sql(r.get(1)?, r.get(2)?)))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Why nothing else is done to Managed Folder `folder` now, when an approved deletion of its kept files is being
/// carried out, or waits for recovery.
pub(super) fn deletion_active(c: &Connection, folder: i64) -> Result<Option<String>> {
    let found: Option<(i64, Option<String>)> = c
        .query_row(
            "SELECT id, recovery_error FROM kept_deletions
             WHERE folder_id = ?1 AND approved_at IS NOT NULL AND finished_at IS NULL",
            [folder],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(found.map(|(id, error)| match error {
        None => format!("kept files of managed folder {folder} are being deleted: deletion {id}"),
        Some(error) => format!(
            "deletion {id} of kept files of managed folder {folder} is not finished: {error}. The service \
             finishes it when it next starts, once the folder can be shown to be this store's"
        ),
    }))
}

/// The name a generation's folder is recorded under in `folder_dirs`.
fn generation_dir(generation: u64) -> String {
    format!("{RETENTION}/{generation}")
}

/// Why an entry in the retention area is not to be used.
fn not_ours(dir: &FolderDir, name: &str) -> String {
    format!(
        "{}/{name} is not something a build of this store created; it was left as it is. Move it out of the folder, \
         then try again",
        dir.path().display()
    )
}

/// A Managed Folder's retention area and one generation's folder in it, each opened without following a symlink
/// and found to be the directory a Build recorded creating.
pub(super) struct Retention {
    pub area: FolderDir,
    pub generation: FolderDir,
}

/// What marks a directory's temporary name while it is created: `<name>.new-<unique>`.
const DIR_TEMP: &str = ".new-";

/// The private folder in the retention area a kept file is moved into, by one rename that never replaces anything,
/// to be deleted there once it is found to be the file that was kept.
pub(super) const DELETING: &str = ".deleting";

/// Whether `name`, in a Managed Folder, is the temporary name its retention area or a Build's staging directory is
/// created under.
pub(super) fn is_dir_temp(name: &str) -> bool {
    [RETENTION, STAGING].into_iter().any(|dir| {
        name.strip_prefix(dir)
            .is_some_and(|rest| rest.starts_with(DIR_TEMP))
    })
}

/// A name no other attempt, in this process or another, uses.
pub(super) fn unique() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!(
        "{}-{nanos}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// `key` with its last component replaced by `name`.
fn sibling(key: &str, name: &str) -> String {
    match key.rsplit_once('/') {
        Some((parent, _)) => format!("{parent}/{name}"),
        None => name.to_owned(),
    }
}

fn is_dir(st: &rustix::fs::Stat) -> bool {
    FolderDir::kind(st) == FileType::Directory
}

/// Creates the directory `name` in `parent`, which is recorded under `key`: in `folder_dirs`, or, for a Build's
/// staging directory, in `folder_temps`. It is made for this service's user only, under a unique temporary name
/// committed to `folder_temp_names` before it is made, checked through its open descriptor to be the empty, private
/// directory just made ([`FolderDir::private`]), recorded in `folder_temps` by identity, made durable, and renamed
/// to `name` without replacing anything; its parent is then made durable, and it is used only if what is at `name`
/// is still the directory made, which is then recorded. Nothing is ever removed by name: a temporary folder that was
/// not renamed into place is recorded as a leftover of this store and left. Recovery records one a stop left behind,
/// whichever step it stopped at, from the name or identity committed before it.
pub(super) fn create_dir(
    store: &Store,
    folder: impl Into<Scope>,
    parent: &FolderDir,
    name: &str,
    key: &str,
) -> Result<std::result::Result<FolderDir, String>> {
    let folder: Scope = folder.into();
    let temp = format!("{name}{DIR_TEMP}{}", unique());
    let temp_key = sibling(key, &temp);
    store.write(|tx| {
        tx.execute(
            &folder.sql("INSERT INTO {temp_names} (folder_id, name) VALUES (?1, ?2)"),
            params![folder.id(), temp_key],
        )?;
        Ok(())
    })?;
    // Once something found at the temporary name is not the directory just made, its name is forgotten: it is then
    // never recorded as this store's, and blocks the folder until it is moved out.
    let not_made = |why: String| -> Result<std::result::Result<FolderDir, String>> {
        forget_name(store, folder, &temp_key)?;
        Ok(Err(why))
    };
    if let Err(e) = parent.mkdir_private(&temp) {
        return not_made(e.to_string());
    }
    store.fault_point(if key == STAGING {
        Fault::MadeStaging
    } else {
        Fault::MadeDir
    });
    let made = match parent.sub(&temp) {
        Ok(made) => made,
        Err(e) => return not_made(e.to_string()),
    };
    let id = match made.private(true) {
        Ok(Ok(id)) => id,
        Ok(Err(why)) => {
            return not_made(format!(
                "{}/{temp} is not the folder Title Select just created there: {why}; it was left as it is",
                parent.path().display()
            ));
        }
        Err(e) => return not_made(e.to_string()),
    };
    let (dev, ino) = id.to_sql();
    store.write(|tx| {
        tx.execute(
            &folder.sql("INSERT OR REPLACE INTO {temps} (folder_id, name, dev, ino) VALUES (?1, ?2, ?3, ?4)"),
            params![folder.id(), temp_key, dev, ino],
        )?;
        tx.execute(
            &folder.sql("DELETE FROM {temp_names} WHERE folder_id = ?1 AND name = ?2"),
            params![folder.id(), temp_key],
        )?;
        Ok(())
    })?;
    made.sync().map_err(|e| Error::Io(e.to_string()))?;
    drop(made);
    store.fault_point(if key == STAGING {
        Fault::BeforePublishStaging
    } else {
        Fault::BeforePublishDir
    });
    if let Err(e) = parent.rename_new(&temp, parent, name) {
        let ours =
            matches!(parent.stat(&temp), Ok(Some(st)) if is_dir(&st) && Identity::of(&st) == id);
        if ours {
            record_leftover(store, folder, &temp_key, id)?;
        } else {
            forget_temp(store, folder, &temp_key)?;
        }
        return Ok(Err(match e.kind() {
            io::ErrorKind::AlreadyExists => not_ours(parent, name),
            _ => e.to_string(),
        }));
    }
    parent.sync().map_err(|e| Error::Io(e.to_string()))?;
    let dir = match parent.sub(name) {
        Ok(dir) => dir,
        Err(e) => return Ok(Err(e.to_string())),
    };
    match dir.private(true) {
        Ok(Ok(found)) if found == id => {}
        Ok(_) => {
            forget_temp(store, folder, &temp_key)?;
            return Ok(Err(not_ours(parent, name)));
        }
        Err(e) => return Ok(Err(e.to_string())),
    }
    store.write(|tx| {
        record_made(tx, folder, key, id)?;
        tx.execute(
            &folder.sql("DELETE FROM {temps} WHERE folder_id = ?1 AND name = ?2"),
            params![folder.id(), temp_key],
        )?;
        Ok(())
    })?;
    Ok(Ok(dir))
}

/// Records the directory `key` of Managed Folder `folder`, made as `id` and renamed into place: a Build's staging
/// directory among its temporary entries, which go with it, and any other among the directories that outlive it.
fn record_made(tx: &Transaction, folder: impl Into<Scope>, key: &str, id: Identity) -> Result<()> {
    let folder: Scope = folder.into();
    let table = if key == STAGING { "{temps}" } else { "{dirs}" };
    let (dev, ino) = id.to_sql();
    tx.execute(
        &folder.sql(&format!(
            "INSERT OR REPLACE INTO {table} (folder_id, name, dev, ino) VALUES (?1, ?2, ?3, ?4)"
        )),
        params![folder.id(), key, dev, ino],
    )?;
    Ok(())
}

/// Whether the directory `key` of Managed Folder `folder` is recorded as made, as [`record_made`] records it.
fn is_made(c: &Connection, folder: impl Into<Scope>, key: &str) -> Result<bool> {
    let folder: Scope = folder.into();
    Ok(if key == STAGING {
        scoped_temps(c, folder)?.contains_key(key)
    } else {
        folder_dirs(c, folder)?.contains_key(key)
    })
}

fn forget_name(store: &Store, folder: impl Into<Scope>, key: &str) -> Result<()> {
    let folder: Scope = folder.into();
    store.write(|tx| {
        tx.execute(
            &folder.sql("DELETE FROM {temp_names} WHERE folder_id = ?1 AND name = ?2"),
            params![folder.id(), key],
        )?;
        Ok(())
    })
}

fn forget_temp(store: &Store, folder: impl Into<Scope>, key: &str) -> Result<()> {
    let folder: Scope = folder.into();
    store.write(|tx| {
        tx.execute(
            &folder.sql("DELETE FROM {temps} WHERE folder_id = ?1 AND name = ?2"),
            params![folder.id(), key],
        )?;
        Ok(())
    })
}

/// Records the temporary folder `key`, made as `id` and never renamed into place, as this store's leftover: it is
/// left where it is, and never reported as something no build of this store made.
fn record_leftover(store: &Store, folder: impl Into<Scope>, key: &str, id: Identity) -> Result<()> {
    let folder: Scope = folder.into();
    let (dev, ino) = id.to_sql();
    store.write(|tx| {
        tx.execute(
            &folder.sql("INSERT OR REPLACE INTO {dirs} (folder_id, name, dev, ino) VALUES (?1, ?2, ?3, ?4)"),
            params![folder.id(), key, dev, ino],
        )?;
        tx.execute(
            &folder.sql("DELETE FROM {temps} WHERE folder_id = ?1 AND name = ?2"),
            params![folder.id(), key],
        )?;
        Ok(())
    })
}

/// Opens the directory `name` in `parent`, which `folder_dirs` records under `key`, and checks it is the directory
/// recorded, still private to this service's user ([`FolderDir::private`]). When nothing is there and `create` is
/// set, creates it with [`create_dir`]; otherwise returns `None`.
fn own_dir(
    store: &Store,
    folder: impl Into<Scope>,
    parent: &FolderDir,
    name: &str,
    key: &str,
    create: bool,
) -> Result<std::result::Result<Option<FolderDir>, String>> {
    let folder: Scope = folder.into();
    let recorded = store.read(|c| folder_dirs(c, folder))?.get(key).copied();
    let found = match parent.stat(name) {
        Ok(found) => found,
        Err(e) => return Ok(Err(e.to_string())),
    };
    match found {
        None if !create => Ok(Ok(None)),
        None => Ok(create_dir(store, folder, parent, name, key)?.map(Some)),
        Some(st) => {
            if !is_dir(&st) || recorded != Some(Identity::of(&st)) {
                return Ok(Err(not_ours(parent, name)));
            }
            let dir = match parent.sub(name) {
                Ok(dir) => dir,
                Err(e) => return Ok(Err(e.to_string())),
            };
            match dir.private(false) {
                Ok(Ok(id)) if Some(id) == recorded => Ok(Ok(Some(dir))),
                Ok(Ok(_)) => Ok(Err(not_ours(parent, name))),
                Ok(Err(why)) => Ok(Err(format!(
                    "{}/{name} is no longer private to this service: {why}; it was left as it is",
                    parent.path().display()
                ))),
                Err(e) => Ok(Err(e.to_string())),
            }
        }
    }
}

/// Opens the private folder kept files are deleted from, in the retention area `area` of Managed Folder `folder`.
fn open_deleting(
    store: &Store,
    folder: impl Into<Scope>,
    area: &FolderDir,
    create: bool,
) -> Result<std::result::Result<Option<FolderDir>, String>> {
    let folder: Scope = folder.into();
    own_dir(
        store,
        folder,
        area,
        DELETING,
        &format!("{RETENTION}/{DELETING}"),
        create,
    )
}

/// Opens the retention area of `folder`, through its handle `dir`, and the private folder in it that entries are
/// deleted from; with `create`, creating either that is absent. `None` when one is absent and `create` is not set.
pub(super) fn open_area_deleting(
    store: &Store,
    folder: impl Into<Scope>,
    dir: &FolderDir,
    create: bool,
) -> Result<std::result::Result<Option<FolderDir>, String>> {
    let folder: Scope = folder.into();
    match own_dir(store, folder, dir, RETENTION, RETENTION, create)? {
        Ok(Some(area)) => open_deleting(store, folder, &area, create),
        other => Ok(other.map(|_| None)),
    }
}

/// Settles each directory Title Select was creating in Managed Folder `folder`, opened as `dir`, when it stopped:
/// one still under its temporary name, committed by name before it was made or by identity once it was checked, is
/// recorded as this store's leftover and left, and one already renamed into place, still the directory made, is
/// recorded. Anything else at either name is left as it is and forgotten, and nothing is removed. Run only once the
/// folder's marker shows it is the store's. Returns why the folder could not be read, if it could not.
pub(super) fn settle_dir_temps(
    store: &Store,
    folder: impl Into<Scope>,
    dir: &FolderDir,
) -> Result<std::result::Result<(), String>> {
    let folder: Scope = folder.into();
    let is_pending = |key: &str| {
        let (parent, last) = match key.split_once('/') {
            Some((parent, last)) => (Some(parent), last),
            None => (None, key),
        };
        parent.is_none_or(|p| p == RETENTION)
            && !last.contains('/')
            && (parent.is_some() || is_dir_temp(last))
            && last
                .split_once(DIR_TEMP)
                .is_some_and(|(dest, _)| !dest.is_empty())
    };
    let (temps, names) = store.read(|c| {
        let mut stmt = c.prepare(&folder.sql("SELECT name FROM {temp_names} WHERE folder_id = ?1"))?;
        let names = stmt
            .query_map([folder.id()], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok((scoped_temps(c, folder)?, names))
    })?;
    // A name with no identity yet: committed before its directory was made, which may not have been.
    let mut pending: Vec<(String, Option<Identity>)> = temps
        .into_iter()
        .map(|(key, id)| (key, Some(id)))
        .chain(names.into_iter().map(|key| (key, None)))
        .filter(|(key, _)| is_pending(key))
        .collect();
    // The retention area before what is created in it.
    pending.sort_by_key(|(key, _)| key.contains('/'));
    for (key, id) in pending {
        let area;
        let (parent, last) = match key.split_once('/') {
            None => (dir, key.as_str()),
            Some((_, last)) => match own_dir(store, folder, dir, RETENTION, RETENTION, false)? {
                Ok(Some(found)) => {
                    area = found;
                    (&area, last)
                }
                Ok(None) => {
                    forget_temp(store, folder, &key)?;
                    forget_name(store, folder, &key)?;
                    continue;
                }
                Err(reason) => return Ok(Err(reason)),
            },
        };
        let Some(id) = id else {
            // Made, if at all, by this store's last attempt: recorded only when it is still the empty directory,
            // private to this service, that attempt made.
            let made = match parent.stat(last) {
                Err(e) => return Ok(Err(e.to_string())),
                Ok(None) => None,
                Ok(Some(st)) if !is_dir(&st) => None,
                Ok(Some(_)) => match parent.sub(last).and_then(|made| made.private(true)) {
                    Ok(Ok(id)) => Some(id),
                    Ok(Err(_)) => None,
                    Err(e) => return Ok(Err(e.to_string())),
                },
            };
            if let Some(id) = made {
                record_leftover(store, folder, &key, id)?;
            }
            forget_name(store, folder, &key)?;
            continue;
        };
        let dest = last
            .split_once(DIR_TEMP)
            .map(|(dest, _)| dest)
            .unwrap_or(last);
        let dest_key = sibling(&key, dest);
        match parent.stat(last) {
            Err(e) => return Ok(Err(e.to_string())),
            // Never renamed into place: left as this store's leftover, never removed by name.
            Ok(Some(st)) if is_dir(&st) && Identity::of(&st) == id => {
                record_leftover(store, folder, &key, id)?;
                continue;
            }
            Ok(Some(_)) => {}
            Ok(None) => {
                let recorded = store.read(|c| is_made(c, folder, &dest_key))?;
                let made = matches!(
                    parent.stat(dest),
                    Ok(Some(st)) if is_dir(&st) && Identity::of(&st) == id
                );
                if made && !recorded {
                    parent.sync().map_err(|e| Error::Io(e.to_string()))?;
                    store.write(|tx| record_made(tx, folder, &dest_key, id))?;
                }
            }
        }
        forget_temp(store, folder, &key)?;
    }
    Ok(Ok(()))
}

/// Opens the retention area of Managed Folder `folder`, through its handle `dir`, and the folder of `generation` in
/// it; with `create`, creating either that is absent. `None` when one is absent and `create` is not set.
pub(super) fn open_retention(
    store: &Store,
    folder: impl Into<Scope>,
    dir: &FolderDir,
    generation: u64,
    create: bool,
) -> Result<std::result::Result<Option<Retention>, String>> {
    let folder: Scope = folder.into();
    let area = match own_dir(store, folder, dir, RETENTION, RETENTION, create)? {
        Ok(Some(area)) => area,
        other => return Ok(other.map(|_| None)),
    };
    let name = generation.to_string();
    let key = generation_dir(generation);
    Ok(own_dir(store, folder, &area, &name, &key, create)?
        .map(|found| found.map(|generation| Retention { area, generation })))
}

/// Whether `name` in `dir` is, without following a symlink, the regular file `id` with `file`'s size and
/// modification time.
fn is_kept_file(dir: &FolderDir, name: &str, id: Identity, file: Option<&FileStat>) -> bool {
    is_kept_entry(dir, name, id, file, false)
}

/// Whether `name` in `dir` is, without following a symlink, the regular file, or with `directory` the directory,
/// `id` with `file`'s size and modification time.
pub(super) fn is_kept_entry(
    dir: &FolderDir,
    name: &str,
    id: Identity,
    file: Option<&FileStat>,
    directory: bool,
) -> bool {
    let kind = if directory {
        FileType::Directory
    } else {
        FileType::RegularFile
    };
    matches!(
        dir.stat(name),
        Ok(Some(st)) if FolderDir::kind(&st) == kind
            && Identity::of(&st) == id
            && file.is_none_or(|f| *f == file_stat(&st))
    )
}

/// One file to keep aside: the item `seq` of Build `build`, which keeps it in the folder of `generation`.
pub(super) struct Keep<'a> {
    pub folder: i64,
    pub build: i64,
    pub seq: i64,
    pub generation: u64,
    pub keep: &'a KeepAside,
    /// The SHA-256 of the file, read whole by this Build while it was, before and after the read, as seen: trusted,
    /// rather than read again, while the file is still as seen, change time included.
    pub hashed: Option<(Seen, String)>,
}

/// Records kept file `id` kept: its state, the citations of its provenance while not pruned, and, the file being no
/// longer at its name in the folder, the end of its built file's record. A removal's item is then applied.
pub(super) fn settle_kept(tx: &Transaction, id: i64) -> Result<()> {
    let (folder, build, seq, name): (i64, i64, i64, String) = tx.query_row(
        "SELECT folder_id, build_id, seq, name FROM kept_files WHERE id = ?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    tx.execute(
        "UPDATE kept_files SET state = 'kept', kept_at = ?2, error = NULL WHERE id = ?1",
        params![id, now()],
    )?;
    let key = cited_by(id);
    tx.execute(
        "INSERT OR IGNORE INTO selection_citations (selection_id, cited_by)
         SELECT k.selection_id, ?2 FROM kept_files k JOIN selections s ON s.id = k.selection_id
         WHERE k.id = ?1 AND s.pruned_at IS NULL",
        params![id, key],
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO audit_citations (audit_id, cited_by)
         SELECT k.audit_id, ?2 FROM kept_files k JOIN audits a ON a.id = k.audit_id
         WHERE k.id = ?1 AND a.pruned_at IS NULL",
        params![id, key],
    )?;
    tx.execute(
        "DELETE FROM built_files WHERE folder_id = ?1 AND target = ?2",
        params![folder, name],
    )?;
    tx.execute(
        "UPDATE build_items SET state = 'applied', error = NULL
         WHERE build_id = ?1 AND seq = ?2 AND kind IN ('remove', 'keep_aside')",
        params![build, seq],
    )?;
    // A file kept aside to be repacked is, when its game is selected by the folder's current build, a selected
    // game's file the folder lacks until its repacked copy is put in place: recorded so, in the group the cleanup's
    // plan selects it in, until it is. A built file no selected game's is not something the folder lacks.
    tx.execute(
        "INSERT INTO build_incomplete (build_id, seq, reason, grp, game, target, detail)
         SELECT m.current_build_id,
                (SELECT COALESCE(MAX(i.seq), -1) + 1 FROM build_incomplete i WHERE i.build_id = m.current_build_id),
                'failed',
                COALESCE((SELECT json_extract(r.value, '$.group')
                          FROM builds b JOIN plans p ON p.id = b.plan_id,
                               json_each(p.content, '$.cleanup.repacks') r
                          WHERE b.id = k.build_id AND json_extract(r.value, '$.target') = k.name), ''),
                k.game, k.name, ?2
         FROM kept_files k JOIN managed_folders m ON m.id = k.folder_id
         WHERE k.id = ?1 AND k.reason = 'repack' AND k.game IS NOT NULL AND m.current_build_id IS NOT NULL
           AND EXISTS (SELECT 1 FROM builds c JOIN selection_releases r ON r.selection_id = c.selection_id
                       WHERE c.id = m.current_build_id AND r.role = 'winner'
                         AND lower(r.title || '.zip') = lower(k.name))
           AND NOT EXISTS (SELECT 1 FROM build_incomplete i WHERE i.build_id = m.current_build_id
                           AND lower(i.target) = lower(k.name) AND i.reason = 'failed' AND i.detail = ?2)",
        params![id, REPACK_UNPUBLISHED],
    )?;
    Ok(())
}

/// Why a selected game's file is missing that a cleanup kept aside to repack, while its repacked copy is not in place.
pub(super) const REPACK_UNPUBLISHED: &str =
    "it was kept aside to be repacked, but the repacked copy is not in place";

impl Store {
    /// Keeps aside, through the folder's handle `dir`, the file `k.keep` names: only while it is the regular file the
    /// plan found there, with the SHA-256 the plan recorded, if it did. Its intent is recorded first; it is then
    /// moved by one rename that never replaces anything into the retention area `retention`, and recorded kept once
    /// it is found there. Returns the kept file's id, or, when it was left in place, why; an error stops the Build.
    pub(super) fn keep_aside(
        &self,
        dir: &FolderDir,
        retention: &Retention,
        k: &Keep,
    ) -> Result<std::result::Result<i64, String>> {
        let name = k.keep.name.as_str();
        let id = Identity::from(k.keep.identity);
        let directory = k.keep.directory;
        let left = |why: &str| {
            Ok(Err(format!(
                "{name} was not kept aside: {why}; it was left as it is"
            )))
        };
        if !is_kept_entry(dir, name, id, Some(&k.keep.file), directory) {
            return left("it changed since you approved the plan");
        }
        let seen = |dir: &FolderDir| match dir.stat(name) {
            Ok(Some(st)) => Some(Seen::of(&st)),
            _ => None,
        };
        let trusted = k
            .hashed
            .as_ref()
            .filter(|(was, _)| !directory && seen(dir).as_ref() == Some(was));
        // A directory is moved whole, by identity, and never read.
        let sha256 = if directory {
            None
        } else if let Some((_, sha256)) = trusted {
            if k.keep.sha256.as_ref().is_some_and(|s| s != sha256) {
                return left("it is not the file the plan was made with");
            }
            Some(sha256.clone())
        } else {
            self.fault_point(Fault::KeepRead(k.seq));
            let file = match dir.open_file(name) {
                Ok(file) => file,
                Err(e) => return left(&e.to_string()),
            };
            let same =
                |st: &rustix::fs::Stat| Identity::of(st) == id && file_stat(st) == k.keep.file;
            match rustix::fs::fstat(&file) {
                Ok(st) if same(&st) => {}
                _ => return left("it changed while it was read"),
            }
            let (sha256, _) = match sha256_of(&file) {
                Ok(read) => read,
                Err(e) => return left(&e.to_string()),
            };
            match rustix::fs::fstat(&file) {
                Ok(st) if same(&st) && is_kept_file(dir, name, id, Some(&k.keep.file)) => {}
                _ => return left("it changed while it was read"),
            }
            drop(file);
            if k.keep.sha256.as_ref().is_some_and(|s| *s != sha256) {
                return left("it is not the file the plan was made with");
            }
            Some(sha256)
        };
        let size = k.keep.file.size;
        let taken: BTreeSet<String> = self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT kept_name FROM kept_files WHERE folder_id = ?1 AND generation = ?2
                 AND state IN ('keeping', 'kept', 'deleting', 'restoring')",
            )?;
            let rows = stmt.query_map(params![k.folder, k.generation as i64], |r| r.get(0))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })?;
        let mut nth = 1;
        let kept = loop {
            let candidate = kept_name(name, nth);
            match retention.generation.stat(&candidate) {
                Ok(None) if !taken.contains(&candidate) => break candidate,
                Ok(_) => nth += 1,
                Err(e) => return Err(Error::Io(e.to_string())),
            }
        };
        let (dev, ino) = id.to_sql();
        let row = self.write(|tx| {
            tx.execute(
                "INSERT INTO kept_files (folder_id, build_id, seq, generation, name, kept_name, reason, built_by, game,
                                         roms, selection_id, audit_id, dat_revision_id, sha256, size, mtime_ns, dev,
                                         ino, state, created_at, directory)
                 SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, f.build_id, f.game, f.roms, f.selection_id, f.audit_id,
                        f.dat_revision_id, ?8, ?9, ?10, ?11, ?12, 'keeping', ?13, ?14
                 FROM (SELECT 1) LEFT JOIN built_files f ON f.folder_id = ?1 AND f.target = ?5
                     AND ?7 IN ('removal', 'replacement', 'repack')",
                params![
                    k.folder,
                    k.build,
                    k.seq,
                    k.generation as i64,
                    name,
                    kept,
                    k.keep.reason.as_str(),
                    sha256,
                    size as i64,
                    k.keep.file.mtime_ns as i64,
                    dev,
                    ino,
                    now(),
                    directory
                ],
            )?;
            Ok(tx.last_insert_rowid())
        })?;
        self.fault_point(Fault::BeforeKeep(k.seq));
        let forget = |why: String| -> Result<std::result::Result<i64, String>> {
            self.write(|tx| {
                tx.execute("DELETE FROM kept_files WHERE id = ?1", [row])?;
                Ok(())
            })?;
            Ok(Err(why))
        };
        if !is_kept_entry(dir, name, id, Some(&k.keep.file), directory) {
            return forget(format!(
                "{name} was not kept aside: it changed while it was read; it was left as it is"
            ));
        }
        // A hash this step did not read is trusted only while nothing about the file has changed since it was read.
        if let Some((was, sha256)) = trusted
            && seen(dir).as_ref() != Some(was)
        {
            self.fault_point(Fault::KeepRead(k.seq));
            let read = dir.open_file(name).and_then(|file| {
                let before = rustix::fs::fstat(&file)?;
                let (read, _) = sha256_of(&file)?;
                let after = rustix::fs::fstat(&file)?;
                Ok((Seen::of(&before) == Seen::of(&after)
                    && Identity::of(&after) == id
                    && file_stat(&after) == k.keep.file)
                    .then_some(read))
            });
            if !matches!(read, Ok(Some(ref read)) if read == sha256)
                || !is_kept_entry(dir, name, id, Some(&k.keep.file), directory)
            {
                return forget(format!(
                    "{name} was not kept aside: it changed while it was read; it was left as it is"
                ));
            }
        }
        if let Err(e) = dir.rename_new(name, &retention.generation, &kept) {
            return match e.kind() {
                io::ErrorKind::AlreadyExists => forget(format!(
                    "{name} was not kept aside: {kept} appeared in the retention area; it was left as it is"
                )),
                _ => {
                    let _ = forget(e.to_string())?;
                    Err(Error::Io(e.to_string()))
                }
            };
        }
        self.fault_point(Fault::KeptUnsynced(k.seq));
        retention
            .generation
            .sync()
            .and_then(|()| dir.sync())
            .map_err(|e| Error::Io(e.to_string()))?;
        self.fault_point(Fault::AfterKeep(k.seq));
        if !is_kept_entry(&retention.generation, &kept, id, None, directory) {
            let error = format!(
                "{}/{kept} is not the file moved there; it was left as it is",
                retention.generation.path().display()
            );
            self.write(|tx| {
                tx.execute(
                    "UPDATE kept_files SET state = 'lost', error = ?2 WHERE id = ?1",
                    params![row, error],
                )?;
                Ok(())
            })?;
            return Err(Error::Refused(error));
        }
        self.write(|tx| settle_kept(tx, row))?;
        Ok(Ok(row))
    }

    /// Settles each kept file Build `build` was keeping aside when it stopped, from what is in its folder, opened as
    /// `dir`: kept when the kept entry is the file, dropped when the file is still in place, and lost, and reported
    /// on its item, when it is neither. Returns why the folder could not be read, if it could not.
    pub(super) fn settle_keeping(
        &self,
        dir: &FolderDir,
        folder: i64,
        build: i64,
    ) -> Result<std::result::Result<(), String>> {
        type Row = (i64, i64, u64, String, String, Identity, bool);
        let keeping: Vec<Row> = self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT id, seq, generation, name, kept_name, dev, ino, directory FROM kept_files
                 WHERE build_id = ?1 AND state = 'keeping' ORDER BY id",
            )?;
            let rows = stmt.query_map([build], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get::<_, i64>(2)? as u64,
                    r.get(3)?,
                    r.get(4)?,
                    Identity::from_sql(r.get(5)?, r.get(6)?),
                    r.get(7)?,
                ))
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })?;
        for (id, seq, generation, name, kept, identity, directory) in keeping {
            let retention = match open_retention(self, folder, dir, generation, false)? {
                Ok(retention) => retention,
                Err(reason) => return Ok(Err(reason)),
            };
            let moved = retention
                .as_ref()
                .is_some_and(|r| is_kept_entry(&r.generation, &kept, identity, None, directory));
            let in_place = is_kept_entry(dir, &name, identity, None, directory);
            // The move is recorded only once it is durable: the build may have stopped before it synced either folder.
            if let (true, Some(r)) = (moved, &retention)
                && let Err(e) = r.generation.sync().and_then(|()| dir.sync())
            {
                return Ok(Err(e.to_string()));
            }
            self.write(|tx| {
                let note = |state: &str, error: String| {
                    tx.execute(
                        "UPDATE build_items SET state = ?3, error = ?4
                         WHERE build_id = ?1 AND seq = ?2 AND state IN ('pending', 'publishing')",
                        params![build, seq, state, error],
                    )
                };
                if moved {
                    settle_kept(tx, id)?;
                    tx.execute(
                        "UPDATE build_items SET error = ?3 WHERE build_id = ?1 AND seq = ?2 AND kind = 'remove'",
                        params![
                            build,
                            seq,
                            format!(
                                "the build stopped while keeping {name} aside; it is kept as \
                                 {RETENTION}/{generation}/{kept}"
                            )
                        ],
                    )?;
                    note(
                        "failed",
                        format!(
                            "the build stopped after keeping {name} aside as {RETENTION}/{generation}/{kept}, before \
                             it wrote the new file"
                        ),
                    )?;
                } else if in_place {
                    tx.execute("DELETE FROM kept_files WHERE id = ?1", [id])?;
                    note(
                        "failed",
                        format!("the build stopped before it kept {name} aside; it is still in place"),
                    )?;
                } else {
                    let error = format!(
                        "the build stopped while keeping {name} aside, and it is now neither in place nor at \
                         {RETENTION}/{generation}/{kept}; something else moved it"
                    );
                    tx.execute(
                        "UPDATE kept_files SET state = 'lost', error = ?2 WHERE id = ?1",
                        params![id, error],
                    )?;
                    note("uncertain", error)?;
                }
                Ok(())
            })?;
        }
        Ok(Ok(()))
    }

    /// The kept files of Managed Folder `folder`, by generation, newest first.
    pub fn kept_files(&self, folder: i64) -> Result<Vec<KeptFile>> {
        self.read(|c| {
            folder_by_id(c, folder)?;
            kept_where(c, "k.folder_id = ?1 AND k.state = 'kept'", [folder])
        })
    }

    /// Plans deleting the kept files of Managed Folder `folder` that `request` picks, and stores the plan for review.
    /// Nothing is deleted until you approve it.
    pub fn plan_kept_deletion(
        &self,
        folder: i64,
        request: &PlanKeptDeletion,
    ) -> Result<KeptDeletion> {
        self.write(|tx| {
            folder_by_id(tx, folder)?;
            let kept = kept_where(tx, "k.folder_id = ?1 AND k.state = 'kept'", [folder])?;
            // A kept directory is never deleted: what is in it was never read, so nothing could show it is still
            // what was kept.
            let files: Vec<KeptFile> = if request.all {
                kept.into_iter().filter(|k| !k.directory).collect()
            } else {
                let mut files = Vec::new();
                for id in request.ids.iter().collect::<BTreeSet<_>>() {
                    match kept.iter().find(|k| k.id == *id) {
                        Some(k) if k.directory => {
                            return Err(Error::Refused(format!(
                                "kept file {id} ({}) is a folder, which Title Select does not delete; put it back with \
                                 a cleanup and remove it yourself if you no longer want it",
                                k.name
                            )));
                        }
                        Some(k) => files.push(k.clone()),
                        None => {
                            return Err(Error::Refused(format!(
                                "managed folder {folder} has no kept file {id}"
                            )));
                        }
                    }
                }
                files
            };
            if files.is_empty() {
                return Err(Error::Refused("pick at least one kept file to delete".into()));
            }
            let content = KeptDeletionContent { folder, files };
            tx.execute(
                "INSERT INTO kept_deletions (folder_id, content, digest, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![folder, to_json(&content), digest(&content), now()],
            )?;
            deletion_by_id(tx, tx.last_insert_rowid())
        })
    }

    pub fn kept_deletion(&self, id: i64) -> Result<KeptDeletion> {
        self.read(|c| deletion_by_id(c, id))
    }

    /// Approves deleting kept files as reviewed with `digest`, and carries it out. Each file is deleted only while
    /// its kept entry is the file that was kept; anything else there is left, and its outcome says why. Refused while
    /// a Build of the folder is queued or running, and when the kept files changed since the plan.
    pub fn approve_kept_deletion(
        &self,
        config: &Config,
        id: i64,
        digest_reviewed: &str,
        actor: &str,
    ) -> Result<KeptDeletion> {
        let plan = self.kept_deletion(id)?;
        if plan.approval.is_some() {
            return Err(Error::Refused(format!("deletion {id} is already approved")));
        }
        if digest_reviewed != plan.digest {
            return Err(Error::Conflict(format!(
                "you reviewed a different version of deletion {id}; review it again"
            )));
        }
        let content = &plan.content;
        let folder = content.folder;
        self.write(|tx| {
            let active: Option<i64> = tx
                .query_row(
                    "SELECT id FROM jobs WHERE managed_folder_id = ?1 AND kind = 'build'
                     AND state IN ('queued', 'running')",
                    [folder],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(job) = active {
                return Err(Error::Conflict(format!(
                    "managed folder {folder} has a build queued or running: job {job}"
                )));
            }
            if let Some(busy) = deletion_active(tx, folder)? {
                return Err(Error::Conflict(busy));
            }
            let ids: Vec<i64> = content.files.iter().map(|f| f.id).collect();
            let now_kept = kept_where(tx, "k.folder_id = ?1 AND k.state = 'kept'", [folder])?;
            let still: Vec<&KeptFile> = ids
                .iter()
                .filter_map(|id| now_kept.iter().find(|k| k.id == *id))
                .collect();
            if still.len() != ids.len() || still.iter().zip(&content.files).any(|(a, b)| *a != b) {
                return Err(Error::Conflict(format!(
                    "the kept files changed since you reviewed deletion {id}; plan it again"
                )));
            }
            tx.execute(
                "UPDATE kept_deletions SET approval_digest = ?2, actor = ?3, approved_at = ?4 WHERE id = ?1",
                params![id, plan.digest, actor, now()],
            )?;
            Ok(())
        })?;
        let outcomes = match self.delete_kept(config, id, content) {
            Ok(outcomes) => outcomes,
            // A kept file may have been mid-deletion: the deletion stays unfinished, and recovery settles it.
            Err(err) => {
                self.write(|tx| {
                    tx.execute(
                        "UPDATE kept_deletions SET recovery_error = ?2 WHERE id = ?1",
                        params![id, err.to_string()],
                    )?;
                    Ok(())
                })?;
                return Err(err);
            }
        };
        let pending = self.read(|c| unresolved(c, id, &outcomes))?;
        self.write(|tx| {
            match &pending {
                None => tx.execute(
                    "UPDATE kept_deletions SET finished_at = ?2, outcomes = ?3, recovery_error = NULL WHERE id = ?1",
                    params![id, now(), to_json(&outcomes)],
                )?,
                // Not finished while what was moved aside is not back where it was found.
                Some(reason) => tx.execute(
                    "UPDATE kept_deletions SET outcomes = ?2, recovery_error = ?3 WHERE id = ?1",
                    params![id, to_json(&outcomes), reason],
                )?,
            };
            deletion_by_id(tx, id)
        })
    }

    fn delete_kept(
        &self,
        config: &Config,
        deletion: i64,
        content: &KeptDeletionContent,
    ) -> Result<Vec<KeptDeletionOutcome>> {
        let folder = content.folder;
        let (row, store) = self.read(|c| Ok((folder_by_id(c, folder)?, store_id(c)?)))?;
        let dir = match open_writable_folder(config, &row.root, &row.path) {
            Ok(dir) => dir,
            Err(reason) => {
                return Ok(content
                    .files
                    .iter()
                    .map(|f| left(f, reason.clone()))
                    .collect());
            }
        };
        let expected = Marker {
            store,
            folder,
            generation: row.generation,
        };
        let mut outcomes = Vec::new();
        for file in &content.files {
            if let Err(reason) = check_marker(&dir, &expected) {
                outcomes.push(left(file, reason));
                continue;
            }
            outcomes.push(
                match open_retention(self, folder, &dir, file.generation, false)? {
                    Ok(Some(retention)) => self.delete_one(&dir, &retention, file, deletion)?,
                    Ok(None) => self.gone(&dir, file, deletion)?,
                    Err(reason) => left(file, reason),
                },
            );
        }
        Ok(outcomes)
    }

    /// Deletes kept file `file` from `retention`, for deletion `deletion`: only while its kept entry is the file that
    /// was kept. The intent is committed first; the file is then moved, by one rename that never replaces anything,
    /// into the private folder kept files are deleted from, and deleted there only once what was moved is found to be
    /// the file. Anything else moved is recorded where it is, then put back.
    fn delete_one(
        &self,
        dir: &FolderDir,
        retention: &Retention,
        file: &KeptFile,
        deletion: i64,
    ) -> Result<KeptDeletionOutcome> {
        let id = Identity::from(file.identity);
        let (gen_dir, kept) = (&retention.generation, file.kept_name.as_str());
        match gen_dir.stat(kept) {
            Err(e) => return Ok(left(file, e.to_string())),
            Ok(None) => return self.gone(dir, file, deletion),
            Ok(Some(_)) if !is_kept_file(gen_dir, kept, id, Some(&file.file)) => {
                return Ok(left(file, NOT_THE_FILE.into()));
            }
            Ok(Some(_)) => {}
        }
        let deleting = match open_deleting(self, file.folder, &retention.area, true)? {
            Ok(Some(deleting)) => deleting,
            Ok(None) => return Ok(left(file, "it could not be moved to be deleted".into())),
            Err(reason) => return Ok(left(file, reason)),
        };
        let slot = file.id.to_string();
        match deleting.stat(&slot) {
            Ok(None) => {}
            Ok(Some(_)) => return Ok(left(file, slot_taken(&slot))),
            Err(e) => return Ok(left(file, e.to_string())),
        }
        self.write(|tx| {
            tx.execute(
                "UPDATE kept_files SET state = 'deleting', deletion_id = ?2 WHERE id = ?1 AND state = 'kept'",
                params![file.id, deletion],
            )?;
            Ok(())
        })?;
        let keep = |why: String| -> Result<KeptDeletionOutcome> {
            self.write(|tx| still_kept(tx, file.id))?;
            Ok(left(file, why))
        };
        if !is_kept_file(gen_dir, kept, id, Some(&file.file)) {
            return keep("it changed while it was deleted, so it is never deleted".into());
        }
        self.fault_point(Fault::BeforeDeleteMove(file.id));
        if let Err(e) = gen_dir.rename_new(kept, &deleting, &slot) {
            return keep(format!(
                "it could not be moved to be deleted ({e}), so it is never deleted"
            ));
        }
        gen_dir
            .sync()
            .and_then(|()| deleting.sync())
            .map_err(|e| Error::Io(e.to_string()))?;
        self.fault_point(Fault::AfterDeleteMove(file.id));
        if !is_kept_file(&deleting, &slot, id, Some(&file.file)) {
            let note = self.quarantine(Some(gen_dir), &deleting, file)?;
            return Ok(left(file, format!("{MOVED_NOT_THE_FILE}; {note}")));
        }
        if let Err(e) = deleting.unlink(&slot) {
            let note = self.quarantine(Some(gen_dir), &deleting, file)?;
            return Ok(left(file, format!("it could not be deleted ({e}); {note}")));
        }
        deleting.sync().map_err(|e| Error::Io(e.to_string()))?;
        self.fault_point(Fault::AfterUnlink(file.id));
        self.write(|tx| deleted(tx, file.id))?;
        Ok(deleted_outcome(file))
    }

    /// Records what is at `.deleting/<id>` for kept file `file`, which is not to be deleted, by its identity, keeping
    /// the file's deletion; then moves it back to the file's kept entry. Until that move is made durable, the record
    /// says where the entry is, and the deletion is not finished. Returns where the entry is.
    fn quarantine(
        &self,
        generation: Option<&FolderDir>,
        deleting: &FolderDir,
        file: &KeptFile,
    ) -> Result<String> {
        let slot = file.id.to_string();
        let moved = match deleting.stat(&slot) {
            Ok(Some(st)) => Identity::of(&st),
            Ok(None) => {
                self.write(|tx| still_kept(tx, file.id))?;
                return Ok("what was moved aside to be deleted is gone".into());
            }
            Err(e) => return Ok(format!("it is at {RETENTION}/{DELETING}/{slot} ({e})")),
        };
        let (dev, ino) = moved.to_sql();
        self.write(|tx| {
            tx.execute(
                "UPDATE kept_files SET state = 'quarantined', quarantine_dev = ?2, quarantine_ino = ?3
                 WHERE id = ?1 AND state IN ('deleting', 'quarantined')",
                params![file.id, dev, ino],
            )?;
            Ok(())
        })?;
        self.fault_point(Fault::Quarantined(file.id));
        self.put_back(generation, deleting, file, moved)
    }

    /// Moves what is recorded at `.deleting/<id>` for kept file `file`, as `moved`, back to the file's kept entry in
    /// `generation`, without replacing anything, and records the file kept once that is durable. Returns where it is.
    fn put_back(
        &self,
        generation: Option<&FolderDir>,
        deleting: &FolderDir,
        file: &KeptFile,
        moved: Identity,
    ) -> Result<String> {
        let slot = file.id.to_string();
        let kept = file.kept_name.as_str();
        let at = format!("{RETENTION}/{DELETING}/{slot}");
        let back = format!("{RETENTION}/{}/{kept}", file.generation);
        let found = match deleting.stat(&slot) {
            Ok(found) => found,
            Err(e) => return Ok(format!("it is at {at} ({e})")),
        };
        let Some(st) = found else {
            // Moved back before the service stopped, perhaps not yet durably: cleared only once it is found back
            // under its name and both folders are synced, so a power loss cannot undo a move-back already recorded.
            let Some(generation) = generation else {
                return Ok(format!(
                    "it is no longer at {at}, and {back} is gone; it stays recorded as held aside"
                ));
            };
            let found = match generation.stat(kept) {
                Ok(found) => found,
                Err(e) => {
                    return Ok(format!(
                        "it is no longer at {at}, and {back} could not be checked ({e}); it stays recorded as held \
                         aside"
                    ));
                }
            };
            match found {
                Some(st) if Identity::of(&st) == moved => {}
                Some(_) => {
                    return Ok(format!(
                        "it is no longer at {at}, and {back} holds something else; it stays recorded as held aside"
                    ));
                }
                None => {
                    return Ok(format!(
                        "it is no longer at {at} nor at {back}; it stays recorded as held aside"
                    ));
                }
            }
            if let Err(e) = generation.sync().and_then(|()| deleting.sync()) {
                return Ok(format!(
                    "it was moved back to {back}, but that could not be made durable ({e}); the service tries again \
                     when it next starts"
                ));
            }
            self.fault_point(Fault::MoveBackSynced(file.id));
            self.write(|tx| still_kept(tx, file.id))?;
            return Ok(format!("it was moved back to {back}"));
        };
        if Identity::of(&st) != moved {
            return Ok(format!(
                "{at} now holds something else again; it is left there until it can be moved back to {back}"
            ));
        }
        let Some(generation) = generation else {
            return Ok(format!(
                "it is at {at}, and {back} is gone; it is left there until it can be moved back"
            ));
        };
        match deleting
            .rename_new(&slot, generation, kept)
            .map(|()| self.fault_point(Fault::MovedBack(file.id)))
            .and_then(|()| generation.sync())
            .and_then(|()| deleting.sync())
        {
            Ok(()) => {
                self.fault_point(Fault::MoveBackSynced(file.id));
                self.write(|tx| still_kept(tx, file.id))?;
                Ok(format!("it was moved back to {back}"))
            }
            Err(e) => Ok(format!(
                "it is at {at}, and could not be moved back to {back} ({e}); move what is at {back} out of the way, \
                 and the service moves it back when it next starts"
            )),
        }
    }

    /// Records kept file `file`, found gone when deletion `deletion` was carried out, no longer kept; but only while
    /// nothing moved aside for it is held in the private folder kept files are deleted from.
    fn gone(&self, dir: &FolderDir, file: &KeptFile, deletion: i64) -> Result<KeptDeletionOutcome> {
        let area = match own_dir(self, file.folder, dir, RETENTION, RETENTION, false)? {
            Ok(area) => area,
            Err(reason) => return Ok(left(file, reason)),
        };
        if let Some(area) = area {
            match open_deleting(self, file.folder, &area, false)? {
                Ok(None) => {}
                Ok(Some(deleting)) => match deleting.stat(&file.id.to_string()) {
                    Ok(None) => {}
                    Ok(Some(_)) => return Ok(left(file, slot_taken(&file.id.to_string()))),
                    Err(e) => return Ok(left(file, e.to_string())),
                },
                Err(reason) => return Ok(left(file, reason)),
            }
        }
        self.write(|tx| {
            tx.execute(
                "UPDATE kept_files SET state = 'gone', deletion_id = ?2, deleted_at = ?3, error = ?4
                 WHERE id = ?1 AND state IN ('kept', 'deleting')",
                params![
                    file.id,
                    deletion,
                    now(),
                    "it was gone when its deletion was carried out"
                ],
            )?;
            drop_citations(tx, file.id)
        })?;
        Ok(gone_outcome(file))
    }

    /// Run when the service starts, after [`Store::recover_builds`]: finishes each approved deletion of kept files
    /// the service stopped while carrying out, or could not finish, settling each kept file it was deleting from where
    /// that file is now, and moving back what was moved aside. Nothing is settled unless the folder's root is writable
    /// and its marker shows it is the store's folder; a deletion left so keeps its kept files recorded and its
    /// folder's plans blocked until the next start. Returns, for each, the deletion and why.
    pub fn recover_kept_deletions(&self, config: &Config) -> Result<Vec<(i64, String)>> {
        let unfinished: Vec<(i64, i64)> = self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT id, folder_id FROM kept_deletions WHERE approved_at IS NOT NULL AND finished_at IS NULL",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })?;
        let mut blocked = Vec::new();
        for (deletion, folder) in unfinished {
            let error = self.finish_deletion(config, deletion, folder)?.err();
            self.write(|tx| {
                tx.execute(
                    "UPDATE kept_deletions SET recovery_error = ?2 WHERE id = ?1",
                    params![deletion, error],
                )?;
                Ok(())
            })?;
            if let Some(reason) = error {
                blocked.push((deletion, reason));
            }
        }
        Ok(blocked)
    }

    fn finish_deletion(
        &self,
        config: &Config,
        deletion: i64,
        folder: i64,
    ) -> Result<std::result::Result<(), String>> {
        let plan = self.kept_deletion(deletion)?;
        let (row, store) = self.read(|c| Ok((folder_by_id(c, folder)?, store_id(c)?)))?;
        let dir = match open_writable_folder(config, &row.root, &row.path) {
            Ok(dir) => dir,
            Err(reason) => return Ok(Err(reason)),
        };
        let expected = Marker {
            store,
            folder,
            generation: row.generation,
        };
        if let Err(reason) = check_marker(&dir, &expected) {
            return Ok(Err(reason));
        }
        if let Err(reason) = settle_dir_temps(self, folder, &dir)? {
            return Ok(Err(reason));
        }
        let unsettled: Vec<KeptFile> = self.read(|c| {
            kept_where(
                c,
                "k.deletion_id = ?1 AND k.state IN ('deleting', 'quarantined')",
                [deletion],
            )
        })?;
        let mut notes = HashMap::new();
        for file in unsettled {
            if let Err(reason) = check_marker(&dir, &expected) {
                return Ok(Err(reason));
            }
            match self.settle_deleting(&dir, &file)? {
                Ok(Some(note)) => {
                    notes.insert(file.id, note);
                }
                Ok(None) => {}
                Err(reason) => return Ok(Err(reason)),
            }
        }
        let states: BTreeMap<i64, String> = self.read(|c| {
            let mut stmt = c.prepare("SELECT id, state FROM kept_files WHERE deletion_id = ?1")?;
            let rows = stmt.query_map([deletion], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })?;
        let outcomes: Vec<KeptDeletionOutcome> = plan
            .content
            .files
            .iter()
            .map(|f| match states.get(&f.id).map(String::as_str) {
                Some("deleted") => deleted_outcome(f),
                Some("gone") => gone_outcome(f),
                _ => left(
                    f,
                    notes
                        .remove(&f.id)
                        .unwrap_or_else(|| "the service stopped before it was deleted".into()),
                ),
            })
            .collect();
        if let Some(reason) = self.read(|c| unresolved(c, deletion, &outcomes))? {
            self.write(|tx| {
                tx.execute(
                    "UPDATE kept_deletions SET outcomes = ?2 WHERE id = ?1",
                    params![deletion, to_json(&outcomes)],
                )?;
                Ok(())
            })?;
            return Ok(Err(reason));
        }
        self.write(|tx| {
            tx.execute(
                "UPDATE kept_deletions SET finished_at = ?2, outcomes = ?3 WHERE id = ?1",
                params![deletion, now(), to_json(&outcomes)],
            )?;
            Ok(())
        })?;
        Ok(Ok(()))
    }

    /// Settles kept file `file`, which a deletion was deleting, or had moved something aside for, when the service
    /// stopped, in its folder opened as `dir`: deleted once it is found in the private folder it is deleted from, or
    /// gone from its kept entry; still kept when it is at its kept entry. Anything else moved aside is recorded where
    /// it is and moved back; it is never deleted. Returns a note for its outcome, if it is not deleted, or why the
    /// folder could not be read.
    fn settle_deleting(
        &self,
        dir: &FolderDir,
        file: &KeptFile,
    ) -> Result<std::result::Result<Option<String>, String>> {
        let folder = file.folder;
        let id = Identity::from(file.identity);
        let slot = file.id.to_string();
        let quarantined: Option<Identity> = self.read(|c| {
            Ok(c.query_row(
                "SELECT quarantine_dev, quarantine_ino FROM kept_files WHERE id = ?1 AND state = 'quarantined'",
                [file.id],
                |r| Ok(Identity::from_sql(r.get(0)?, r.get(1)?)),
            )
            .optional()?)
        })?;
        let area = match own_dir(self, folder, dir, RETENTION, RETENTION, false)? {
            Ok(Some(area)) => area,
            Ok(None) if quarantined.is_some() => {
                return Ok(Err(format!(
                    "{RETENTION} is gone, and with it what was moved aside while deleting {}",
                    file.kept_name
                )));
            }
            Ok(None) => {
                self.write(|tx| deleted(tx, file.id))?;
                return Ok(Ok(None));
            }
            Err(reason) => return Ok(Err(reason)),
        };
        let deleting = match open_deleting(self, folder, &area, false)? {
            Ok(deleting) => deleting,
            Err(reason) => return Ok(Err(reason)),
        };
        let name = file.generation.to_string();
        let generation = match own_dir(
            self,
            folder,
            &area,
            &name,
            &generation_dir(file.generation),
            false,
        )? {
            Ok(generation) => generation,
            Err(reason) => return Ok(Err(reason)),
        };
        if let Some(moved) = quarantined {
            let Some(deleting) = &deleting else {
                self.write(|tx| still_kept(tx, file.id))?;
                return Ok(Ok(Some(format!(
                    "{MOVED_NOT_THE_FILE}; it is no longer at {RETENTION}/{DELETING}/{slot}"
                ))));
            };
            let note = self.put_back(generation.as_ref(), deleting, file, moved)?;
            return Ok(Ok(Some(format!("{MOVED_NOT_THE_FILE}; {note}"))));
        }
        let kept = |note: &str| -> Result<std::result::Result<Option<String>, String>> {
            self.write(|tx| still_kept(tx, file.id))?;
            Ok(Ok(Some(note.to_owned())))
        };
        if let Some(deleting) = &deleting {
            match deleting.stat(&slot) {
                Err(e) => return Ok(Err(e.to_string())),
                Ok(Some(_)) if is_kept_file(deleting, &slot, id, Some(&file.file)) => {
                    if let Err(e) = deleting.unlink(&slot).and_then(|()| deleting.sync()) {
                        return Ok(Err(format!(
                            "{}/{slot} could not be deleted ({e})",
                            deleting.path().display()
                        )));
                    }
                    self.write(|tx| deleted(tx, file.id))?;
                    return Ok(Ok(None));
                }
                Ok(Some(_)) => {
                    let note = self.quarantine(generation.as_ref(), deleting, file)?;
                    return Ok(Ok(Some(format!("{MOVED_NOT_THE_FILE}; {note}"))));
                }
                Ok(None) => {}
            }
        }
        let Some(generation) = generation else {
            self.write(|tx| deleted(tx, file.id))?;
            return Ok(Ok(None));
        };
        match generation.stat(&file.kept_name) {
            Err(e) => Ok(Err(e.to_string())),
            Ok(None) => {
                self.write(|tx| deleted(tx, file.id))?;
                Ok(Ok(None))
            }
            Ok(Some(_)) if is_kept_file(&generation, &file.kept_name, id, Some(&file.file)) => {
                kept("the service stopped before it was deleted")
            }
            Ok(Some(_)) => kept(NOT_THE_FILE),
        }
    }
}

const NOT_THE_FILE: &str =
    "what is there now is not the file that was kept, so it is never deleted";
const MOVED_NOT_THE_FILE: &str =
    "what was moved to be deleted is not the file that was kept, so it is never deleted";

fn slot_taken(slot: &str) -> String {
    format!(
        "something is at {RETENTION}/{DELETING}/{slot}, where it would be moved to be deleted, so it is never deleted"
    )
}

/// Why deletion `deletion`, whose outcomes are `outcomes`, is not finished: a kept file it was deleting, or held
/// something aside for, that is not yet settled.
fn unresolved(
    c: &Connection,
    deletion: i64,
    outcomes: &[KeptDeletionOutcome],
) -> Result<Option<String>> {
    let mut stmt = c.prepare(
        "SELECT id FROM kept_files WHERE deletion_id = ?1 AND state IN ('deleting', 'quarantined') ORDER BY id",
    )?;
    let ids: Vec<i64> = stmt
        .query_map([deletion], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    if ids.is_empty() {
        return Ok(None);
    }
    Ok(Some(
        ids.iter()
            .map(|id| match outcomes.iter().find(|o| o.id == *id) {
                Some(o) => format!(
                    "{RETENTION}/{}/{}: {}",
                    o.generation,
                    o.kept_name,
                    o.detail.as_deref().unwrap_or("it was being deleted")
                ),
                None => format!("kept file {id} was being deleted"),
            })
            .collect::<Vec<_>>()
            .join("; "),
    ))
}

/// Records kept file `id`, which was being deleted, still kept.
fn still_kept(tx: &Transaction, id: i64) -> Result<()> {
    tx.execute(
        "UPDATE kept_files SET state = 'kept', deletion_id = NULL, quarantine_dev = NULL, quarantine_ino = NULL
         WHERE id = ?1",
        [id],
    )?;
    Ok(())
}

fn deleted_outcome(file: &KeptFile) -> KeptDeletionOutcome {
    KeptDeletionOutcome {
        id: file.id,
        kept_name: file.kept_name.clone(),
        generation: file.generation,
        deleted: true,
        detail: None,
    }
}

fn gone_outcome(file: &KeptFile) -> KeptDeletionOutcome {
    KeptDeletionOutcome {
        id: file.id,
        kept_name: file.kept_name.clone(),
        generation: file.generation,
        deleted: false,
        detail: Some("it is gone, so it is no longer listed as kept".into()),
    }
}

/// The outcome of a kept file left as it is, and why.
fn left(file: &KeptFile, why: String) -> KeptDeletionOutcome {
    KeptDeletionOutcome {
        id: file.id,
        kept_name: file.kept_name.clone(),
        generation: file.generation,
        deleted: false,
        detail: Some(format!("{why}; it was left as it is")),
    }
}

/// Records kept file `id` deleted, and drops what it cited.
fn deleted(tx: &Transaction, id: i64) -> Result<()> {
    tx.execute(
        "UPDATE kept_files SET state = 'deleted', deleted_at = ?2 WHERE id = ?1",
        params![id, now()],
    )?;
    drop_citations(tx, id)
}

/// Drops what kept file `id` cited, so that pruning no longer holds it.
pub(super) fn drop_citations(tx: &Transaction, id: i64) -> Result<()> {
    let key = cited_by(id);
    tx.execute(
        "DELETE FROM selection_citations WHERE cited_by = ?1",
        [&key],
    )?;
    tx.execute("DELETE FROM audit_citations WHERE cited_by = ?1", [&key])?;
    Ok(())
}

fn digest(content: &KeptDeletionContent) -> String {
    let bytes = serde_json::to_vec(content).expect("a deletion always serializes");
    Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn kept_from_row(r: &Row) -> rusqlite::Result<KeptFile> {
    let reason: String = r.get(5)?;
    Ok(KeptFile {
        id: r.get(0)?,
        folder: r.get(1)?,
        generation: r.get::<_, i64>(2)? as u64,
        name: r.get(3)?,
        kept_name: r.get(4)?,
        reason: KeepReason::parse(&reason).unwrap_or(KeepReason::Removal),
        build: r.get(6)?,
        built_by: r.get(7)?,
        game: r.get(8)?,
        sha256: r.get(9)?,
        file: FileStat {
            size: r.get::<_, i64>(10)? as u64,
            mtime_ns: r.get::<_, i64>(11)? as i128,
        },
        identity: Identity::from_sql(r.get(12)?, r.get(13)?).into(),
        kept_at: r.get::<_, Option<String>>(14)?.unwrap_or_default(),
        directory: r.get(15)?,
    })
}

pub(super) fn kept_where(
    c: &Connection,
    condition: &str,
    params: impl rusqlite::Params,
) -> Result<Vec<KeptFile>> {
    let mut stmt = c.prepare(&format!(
        "SELECT k.id, k.folder_id, k.generation, k.name, k.kept_name, k.reason, k.build_id, k.built_by, k.game,
                k.sha256, k.size, k.mtime_ns, k.dev, k.ino, k.kept_at, k.directory
         FROM kept_files k WHERE {condition} ORDER BY k.generation DESC, k.id"
    ))?;
    let rows = stmt.query_map(params, kept_from_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn deletion_by_id(c: &Connection, id: i64) -> Result<KeptDeletion> {
    c.query_row(
        "SELECT id, digest, created_at, content, approval_digest, actor, approved_at, finished_at, outcomes,
                recovery_error
         FROM kept_deletions WHERE id = ?1",
        [id],
        |r| {
            let approval = match r.get::<_, Option<String>>(4)? {
                Some(digest) => Some(Approval {
                    digest,
                    actor: r.get(5)?,
                    at: r.get(6)?,
                }),
                None => None,
            };
            let outcomes: Option<String> = r.get(8)?;
            Ok(KeptDeletion {
                id: r.get(0)?,
                digest: r.get(1)?,
                created_at: r.get(2)?,
                content: json(r, 3)?,
                approval,
                finished_at: r.get(7)?,
                outcomes: outcomes
                    .map(|text| serde_json::from_str(&text))
                    .transpose()
                    .map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            8,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?
                    .unwrap_or_default(),
                recovery_error: r.get(9)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("no deletion {id}")))
}
