//! Carrying out an approved cleanup in its Managed Folder, as a Build. Each action is one you chose for one entry,
//! kept file or built file, and acts only on what the plan found: an entry is moved only while it is, by `lstat`
//! through the folder's handle, the identity, size, modification and status change times the plan's check read,
//! and is recorded only once what is at its new name is that identity again.
//!
//! The threat model: the Host keeps a Managed Folder mode 2750, so only the service's own user can create, rename
//! or remove entries in it, and the folders Title Select creates inside it are private to that user. A process
//! running as that user is out of scope (it can rewrite the store). What another user, or something left over from
//! before, could have put at a name is never acted on: a cleanup moves an entry only by one rename that never
//! replaces anything, checks it by identity before and after, records the move as an intent first, and makes both
//! folders durable before recording it done. A stop at any point leaves the intent, which recovery settles only
//! forward, from where the entry is found by identity: moved, still where it was, or neither, which is reported and
//! never lost track of in the store. Nothing is deleted, and no folder is removed by name: what is kept aside, and
//! the file as it was before a repack, are kept files.

use std::collections::HashMap;
use std::io;
use std::time::SystemTime;

use rusqlite::{Transaction, params};
use rustix::fs::{FileType, Stat};
use title_select_api::JobState;
use title_select_api::build::{
    Cleanup, CleanupAction, CleanupEntry, CleanupRepack, CleanupRestore, DriftKind, ItemKind,
    ItemState, KeepAside, KeepReason, Marker, PlanContent, PlanItem, PlanSource, RETENTION,
    STAGING,
};
use title_select_engine::Progress;
use title_select_engine::build::{Probe, holds_as, repack_from, verify};

use super::{
    Publishing, cited_by, clear_staging, is_file, record_applied, record_temp, remove_staged,
    set_item, sha256_of, staged_temp,
};
use crate::checks::Seen;
use crate::config::Config;
use crate::folder_dir::{FolderDir, Identity, file_stat};
use crate::folders::{
    MARKER_NEW, check_marker, folder_by_id, open_folder, open_writable_folder, store_id,
    write_marker,
};
use crate::jobs::finish_running;
use crate::kept::{Keep, Retention, create_dir, drop_citations, open_retention};
use crate::plans::Planned;
use crate::{Error, Fault, Result, Store, now};

/// One thing a cleanup does, in the order it does them: what is kept aside first, freeing its name, then what is
/// moved into place, then what is rewritten, and last what is only acknowledged.
#[derive(Clone, Copy)]
enum Step<'a> {
    Keep(&'a CleanupEntry),
    Adopt(&'a CleanupEntry),
    Restore(&'a CleanupRestore),
    Repack(&'a CleanupRepack),
    Acknowledge(&'a CleanupEntry),
}

impl Step<'_> {
    fn target(&self) -> &str {
        match self {
            Step::Keep(e) | Step::Acknowledge(e) => &e.name,
            Step::Adopt(e) => e.target.as_deref().unwrap_or(&e.name),
            Step::Restore(r) => &r.target,
            Step::Repack(r) => &r.target,
        }
    }

    fn game(&self) -> &str {
        match self {
            Step::Keep(e) | Step::Acknowledge(e) | Step::Adopt(e) => {
                e.game.as_deref().unwrap_or_default()
            }
            Step::Restore(r) => r.game.as_deref().unwrap_or_default(),
            Step::Repack(r) => &r.game,
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Step::Keep(_) => "keep_aside",
            Step::Adopt(_) => "adopt",
            Step::Restore(_) => "restore",
            Step::Repack(_) => "repack",
            Step::Acknowledge(_) => "acknowledge",
        }
    }
}

fn steps(cleanup: &Cleanup) -> Vec<Step<'_>> {
    let chosen = |action: CleanupAction| {
        cleanup
            .entries
            .iter()
            .filter(move |e| e.action == Some(action))
    };
    chosen(CleanupAction::KeepAside)
        .map(Step::Keep)
        .chain(chosen(CleanupAction::Adopt).map(Step::Adopt))
        .chain(cleanup.restores.iter().map(Step::Restore))
        .chain(cleanup.repacks.iter().map(Step::Repack))
        .chain(chosen(CleanupAction::Acknowledge).map(Step::Acknowledge))
        .collect()
}

/// Whether a cleanup stores `target` as a loose file: one recorded under its one ROM's name, rather than its game's.
fn stored_loose(target: &str, game: &str, roms: &[title_select_api::build::ExpectedRom]) -> bool {
    roms.len() == 1 && target != format!("{game}.zip")
}

/// The plan item a cleanup records a built file with.
fn built_item(
    target: &str,
    group: &str,
    game: &str,
    kind: ItemKind,
    roms: &[title_select_api::build::ExpectedRom],
    sources: &[PlanSource],
) -> PlanItem {
    PlanItem {
        group: group.to_owned(),
        game: game.to_owned(),
        target: target.to_owned(),
        kind,
        loose: stored_loose(target, game, roms),
        roms: roms.to_vec(),
        sources: sources.to_vec(),
        reason: None,
        existing: None,
        adoptable: false,
        keepable: false,
        keep: None,
        converts: None,
    }
}

/// The items of a cleanup that write a file, as plan items, for recovery to settle as a Build's.
pub(super) fn written_items(cleanup: &Cleanup) -> Vec<PlanItem> {
    cleanup
        .repacks
        .iter()
        .map(|r| {
            built_item(
                &r.target,
                &r.group,
                &r.game,
                ItemKind::Repack,
                &r.roms,
                &r.sources,
            )
        })
        .collect()
}

/// What a moved entry is recorded as once it is in place, if anything: a built file, and whether it is TorrentZip.
fn recorded_as(
    cleanup: &Cleanup,
    kind: &str,
    target: &str,
    kept: Option<i64>,
) -> Option<(PlanItem, bool)> {
    match kind {
        "adopt" => cleanup
            .entries
            .iter()
            .find(|e| e.action == Some(CleanupAction::Adopt) && e.target.as_deref() == Some(target))
            .map(|e| {
                (
                    built_item(
                        target,
                        &e.group,
                        e.game.as_deref().unwrap_or_default(),
                        ItemKind::Adopt,
                        &e.roms,
                        &[],
                    ),
                    e.torrentzip,
                )
            }),
        "restore" => cleanup
            .restores
            .iter()
            .find(|r| Some(r.kept.id) == kept)
            .and_then(|r| {
                let game = r.game.as_deref()?;
                Some((
                    built_item(target, &r.group, game, ItemKind::Adopt, &r.roms, &[]),
                    r.torrentzip,
                ))
            }),
        _ => None,
    }
}

/// Whether `st` is of the same kind of entry as `directory` says.
fn kind_is(st: &Stat, directory: bool) -> bool {
    match FolderDir::kind(st) {
        FileType::Directory => directory,
        FileType::RegularFile => !directory,
        _ => false,
    }
}

/// Records kept file `kept` put back by item `seq` of Build `build`, and drops what it cited.
fn restored(tx: &Transaction, kept: i64) -> Result<()> {
    tx.execute(
        "UPDATE kept_files SET state = 'restored', restored_at = ?2, error = NULL WHERE id = ?1",
        params![kept, now()],
    )?;
    drop_citations(tx, kept)
}

/// Records kept file `kept` still kept, its move back into the folder not made.
fn still_kept(tx: &Transaction, kept: i64) -> Result<()> {
    tx.execute(
        "UPDATE kept_files SET state = 'kept' WHERE id = ?1 AND state = 'restoring'",
        [kept],
    )?;
    Ok(())
}

/// What a cleanup step did with its item, when it did not stop the Build: done, or left as it was and why.
type Outcome = std::result::Result<(), String>;

/// The folder a cleanup changes, opened and checked, and what it creates in it as it goes.
struct Run<'a> {
    store: &'a Store,
    dir: FolderDir,
    expected: Marker,
    build: i64,
    folder: i64,
    generation: u64,
    retention: Option<Retention>,
    staging: Option<(FolderDir, Identity)>,
    staged: HashMap<String, Identity>,
    built_at: SystemTime,
}

impl Run<'_> {
    fn area(&mut self) -> Result<&Retention> {
        if self.retention.is_none() {
            self.retention = Some(
                open_retention(self.store, self.folder, &self.dir, self.generation, true)?
                    .map_err(Error::Refused)?
                    .ok_or_else(|| Error::Io("the retention area was not created".into()))?,
            );
        }
        Ok(self.retention.as_ref().expect("opened above"))
    }

    fn fail(&self, seq: i64, state: ItemState, error: &str) -> Result<()> {
        self.store
            .write(|tx| set_item(tx, self.build, seq, state, error))
    }

    /// Keeps aside entry `e`, a file or a folder, as a kept file.
    fn keep(&mut self, seq: i64, e: &CleanupEntry) -> Result<Outcome> {
        let keep = KeepAside {
            reason: KeepReason::Drift,
            name: e.name.clone(),
            file: e.file.clone(),
            identity: e.identity,
            sha256: e.sha256.clone(),
            directory: e.kind == DriftKind::Directory,
        };
        let (store, build, folder, generation) =
            (self.store, self.build, self.folder, self.generation);
        self.area()?;
        let k = Keep {
            folder,
            build,
            seq,
            generation,
            keep: &keep,
            hashed: None,
        };
        let area = self.retention.as_ref().expect("opened above");
        Ok(store.keep_aside(&self.dir, area, &k)?.map(|_| ()))
    }

    /// Adopts entry `e`: renamed, by one rename that never replaces anything, to its game's file name when it is not
    /// that already, and recorded as built. It is not read again: the plan's check read it, as `seen`, and it is
    /// moved only while it is still that file.
    fn adopt(&self, seq: i64, e: &CleanupEntry, seen: Option<&(Seen, Probe)>) -> Result<Outcome> {
        let target = e.target.as_deref().unwrap_or(&e.name);
        let left = |why: &str| {
            Ok(Err(format!(
                "{} was not adopted: {why}; it was left as it is",
                e.name
            )))
        };
        let Some((seen, probe)) = seen else {
            return left("the check could not read it");
        };
        if seen.identity != Identity::from(e.identity)
            || seen.file != e.file
            || e.sha256.as_ref() != Some(&probe.sha256)
            || !holds_as(
                probe,
                &e.roms,
                stored_loose(target, e.game.as_deref().unwrap_or_default(), &e.roms),
            )
        {
            return left("it changed since you approved the plan");
        }
        let dir = &self.dir;
        let same = |name: &str| -> io::Result<Option<Stat>> {
            Ok(dir
                .stat(name)?
                .filter(|st| kind_is(st, false) && Seen::of(st) == *seen))
        };
        let Some(st) = same(&e.name).map_err(|e| Error::Io(e.to_string()))? else {
            return left("it changed since you approved the plan");
        };
        let item = built_item(
            target,
            &e.group,
            e.game.as_deref().unwrap_or_default(),
            ItemKind::Adopt,
            &e.roms,
            &[],
        );
        let (dev, ino) = seen.identity.to_sql();
        let intent = |tx: &Transaction, origin: Option<&str>| {
            tx.execute(
                "UPDATE build_items SET state = ?3, origin = ?4, sha256 = ?5, size = ?6, dev = ?7, ino = ?8
                 WHERE build_id = ?1 AND seq = ?2",
                params![
                    self.build,
                    seq,
                    if origin.is_some() { "publishing" } else { "pending" },
                    origin,
                    probe.sha256,
                    seen.file.size as i64,
                    dev,
                    ino
                ],
            )
        };
        if target == e.name {
            // Already at its game's file name: only recorded.
            let applied = self.store.write(|tx| {
                intent(tx, None)?;
                record_applied(tx, self.build, self.folder, seq, &item, &st, e.torrentzip)
            })?;
            if !applied {
                return Err(Error::Refused(format!(
                    "{}: {target} is not the file the plan found there; it was left as it is",
                    dir.path().display()
                )));
            }
            return Ok(Ok(()));
        }
        self.store.write(|tx| {
            intent(tx, Some(&e.name))?;
            Ok(())
        })?;
        self.store.fault_point(Fault::BeforeMove(seq));
        if same(&e.name)
            .map_err(|e| Error::Io(e.to_string()))?
            .is_none()
        {
            return left("it changed since you approved the plan");
        }
        if let Err(err) = dir.rename_new(&e.name, dir, target) {
            return match err.kind() {
                io::ErrorKind::AlreadyExists => left(&format!(
                    "{target} appeared in the folder during the cleanup"
                )),
                _ => Err(Error::Io(err.to_string())),
            };
        }
        // From here until it is recorded, recovery settles the move from its intent.
        self.store.fault_point(Fault::AfterMove(seq));
        dir.sync().map_err(|e| Error::Io(e.to_string()))?;
        self.store.fault_point(Fault::MoveSynced(seq));
        let found = dir.stat(target).map_err(|e| Error::Io(e.to_string()))?;
        let applied = match &found {
            Some(st) => self.store.write(|tx| {
                record_applied(tx, self.build, self.folder, seq, &item, st, e.torrentzip)
            })?,
            None => false,
        };
        if !applied {
            return Err(Error::Refused(format!(
                "{}: {target} is not the file the cleanup moved there; it was left as it is",
                dir.path().display()
            )));
        }
        Ok(Ok(()))
    }

    /// Puts kept file `r.kept` back into the folder, under `r.target`, by one rename out of the retention area that
    /// never replaces anything, and records it as built when it holds a selected game. It is moved only while it is
    /// what the plan found: for a file, the identity, size, modification and status change times the plan read.
    fn restore(&self, seq: i64, r: &CleanupRestore) -> Result<Outcome> {
        let kept = &r.kept;
        let left = |why: &str| {
            Ok(Err(format!(
                "kept file {} was not put back: {why}; it was left where it is",
                kept.name
            )))
        };
        let retention =
            match open_retention(self.store, self.folder, &self.dir, kept.generation, false)? {
                Ok(Some(retention)) => retention,
                Ok(None) => return left(&format!("{RETENTION}/{} is gone", kept.generation)),
                Err(why) => return left(&why),
            };
        let id = Identity::from(kept.identity);
        let ctime: Option<Option<i64>> = self.store.read(|c| {
            Ok(rusqlite::OptionalExtension::optional(c.query_row(
                "SELECT probe_ctime_ns FROM kept_files WHERE id = ?1 AND state = 'kept'",
                [kept.id],
                |r| r.get(0),
            ))?)
        })?;
        let Some(ctime) = ctime else {
            return left("it is no longer kept");
        };
        let same = |st: &Stat| {
            kind_is(st, kept.directory)
                && Identity::of(st) == id
                && file_stat(st) == kept.file
                && (kept.directory || ctime == Some(Seen::of(st).ctime_ns as i64))
        };
        let source = &retention.generation;
        let found = |dir: &FolderDir, name: &str| -> Result<Option<Stat>> {
            dir.stat(name).map_err(|e| Error::Io(e.to_string()))
        };
        if !found(source, &kept.kept_name)?.is_some_and(|st| same(&st)) {
            return left("it changed since you approved the plan");
        }
        let origin = format!("{RETENTION}/{}/{}", kept.generation, kept.kept_name);
        let (dev, ino) = id.to_sql();
        let marked = self.store.write(|tx| {
            let marked = tx.execute(
                "UPDATE kept_files SET state = 'restoring' WHERE id = ?1 AND state = 'kept'",
                [kept.id],
            )?;
            tx.execute(
                "UPDATE build_items SET state = 'publishing', origin = ?3, sha256 = ?4, size = ?5, dev = ?6, ino = ?7,
                                        kept_id = ?8
                 WHERE build_id = ?1 AND seq = ?2",
                params![
                    self.build,
                    seq,
                    origin,
                    kept.sha256,
                    kept.file.size as i64,
                    dev,
                    ino,
                    kept.id
                ],
            )?;
            Ok(marked == 1)
        })?;
        if !marked {
            return left("it is no longer kept");
        }
        self.store.fault_point(Fault::BeforeMove(seq));
        let back = |why: &str| -> Result<Outcome> {
            self.store.write(|tx| still_kept(tx, kept.id))?;
            left(why)
        };
        if !found(source, &kept.kept_name)?.is_some_and(|st| same(&st)) {
            return back("it changed since you approved the plan");
        }
        if let Err(err) = source.rename_new(&kept.kept_name, &self.dir, &r.target) {
            return match err.kind() {
                io::ErrorKind::AlreadyExists => back(&format!(
                    "{} appeared in the folder during the cleanup",
                    r.target
                )),
                _ => Err(Error::Io(err.to_string())),
            };
        }
        // From here until it is recorded, recovery settles the move from its intent.
        self.store.fault_point(Fault::AfterMove(seq));
        source
            .sync()
            .and_then(|()| self.dir.sync())
            .map_err(|e| Error::Io(e.to_string()))?;
        self.store.fault_point(Fault::MoveSynced(seq));
        let at = found(&self.dir, &r.target)?
            .filter(|st| kind_is(st, kept.directory) && Identity::of(st) == id);
        let Some(st) = at else {
            return Err(Error::Refused(format!(
                "{}: {} is not the kept file the cleanup moved there; it was left as it is",
                self.dir.path().display(),
                r.target
            )));
        };
        let built = r
            .game
            .as_deref()
            .map(|game| built_item(&r.target, &r.group, game, ItemKind::Adopt, &r.roms, &[]));
        let applied = self.store.write(|tx| {
            let applied = match &built {
                Some(item) => {
                    record_applied(tx, self.build, self.folder, seq, item, &st, r.torrentzip)?
                }
                None => {
                    tx.execute(
                        "UPDATE build_items SET state = 'applied', error = NULL WHERE build_id = ?1 AND seq = ?2",
                        params![self.build, seq],
                    )?;
                    true
                }
            };
            restored(tx, kept.id)?;
            Ok(applied)
        })?;
        if !applied {
            return Err(Error::Refused(format!(
                "{}: {} is not the kept file the cleanup moved there; it was left as it is",
                self.dir.path().display(),
                r.target
            )));
        }
        Ok(Ok(()))
    }

    /// Rewrites built file `r.target` as TorrentZip from its own contents: written and verified in the staging
    /// directory, the file as it was kept aside as a kept file, and the new one renamed into its place, as a Build
    /// publishes a file.
    fn repack(&mut self, seq: i64, r: &CleanupRepack) -> Result<Outcome> {
        let left = |why: &str| {
            Ok(Err(format!(
                "{} was not repacked: {why}; it was left as it is",
                r.target
            )))
        };
        let folder_io = |e: io::Error| Error::Io(e.to_string());
        let id = Identity::from(r.identity);
        let same = |st: &Stat| Identity::of(st) == id && file_stat(st) == r.file;
        let source = match self.dir.open_file(&r.target) {
            Ok(source) => source,
            Err(e) => return left(&e.to_string()),
        };
        let before = match rustix::fs::fstat(&source) {
            Ok(st) if same(&st) && is_file(&self.dir, &r.target, id).unwrap_or(false) => {
                Seen::of(&st)
            }
            _ => return left("it changed since you approved the plan"),
        };
        if self.staging.is_none() {
            let staging = create_dir(self.store, self.folder, &self.dir, STAGING, STAGING)?
                .map_err(Error::Refused)?;
            let own = staging.identity().map_err(folder_io)?;
            self.staging = Some((staging, own));
        }
        let name = format!("{seq}.zip");
        let (staging, _) = self.staging.as_ref().expect("made above");
        let file = staging.create_new(&name).map_err(folder_io)?;
        let file_id = Identity::of_fd(&file).map_err(folder_io)?;
        record_temp(self.store, self.folder, &staged_temp(&name), file_id)?;
        self.staged.insert(name.clone(), file_id);
        let staged_path = staging.path().join(&name);
        // The old ZIP is read once: the rewrite hashes it whole as it reads it, for keeping it aside.
        let mut read = None;
        let written = repack_from(&file, &r.roms, &source)
            .and_then(|(sha256, size)| {
                if sha256 != r.sha256 || size != r.file.size {
                    return Err("it is not the file the plan was made with".to_owned());
                }
                read = Some(sha256);
                verify(&file, &r.roms)
            })
            .and_then(|()| {
                file.set_modified(self.built_at)
                    .and_then(|()| file.sync_all())
                    .map_err(|e| format!("{}: {e}", staged_path.display()))
            })
            .and_then(|()| sha256_of(&file).map_err(|e| format!("{}: {e}", staged_path.display())))
            .and_then(|written| match rustix::fs::fstat(&source) {
                Ok(st) if same(&st) => Ok((written, Seen::of(&st) == before)),
                _ => Err("it changed while it was read".to_owned()),
            });
        drop(source);
        let unstage = |run: &Self, file: std::fs::File, why: &str| -> Result<Outcome> {
            drop(file);
            let (staging, _) = run.staging.as_ref().expect("made above");
            remove_staged(staging, &name, file_id).map_err(Error::Refused)?;
            left(why)
        };
        let ((sha256, size), unchanged) = match written {
            Ok(written) => written,
            Err(why) => return unstage(self, file, &why),
        };
        let hashed = read
            .filter(|_| unchanged)
            .map(|read| (before.clone(), read));
        let keep = KeepAside {
            reason: KeepReason::Repack,
            name: r.target.clone(),
            file: r.file.clone(),
            identity: r.identity,
            sha256: Some(r.sha256.clone()),
            directory: false,
        };
        check_marker(&self.dir, &self.expected).map_err(Error::Refused)?;
        self.area()?;
        let k = Keep {
            folder: self.folder,
            build: self.build,
            seq,
            generation: self.generation,
            keep: &keep,
            hashed,
        };
        let area = self.retention.as_ref().expect("opened above");
        if let Err(why) = self.store.keep_aside(&self.dir, area, &k)? {
            return unstage(self, file, &why);
        }
        let (dev, ino) = file_id.to_sql();
        self.store.write(|tx| {
            tx.execute(
                "UPDATE build_items SET state = 'publishing', sha256 = ?3, size = ?4, dev = ?5, ino = ?6
                 WHERE build_id = ?1 AND seq = ?2",
                params![self.build, seq, sha256, size as i64, dev, ino],
            )?;
            Ok(())
        })?;
        self.store.fault_point(Fault::BeforePublish(seq));
        let (staging, _) = self.staging.as_ref().expect("made above");
        let renamed = check_marker(&self.dir, &self.expected)
            .and_then(|()| match is_file(staging, &name, file_id)? {
                true => Ok(()),
                false => Err(format!(
                    "{} was replaced after it was verified; it was left as it is",
                    staged_path.display()
                )),
            })
            .and_then(|()| {
                staging
                    .rename_new(&name, &self.dir, &r.target)
                    .map_err(|e| match e.kind() {
                        io::ErrorKind::AlreadyExists => format!(
                            "{} appeared in the folder during the cleanup; it was left as it is",
                            r.target
                        ),
                        _ => e.to_string(),
                    })
            });
        if let Err(error) = renamed {
            self.fail(seq, ItemState::Failed, &error)?;
            return Err(Error::Refused(error));
        }
        self.store.fault_point(Fault::PublishedUnsynced(seq));
        staging
            .sync()
            .and_then(|()| self.dir.sync())
            .map_err(folder_io)?;
        self.store.fault_point(Fault::AfterPublish(seq));
        let item = built_item(
            &r.target,
            &r.group,
            &r.game,
            ItemKind::Repack,
            &r.roms,
            &r.sources,
        );
        let applied = match self.dir.stat(&r.target).map_err(folder_io)? {
            Some(st) => self
                .store
                .write(|tx| record_applied(tx, self.build, self.folder, seq, &item, &st, true))?,
            None => false,
        };
        drop(file);
        if !applied {
            return Err(Error::Refused(format!(
                "{}: {} is not the file the cleanup put there; it was left as it is",
                self.dir.path().display(),
                r.target
            )));
        }
        Ok(Ok(()))
    }
}

/// Stores the check a cleanup of Managed Folder `folder` made of it as it left it, if it made one.
fn store_refreshed(tx: &Transaction, folder: i64, refreshed: Option<Planned>) -> Result<()> {
    if let Some(Planned {
        check: Some(check),
        scan,
        ..
    }) = refreshed
    {
        crate::checks::store_check(tx, folder, None, scan.as_ref(), check)?;
    }
    Ok(())
}

/// Acknowledges entry `e` for item `seq` of Build `build`, if `found`, its `lstat` now, is the entry the plan
/// found: a check then reports it as acknowledged until it changes. Nothing in the folder is written.
fn acknowledge(
    tx: &Transaction,
    build: i64,
    folder: i64,
    seq: i64,
    e: &CleanupEntry,
    found: Option<&Stat>,
) -> Result<()> {
    let unchanged = found.is_some_and(|st| {
        Identity::of(st) == Identity::from(e.identity)
            && file_stat(st) == e.file
            && (FolderDir::kind(st) == FileType::Directory) == (e.kind == DriftKind::Directory)
    });
    if !unchanged {
        return set_item(
            tx,
            build,
            seq,
            ItemState::Failed,
            &format!(
                "{} was not acknowledged: it changed since you approved the plan",
                e.name
            ),
        );
    }
    let (dev, ino) = Identity::from(e.identity).to_sql();
    tx.execute(
        "INSERT INTO drift_acks (folder_id, name, dev, ino, size, mtime_ns, build_id, acknowledged_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT (folder_id, name) DO UPDATE SET dev = excluded.dev, ino = excluded.ino, size = excluded.size,
             mtime_ns = excluded.mtime_ns, build_id = excluded.build_id, acknowledged_at = excluded.acknowledged_at",
        params![
            folder,
            e.name,
            dev,
            ino,
            e.file.size as i64,
            e.file.mtime_ns as i64,
            build,
            now()
        ],
    )?;
    tx.execute(
        "UPDATE build_items SET state = 'applied', error = NULL WHERE build_id = ?1 AND seq = ?2",
        params![build, seq],
    )?;
    Ok(())
}

impl Store {
    /// Carries out the approved cleanup `cleanup`, of plan `plan_id` with content `content`, as Build Job `job`;
    /// `again` is the plan computed again just now, found to be the one approved. A cleanup that only acknowledges
    /// entries writes nothing to the folder, and leaves its generation and marker as they are.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn carry_out_cleanup(
        &self,
        config: &Config,
        job: i64,
        plan_id: i64,
        content: &PlanContent,
        cleanup: &Cleanup,
        again: &Planned,
        build_id: &mut Option<i64>,
        progress: Option<&(dyn Fn(Progress) + Sync)>,
    ) -> Result<()> {
        let audit = content
            .audit
            .ok_or_else(|| Error::Io(format!("plan {plan_id} has no audit")))?;
        let (folder_row, store) =
            self.read(|c| Ok((folder_by_id(c, content.folder)?, store_id(c)?)))?;
        let changes = cleanup.changes_folder();
        let dir = if changes {
            open_writable_folder(config, &folder_row.root, &folder_row.path)
        } else {
            open_folder(config, &folder_row.root, &folder_row.path)
        }
        .map_err(Error::Refused)?;
        let expected = Marker {
            store,
            folder: content.folder,
            generation: content.generation,
        };
        check_marker(&dir, &expected).map_err(Error::Refused)?;
        if changes {
            for leftover in [STAGING, MARKER_NEW] {
                if dir
                    .stat(leftover)
                    .map_err(|e| Error::Io(e.to_string()))?
                    .is_some()
                {
                    return Err(Error::Refused(format!(
                        "{} holds {leftover}, which no build of this store left there; nothing was changed",
                        dir.path().display()
                    )));
                }
            }
        }
        let steps = steps(cleanup);
        let folder = content.folder;
        let insert = |tx: &Transaction, state: &str, generation: u64| -> Result<i64> {
            let revision: i64 = tx.query_row(
                "SELECT dat_revision_id FROM selections WHERE id = ?1",
                [content.selection],
                |r| r.get(0),
            )?;
            tx.execute(
                "INSERT INTO builds (folder_id, plan_id, job_id, selection_id, audit_id, dat_revision_id, state,
                                     generation, started_at, kind)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'cleanup')",
                params![
                    folder,
                    plan_id,
                    job,
                    content.selection,
                    audit,
                    revision,
                    state,
                    generation as i64,
                    now()
                ],
            )?;
            let id = tx.last_insert_rowid();
            for (seq, step) in steps.iter().enumerate() {
                tx.execute(
                    "INSERT INTO build_items (build_id, seq, target, game, kind, state)
                     VALUES (?1, ?2, ?3, ?4, ?5, 'pending')",
                    params![id, seq as i64, step.target(), step.game(), step.kind()],
                )?;
            }
            Ok(id)
        };

        if !changes {
            // Acknowledging only: the store records it, all at once; the folder is only read.
            let found = steps
                .iter()
                .map(|step| {
                    dir.stat(step.target())
                        .map_err(|e| Error::Io(e.to_string()))
                })
                .collect::<Result<Vec<_>>>()?;
            let id = self.write(|tx| {
                let id = insert(tx, "running", content.generation)?;
                for (seq, (step, found)) in steps.iter().zip(&found).enumerate() {
                    if let Step::Acknowledge(e) = step {
                        acknowledge(tx, id, folder, seq as i64, e, found.as_ref())?;
                    }
                }
                tx.execute(
                    "UPDATE builds SET state = 'succeeded', finished_at = ?2, cleaned_at = ?2 WHERE id = ?1",
                    params![id, now()],
                )?;
                Ok(id)
            })?;
            *build_id = Some(id);
            let refreshed = self.refresh_check(config, folder);
            return self.write(|tx| {
                store_refreshed(tx, folder, refreshed)?;
                finish_running(tx, job, JobState::Succeeded, None)
            });
        }

        let generation = content.generation + 1;
        let id = self.write(|tx| insert(tx, "running", generation))?;
        *build_id = Some(id);
        let mut run = Run {
            store: self,
            dir,
            expected: expected.clone(),
            build: id,
            folder,
            generation,
            retention: None,
            staging: None,
            staged: HashMap::new(),
            built_at: SystemTime::now(),
        };
        let probes = again.scan.as_ref().map(|s| &s.probes);
        let total = steps.len();
        if let Some(progress) = progress {
            progress(Progress {
                files_done: 0,
                files_total: total,
            });
        }
        let mut cancelled = false;
        for (seq, step) in steps.iter().enumerate() {
            // You can cancel a cleanup between two of its steps.
            if self.cancel_requested(job)? {
                cancelled = true;
                break;
            }
            let seq = seq as i64;
            check_marker(&run.dir, &expected).map_err(Error::Refused)?;
            let outcome = match *step {
                Step::Keep(e) => run.keep(seq, e)?,
                Step::Adopt(e) => run.adopt(seq, e, probes.and_then(|p| p.get(&e.name)))?,
                Step::Restore(r) => run.restore(seq, r)?,
                Step::Repack(r) => run.repack(seq, r)?,
                Step::Acknowledge(e) => {
                    let found = run
                        .dir
                        .stat(&e.name)
                        .map_err(|e| Error::Io(e.to_string()))?;
                    self.write(|tx| acknowledge(tx, id, folder, seq, e, found.as_ref()))?;
                    Ok(())
                }
            };
            if let Err(error) = outcome {
                run.fail(seq, ItemState::Failed, &error)?;
            }
            if let Some(progress) = progress {
                progress(Progress {
                    files_done: seq as usize + 1,
                    files_total: total,
                });
            }
        }
        if let Some((staging, own)) = run.staging.take() {
            clear_staging(&run.dir, &staging, own, &run.staged).map_err(Error::Refused)?;
        }
        let Run { dir, .. } = run;
        if cancelled {
            return self.write(|tx| super::end_cancelled(tx, job, id, folder));
        }

        // As a Build finishes: the store decides the next generation first, and the marker follows it.
        check_marker(&dir, &expected).map_err(Error::Refused)?;
        let key = cited_by(folder);
        self.write(|tx| {
            let advanced = tx.execute(
                "UPDATE managed_folders SET generation = ?2, current_build_id = ?3 WHERE id = ?1 AND generation = ?4",
                params![folder, generation as i64, id, content.generation as i64],
            )?;
            if advanced != 1 {
                return Err(Error::Io(format!(
                    "managed folder {folder} is no longer at generation {}",
                    content.generation
                )));
            }
            tx.execute(
                "UPDATE builds SET finalizing_at = ?2 WHERE id = ?1",
                params![id, now()],
            )?;
            // What the folder lacked is what it lacked before, but for what is now built.
            tx.execute(
                "INSERT INTO build_incomplete (build_id, seq, reason, grp, game, target, detail)
                 SELECT ?1, i.seq, i.reason, i.grp, i.game, i.target, i.detail FROM build_incomplete i
                 WHERE i.build_id = ?2 AND NOT EXISTS (
                     SELECT 1 FROM built_files f WHERE f.folder_id = ?3 AND lower(f.target) = lower(i.target))",
                params![id, cleanup.build, folder],
            )?;
            tx.execute("DELETE FROM audit_citations WHERE cited_by = ?1", [&key])?;
            tx.execute(
                "INSERT INTO audit_citations (audit_id, cited_by) VALUES (?1, ?2)",
                params![audit, key],
            )?;
            tx.execute(
                "DELETE FROM selection_citations WHERE cited_by = ?1",
                [&key],
            )?;
            tx.execute(
                "INSERT INTO selection_citations (selection_id, cited_by) VALUES (?1, ?2)",
                params![content.selection, key],
            )?;
            Ok(())
        })?;
        self.fault_point(Fault::AfterFinalize);
        write_marker(
            &dir,
            &Marker {
                generation,
                ..expected.clone()
            },
            &mut |tmp| record_temp(self, folder, MARKER_NEW, tmp),
        )?;
        self.fault_point(Fault::AfterMarker);
        let refreshed = self.refresh_check(config, folder);
        self.write(|tx| {
            tx.execute(
                "UPDATE builds SET state = 'succeeded', finished_at = ?2, cleaned_at = ?2 WHERE id = ?1",
                params![id, now()],
            )?;
            tx.execute("DELETE FROM folder_temps WHERE folder_id = ?1", [folder])?;
            store_refreshed(tx, folder, refreshed)?;
            finish_running(tx, job, JobState::Succeeded, None)
        })
    }

    /// Checks Managed Folder `folder` again once a cleanup changed what a check finds in it, so that what it reports
    /// is the folder as the cleanup left it, to be stored with [`store_refreshed`] before the cleanup's job ends,
    /// while nothing else acts on the folder. Only read; if it cannot be made, the last check stands.
    fn refresh_check(&self, config: &Config, folder: i64) -> Option<Planned> {
        self.compute_cleanup(
            config,
            folder,
            &title_select_api::build::PlanCleanup::default(),
        )
        .ok()
        .filter(|planned| planned.check.is_some())
    }

    /// Settles item `p` of the unfinished cleanup Build `build`, which was moving an entry within the folder `dir`
    /// from `p.origin` to `p.target`: from where the entry, by the identity its intent records, is found. Moved, it
    /// is recorded once both folders are durable; still where it was, the item failed; neither, it is reported, and
    /// a kept file it was putting back is recorded lost. Returns why the folder could not be read, if it could not.
    pub(super) fn settle_move(
        &self,
        dir: &FolderDir,
        build: i64,
        folder: i64,
        p: &Publishing,
        cleanup: &Cleanup,
    ) -> Result<std::result::Result<(), String>> {
        let origin = p.origin.as_deref().unwrap_or_default();
        let (Some(dev), Some(ino)) = (p.dev, p.ino) else {
            return Err(Error::Io(format!(
                "build {build}: item {} has no identity",
                p.seq
            )));
        };
        let id = Identity::from_sql(dev, ino);
        let retention;
        let (source, name): (Option<&FolderDir>, &str) =
            match origin.strip_prefix(&format!("{RETENTION}/")) {
                None => (Some(dir), origin),
                Some(rest) => {
                    let Some((generation, kept_name)) = rest
                        .split_once('/')
                        .and_then(|(g, n)| Some((g.parse::<u64>().ok()?, n)))
                    else {
                        return Err(Error::Io(format!(
                            "build {build}: item {} moves from {origin}",
                            p.seq
                        )));
                    };
                    retention = match open_retention(self, folder, dir, generation, false)? {
                        Ok(retention) => retention,
                        Err(reason) => return Ok(Err(reason)),
                    };
                    (retention.as_ref().map(|r| &r.generation), kept_name)
                }
            };
        let at = |d: &FolderDir, n: &str| -> std::result::Result<Option<Stat>, String> {
            Ok(d.stat(n)
                .map_err(|e| e.to_string())?
                .filter(|st| Identity::of(st) == id && FolderDir::kind(st) != FileType::Symlink))
        };
        let moved = match at(dir, &p.target) {
            Ok(moved) => moved,
            Err(reason) => return Ok(Err(reason)),
        };
        let in_place = match source.map(|s| at(s, name)).transpose() {
            Ok(found) => found.flatten().is_some(),
            Err(reason) => return Ok(Err(reason)),
        };
        // The move is recorded only once it is durable: the cleanup may have stopped before it synced either folder.
        if moved.is_some()
            && let Err(e) = source
                .map_or(Ok(()), |s| s.sync())
                .and_then(|()| dir.sync())
        {
            return Ok(Err(e.to_string()));
        }
        let recorded = recorded_as(cleanup, &p.kind, &p.target, p.kept_id);
        self.write(|tx| {
            match &moved {
                Some(st) => {
                    match &recorded {
                        Some((item, torrentzip)) => {
                            record_applied(tx, build, folder, p.seq, item, st, *torrentzip)?;
                        }
                        None => {
                            tx.execute(
                                "UPDATE build_items SET state = 'applied', error = NULL
                                 WHERE build_id = ?1 AND seq = ?2",
                                params![build, p.seq],
                            )?;
                        }
                    }
                    if let Some(kept) = p.kept_id {
                        restored(tx, kept)?;
                    }
                }
                None if in_place => {
                    set_item(
                        tx,
                        build,
                        p.seq,
                        ItemState::Failed,
                        &format!(
                            "the cleanup stopped before it moved {origin} to {}; it is still where it was",
                            p.target
                        ),
                    )?;
                    if let Some(kept) = p.kept_id {
                        still_kept(tx, kept)?;
                    }
                }
                None => {
                    let error = format!(
                        "the cleanup stopped while moving {origin} to {}, and it is now at neither; something else \
                         moved it",
                        p.target
                    );
                    set_item(tx, build, p.seq, ItemState::Uncertain, &error)?;
                    if let Some(kept) = p.kept_id {
                        tx.execute(
                            "UPDATE kept_files SET state = 'lost', error = ?2 WHERE id = ?1 AND state = 'restoring'",
                            params![kept, error],
                        )?;
                    }
                }
            }
            Ok(())
        })?;
        Ok(Ok(()))
    }
}
