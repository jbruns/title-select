//! Checks of Library Folders: what is in a folder now against the store's record of what Builds put there. A check
//! only reads the folder, through the handle it was opened by and without following a symlink; what it finds is
//! written to the store alone. A built file is read only when its size or modification time differ from its record,
//! and every entry Title Select did not build is classified as Drift, read once and then only when it changes.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use rustix::fs::{FileType, Stat};
use title_select_api::build::{
    CheckSummary, CheckedFile, DriftEntry, DriftKind, FileOutcome, FileStat, FolderCheck,
};
use title_select_api::storage::{Storage, StorageFormat, target_of};
use title_select_api::{Job, JobKind, JobState};
use title_select_engine::Progress;
use title_select_engine::build::{GameIndex, Probe, holds_as, probe};
use title_select_engine::dat::Dat;

use super::audits::choose_revision;
use super::dats::revision_by_id;
use super::folder_dir::{FolderDir, Identity, ctime_ns, file_stat};
use super::folders::{folder_by_id, is_own, marker_problem, open_folder, store_id};
use super::jobs::{finish_running, job_by_id};
use super::plans::{Built, json, read_built, to_json};
use super::sets::set_by_id;
use super::{Error, Result, Store, now};
use crate::config::Config;

/// A built file's record to bring up to date: only while it is still as the check read it.
pub(super) struct Update {
    target: String,
    build: i64,
    was: FileStat,
    now: FileStat,
    /// Its SHA-256, when the check read all of it; a CHD's header alone is read, so its SHA-256 is no longer known.
    sha256: Option<String>,
    torrentzip: bool,
}

/// A file as `lstat` or `fstat` saw it: what it is, and every time its content or metadata could change. What was
/// read of a file is reused only while all of it is the same.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Seen {
    pub file: FileStat,
    pub identity: Identity,
    pub ctime_ns: i128,
}

impl Seen {
    pub fn of(st: &Stat) -> Seen {
        Seen {
            file: file_stat(st),
            identity: Identity::of(st),
            ctime_ns: ctime_ns(st),
        }
    }
}

/// An entry Title Select did not build that you acknowledged, as it was then.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Ack {
    pub identity: Identity,
    pub file: FileStat,
}

/// The acknowledged entries of Library Folder `folder`, by name.
pub(super) fn read_acks(c: &Connection, folder: i64) -> Result<HashMap<String, Ack>> {
    let mut stmt =
        c.prepare("SELECT name, dev, ino, size, mtime_ns FROM drift_acks WHERE folder_id = ?1")?;
    let rows = stmt.query_map([folder], |r| {
        Ok((
            r.get::<_, String>(0)?,
            Ack {
                identity: Identity::from_sql(r.get(1)?, r.get(2)?),
                file: FileStat {
                    size: r.get::<_, i64>(3)? as u64,
                    mtime_ns: r.get::<_, i64>(4)? as i128,
                },
            },
        ))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// What a check found, before it is stored.
pub(super) struct Scan {
    pub files: Vec<CheckedFile>,
    pub drift: Vec<DriftEntry>,
    /// Entries you acknowledged, found as they were then: neither read nor reported as drift.
    pub acknowledged: Vec<DriftEntry>,
    updates: Vec<Update>,
    /// What was read of each entry, by name, with the file as it was when read.
    pub probes: HashMap<String, (Seen, Probe)>,
    /// The acknowledgements the check was made with: only these can it find changed.
    acks: HashMap<String, Ack>,
}

/// What a check compares the folder with.
pub(super) struct ScanInput<'a> {
    pub built: &'a BTreeMap<String, Built>,
    /// The selected Games, by their target's lowercased name.
    pub selected: &'a HashMap<String, String>,
    pub dat: Option<&'a Dat>,
    /// What the storage policy says for the folder's system.
    pub storage: Storage,
    /// What earlier checks read, by name.
    pub cached: &'a HashMap<String, (Seen, Probe)>,
    /// The entries you acknowledged, by name.
    pub acks: &'a HashMap<String, Ack>,
}

/// The names in a folder other than Title Select's own entries, sorted.
pub(super) fn listing(dir: &FolderDir) -> std::io::Result<Vec<String>> {
    let mut names: Vec<String> = dir
        .entries()?
        .into_iter()
        .map(|(name, _)| name)
        .filter(|name| !is_own(name))
        .collect();
    names.sort();
    Ok(names)
}

/// Reads the regular file `name`, found by `lstat` as `seen`, through `dir`: opened without following a symlink, and
/// only trusted when it is the same file, the same size and modification time, before and after it is read.
fn read(dir: &FolderDir, name: &str, seen: &Stat) -> std::result::Result<Probe, String> {
    let changed = || "it changed while it was read".to_owned();
    let file: File = dir.open_file(name).map_err(|e| e.to_string())?;
    let before = rustix::fs::fstat(&file).map_err(|e| e.to_string())?;
    if Seen::of(&before) != Seen::of(seen) {
        return Err(changed());
    }
    let probe = probe(&file).map_err(|e| e.to_string())?;
    still(dir, name, &file, seen)?;
    Ok(probe)
}

/// The raw data SHA-1 in the CHD header of the regular file `name`, found by `lstat` as `seen`, read through `dir`
/// while it is the same file: its header alone is read, never its compressed data.
fn chd_sha1(dir: &FolderDir, name: &str, seen: &Stat) -> std::result::Result<String, String> {
    let file: File = dir.open_file(name).map_err(|e| e.to_string())?;
    let before = rustix::fs::fstat(&file).map_err(|e| e.to_string())?;
    if Seen::of(&before) != Seen::of(seen) {
        return Err("it changed while it was read".to_owned());
    }
    let header = title_select_engine::chd::header(&file)?;
    still(dir, name, &file, seen)?;
    Ok(header.raw_sha1)
}

/// Whether `file`, opened from `name` in `dir`, and the entry at `name` are both still the file `seen`.
fn still(dir: &FolderDir, name: &str, file: &File, seen: &Stat) -> std::result::Result<(), String> {
    let after = rustix::fs::fstat(file).map_err(|e| e.to_string())?;
    match dir.stat(name) {
        Ok(Some(st))
            if FolderDir::kind(&st) == FileType::RegularFile
                && Seen::of(&after) == Seen::of(seen)
                && Seen::of(&st) == Seen::of(seen) =>
        {
            Ok(())
        }
        _ => Err("it changed while it was read".to_owned()),
    }
}

/// Reuses `cached`, what an earlier check read of `name`, only when the file found by `lstat` as `seen` is the one
/// it was read from: the same identity, size, modification time and status change time, through the folder's handle
/// as well as by name. Otherwise reads it again.
pub(super) fn reuse(
    dir: &FolderDir,
    name: &str,
    seen: &Stat,
    cached: Option<&(Seen, Probe)>,
) -> (std::result::Result<Probe, String>, bool) {
    if let Some((was, probe)) = cached
        && *was == Seen::of(seen)
        && let Ok(file) = dir.open_file(name)
        && still(dir, name, &file, seen).is_ok()
    {
        return (Ok(probe.clone()), false);
    }
    (read(dir, name, seen), true)
}

/// Checks the folder `dir`, whose entries are `entries`, against `input`. Nothing in it is changed.
/// The names the storage policy stores games of `dat` under other than `<game>.zip`, by game: loose, or as CHD; none
/// unless it says either.
pub(crate) fn loose_names(storage: Storage, dat: Option<&Dat>) -> HashMap<String, String> {
    if let Some(dat) = dat.filter(|_| storage.format == StorageFormat::Chd) {
        return super::plans::chd_targets_of(storage, dat)
            .into_iter()
            .filter(|(_, chd)| chd.is_ok())
            .map(|(game, _)| {
                let target = format!("{game}.chd");
                (game, target)
            })
            .collect();
    }
    let Some(dat) = dat.filter(|_| storage.format == StorageFormat::Loose) else {
        return HashMap::new();
    };
    let mut names: Vec<Vec<&str>> = vec![Vec::new(); dat.games.len()];
    for rom in &dat.roms {
        names[rom.game].push(&rom.name);
    }
    dat.games
        .iter()
        .zip(names)
        .filter_map(|(game, roms)| match target_of(storage, game, &roms) {
            (target, true, _) => Some((game.clone(), target)),
            _ => None,
        })
        .collect()
}

/// The selected games of `selected`, keyed by `<game>.zip` lowercased, by the lowercased name the storage policy
/// stores each under, given the games it stores loose.
pub(crate) fn storage_targets(
    selected: &HashMap<String, String>,
    loose: &HashMap<String, String>,
) -> HashMap<String, String> {
    selected
        .iter()
        .map(|(key, game)| match loose.get(game) {
            Some(target) => (target.to_lowercase(), game.clone()),
            None => (key.clone(), game.clone()),
        })
        .collect()
}

pub(super) fn scan(
    dir: &FolderDir,
    entries: &[String],
    input: &ScanInput,
    progress: Option<&(dyn Fn(Progress) + Sync)>,
) -> Scan {
    let loose = loose_names(input.storage, input.dat);
    let selected = &storage_targets(input.selected, &loose);
    let mut out = Scan {
        files: Vec::new(),
        drift: Vec::new(),
        acknowledged: Vec::new(),
        updates: Vec::new(),
        probes: HashMap::new(),
        acks: input.acks.clone(),
    };
    let by_lower: HashMap<String, &str> = entries
        .iter()
        .map(|name| (name.to_lowercase(), name.as_str()))
        .collect();
    let total = input.built.len() + entries.len();
    let mut done = 0;
    let mut tick = || {
        done += 1;
        if let Some(progress) = progress {
            progress(Progress {
                files_done: done,
                files_total: total,
            });
        }
    };
    // Reads `name`, or reuses what an earlier check read of it while it is unchanged since.
    let probe_of = |name: &str, st: &Stat, out: &mut Scan| {
        let (probe, hashed) = reuse(dir, name, st, input.cached.get(name));
        if let Ok(p) = &probe {
            out.probes
                .insert(name.to_owned(), (Seen::of(st), p.clone()));
        }
        (probe, hashed)
    };

    let mut consumed = HashSet::new();
    for (target, built) in input.built {
        let mut file = CheckedFile {
            target: target.clone(),
            game: built.game.clone(),
            outcome: FileOutcome::Unchanged,
            hashed: false,
            torrentzip: built.torrentzip || built.loose || built.chd,
            selected: selected.contains_key(&target.to_lowercase()),
            detail: None,
        };
        let found = by_lower.get(&target.to_lowercase()).copied();
        if let Some(name) = found {
            consumed.insert(name.to_owned());
        }
        let st = match found {
            None => None,
            Some(name) if name != target => {
                file.outcome = FileOutcome::Conflict;
                file.detail = Some(format!("it is now named {name}"));
                None
            }
            Some(_) => match dir.stat(target) {
                Ok(st) => st,
                Err(e) => {
                    file.outcome = FileOutcome::Unreadable;
                    file.detail = Some(e.to_string());
                    None
                }
            },
        };
        match st {
            None if found.is_none() => file.outcome = FileOutcome::Missing,
            None => {}
            Some(st) if FolderDir::kind(&st) != FileType::RegularFile => {
                file.outcome = FileOutcome::Conflict;
                file.detail = Some("it is no longer a regular file".into());
            }
            Some(st) if file_stat(&st) == built.file => {}
            // A CHD's data was checked against the DAT when it was built: it is the same data while the raw data
            // SHA-1 its header holds is the one recorded then, however it was recompressed since.
            Some(st) if built.chd => match chd_sha1(dir, target, &st) {
                Err(e) => {
                    file.outcome = FileOutcome::Unreadable;
                    file.detail = Some(e);
                }
                Ok(sha1) if built.chd_sha1.as_deref() == Some(sha1.as_str()) => {
                    file.outcome = FileOutcome::Refreshed;
                    out.updates.push(Update {
                        target: target.clone(),
                        build: built.build,
                        was: built.file.clone(),
                        now: file_stat(&st),
                        sha256: None,
                        torrentzip: false,
                    });
                }
                Ok(_) => {
                    file.outcome = FileOutcome::Conflict;
                    file.detail = Some(
                        "it changed since it was built, and its CHD no longer holds the data that was checked \
                         against the DAT"
                            .into(),
                    );
                }
            },
            Some(st) => {
                let (probe, hashed) = probe_of(target, &st, &mut out);
                file.hashed = hashed;
                let now = file_stat(&st);
                match probe {
                    Err(e) => {
                        file.outcome = FileOutcome::Unreadable;
                        file.detail = Some(e);
                    }
                    Ok(p) if built.sha256.as_deref() == Some(p.sha256.as_str()) => {
                        file.outcome = FileOutcome::Refreshed;
                        out.updates.push(Update {
                            target: target.clone(),
                            build: built.build,
                            was: built.file.clone(),
                            now,
                            sha256: Some(p.sha256),
                            torrentzip: built.torrentzip,
                        });
                    }
                    Ok(p) if holds_as(&p, &built.roms, built.loose) => {
                        file.outcome = FileOutcome::Repacked;
                        file.torrentzip = p.torrentzip || built.loose;
                        if !file.torrentzip {
                            file.detail = Some("not TorrentZip".into());
                        }
                        out.updates.push(Update {
                            target: target.clone(),
                            build: built.build,
                            was: built.file.clone(),
                            now,
                            sha256: Some(p.sha256),
                            torrentzip: p.torrentzip,
                        });
                    }
                    Ok(_) => {
                        file.outcome = FileOutcome::Conflict;
                        file.detail = Some(
                            "it changed since it was built and no longer holds the ROMs the DAT expects there"
                                .into(),
                        );
                    }
                }
            }
        }
        out.files.push(file);
        tick();
    }

    let index = input.dat.map(GameIndex::new);
    for name in entries {
        if consumed.contains(name) {
            tick();
            continue;
        }
        let lower = name.to_lowercase();
        let mut entry = DriftEntry {
            name: name.clone(),
            kind: DriftKind::Unknown,
            game: None,
            conflict: selected.contains_key(&lower),
            hashed: false,
            detail: None,
        };
        match dir.stat(name) {
            Ok(None) => {
                tick();
                continue;
            }
            // Acknowledged and unchanged since, by identity, size and modification time: not read again.
            Ok(Some(st))
                if input.acks.get(name).is_some_and(|ack| {
                    ack.identity == Identity::of(&st) && ack.file == file_stat(&st)
                }) =>
            {
                if FolderDir::kind(&st) == FileType::Directory {
                    entry.kind = DriftKind::Directory;
                }
                out.acknowledged.push(entry);
                tick();
                continue;
            }
            Err(e) => entry.detail = Some(e.to_string()),
            Ok(Some(st)) => match FolderDir::kind(&st) {
                FileType::Directory => entry.kind = DriftKind::Directory,
                FileType::RegularFile => {
                    let (probe, hashed) = probe_of(name, &st, &mut out);
                    entry.hashed = hashed;
                    match probe {
                        Err(e) => entry.detail = Some(format!("it cannot be read: {e}")),
                        Ok(p) => match &index {
                            None => {
                                entry.detail = Some(
                                    "the DAT revision is no longer kept, so it was not compared"
                                        .into(),
                                );
                            }
                            Some(index) => {
                                if let Some(game) = index.game_of(&p) {
                                    entry.game = Some(game.to_owned());
                                    let named = loose
                                        .get(game)
                                        .cloned()
                                        .unwrap_or_else(|| format!("{game}.zip"));
                                    entry.kind = if *name != named {
                                        DriftKind::Renamed
                                    } else if selected.contains_key(&lower) {
                                        DriftKind::Selected
                                    } else {
                                        DriftKind::Unselected
                                    };
                                }
                            }
                        },
                    }
                }
                _ => {
                    entry.detail = Some("it is not a regular file, so it was not opened".into());
                }
            },
        }
        out.drift.push(entry);
        tick();
    }
    out
}

impl Scan {
    pub fn summary(&self) -> CheckSummary {
        let mut s = CheckSummary::default();
        for f in &self.files {
            *match f.outcome {
                FileOutcome::Unchanged => &mut s.unchanged,
                FileOutcome::Refreshed => &mut s.refreshed,
                FileOutcome::Repacked => &mut s.repacked,
                FileOutcome::Conflict => &mut s.conflict,
                FileOutcome::Missing => &mut s.missing,
                FileOutcome::Unreadable => &mut s.unreadable,
            } += 1;
            s.hashed += f.hashed as u64;
        }
        for d in &self.drift {
            s.conflict += d.conflict as u64;
            s.hashed += d.hashed as u64;
        }
        s.drift = self.drift.len() as u64;
        s.acknowledged = self.acknowledged.len() as u64;
        s
    }
}

/// The selected Games of Selection `selection`, by their target's lowercased name.
pub(super) fn selected_targets(c: &Connection, selection: i64) -> Result<HashMap<String, String>> {
    let mut stmt = c.prepare(
        "SELECT title FROM selection_releases WHERE selection_id = ?1 AND role = 'winner'",
    )?;
    let rows = stmt.query_map([selection], |r| r.get::<_, String>(0))?;
    let mut selected = HashMap::new();
    for title in rows {
        let title = title?;
        selected.insert(format!("{title}.zip").to_lowercase(), title);
    }
    Ok(selected)
}

pub(super) fn read_probes(c: &Connection, folder: i64) -> Result<HashMap<String, (Seen, Probe)>> {
    let mut stmt = c.prepare(
        "SELECT name, size, mtime_ns, dev, ino, ctime_ns, probe FROM folder_probes WHERE folder_id = ?1",
    )?;
    let rows = stmt.query_map([folder], |r| {
        Ok((
            r.get::<_, String>(0)?,
            (
                Seen {
                    file: FileStat {
                        size: r.get::<_, i64>(1)? as u64,
                        mtime_ns: r.get::<_, i64>(2)? as i128,
                    },
                    identity: Identity::from_sql(r.get(3)?, r.get(4)?),
                    ctime_ns: r.get::<_, i64>(5)? as i128,
                },
                json(r, 6)?,
            ),
        ))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Stores a check of Library Folder `folder`: brings up to date each built file's record that is still as the check
/// read it, keeps what it read for the next check, and records the report. Returns the stored check.
pub(super) fn store_check(
    tx: &Transaction,
    folder: i64,
    job: Option<i64>,
    scan: Option<&Scan>,
    mut check: FolderCheck,
) -> Result<FolderCheck> {
    if let Some(scan) = scan {
        for u in &scan.updates {
            tx.execute(
                "UPDATE built_files SET size = ?6, mtime_ns = ?7, sha256 = ?8, torrentzip = ?9
                 WHERE folder_id = ?1 AND target = ?2 AND build_id = ?3 AND size = ?4 AND mtime_ns = ?5",
                params![
                    folder,
                    u.target,
                    u.build,
                    u.was.size as i64,
                    u.was.mtime_ns as i64,
                    u.now.size as i64,
                    u.now.mtime_ns as i64,
                    u.sha256,
                    u.torrentzip
                ],
            )?;
        }
        tx.execute("DELETE FROM folder_probes WHERE folder_id = ?1", [folder])?;
        // An acknowledgement lasts only while its entry is as it was: one the check found changed, or gone, is
        // dropped, and the entry is drift again. Only one the check was made with, and still as it was then, is: one
        // made, or made again, since the check read the store is not what it found changed.
        let kept: HashSet<&str> = scan.acknowledged.iter().map(|d| d.name.as_str()).collect();
        for (name, ack) in scan.acks.iter().filter(|(n, _)| !kept.contains(n.as_str())) {
            let (dev, ino) = ack.identity.to_sql();
            tx.execute(
                "DELETE FROM drift_acks
                 WHERE folder_id = ?1 AND name = ?2 AND dev = ?3 AND ino = ?4 AND size = ?5 AND mtime_ns = ?6",
                params![
                    folder,
                    name,
                    dev,
                    ino,
                    ack.file.size as i64,
                    ack.file.mtime_ns as i64
                ],
            )?;
        }
        for (name, (at, probe)) in &scan.probes {
            let (dev, ino) = at.identity.to_sql();
            tx.execute(
                "INSERT INTO folder_probes (folder_id, name, size, mtime_ns, dev, ino, ctime_ns, probe)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    folder,
                    name,
                    at.file.size as i64,
                    at.file.mtime_ns as i64,
                    dev,
                    ino,
                    at.ctime_ns as i64,
                    to_json(probe)
                ],
            )?;
        }
    }
    check.summary.job = job;
    check.summary.at = now();
    tx.execute(
        "INSERT INTO folder_checks (folder_id, job_id, generation, created_at, summary, report)
         VALUES (?1, ?2, ?3, ?4, '', '')",
        params![folder, job, check.generation as i64, check.summary.at],
    )?;
    let id = tx.last_insert_rowid();
    check.summary.id = id;
    tx.execute(
        "UPDATE folder_checks SET summary = ?2, report = ?3 WHERE id = ?1",
        params![id, to_json(&check.summary), to_json(&check)],
    )?;
    tx.execute(
        "DELETE FROM folder_checks WHERE folder_id = ?1 AND id < ?2
         AND id NOT IN (SELECT check_id FROM plans WHERE check_id IS NOT NULL)",
        params![folder, id],
    )?;
    Ok(check)
}

/// The latest check of Library Folder `folder`.
pub(super) fn latest_check(c: &Connection, folder: i64) -> Result<Option<FolderCheck>> {
    Ok(c.query_row(
        "SELECT report FROM folder_checks WHERE folder_id = ?1 ORDER BY id DESC LIMIT 1",
        [folder],
        |r| json(r, 0),
    )
    .optional()?)
}

/// The summary of the latest check of Library Folder `folder`.
pub(super) fn latest_summary(c: &Connection, folder: i64) -> Result<Option<CheckSummary>> {
    Ok(c.query_row(
        "SELECT summary FROM folder_checks WHERE folder_id = ?1 ORDER BY id DESC LIMIT 1",
        [folder],
        |r| json(r, 0),
    )
    .optional()?)
}

/// What a Check Job reads from the store, in one transaction.
struct CheckInputs {
    folder: super::folders::FolderRow,
    store: String,
    built: BTreeMap<String, Built>,
    selection: Option<i64>,
    selected: HashMap<String, String>,
    storage: Storage,
    revision: (String, bool),
    cached: HashMap<String, (Seen, Probe)>,
    acks: HashMap<String, Ack>,
}

impl Store {
    /// Queues a read-only Check of Library Folder `folder`, started by `actor`. Refused while the folder has a Build
    /// or a Check queued or running.
    pub fn enqueue_check(&self, folder: i64, actor: &str) -> Result<Job> {
        self.write(|tx| {
            let row = folder_by_id(tx, folder)?;
            row.check_registered()?;
            if let Some(job) = row.active_job {
                return Err(Error::Conflict(format!(
                    "library folder {folder} already has a build or check queued or running: job {job}"
                )));
            }
            let revision: i64 = match row.current_build {
                Some(build) => tx.query_row(
                    "SELECT dat_revision_id FROM builds WHERE id = ?1",
                    [build],
                    |r| r.get(0),
                )?,
                None => choose_revision(tx, &set_by_id(tx, row.source_set_id)?, None)?.0,
            };
            tx.execute(
                "INSERT INTO jobs (state, kind, source_set_id, dat_revision_id, actor, readers, hashers, created_at,
                                   library_folder_id)
                 VALUES ('queued', 'check', ?1, ?2, ?3, 1, 1, ?4, ?5)",
                params![row.source_set_id, revision, actor, now(), folder],
            )?;
            job_by_id(tx, tx.last_insert_rowid())
        })
    }

    /// Runs the Running Check Job `id`: checks its folder against the store's record, calling `progress` as entries
    /// are checked, and stores what it found. Nothing in the folder is changed. A folder that cannot be opened fails
    /// the Job. Returns the Job as it ends.
    pub fn run_check_job(
        &self,
        config: &Config,
        id: i64,
        progress: Option<&(dyn Fn(Progress) + Sync)>,
    ) -> Result<Job> {
        let job = self.job(id)?;
        if job.kind != JobKind::Check {
            return Err(Error::Refused(format!("job {id} is not a check")));
        }
        if job.state != JobState::Running {
            return Err(Error::Refused(format!(
                "job {id} is {}, not running",
                job.state.as_str()
            )));
        }
        let folder = job
            .library_folder
            .ok_or_else(|| Error::Io(format!("check job {id} has no library folder")))?;
        let outcome = self.check_folder(config, id, folder, progress);
        if let Err(err) = outcome {
            let message = err.to_string();
            self.write(
                |tx| match finish_running(tx, id, JobState::Failed, Some(&message)) {
                    Err(Error::Io(_)) | Ok(()) => Ok(()),
                    Err(err) => Err(err),
                },
            )?;
        }
        self.job(id)
    }

    fn check_folder(
        &self,
        config: &Config,
        job: i64,
        folder: i64,
        progress: Option<&(dyn Fn(Progress) + Sync)>,
    ) -> Result<()> {
        let inputs = self.read(|c| {
            let tx = c.unchecked_transaction()?;
            let folder = folder_by_id(&tx, folder)?;
            let selection: Option<i64> = match folder.current_build {
                Some(build) => Some(tx.query_row(
                    "SELECT selection_id FROM builds WHERE id = ?1",
                    [build],
                    |r| r.get(0),
                )?),
                None => None,
            };
            let selected = match selection {
                Some(s) => selected_targets(&tx, s)?,
                None => HashMap::new(),
            };
            let revision_id: i64 = tx.query_row(
                "SELECT dat_revision_id FROM jobs WHERE id = ?1",
                [job],
                |r| r.get(0),
            )?;
            let (_, record) = revision_by_id(&tx, revision_id)?;
            let revision = (record.sha256, record.blob_present);
            let storage = crate::storage::policy(
                &tx,
                title_select_api::storage::StorageScope::PlayLibraries,
                &folder.system,
            )?;
            Ok(CheckInputs {
                storage,
                store: store_id(&tx)?,
                built: read_built(&tx, folder.id)?,
                cached: read_probes(&tx, folder.id)?,
                acks: read_acks(&tx, folder.id)?,
                folder,
                selection,
                selected,
                revision,
            })
        })?;
        let dir = open_folder(config, &inputs.folder.root, &inputs.folder.path)
            .map_err(|e| Error::Refused(format!("the library folder cannot be opened: {e}")))?;
        let entries = listing(&dir).map_err(|e| Error::Io(e.to_string()))?;
        let dat = match inputs.revision.1 {
            true => Some(Dat::read(&self.blob_path(&inputs.revision.0))?),
            false => None,
        };
        let mut check = FolderCheck {
            folder,
            summary: CheckSummary::default(),
            generation: inputs.folder.generation,
            selection: inputs.selection,
            dat_revision: Some(inputs.revision.0.clone()),
            files: Vec::new(),
            drift: Vec::new(),
            acknowledged: Vec::new(),
        };
        let problem = marker_problem(
            &dir,
            &inputs.store,
            folder,
            inputs.folder.generation,
            entries.is_empty(),
        );
        let scan = match problem {
            Some(problem) => {
                check.summary.marker = Some(problem);
                None
            }
            None => {
                let scan = scan(
                    &dir,
                    &entries,
                    &ScanInput {
                        built: &inputs.built,
                        selected: &inputs.selected,
                        dat: dat.as_ref(),
                        storage: inputs.storage,
                        cached: &inputs.cached,
                        acks: &inputs.acks,
                    },
                    progress,
                );
                check.summary = scan.summary();
                check.files = scan.files.clone();
                check.drift = scan.drift.clone();
                check.acknowledged = scan.acknowledged.clone();
                Some(scan)
            }
        };
        self.write(|tx| {
            store_check(tx, folder, Some(job), scan.as_ref(), check.clone())?;
            finish_running(tx, job, JobState::Succeeded, None)
        })
    }

    /// The latest check of Library Folder `folder`.
    pub fn latest_check(&self, folder: i64) -> Result<Option<FolderCheck>> {
        self.read(|c| {
            folder_by_id(c, folder)?;
            latest_check(c, folder)
        })
    }
}
