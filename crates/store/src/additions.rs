//! Operator Additions (#19): files no DAT knows that the Operator chose to keep, each identified by its own hashes, in
//! an Additions Set per system: the folder `<DAT name>` of the Root named `additions`, created by the first addition
//! of its system.
//!
//! An addition is recorded, with its one ROM's hashes read through a descriptor held on its file, as a durable intent
//! when the plan that adds it is approved, and is then published by its Additions Set's Additions Job:
//! - from an Ingest Folder by the one move out of it there is, which writes the addition's file in place and deletes
//!   the original only once that file is found to be exactly the addition's ROM;
//! - from a Source Set Entry or from Drift once the repair or cleanup that keeps it aside has kept it, repacked from
//!   that kept file, which stays kept, and only while it still holds exactly the ROM recorded: nothing is ever deleted
//!   to publish one, and the kept file is reserved, so that it can be neither deleted nor restored, until it is
//!   published or has failed.
//!
//! Retiring an addition moves its file into the Additions Set's own kept files, which you may then delete or restore
//! it from, by the same Job. The Job is safe to run again at any point a stop leaves it: each step is recorded before
//! it is taken, and every file is reached through a handle on its folder that follows no symlink.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io;

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use rustix::fs::FileType;
use title_select_api::additions::{
    Addition, AdditionSource, AdditionState, AdditionTarget, AdditionsCheck, AdditionsEntry,
    AdditionsFinding, AdditionsLeftover, AdditionsMarker, AdditionsSet, EditAddition, NewAddition,
};
use title_select_api::build::{ExpectedRom, FileStat, MARKER, invalid_name};
use title_select_api::storage::{Storage, StorageScope, target_of};
use title_select_api::{Job, JobKind, JobState};
use title_select_engine::build::{
    probe, repack_sources, sole_payload, verify, verify_loose, write_loose,
};

use crate::builds::{sha256_of, still_sha256};
use crate::config::{ADDITIONS_ROOT, Config};
use crate::folder_dir::{FolderDir, Identity, ctime_ns, file_stat};
use crate::folders::{folder_by_id, open_folder, store_id};
use crate::ingest::moves::exactly;
use crate::jobs::{finish_running, job_by_id};
use crate::kept::{Scope, is_kept_entry, open_retention, settle_dir_temps, unique};
use crate::repairs::{MarkerRule, open_repairable};
use crate::{Error, Fault, Result, Store, now};

/// How an addition reaches its Additions Set: the durable intent recorded when it is approved.
pub(crate) enum Link<'a> {
    /// By the ingest move with this id.
    Ingest(i64),
    /// From the file the Keep step of item `key` of repair plan `plan` keeps aside.
    Repair { plan: i64, key: &'a str },
    /// From the drift entry the cleanup plan with this id keeps aside.
    Drift(i64),
}

pub(crate) struct SetRow {
    pub id: i64,
    pub system: String,
    pub root: String,
    pub path: String,
    pub identity: Option<Identity>,
    temp_name: Option<String>,
    temp: Option<Identity>,
    created_at: String,
}

const SET_QUERY: &str =
    "SELECT s.id, d.provider || '/' || d.name, s.root, s.path, s.dev, s.ino, s.temp_name,
                                s.temp_dev, s.temp_ino, s.created_at
                         FROM additions_sets s JOIN dats d ON d.id = s.dat_id";

fn identity_at(r: &Row, i: usize) -> rusqlite::Result<Option<Identity>> {
    Ok(
        match (r.get::<_, Option<i64>>(i)?, r.get::<_, Option<i64>>(i + 1)?) {
            (Some(dev), Some(ino)) => Some(Identity::from_sql(dev, ino)),
            _ => None,
        },
    )
}

fn set_from_row(r: &Row) -> rusqlite::Result<SetRow> {
    Ok(SetRow {
        id: r.get(0)?,
        system: r.get(1)?,
        root: r.get(2)?,
        path: r.get(3)?,
        identity: identity_at(r, 4)?,
        temp_name: r.get(6)?,
        temp: identity_at(r, 7)?,
        created_at: r.get(9)?,
    })
}

fn set_by_id(c: &Connection, id: i64) -> Result<SetRow> {
    c.query_row(&format!("{SET_QUERY} WHERE s.id = ?1"), [id], set_from_row)
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("no additions set {id}")))
}

/// The Additions Set at the folder `rel` of the Root `root`, if there is one.
pub(crate) fn set_at(c: &Connection, root: &str, rel: &str) -> Result<Option<i64>> {
    Ok(c.query_row(
        "SELECT id FROM additions_sets WHERE root = ?1 AND path = ?2",
        params![root, rel],
        |r| r.get(0),
    )
    .optional()?)
}

/// The Additions Set of `system`, as `<provider>/<name>`.
pub(crate) fn set_of_system(c: &Connection, system: &str) -> Result<SetRow> {
    let (dat, _) = dat_of(c, system).map_err(|e| match e {
        Error::Refused(m) => Error::NotFound(m),
        e => e,
    })?;
    c.query_row(
        &format!("{SET_QUERY} WHERE s.dat_id = ?1"),
        [dat],
        set_from_row,
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("{system} has no operator additions yet")))
}

/// The DAT `system` names, as `<provider>/<name>`: its id and name.
fn dat_of(c: &Connection, system: &str) -> Result<(i64, String)> {
    let (provider, name) = system.split_once('/').unwrap_or(("", system));
    c.query_row(
        "SELECT id, name FROM dats WHERE provider = ?1 AND name = ?2",
        params![provider, name],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?
    .ok_or_else(|| {
        Error::Refused(format!(
            "{system} is not a registered DAT; choose the system an addition belongs to by its DAT"
        ))
    })
}

#[derive(Clone)]
struct Published {
    identity: Identity,
    file: FileStat,
    ctime_ns: Option<i128>,
    sha256: String,
}

#[derive(Clone)]
pub(crate) struct AdditionRow {
    pub addition: Addition,
    pub set_id: i64,
    raw_state: String,
    move_id: Option<i64>,
    repair: Option<(i64, String)>,
    cleanup_plan: Option<i64>,
    staged_name: Option<String>,
    staged: Option<Identity>,
    published: Option<Published>,
}

impl AdditionRow {
    /// Its published file's identity and SHA-256, once it is active.
    pub fn published(&self) -> Option<(Identity, FileStat, &str)> {
        self.published
            .as_ref()
            .map(|p| (p.identity, p.file.clone(), p.sha256.as_str()))
    }
}

const ADDITION_QUERY: &str = "SELECT a.id, d.provider || '/' || d.name, a.name, a.file_name, a.rom, a.parent_game,
                                     a.source, a.origin, a.state, a.error, a.approved_by, a.created_at,
                                     a.published_at, a.retired_at, a.set_id, a.move_id, a.repair_plan_id,
                                     a.repair_item, a.cleanup_plan_id, a.staged_name, a.staged_dev, a.staged_ino,
                                     a.dev, a.ino, a.size, a.mtime_ns, a.ctime_ns, a.sha256, a.loose
                              FROM additions a JOIN additions_sets s ON s.id = a.set_id
                              JOIN dats d ON d.id = s.dat_id";

fn parse_ns(s: Option<String>) -> Option<i128> {
    s.and_then(|s| s.parse().ok())
}

fn addition_from_row(r: &Row) -> rusqlite::Result<AdditionRow> {
    let raw_state: String = r.get(8)?;
    let state = match raw_state.as_str() {
        "staging" => AdditionState::Pending,
        other => AdditionState::parse(other).unwrap_or(AdditionState::Failed),
    };
    let rom: Option<String> = r.get(4)?;
    let source: String = r.get(6)?;
    let published = match (
        identity_at(r, 22)?,
        r.get::<_, Option<i64>>(24)?,
        parse_ns(r.get(25)?),
        r.get::<_, Option<String>>(27)?,
    ) {
        (Some(identity), Some(size), Some(mtime_ns), Some(sha256)) => Some(Published {
            identity,
            file: FileStat {
                size: size as u64,
                mtime_ns,
            },
            ctime_ns: parse_ns(r.get(26)?),
            sha256,
        }),
        _ => None,
    };
    Ok(AdditionRow {
        addition: Addition {
            id: r.get(0)?,
            system: r.get(1)?,
            name: r.get(2)?,
            file_name: r.get(3)?,
            rom: rom.and_then(|rom| serde_json::from_str(&rom).ok()),
            parent: r.get(5)?,
            source: AdditionSource::parse(&source).unwrap_or(AdditionSource::Ingest),
            origin: r.get(7)?,
            state,
            error: r.get(9)?,
            approved_by: r.get(10)?,
            created_at: r.get(11)?,
            published_at: r.get(12)?,
            retired_at: r.get(13)?,
            loose: r.get(28)?,
        },
        set_id: r.get(14)?,
        raw_state,
        move_id: r.get(15)?,
        repair: match (
            r.get::<_, Option<i64>>(16)?,
            r.get::<_, Option<String>>(17)?,
        ) {
            (Some(plan), Some(key)) => Some((plan, key)),
            _ => None,
        },
        cleanup_plan: r.get(18)?,
        staged_name: r.get(19)?,
        staged: identity_at(r, 20)?,
        published,
    })
}

fn addition_by_id(c: &Connection, id: i64) -> Result<AdditionRow> {
    c.query_row(
        &format!("{ADDITION_QUERY} WHERE a.id = ?1"),
        [id],
        addition_from_row,
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("no operator addition {id}")))
}

fn additions_where(
    c: &Connection,
    filter: &str,
    args: impl rusqlite::Params,
) -> Result<Vec<AdditionRow>> {
    let mut stmt = c.prepare(&format!(
        "{ADDITION_QUERY} WHERE {filter} ORDER BY a.name, a.id"
    ))?;
    Ok(stmt
        .query_map(args, addition_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The active additions of the Additions Set of DAT `dat`: what a build that opts in reads.
pub(crate) fn active_additions(c: &Connection, dat: i64) -> Result<Vec<AdditionRow>> {
    additions_where(c, "s.dat_id = ?1 AND a.state = 'active'", [dat])
}

/// Why `name` cannot be an addition's name, or `None` when it can: it is the name a build writes it as, `<name>.zip`,
/// so it must be a file name on every filesystem a play library may be shared from.
fn bad_name(name: &str) -> Option<String> {
    if name.trim() != name || name.is_empty() {
        return Some(format!(
            "{name:?}: an addition's name is not empty and has no leading or trailing space"
        ));
    }
    if name.starts_with('.') {
        return Some(format!(
            "{name}: an addition's name does not start with a dot"
        ));
    }
    invalid_name(name).or_else(|| invalid_name(&format!("{name}.zip")))
}

/// The one place an Operator Addition's file name and format are decided, from the storage policy `storage` of the
/// folder it is written into: its Additions Set, or a play library a build writes it into. A TorrentZip is
/// `<name>.zip`; a loose file is `<name>` with its ROM's extension, so that renaming it renames what a build writes.
/// A ROM whose name ends in `.zip`, or that gives no plain file name, stays TorrentZip. Returns the name and whether
/// it is loose.
pub(crate) fn addition_target(storage: Storage, name: &str, rom: &str) -> (String, bool) {
    let loose = match rom.rsplit_once('.') {
        Some((_, ext)) if ext.eq_ignore_ascii_case("zip") => return (format!("{name}.zip"), false),
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => format!("{name}.{ext}"),
        _ => name.to_owned(),
    };
    let (target, loose, _) = target_of(storage, name, &[&loose]);
    (target, loose)
}

/// Where a build writing into a play library stored as `storage` writes `addition`, and whether loose.
pub(crate) fn built_target(storage: Storage, addition: &Addition) -> (String, bool) {
    match &addition.rom {
        Some(rom) => addition_target(storage, &addition.name, &rom.name),
        None => (format!("{}.zip", addition.name), false),
    }
}

/// Why no live addition of the set of DAT `dat` other than `except` may be named `name`, or `None`.
fn name_taken(c: &Connection, dat: i64, name: &str, except: Option<i64>) -> Result<Option<String>> {
    let taken: Option<String> = c
        .query_row(
            "SELECT a.name FROM additions a JOIN additions_sets s ON s.id = a.set_id
             WHERE s.dat_id = ?1 AND lower(a.name) = lower(?2) AND a.id IS NOT ?3
             AND a.state IN ('pending', 'staging', 'active')",
            params![dat, name, except],
            |r| r.get(0),
        )
        .optional()?;
    Ok(taken.map(|taken| {
        format!("this system already has an addition named {taken}; choose another name")
    }))
}

/// Where a new addition is published in its Additions Set: its DAT's id, its file name, and whether it is loose.
pub(crate) struct NewTarget {
    pub dat: i64,
    pub file_name: String,
    pub loose: bool,
}

impl NewTarget {
    /// What a plan records of it, for its approval to check.
    pub(crate) fn reviewed(&self) -> AdditionTarget {
        AdditionTarget {
            file_name: self.file_name.clone(),
            loose: self.loose,
        }
    }

    /// Refuses, as stale, a plan that reviewed `new` as published at `reviewed` when it would now be published here.
    pub(crate) fn check_reviewed(
        &self,
        new: &NewAddition,
        reviewed: Option<&AdditionTarget>,
    ) -> Result<()> {
        let now = self.reviewed();
        match reviewed {
            Some(reviewed) if *reviewed == now => Ok(()),
            Some(reviewed) => Err(Error::Conflict(format!(
                "{} of {} would now be published as {}{}, not as {}{} as the plan said, because the storage policy \
                 changed; nothing was approved. Plan again and review the new plan",
                new.name,
                new.system,
                now.file_name,
                if now.loose { " (loose)" } else { "" },
                reviewed.file_name,
                if reviewed.loose { " (loose)" } else { "" },
            ))),
            None => Err(Error::Conflict(format!(
                "the plan does not say where {} of {} is published; nothing was approved. Plan again",
                new.name, new.system
            ))),
        }
    }
}

/// Checks that `new`, whose one ROM is named `rom`, can be added: its system is a registered DAT, and its name a
/// valid one no other addition of that system has. Returns where it is published, in the format the storage policy
/// says for its system's source sets now.
pub(crate) fn check_new(c: &Connection, new: &NewAddition, rom: &str) -> Result<NewTarget> {
    check_new_except(c, new, None, rom)
}

/// As [`check_new`], but not counting addition `except`, which is the very addition being checked again.
pub(crate) fn check_new_except(
    c: &Connection,
    new: &NewAddition,
    except: Option<i64>,
    rom: &str,
) -> Result<NewTarget> {
    let (dat, _) = dat_of(c, &new.system)?;
    if let Some(why) = bad_name(&new.name) {
        return Err(Error::Refused(why));
    }
    if let Some(why) = name_taken(c, dat, &new.name, except)? {
        return Err(Error::Conflict(why));
    }
    let storage = crate::storage::policy(c, StorageScope::SourceSets, &new.system)?;
    let (file_name, loose) = addition_target(storage, &new.name, rom);
    if let Some(why) = invalid_name(&file_name) {
        return Err(Error::Refused(format!("{file_name}: {why}")));
    }
    let used: bool = c.query_row(
        "SELECT EXISTS (SELECT 1 FROM additions a JOIN additions_sets s ON s.id = a.set_id
                        WHERE s.dat_id = ?1 AND lower(a.file_name) = lower(?2) AND a.id IS NOT ?3)",
        params![dat, file_name, except],
        |r| r.get(0),
    )?;
    if used {
        return Err(Error::Conflict(format!(
            "an addition of {} was published as {file_name} before; choose another name",
            new.system
        )));
    }
    if let Some(parent) = &new.parent
        && parent.trim().is_empty()
    {
        return Err(Error::Refused(
            "a parent game has a name; leave it out to have none".into(),
        ));
    }
    Ok(NewTarget {
        dat,
        file_name,
        loose,
    })
}

/// The addition an approved cleanup of Managed Folder `folder` recorded for its drift entry `origin`, while it is
/// still to be published: a cleanup planned again as its Build starts checks its addition against the others only.
pub(crate) fn pending_drift_addition(
    c: &Connection,
    folder: i64,
    origin: &str,
) -> Result<Option<i64>> {
    Ok(c.query_row(
        "SELECT a.id FROM additions a JOIN plans p ON p.id = a.cleanup_plan_id
         WHERE p.folder_id = ?1 AND a.origin = ?2 AND a.source = 'drift' AND a.state IN ('pending', 'staging')
         ORDER BY a.id DESC LIMIT 1",
        params![folder, origin],
        |r| r.get(0),
    )
    .optional()?)
}

/// Records the Additions Set of DAT `dat`, if it is not recorded yet; its folder is created when it is first needed.
pub(crate) fn ensure_set(tx: &Transaction, dat: i64) -> Result<i64> {
    let name: String = tx.query_row("SELECT name FROM dats WHERE id = ?1", [dat], |r| r.get(0))?;
    if let Some(why) = invalid_name(&name) {
        return Err(Error::Refused(format!(
            "the additions set of this system would be the folder {name}, which cannot be: {why}"
        )));
    }
    tx.execute(
        "INSERT OR IGNORE INTO additions_sets (dat_id, root, path, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![dat, ADDITIONS_ROOT, name, now()],
    )?;
    Ok(tx.query_row(
        "SELECT id FROM additions_sets WHERE dat_id = ?1",
        [dat],
        |r| r.get(0),
    )?)
}

/// Records the intent to add `new`, which came from `origin`, reached by `link`, as approved by `actor`, with `rom`,
/// the one ROM its file was read to hold when the plan was made: it is published only while it still holds exactly
/// that, and only where the plan said, `reviewed`. Returns its id. Its Additions Job is queued by whatever approved it,
/// once what adds it may be published.
pub(crate) fn insert_addition(
    tx: &Transaction,
    new: &NewAddition,
    origin: &str,
    link: Link,
    rom: &ExpectedRom,
    reviewed: Option<&AdditionTarget>,
    actor: &str,
) -> Result<i64> {
    let target = check_new(tx, new, &rom.name)?;
    target.check_reviewed(new, reviewed)?;
    let NewTarget {
        dat,
        file_name,
        loose,
    } = target;
    let set = ensure_set(tx, dat)?;
    let (source, move_id, repair_plan, repair_item, cleanup_plan) = match link {
        Link::Ingest(id) => (AdditionSource::Ingest, Some(id), None, None, None),
        Link::Repair { plan, key } => {
            (AdditionSource::SourceSet, None, Some(plan), Some(key), None)
        }
        Link::Drift(plan) => (AdditionSource::Drift, None, None, None, Some(plan)),
    };
    tx.execute(
        "INSERT INTO additions (set_id, name, file_name, rom, parent_game, source, origin, state, move_id,
                                repair_plan_id, repair_item, cleanup_plan_id, approved_by, created_at, loose)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            set,
            new.name,
            file_name,
            serde_json::to_string(rom).expect("a ROM serializes"),
            new.parent,
            source.as_str(),
            origin,
            move_id,
            repair_plan,
            repair_item,
            cleanup_plan,
            actor,
            now(),
            loose
        ],
    )?;
    Ok(tx.last_insert_rowid())
}

/// The states of an addition its Additions Job has still to settle.
const UNSETTLED: &str = "('pending', 'staging', 'retiring', 'restoring')";

/// Binds each addition to settle that no queued or running Additions Job holds to the queued Additions Job of its
/// Additions Set you have not cancelled, queuing one where there is none. An addition the Job you cancelled left
/// pending, with nothing yet done for it, fails instead, and each ingest plan that leaves with nothing more to add is
/// finished. Returns the Jobs queued.
pub(crate) fn queue_additions_jobs(tx: &Transaction) -> Result<Vec<i64>> {
    queue_additions(tx, None)
}

/// [`queue_additions_jobs`], binding only the additions in `only`, if given.
fn queue_additions(tx: &Transaction, only: Option<&BTreeSet<i64>>) -> Result<Vec<i64>> {
    let loose: Vec<(i64, i64, String, Option<bool>)> = {
        let mut stmt = tx.prepare(&format!(
            "SELECT a.id, a.set_id, a.approved_by,
                    (SELECT j.cancel_requested_at IS NOT NULL FROM additions_job_members m JOIN jobs j ON j.id = m.job_id
                     WHERE m.addition_id = a.id ORDER BY m.job_id DESC LIMIT 1)
             FROM additions a
             WHERE a.state IN {UNSETTLED}
               AND NOT EXISTS (SELECT 1 FROM additions_job_members m JOIN jobs j ON j.id = m.job_id
                               WHERE m.addition_id = a.id AND j.state IN ('queued', 'running'))
             ORDER BY a.id"
        ))?;
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut queued = Vec::new();
    for (addition, set, actor, cancelled) in loose {
        if cancelled == Some(true) && cancel_member(tx, addition)? {
            continue;
        }
        if only.is_some_and(|only| !only.contains(&addition)) {
            continue;
        }
        let job: Option<i64> = tx
            .query_row(
                "SELECT id FROM jobs WHERE kind = 'additions' AND state = 'queued' AND additions_set_id = ?1
                   AND cancel_requested_at IS NULL",
                [set],
                |r| r.get(0),
            )
            .optional()?;
        let job = match job {
            Some(job) => job,
            None => {
                tx.execute(
                    "INSERT INTO jobs (state, kind, additions_set_id, actor, readers, hashers, created_at)
                     VALUES ('queued', 'additions', ?1, ?2, 1, 1, ?3)",
                    params![set, actor, now()],
                )?;
                queued.push(tx.last_insert_rowid());
                tx.last_insert_rowid()
            }
        };
        tx.execute(
            "INSERT OR IGNORE INTO additions_job_members (job_id, addition_id) VALUES (?1, ?2)",
            params![job, addition],
        )?;
    }
    finish_addition_plans(tx)?;
    Ok(queued)
}

/// Finishes each approved ingest plan that adds Operator Additions once every addition it records has settled,
/// with what each item came to.
fn finish_addition_plans(tx: &Transaction) -> Result<()> {
    // What each plan recorded and what became of its additions are read as they are finished, in one transaction:
    // an outcome recorded meanwhile is never overwritten.
    let plans: Vec<(i64, Option<String>)> = {
        let mut stmt = tx.prepare(
            "SELECT p.id, p.outcomes FROM ingest_plans p
             WHERE p.approved_at IS NOT NULL AND p.finished_at IS NULL
               AND EXISTS (SELECT 1 FROM ingest_moves m JOIN additions a ON a.move_id = m.id
                           WHERE m.plan_id = p.id)
               AND NOT EXISTS (SELECT 1 FROM ingest_moves m JOIN additions a ON a.move_id = m.id
                               WHERE m.plan_id = p.id AND a.state IN ('pending', 'staging'))
             ORDER BY p.id",
        )?;
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (plan, recorded) in plans {
        let mut outcomes: Vec<title_select_api::ingest::IngestOutcome> = recorded
            .as_deref()
            .and_then(|o| serde_json::from_str(o).ok())
            .unwrap_or_default();
        let mut stmt = tx.prepare(
            "SELECT m.item_id, m.name, m.note, a.id FROM ingest_moves m JOIN additions a ON a.move_id = m.id
             WHERE m.plan_id = ?1 ORDER BY m.id",
        )?;
        let added = stmt
            .query_map([plan], |r| {
                Ok((
                    r.get::<_, Option<i64>>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (item, name, note, addition) in added {
            let a = addition_by_id(tx, addition)?.addition;
            let active = a.state != AdditionState::Failed;
            outcomes.push(title_select_api::ingest::IngestOutcome {
                item: item.unwrap_or_default(),
                name,
                moved: active,
                detail: active.then(|| {
                    std::iter::once(format!(
                        "added as the operator addition {} of {}, published as {}",
                        a.name, a.system, a.file_name
                    ))
                    .chain(note)
                    .collect::<Vec<_>>()
                    .join("; ")
                }),
                error: (!active).then(|| {
                    a.error
                        .unwrap_or_else(|| "it was not added; it was left as it is".into())
                }),
            });
        }
        tx.execute(
            "UPDATE ingest_plans SET finished_at = ?2, outcomes = ?3 WHERE id = ?1 AND finished_at IS NULL",
            params![plan, now(), serde_json::to_string(&outcomes).expect("outcomes serialize")],
        )?;
    }
    Ok(())
}

/// Records addition `id`, being restored, retired again, saying why it could not be restored.
fn unrestored(tx: &Transaction, id: i64, why: &str) -> Result<()> {
    tx.execute(
        "UPDATE additions SET state = 'retired', error = ?2 WHERE id = ?1 AND state = 'restoring'",
        params![id, format!("it could not be restored: {why}")],
    )?;
    Ok(())
}

/// Whether addition `id` is still waiting for the repair or cleanup that adds it to keep its file aside.
fn still_waiting(tx: &Transaction, id: i64) -> Result<bool> {
    type Row = (String, Option<i64>, Option<String>, Option<i64>, String);
    let (state, repair, item, cleanup, origin): Row = tx.query_row(
        "SELECT state, repair_plan_id, repair_item, cleanup_plan_id, origin FROM additions WHERE id = ?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )?;
    if state != "pending" {
        return Ok(false);
    }
    if let (Some(plan), Some(item)) = (repair, item) {
        let step: Option<String> = tx
            .query_row(
                "SELECT state FROM repair_steps WHERE plan_id = ?1 AND item = ?2",
                params![plan, item],
                |r| r.get(0),
            )
            .optional()?;
        return Ok(step.is_some_and(|s| !matches!(s.as_str(), "done" | "failed" | "skipped")));
    }
    if let Some(plan) = cleanup {
        let kept: Option<String> = tx
            .query_row(
                "SELECT k.state FROM kept_files k JOIN builds b ON b.id = k.build_id
                 WHERE b.plan_id = ?1 AND k.name = ?2 AND k.reason = 'drift'
                 ORDER BY k.id DESC LIMIT 1",
                params![plan, origin],
                |r| r.get(0),
            )
            .optional()?;
        return Ok(match kept {
            Some(state) => state == "keeping",
            None => tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM jobs WHERE plan_id = ?1 AND state IN ('queued', 'running'))
                     OR EXISTS (SELECT 1 FROM builds WHERE plan_id = ?1 AND state = 'running')",
                [plan],
                |r| r.get(0),
            )?,
        });
    }
    Ok(false)
}

const CANCELLED: &str = "you cancelled its additions job before it was added; whatever it came from was left where it was";

/// Ends addition `id` failed, as you cancelled the Additions Job it is a member of, if nothing was done for it yet: it
/// is pending, and its ingest move, if any, has not started. Returns whether it did.
fn cancel_member(tx: &Transaction, id: i64) -> Result<bool> {
    let (state, move_id): (String, Option<i64>) = tx.query_row(
        "SELECT state, move_id FROM additions WHERE id = ?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if state != "pending" {
        return Ok(false);
    }
    if let Some(mv) = move_id
        && tx.execute(
            "UPDATE ingest_moves SET state = 'failed', error = ?2 WHERE id = ?1 AND state = 'pending'",
            params![mv, CANCELLED],
        )? != 1
    {
        return Ok(false);
    }
    Ok(tx.execute(
        "UPDATE additions SET state = 'failed', error = ?2 WHERE id = ?1 AND state = 'pending'",
        params![id, CANCELLED],
    )? == 1)
}

/// Why kept file `kept` of a Source Set may be neither deleted nor restored now, if it may not: an Operator Addition
/// is still to be published from it.
pub(crate) fn source_kept_reserved(c: &Connection, kept: i64) -> Result<Option<String>> {
    let found: Option<(i64, String)> = c
        .query_row(
            "SELECT a.id, a.name FROM additions a
             JOIN repair_steps s ON s.plan_id = a.repair_plan_id AND s.item = a.repair_item
             WHERE s.kept_id = ?1 AND a.state IN ('pending', 'staging')",
            [kept],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(found.map(reserved_for))
}

/// Why kept file `kept` of a Managed Folder may be neither deleted nor restored now, if it may not: an Operator
/// Addition is still to be published from it.
pub(crate) fn managed_kept_reserved(c: &Connection, kept: i64) -> Result<Option<String>> {
    let found: Option<(i64, String)> = c
        .query_row(
            "SELECT a.id, a.name FROM additions a
             JOIN builds b ON b.plan_id = a.cleanup_plan_id
             JOIN kept_files k ON k.build_id = b.id AND k.name = a.origin AND k.reason = 'drift'
             WHERE k.id = ?1 AND a.state IN ('pending', 'staging')",
            [kept],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(found.map(reserved_for))
}

fn reserved_for((id, name): (i64, String)) -> String {
    format!(
        "the operator addition {name} ({id}) is still being added from it; wait until its additions job has ended"
    )
}

/// Why kept file `kept` of an Additions Set may be neither deleted nor restored now, if it may not: it is being
/// retired into, or restored from, the kept files.
pub(crate) fn additions_kept_reserved(c: &Connection, kept: i64) -> Result<Option<String>> {
    let found: Option<(i64, String)> = c
        .query_row(
            "SELECT a.id, a.name FROM additions a JOIN additions_kept k ON k.addition_id = a.id
             WHERE k.id = ?1 AND a.state IN ('retiring', 'restoring')",
            [kept],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(found.map(|(id, name)| {
        format!(
            "the operator addition {name} ({id}) is being retired or restored; wait until its additions job has \
             ended"
        )
    }))
}

/// Why an addition of the Additions Set `set` may not be retired now, if it may not: a build that may read it is
/// queued or running.
fn build_reading(c: &Connection, set: i64) -> Result<Option<String>> {
    let job: Option<i64> = c
        .query_row(
            "SELECT j.id FROM jobs j JOIN managed_folders f ON f.id = j.managed_folder_id
             JOIN source_sets ss ON ss.id = f.source_set_id
             WHERE j.kind = 'build' AND j.state IN ('queued', 'running')
               AND ss.dat_id = (SELECT dat_id FROM additions_sets WHERE id = ?1)
             LIMIT 1",
            [set],
            |r| r.get(0),
        )
        .optional()?;
    Ok(job.map(|job| {
        format!(
            "build job {job} may read this system's additions; retire it once that job has ended"
        )
    }))
}

/// Whether `found`, read from a file now, is `recorded`: the same name and exactly the same hashes and size.
fn same_rom(found: &ExpectedRom, recorded: &ExpectedRom) -> bool {
    found.name == recorded.name
        && found.size == recorded.size
        && found.crc == recorded.crc
        && found.md5 == recorded.md5
        && found.sha1 == recorded.sha1
        && found.sha256 == recorded.sha256
}

/// The ROM a file whose one payload is `payload` holds, named as its member, or as `loose` for a file that is not a
/// ZIP. `None` unless it holds exactly one payload, read whole.
pub(crate) fn sole_rom(
    p: &title_select_engine::build::Probe,
    loose: &str,
) -> Option<(ExpectedRom, Option<String>)> {
    match (p.payloads.as_slice(), &p.error) {
        ([only], None) => {
            let name = only
                .name
                .clone()
                .unwrap_or_else(|| loose.rsplit('/').next().unwrap_or(loose).to_owned());
            Some((
                ExpectedRom {
                    name,
                    size: only.size,
                    crc: Some(only.crc.clone()),
                    md5: Some(only.md5.clone()),
                    sha1: Some(only.sha1.clone()),
                    sha256: Some(only.sha256.clone()),
                },
                only.name.clone(),
            ))
        }
        _ => None,
    }
}

fn io(e: io::Error) -> Error {
    Error::Io(e.to_string())
}

/// Whether `name` in `dir`, found with the identity, size and mtime of the published file `p`, is still that file: it
/// has the ctime recorded, or, when that moved, as a change of its mode or links does, it hashes as recorded, read
/// through a descriptor held on it while its ctime, size and mtime stay still.
fn still_published(dir: &FolderDir, name: &str, p: &Published) -> Result<bool> {
    let held = match dir.open_file(name) {
        Ok(held) => held,
        Err(_) => return Ok(false),
    };
    let st = rustix::fs::fstat(&held).map_err(|e| io(e.into()))?;
    if Identity::of(&st) != p.identity || file_stat(&st) != p.file {
        return Ok(false);
    }
    if p.ctime_ns == Some(ctime_ns(&st)) {
        return Ok(true);
    }
    let sha256 = still_sha256(&held, ctime_ns(&st)).map_err(io)?;
    Ok(sha256.as_deref() == Some(p.sha256.as_str()))
}

/// Whether `file` holds `rom` and nothing else, exactly: as a TorrentZip, or, `loose`, as its bytes.
fn holds_exactly(file: &std::fs::File, rom: &ExpectedRom, loose: bool) -> bool {
    if loose {
        return verify_loose(file, rom).is_ok();
    }
    verify(file, std::slice::from_ref(rom)).is_ok()
        && matches!(sole_payload(file), Ok((name, sums)) if name == rom.name && exactly(rom, &sums))
}

impl Store {
    /// Every Additions Set, with its additions.
    pub fn additions_sets(&self) -> Result<Vec<AdditionsSet>> {
        self.read(|c| {
            let mut stmt = c.prepare(&format!("{SET_QUERY} ORDER BY 2"))?;
            let sets = stmt
                .query_map([], set_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            sets.into_iter().map(|set| set_record(c, set)).collect()
        })
    }

    /// The Additions Set of `system`, as `<provider>/<name>`.
    pub fn additions_set(&self, system: &str) -> Result<AdditionsSet> {
        self.read(|c| set_record(c, set_of_system(c, system)?))
    }

    pub fn addition(&self, id: i64) -> Result<Addition> {
        self.read(|c| addition_by_id(c, id).map(|a| a.addition))
    }

    /// Renames addition `id`, or sets or clears its parent Game. A new name is what the next build writes it as; its
    /// file in the Additions Set keeps the name it was published under.
    pub fn edit_addition(&self, id: i64, edit: &EditAddition) -> Result<Addition> {
        self.write(|tx| {
            let row = addition_by_id(tx, id)?;
            if matches!(
                row.addition.state,
                AdditionState::Retired | AdditionState::Failed
            ) {
                return Err(Error::Refused(format!(
                    "addition {id} is {}; only an addition being added or added can be changed",
                    row.addition.state.as_str()
                )));
            }
            if let Some(name) = &edit.name {
                if let Some(why) = bad_name(name) {
                    return Err(Error::Refused(why));
                }
                let dat: i64 = tx.query_row(
                    "SELECT dat_id FROM additions_sets WHERE id = ?1",
                    [row.set_id],
                    |r| r.get(0),
                )?;
                if let Some(why) = name_taken(tx, dat, name, Some(id))? {
                    return Err(Error::Conflict(why));
                }
                tx.execute(
                    "UPDATE additions SET name = ?2 WHERE id = ?1",
                    params![id, name],
                )?;
            }
            if let Some(parent) = &edit.parent {
                if parent.as_deref().is_some_and(|p| p.trim().is_empty()) {
                    return Err(Error::Refused(
                        "a parent game has a name; clear it to have none".into(),
                    ));
                }
                tx.execute(
                    "UPDATE additions SET parent_game = ?2 WHERE id = ?1",
                    params![id, parent],
                )?;
            }
            addition_by_id(tx, id).map(|a| a.addition)
        })
    }

    /// Retires addition `id`: builds no longer read it, and its Additions Job moves its file into the Additions Set's
    /// kept files, which you may then delete or restore it from. Refused while a build that may read it is queued or
    /// running. Returns it as it is now, retiring.
    pub fn retire_addition(&self, id: i64) -> Result<Addition> {
        self.write(|tx| {
            let row = addition_by_id(tx, id)?;
            let Some(p) = row.published.as_ref().filter(|_| row.raw_state == "active") else {
                return Err(Error::Refused(format!(
                    "addition {id} is {}; only an added addition can be retired",
                    row.addition.state.as_str()
                )));
            };
            if let Some(why) = build_reading(tx, row.set_id)? {
                return Err(Error::Conflict(why));
            }
            let (dev, ino) = p.identity.to_sql();
            tx.execute(
                "INSERT INTO additions_kept (additions_set_id, addition_id, generation, name, kept_name, reason,
                                             sha256, size, mtime_ns, dev, ino, state, kept_at)
                 VALUES (?1, ?2, ?2, ?3, ?3, 'retired', ?4, ?5, ?6, ?7, ?8, 'keeping', ?9)",
                params![
                    row.set_id,
                    id,
                    row.addition.file_name,
                    p.sha256,
                    p.file.size as i64,
                    p.file.mtime_ns.to_string(),
                    dev,
                    ino,
                    now()
                ],
            )?;
            tx.execute(
                "UPDATE additions SET state = 'retiring', error = NULL, requests = requests + 1 WHERE id = ?1",
                [id],
            )?;
            queue_additions_jobs(tx)?;
            addition_by_id(tx, id).map(|a| a.addition)
        })
    }

    /// Restores the retired addition `id`: its Additions Job moves its file back from the Additions Set's kept files,
    /// without replacing anything, and builds that opt in read it again. Refused unless its file is still kept, and
    /// while another addition of its system has its name. Returns it as it is now, restoring.
    pub fn restore_addition(&self, id: i64) -> Result<Addition> {
        self.write(|tx| {
            let row = addition_by_id(tx, id)?;
            if row.raw_state != "retired" {
                return Err(Error::Refused(format!(
                    "addition {id} is {}; only a retired addition can be restored",
                    row.addition.state.as_str()
                )));
            }
            let kept: Option<String> = tx
                .query_row(
                    "SELECT state FROM additions_kept WHERE addition_id = ?1 ORDER BY id DESC LIMIT 1",
                    [id],
                    |r| r.get(0),
                )
                .optional()?;
            if kept.as_deref() != Some("kept") || kept_untrusted(tx, id)? {
                return Err(Error::Refused(format!(
                    "addition {id}'s file is not among the kept files of its additions set, so it cannot be restored"
                )));
            }
            let dat: i64 = tx.query_row(
                "SELECT dat_id FROM additions_sets WHERE id = ?1",
                [row.set_id],
                |r| r.get(0),
            )?;
            if let Some(why) = name_taken(tx, dat, &row.addition.name, Some(id))? {
                return Err(Error::Conflict(format!("{why}, or rename the other first")));
            }
            tx.execute(
                "UPDATE additions SET state = 'restoring', error = NULL, requests = requests + 1 WHERE id = ?1",
                [id],
            )?;
            queue_additions_jobs(tx)?;
            addition_by_id(tx, id).map(|a| a.addition)
        })
    }

    /// The Additions Jobs queued now, for the service to announce.
    pub fn queued_additions_jobs(&self) -> Result<Vec<Job>> {
        self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT id FROM jobs WHERE kind = 'additions' AND state = 'queued' ORDER BY id",
            )?;
            let ids = stmt
                .query_map([], |r| r.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            ids.into_iter().map(|id| job_by_id(c, id)).collect()
        })
    }

    /// Run when the service starts, after [`Store::recover_jobs`]: queues an Additions Job for each Additions Set with
    /// an addition a stop left to settle. Returns the Jobs queued.
    pub fn recover_additions(&self) -> Result<Vec<Job>> {
        self.write(|tx| {
            queue_additions_jobs(tx)?
                .into_iter()
                .map(|id| job_by_id(tx, id))
                .collect()
        })
    }

    /// Checks the Additions Set of `system` against its additions' records, which act as its DAT, only reading it:
    /// each file an active addition was published as must still be that file, unchanged, and anything else is drift.
    /// Title Select's own entries are not drift.
    pub fn check_additions(&self, config: &Config, system: &str) -> Result<AdditionsCheck> {
        let (set, rows) = self.read(|c| {
            let (dat, _) = dat_of(c, system)?;
            let set = c
                .query_row(
                    &format!("{SET_QUERY} WHERE s.dat_id = ?1"),
                    [dat],
                    set_from_row,
                )
                .optional()?
                .ok_or_else(|| {
                    Error::NotFound(format!("{system} has no operator additions yet"))
                })?;
            let rows = additions_where(c, "a.set_id = ?1", [set.id])?;
            Ok((set, rows))
        })?;
        let mut entries = Vec::new();
        let mut by_file: HashMap<String, &AdditionRow> = HashMap::new();
        for row in &rows {
            // Only an addition whose file is meant to be in the folder speaks for its name; no two do at once.
            if !matches!(
                row.addition.state,
                AdditionState::Failed | AdditionState::Retired
            ) {
                by_file.insert(row.addition.file_name.clone(), row);
            }
        }
        let dir = match set.identity {
            None => None,
            Some(_) => Some(self.open_set(config, &set)?.map_err(Error::Refused)?),
        };
        let listed = match &dir {
            Some(dir) => dir.entries().map_err(io)?,
            None => Vec::new(),
        };
        let mut seen = std::collections::HashSet::new();
        for (name, _) in listed {
            if name.starts_with(MARKER) || name.contains(".title-select-new-") {
                continue;
            }
            let dir = dir.as_ref().expect("listed only from an open folder");
            let st = dir.stat(&name).map_err(io)?;
            let Some(st) = st else { continue };
            seen.insert(name.clone());
            let regular = FolderDir::kind(&st) == FileType::RegularFile;
            let entry = |finding, addition: Option<i64>, detail: Option<String>| AdditionsEntry {
                name: name.clone(),
                finding,
                addition,
                detail,
            };
            entries.push(match by_file.get(&name) {
                None if regular => entry(AdditionsFinding::Unknown, None, None),
                None => entry(AdditionsFinding::Other, None, None),
                Some(row) => {
                    let id = Some(row.addition.id);
                    match (row.addition.state, &row.published) {
                        (AdditionState::Retiring | AdditionState::Restoring, _) => entry(
                            AdditionsFinding::Retired,
                            id,
                            Some("its addition is being retired or restored".into()),
                        ),
                        (AdditionState::Active, Some(p))
                            if regular
                                && Identity::of(&st) == p.identity
                                && file_stat(&st) == p.file
                                && still_published(dir, &name, p)? =>
                        {
                            entry(AdditionsFinding::Intact, id, None)
                        }
                        (AdditionState::Active, _) => entry(
                            AdditionsFinding::Changed,
                            id,
                            Some("it is no longer the file that was published".into()),
                        ),
                        _ => entry(
                            AdditionsFinding::Unknown,
                            id,
                            Some("its addition is still being added".into()),
                        ),
                    }
                }
            });
        }
        for row in &rows {
            if row.addition.state == AdditionState::Active
                && !seen.contains(&row.addition.file_name)
            {
                entries.push(AdditionsEntry {
                    name: row.addition.file_name.clone(),
                    finding: AdditionsFinding::Missing,
                    addition: Some(row.addition.id),
                    detail: None,
                });
            }
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(AdditionsCheck {
            system: set.system,
            checked_at: now(),
            entries,
        })
    }

    /// Opens the folder of Additions Set `set`, which must be in place: in the configured, writable Root `additions`,
    /// the directory recorded, with this store's marker for it.
    pub(crate) fn open_set(
        &self,
        config: &Config,
        set: &SetRow,
    ) -> Result<std::result::Result<FolderDir, String>> {
        let root = match self.additions_root(config) {
            Ok(root) => root,
            Err(why) => return Ok(Err(why)),
        };
        let Some(recorded) = set.identity else {
            return Ok(Err(format!(
                "the additions set of {} has no folder yet",
                set.system
            )));
        };
        let dir = match root.sub(&set.path) {
            Ok(dir) => dir,
            Err(e) => {
                return Ok(Err(format!(
                    "the additions set of {} cannot be opened: {e}",
                    set.system
                )));
            }
        };
        if dir.identity().map_err(io)? != recorded {
            return Ok(Err(format!(
                "{} is not the folder Title Select created for the additions of {}; it was left as it is",
                dir.path().display(),
                set.system
            )));
        }
        let expected = AdditionsMarker {
            store: self.read(store_id)?,
            additions_set: set.id,
        };
        let marker = dir
            .read(MARKER, 4096)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<AdditionsMarker>(&bytes).ok());
        if marker.as_ref() != Some(&expected) {
            return Ok(Err(format!(
                "{} does not hold this store's marker for the additions of {}; it was left as it is",
                dir.path().display(),
                set.system
            )));
        }
        Ok(Ok(dir))
    }

    /// Opens the folder of Additions Set `id`, which a build reads its Operator Additions from.
    pub(crate) fn open_additions(
        &self,
        config: &Config,
        id: i64,
    ) -> Result<std::result::Result<FolderDir, String>> {
        let set = self.read(|c| set_by_id(c, id))?;
        self.open_set(config, &set)
    }

    /// Opens the folder of the Additions Set at `rel` in the Root `root`, the destination of an ingest move, if there
    /// is one there.
    pub(crate) fn open_set_at(
        &self,
        config: &Config,
        root: &str,
        rel: &str,
    ) -> Result<Option<std::result::Result<FolderDir, String>>> {
        let Some(id) = self.read(|c| set_at(c, root, rel))? else {
            return Ok(None);
        };
        let set = self.read(|c| set_by_id(c, id))?;
        self.open_set(config, &set).map(Some)
    }

    fn additions_root(&self, config: &Config) -> std::result::Result<FolderDir, String> {
        match config.root(ADDITIONS_ROOT) {
            Some(root) if root.writable => {
                FolderDir::open(&root.path, "").map_err(|e| e.to_string())
            }
            Some(_) => Err(format!(
                "root {ADDITIONS_ROOT} is not writable now; declare it with writable = true, then restart the service"
            )),
            None => Err(format!(
                "there is no root {ADDITIONS_ROOT} in the configuration; add one to hold the additions sets"
            )),
        }
    }

    /// Opens the folder of Additions Set `id`, creating it first if it is not created yet: made private under a
    /// recorded temporary name, checked to be the empty directory made, given its marker, and renamed into place
    /// without replacing anything. A temporary folder a stop left is used only once it is found in place as the one
    /// recorded; otherwise it is left as it is, recorded among the set's leftovers, and a new one made.
    pub(crate) fn ensure_set_dir(
        &self,
        config: &Config,
        id: i64,
    ) -> Result<std::result::Result<FolderDir, String>> {
        let _guard = self.set_dirs.lock().unwrap_or_else(|e| e.into_inner());
        let set = self.read(|c| set_by_id(c, id))?;
        if set.identity.is_some() {
            return self.open_set(config, &set);
        }
        let root = match self.additions_root(config) {
            Ok(root) => root,
            Err(why) => return Ok(Err(why)),
        };
        if let (Some(temp), Some(_)) = (set.temp, &set.temp_name)
            && let Some(st) = root.stat(&set.path).map_err(io)?
            && FolderDir::kind(&st) == FileType::Directory
            && Identity::of(&st) == temp
        {
            // Renamed into place before a stop: its marker was written and synced before the rename.
            root.sync().map_err(io)?;
            self.record_set_dir(id, temp)?;
            return self.open_set(config, &self.read(|c| set_by_id(c, id))?);
        }
        let temp_name = format!("{}.new-{}", set.path, unique());
        self.write(|tx| {
            // An earlier attempt's temporary folder, if one was made, is left where it is; its record moves to the
            // leftovers in the same transaction that replaces it, so that it is never forgotten.
            if let Some(old) = &set.temp_name {
                let (dev, ino) = match set.temp.map(Identity::to_sql) {
                    Some((dev, ino)) => (Some(dev), Some(ino)),
                    None => (None, None),
                };
                tx.execute(
                    "INSERT INTO additions_set_leftovers (set_id, name, dev, ino, found_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT (set_id, name) DO UPDATE SET dev = coalesce(excluded.dev, dev),
                                                              ino = coalesce(excluded.ino, ino)",
                    params![id, old, dev, ino, now()],
                )?;
            }
            tx.execute(
                "UPDATE additions_sets SET temp_name = ?2, temp_dev = NULL, temp_ino = NULL WHERE id = ?1",
                params![id, temp_name],
            )?;
            Ok(())
        })?;
        if let Err(e) = root.mkdir_private(&temp_name) {
            return Ok(Err(format!(
                "the additions set of {} cannot be created: {e}",
                set.system
            )));
        }
        let made = root.sub(&temp_name).map_err(io)?;
        let temp = match made.private(true).map_err(io)? {
            Ok(temp) => temp,
            Err(why) => {
                return Ok(Err(format!(
                    "{} is not the folder Title Select made: {why}; it was left as it is",
                    made.path().display()
                )));
            }
        };
        let (dev, ino) = temp.to_sql();
        self.write(|tx| {
            tx.execute(
                "UPDATE additions_sets SET temp_dev = ?2, temp_ino = ?3 WHERE id = ?1",
                params![id, dev, ino],
            )?;
            Ok(())
        })?;
        self.fault_point(Fault::AdditionSetMade);
        let marker = AdditionsMarker {
            store: self.read(store_id)?,
            additions_set: id,
        };
        let written = made.create_new(MARKER).and_then(|mut file| {
            use std::io::Write;
            file.write_all(&serde_json::to_vec(&marker).expect("a marker serializes"))?;
            file.sync_all()
        });
        if let Err(e) = written.and_then(|()| made.sync()) {
            return Ok(Err(format!(
                "the additions set of {} cannot be created: {e}",
                set.system
            )));
        }
        if let Err(e) = root.rename_new(&temp_name, &root, &set.path) {
            return Ok(Err(match e.kind() {
                io::ErrorKind::AlreadyExists => format!(
                    "{} is taken by something Title Select did not create; move it out of the way. The new folder \
                     was left at {temp_name}",
                    root.path().join(&set.path).display()
                ),
                _ => format!("the additions set of {} cannot be created: {e}", set.system),
            }));
        }
        self.fault_point(Fault::AdditionSetPublished);
        root.sync().map_err(io)?;
        match root.stat(&set.path).map_err(io)? {
            Some(st) if Identity::of(&st) == temp => {}
            _ => {
                return Ok(Err(format!(
                    "{} is not the folder Title Select put there; it was left as it is",
                    root.path().join(&set.path).display()
                )));
            }
        }
        self.record_set_dir(id, temp)?;
        self.open_set(config, &self.read(|c| set_by_id(c, id))?)
    }

    fn record_set_dir(&self, id: i64, identity: Identity) -> Result<()> {
        let (dev, ino) = identity.to_sql();
        self.write(|tx| {
            tx.execute(
                "UPDATE additions_sets SET dev = ?2, ino = ?3, temp_name = NULL, temp_dev = NULL, temp_ino = NULL,
                                           marker_at = ?4
                 WHERE id = ?1",
                params![id, dev, ino, now()],
            )?;
            Ok(())
        })
    }

    /// Runs the Running Additions Job `id`: settles each of its members not yet settled, oldest first: the additions
    /// to settle when it was queued, and those that joined it before it started. One being added is published, or ends failed, saying why; one whatever adds it has not finished with waits for
    /// that to queue the Job again. One being retired or restored is moved into or out of the kept files. Once you
    /// cancel the Job, each member nothing was yet done for ends failed instead, and whatever it came from is left
    /// where it was. One that cannot be settled now, because the Root `additions` cannot be written, is left as it is
    /// and the Job Failed; it is tried again when the service next starts. Returns the Job as it ends.
    pub fn run_additions_job(&self, config: &Config, id: i64) -> Result<Job> {
        let job = self.job(id)?;
        if job.kind != JobKind::Additions {
            return Err(Error::Refused(format!("job {id} is not an additions job")));
        }
        if job.state != JobState::Running {
            return Err(Error::Refused(format!(
                "job {id} is {}, not running",
                job.state.as_str()
            )));
        }
        let set: i64 = self.read(|c| {
            Ok(c.query_row(
                "SELECT additions_set_id FROM jobs WHERE id = ?1",
                [id],
                |r| r.get(0),
            )?)
        })?;
        let mut visits = BTreeMap::new();
        let ran = self.run_additions(config, set, id, &mut visits);
        let (to, why) = match &ran {
            Ok(Ok(false)) => (JobState::Succeeded, None),
            Ok(Ok(true)) => (
                JobState::Interrupted,
                Some(
                    "you cancelled it; what it had started was finished, and every addition it had not started \
                     failed, leaving whatever it came from where it was"
                        .to_owned(),
                ),
            ),
            Ok(Err(why)) => (JobState::Failed, Some(why.clone())),
            Err(err) => (JobState::Failed, Some(err.to_string())),
        };
        // The Job ends, and hands what its members were given to do meanwhile to a fresh Job, at once: an addition you
        // asked to retire or restore after the Job visited it, or one whose repair or cleanup kept its file aside after
        // the Job found it waiting. One left blocked, or as it was, waits for whatever settles it to queue a Job, or for
        // the service to start.
        self.write(|tx| {
            match finish_running(tx, id, to, why.as_deref()) {
                Err(Error::Io(_)) | Ok(()) => {}
                Err(err) => return Err(err),
            }
            let mut again = BTreeSet::new();
            for (addition, (visited, settled)) in &visits {
                let (state, requests): (String, i64) = tx.query_row(
                    "SELECT state, requests FROM additions WHERE id = ?1",
                    [addition],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                let actionable = matches!(
                    state.as_str(),
                    "pending" | "staging" | "retiring" | "restoring"
                ) && (requests != *visited
                    || matches!(settled, Settled::Waiting) && !still_waiting(tx, *addition)?);
                if actionable {
                    again.insert(*addition);
                }
            }
            queue_additions(tx, Some(&again))?;
            Ok(())
        })?;
        self.job(id)
    }

    /// Settles the members of Job `job`, additions of Additions Set `set`, recording in `visits` how many times you had
    /// asked to retire or restore each when it was visited, and how it was settled. `Ok(true)` once you cancelled it; `Err` says why an addition was left to be
    /// tried again; it joins a later Job.
    fn run_additions(
        &self,
        config: &Config,
        set: i64,
        job: i64,
        visits: &mut BTreeMap<i64, (i64, Settled)>,
    ) -> Result<std::result::Result<bool, String>> {
        let _guard = self.publishing.lock().unwrap_or_else(|e| e.into_inner());
        let row = self.read(|c| set_by_id(c, set))?;
        if row.identity.is_some()
            && let Ok(dir) = self.open_set(config, &row)?
            && let Err(why) = settle_dir_temps(self, Scope::Additions(set), &dir)?
        {
            return Ok(Err(why));
        }
        let unsettled = self.read(|c| {
            additions_where(
                c,
                &format!(
                    "a.state IN {UNSETTLED}
                     AND a.id IN (SELECT addition_id FROM additions_job_members WHERE job_id = ?1)"
                ),
                [job],
            )
        })?;
        let mut cancelled = false;
        let mut blocked = Vec::new();
        for row in unsettled {
            // Its requests are counted before it is read again: one made after is handed to a fresh Job.
            let (requests, row) = self.read(|c| {
                let requests: i64 = c.query_row(
                    "SELECT requests FROM additions WHERE id = ?1",
                    [row.addition.id],
                    |r| r.get(0),
                )?;
                Ok((requests, addition_by_id(c, row.addition.id)?))
            })?;
            if !matches!(
                row.raw_state.as_str(),
                "pending" | "staging" | "retiring" | "restoring"
            ) {
                continue;
            }
            cancelled = cancelled || self.cancel_requested(job)?;
            if cancelled && self.write(|tx| cancel_member(tx, row.addition.id))? {
                continue;
            }
            let settled = match row.raw_state.as_str() {
                "retiring" => self.retire(config, &row)?,
                "restoring" => self.restore_retired(config, &row)?,
                _ => self.settle(config, &row)?,
            };
            if let Settled::Blocked(why) = &settled {
                blocked.push(format!("operator addition {}: {why}", row.addition.id));
            }
            visits.insert(row.addition.id, (requests, settled));
        }
        Ok(match blocked.is_empty() {
            true => Ok(cancelled),
            false => Err(format!(
                "{}; it is tried again when the service next starts",
                blocked.join("; ")
            )),
        })
    }

    /// Settles addition `row`, being added.
    fn settle(&self, config: &Config, row: &AdditionRow) -> Result<Settled> {
        let id = row.addition.id;
        if let Err(why) = self.additions_root(config) {
            return Ok(Settled::Blocked(why));
        }
        if let Some(move_id) = row.move_id {
            let ended: bool = self.read(|c| {
                Ok(c.query_row(
                    "SELECT state IN ('done', 'failed') FROM ingest_moves WHERE id = ?1",
                    [move_id],
                    |r| r.get(0),
                )?)
            })?;
            if !ended {
                // A folder that cannot be made fails the move, which then never started, and so the addition.
                if let Err(why) = self.ensure_set_dir(config, row.set_id)? {
                    self.write(|tx| {
                        tx.execute(
                            "UPDATE ingest_moves SET state = 'failed', error = ?2 WHERE id = ?1 AND state = 'pending'",
                            params![move_id, why],
                        )?;
                        Ok(())
                    })?;
                }
                self.drive_move(config, move_id)?.ok();
            }
            return self.settle_ingest(row, move_id);
        }
        let kept = if let Some((plan, key)) = &row.repair {
            self.repair_kept(config, *plan, key)?
        } else if let Some(plan) = row.cleanup_plan {
            self.drift_kept(config, plan, &row.addition.origin)?
        } else {
            Kept::Failed("it records no way to be added".into())
        };
        match kept {
            Kept::Waiting => Ok(Settled::Waiting),
            Kept::Failed(why) => self.fail_addition(id, why),
            Kept::Found {
                dir,
                name,
                identity,
                file,
            } => {
                let dest = match self.ensure_set_dir(config, row.set_id)? {
                    Ok(dest) => dest,
                    Err(why) => return self.fail_addition(id, why),
                };
                self.publish_kept(row, &dir, &name, identity, &file, &dest)
            }
        }
    }

    fn fail_addition(&self, id: i64, why: String) -> Result<Settled> {
        self.write(|tx| {
            tx.execute(
                "UPDATE additions SET state = 'failed', error = ?2 WHERE id = ?1 AND state IN ('pending', 'staging')",
                params![id, why],
            )?;
            Ok(())
        })?;
        Ok(Settled::Done)
    }

    /// Moves the file of addition `row`, being retired, into the folder of its id among its Additions Set's kept
    /// files, without replacing anything, and records it kept and the addition retired. A file at its name that is no
    /// longer the file published is left as it is, and the addition retired with nothing kept.
    fn retire(&self, config: &Config, row: &AdditionRow) -> Result<Settled> {
        let id = row.addition.id;
        if let Err(why) = self.additions_root(config) {
            return Ok(Settled::Blocked(why));
        }
        let Some(kept) = self.read(|c| kept_of(c, id, "keeping"))? else {
            return self.retire_ends(id, None, "it records no kept file to retire it into".into());
        };
        let Some(p) = &row.published else {
            return self.retire_ends(id, Some(kept.id), "it records no published file".into());
        };
        let set = self.read(|c| set_by_id(c, row.set_id))?;
        let dir = match self.open_set(config, &set)? {
            Ok(dir) => dir,
            Err(why) => return self.retire_ends(id, Some(kept.id), why),
        };
        let area = match open_retention(
            self,
            Scope::Additions(set.id),
            &dir,
            kept.generation as u64,
            true,
        )? {
            Ok(Some(area)) => area.generation,
            Ok(None) => {
                return self.unretire(id, kept.id, "the kept files folder is missing".into());
            }
            Err(why) => return self.unretire(id, kept.id, why),
        };
        dir.sync().map_err(io)?;
        area.sync().map_err(io)?;
        let file_name = row.addition.file_name.as_str();
        // Moved before a stop.
        if is_kept_entry(&area, &kept.kept_name, p.identity, Some(&p.file), false) {
            return self.retired(id, kept.id);
        }
        if !still_published(&dir, file_name, p)? {
            return self.retire_ends(
                id,
                Some(kept.id),
                format!(
                    "{}/{file_name} is no longer the file that was published; it was left as it is",
                    dir.path().display()
                ),
            );
        }
        let Some(held) = dir.pin(file_name, p.identity) else {
            return self.unretire(id, kept.id, format!("{file_name} cannot be opened"));
        };
        if let Err(e) = dir.rename_new(file_name, &area, &kept.kept_name) {
            return self.unretire(id, kept.id, format!("its file cannot be kept aside: {e}"));
        }
        self.fault_point(Fault::AdditionRetireMoved);
        dir.sync().map_err(io)?;
        area.sync().map_err(io)?;
        if is_kept_entry(&area, &kept.kept_name, p.identity, Some(&p.file), false) {
            drop(held);
            return self.retired(id, kept.id);
        }
        // Something else was renamed in its place: it goes back, or, if it cannot, is recorded kept as found, never
        // to be deleted on a stamp read from its name.
        let found = area.stat(&kept.kept_name).map_err(io)?;
        let back = area
            .rename_new(&kept.kept_name, &dir, file_name)
            .and_then(|_| {
                area.sync()?;
                dir.sync()
            });
        let why = format!(
            "{file_name} was replaced by something else while it was kept aside; that was left as it is"
        );
        match (back, found) {
            (Ok(()), _) => self.unretire(id, kept.id, why),
            (Err(_), Some(st)) => {
                let (dev, ino) = Identity::of(&st).to_sql();
                self.write(|tx| {
                    tx.execute(
                        "UPDATE additions_kept SET dev = ?2, ino = ?3, size = ?4, mtime_ns = ?5, sha256 = NULL,
                                                   reason = 'found', state = 'kept', error = ?6
                         WHERE id = ?1 AND state = 'keeping'",
                        params![
                            kept.id,
                            dev,
                            ino,
                            st.st_size,
                            file_stat(&st).mtime_ns.to_string(),
                            why
                        ],
                    )?;
                    tx.execute(
                        "UPDATE additions SET state = 'retired', retired_at = ?2, error = ?3
                         WHERE id = ?1 AND state = 'retiring'",
                        params![id, now(), why],
                    )?;
                    Ok(())
                })?;
                Ok(Settled::Done)
            }
            (Err(_), None) => self.retire_ends(id, Some(kept.id), why),
        }
    }

    /// Records addition `id` retired and its file, kept as `kept`, kept.
    fn retired(&self, id: i64, kept: i64) -> Result<Settled> {
        self.write(|tx| {
            tx.execute(
                "UPDATE additions_kept SET state = 'kept', error = NULL WHERE id = ?1 AND state = 'keeping'",
                [kept],
            )?;
            tx.execute(
                "UPDATE additions SET state = 'retired', retired_at = ?2, error = NULL
                 WHERE id = ?1 AND state = 'retiring'",
                params![id, now()],
            )?;
            Ok(())
        })?;
        Ok(Settled::Done)
    }

    /// Records addition `id` retired with nothing kept, saying why: builds no longer read it either way.
    fn retire_ends(&self, id: i64, kept: Option<i64>, why: String) -> Result<Settled> {
        self.write(|tx| {
            if let Some(kept) = kept {
                tx.execute(
                    "UPDATE additions_kept SET state = 'gone', error = ?2 WHERE id = ?1 AND state = 'keeping'",
                    params![kept, why],
                )?;
            }
            tx.execute(
                "UPDATE additions SET state = 'retired', retired_at = ?2, error = ?3
                 WHERE id = ?1 AND state = 'retiring'",
                params![id, now(), why],
            )?;
            Ok(())
        })?;
        Ok(Settled::Done)
    }

    /// Returns addition `id` to added, its file left in place, saying why it could not be retired.
    fn unretire(&self, id: i64, kept: i64, why: String) -> Result<Settled> {
        self.write(|tx| {
            tx.execute(
                "DELETE FROM additions_kept WHERE id = ?1 AND state = 'keeping'",
                [kept],
            )?;
            tx.execute(
                "UPDATE additions SET state = 'active', error = ?2 WHERE id = ?1 AND state = 'retiring'",
                params![id, format!("it could not be retired: {why}")],
            )?;
            Ok(())
        })?;
        Ok(Settled::Done)
    }

    /// Moves the kept file of addition `row`, being restored, back to its file name in its Additions Set, without
    /// replacing anything, once it is found, through a descriptor held on it, to hash as recorded; and records the
    /// addition added again with the stamps read through that descriptor.
    fn restore_retired(&self, config: &Config, row: &AdditionRow) -> Result<Settled> {
        let id = row.addition.id;
        if let Err(why) = self.additions_root(config) {
            return Ok(Settled::Blocked(why));
        }
        let Some(kept) = self.read(|c| kept_of(c, id, "kept"))? else {
            return self.unrestore(id, None, "its file is no longer kept".into());
        };
        let Some(sha) = kept.sha256.clone() else {
            return self.unrestore(id, None, "its kept file has no trusted hash".into());
        };
        let set = self.read(|c| set_by_id(c, row.set_id))?;
        let dir = match self.open_set(config, &set)? {
            Ok(dir) => dir,
            Err(why) => return self.unrestore(id, None, why),
        };
        let area = match open_retention(
            self,
            Scope::Additions(set.id),
            &dir,
            kept.generation as u64,
            false,
        )? {
            Ok(Some(area)) => area.generation,
            Ok(None) => {
                return self.unrestore(id, Some(kept.id), "the kept files folder is gone".into());
            }
            Err(why) => return self.unrestore(id, None, why),
        };
        dir.sync().map_err(io)?;
        area.sync().map_err(io)?;
        let file_name = row.addition.file_name.as_str();
        // Moved back before a stop.
        if is_kept_entry(&dir, file_name, kept.identity, Some(&kept.file), false)
            && let Some(held) = dir.pin(file_name, kept.identity)
        {
            return self.restored(id, &kept, &sha, &held, &dir, file_name);
        }
        // Anything else at its kept name, such as what a rollback a stop cut short put back, is kept as found.
        let Some(held) = area.pin(&kept.kept_name, kept.identity) else {
            return self.unrestore_found(
                id,
                &kept,
                &area,
                format!(
                    "{} is no longer the file kept; what is there stays kept, as found, for you to review",
                    kept.kept_name
                ),
            );
        };
        let st = rustix::fs::fstat(&held).map_err(|e| io(e.into()))?;
        if file_stat(&st) != kept.file
            || still_sha256(&held, ctime_ns(&st)).map_err(io)?.as_deref() != Some(sha.as_str())
        {
            return self.unrestore_found(
                id,
                &kept,
                &area,
                format!(
                    "its kept file {} is no longer the file kept; it stays kept, as found, for you to review",
                    kept.kept_name
                ),
            );
        }
        if let Err(e) = area.rename_new(&kept.kept_name, &dir, file_name) {
            return self.unrestore(id, None, format!("it cannot be moved back: {e}"));
        }
        self.fault_point(Fault::AdditionRestoreMoved);
        area.sync().map_err(io)?;
        dir.sync().map_err(io)?;
        if is_kept_entry(&dir, file_name, kept.identity, Some(&kept.file), false) {
            return self.restored(id, &kept, &sha, &held, &dir, file_name);
        }
        let back = dir.rename_new(file_name, &area, &kept.kept_name);
        self.fault_point(Fault::AdditionRestoreReturned);
        if back.is_ok() {
            // Until both folders are synced, what was put back is left for recovery, which keeps it as found.
            if let Err(e) = dir.sync().and_then(|()| area.sync()) {
                return Ok(Settled::Blocked(format!(
                    "{} was replaced by something else while it was moved back, and that was put back among the \
                     kept files, but the folders could not be synced: {e}",
                    kept.kept_name
                )));
            }
        }
        let why = format!(
            "{} was replaced by something else while it was moved back; that was {}",
            kept.kept_name,
            match back {
                Ok(()) => "put back among the kept files, as found, for you to review",
                Err(_) => "left in the additions set, as it is",
            }
        );
        self.unrestore_found(id, &kept, &area, why)
    }

    /// Returns addition `id` to retired, saying why, with whatever is now at the name of its kept file `kept` in
    /// `area` recorded kept as found: the stamps it has now, read through a descriptor held on it where it is a
    /// regular file, and no hash, so that it is neither restored nor deleted until you review it. With nothing there,
    /// the kept file is recorded gone.
    fn unrestore_found(
        &self,
        id: i64,
        kept: &KeptRec,
        area: &FolderDir,
        why: String,
    ) -> Result<Settled> {
        let st = match area.open_file(&kept.kept_name) {
            Ok(held) => Some(rustix::fs::fstat(&held).map_err(|e| io(e.into()))?),
            Err(_) => area.stat(&kept.kept_name).map_err(io)?,
        };
        let Some(st) = st else {
            return self.unrestore(id, Some(kept.id), why);
        };
        let (dev, ino) = Identity::of(&st).to_sql();
        self.write(|tx| {
            tx.execute(
                "UPDATE additions_kept SET dev = ?2, ino = ?3, size = ?4, mtime_ns = ?5, slot_ctime_ns = ?6,
                                           sha256 = NULL, reason = 'found', error = ?7
                 WHERE id = ?1 AND state = 'kept'",
                params![
                    kept.id,
                    dev,
                    ino,
                    st.st_size,
                    file_stat(&st).mtime_ns.to_string(),
                    ctime_ns(&st).to_string(),
                    why
                ],
            )?;
            unrestored(tx, id, &why)
        })?;
        Ok(Settled::Done)
    }

    /// Records addition `id` added again, its file, kept as `kept`, found at `file_name` in `dir` as `held`, which
    /// hashes as `sha` while its stamps stay still; or, if it does not, retired, its file left there as it is.
    fn restored(
        &self,
        id: i64,
        kept: &KeptRec,
        sha: &str,
        held: &std::fs::File,
        dir: &FolderDir,
        file_name: &str,
    ) -> Result<Settled> {
        let st = rustix::fs::fstat(held).map_err(|e| io(e.into()))?;
        let found = still_sha256(held, ctime_ns(&st)).map_err(io)?;
        let in_place = dir
            .stat(file_name)
            .map_err(io)?
            .is_some_and(|at| Identity::of(&at) == kept.identity);
        if found.as_deref() != Some(sha) || !in_place || file_stat(&st) != kept.file {
            return self.unrestore(
                id,
                Some(kept.id),
                format!(
                    "{}/{file_name} is no longer the file that was kept; it was left as it is",
                    dir.path().display()
                ),
            );
        }
        let (dev, ino) = kept.identity.to_sql();
        self.write(|tx| {
            tx.execute(
                "UPDATE additions_kept SET state = 'restored' WHERE id = ?1 AND state = 'kept'",
                [kept.id],
            )?;
            tx.execute(
                "UPDATE additions SET state = 'active', dev = ?2, ino = ?3, size = ?4, mtime_ns = ?5, ctime_ns = ?6,
                                      sha256 = ?7, retired_at = NULL, error = NULL
                 WHERE id = ?1 AND state = 'restoring'",
                params![
                    id,
                    dev,
                    ino,
                    kept.file.size as i64,
                    kept.file.mtime_ns.to_string(),
                    ctime_ns(&st).to_string(),
                    sha
                ],
            )?;
            Ok(())
        })?;
        Ok(Settled::Done)
    }

    /// Returns addition `id` to retired, saying why it could not be restored; with `gone`, its kept file, no longer
    /// where it was kept, is recorded gone.
    fn unrestore(&self, id: i64, gone: Option<i64>, why: String) -> Result<Settled> {
        self.write(|tx| {
            if let Some(kept) = gone {
                tx.execute(
                    "UPDATE additions_kept SET state = 'gone', error = ?2 WHERE id = ?1 AND state = 'kept'",
                    params![kept, why],
                )?;
            }
            unrestored(tx, id, &why)
        })?;
        Ok(Settled::Done)
    }

    /// Settles an addition from an Ingest Folder by how its move ended: its published copy, written, checked and
    /// stamped through the descriptor that wrote it, is the addition's file.
    fn settle_ingest(&self, row: &AdditionRow, move_id: i64) -> Result<Settled> {
        type MoveEnd = (
            String,
            Option<String>,
            Option<Identity>,
            Option<i64>,
            Option<String>,
            Option<String>,
            Option<String>,
        );
        let (state, error, staged, size, mtime, ctime, sha): MoveEnd = self.read(|c| {
            Ok(c.query_row(
                "SELECT state, error, staged_dev, staged_ino, staged_size, staged_mtime_ns, published_ctime_ns,
                        dest_sha256
                 FROM ingest_moves WHERE id = ?1",
                [move_id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        identity_at(r, 2)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                    ))
                },
            )?)
        })?;
        match (state.as_str(), staged, size, parse_ns(mtime), sha) {
            ("done", Some(identity), Some(size), Some(mtime_ns), Some(sha)) => {
                let published = Published {
                    identity,
                    file: FileStat {
                        size: size as u64,
                        mtime_ns,
                    },
                    ctime_ns: parse_ns(ctime),
                    sha256: sha,
                };
                self.record_active(row.addition.id, &published)?;
                Ok(Settled::Done)
            }
            ("done", ..) => self.fail_addition(
                row.addition.id,
                "the move that added it recorded no published copy".into(),
            ),
            ("failed", ..) => self.fail_addition(
                row.addition.id,
                error.unwrap_or_else(|| "the move that added it failed".into()),
            ),
            _ => Ok(Settled::Blocked(
                "its move out of the ingest folder has not finished".into(),
            )),
        }
    }

    fn record_active(&self, id: i64, p: &Published) -> Result<()> {
        let (dev, ino) = p.identity.to_sql();
        self.write(|tx| {
            tx.execute(
                "UPDATE additions SET state = 'active', dev = ?2, ino = ?3, size = ?4, mtime_ns = ?5, ctime_ns = ?6,
                                      sha256 = ?7, published_at = ?8, error = NULL
                 WHERE id = ?1 AND state IN ('pending', 'staging')",
                params![
                    id,
                    dev,
                    ino,
                    p.file.size as i64,
                    p.file.mtime_ns.to_string(),
                    p.ctime_ns.map(|at| at.to_string()),
                    p.sha256,
                    now()
                ],
            )?;
            Ok(())
        })
    }

    /// The file the Keep step of item `key` of repair `plan` kept aside, once it has.
    fn repair_kept(&self, config: &Config, plan: i64, key: &str) -> Result<Kept> {
        type Step = (String, Option<String>, Option<i64>);
        let step: Option<Step> = self.read(|c| {
            Ok(c.query_row(
                "SELECT state, error, kept_id FROM repair_steps WHERE plan_id = ?1 AND item = ?2",
                params![plan, key],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?)
        })?;
        let kept_id = match step {
            None => return Ok(Kept::Failed(format!("repair {plan} has no step for {key}"))),
            Some((state, _, Some(kept))) if state == "done" => kept,
            Some((state, _, None)) if state == "done" => {
                return Ok(Kept::Failed("the repair did not keep it aside".into()));
            }
            Some((state, error, _)) if matches!(state.as_str(), "failed" | "skipped") => {
                return Ok(Kept::Failed(
                    error.unwrap_or_else(|| format!("the repair's step {state}")),
                ));
            }
            Some(_) => return Ok(Kept::Waiting),
        };
        type KeptRow = (i64, i64, String, String, i64, String, Identity);
        let (set, generation, kept_name, state, size, mtime, identity): KeptRow = self.read(|c| {
            Ok(c.query_row(
                "SELECT source_set_id, generation, kept_name, state, size, mtime_ns, dev, ino FROM source_kept
                 WHERE id = ?1",
                [kept_id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        Identity::from_sql(r.get(6)?, r.get(7)?),
                    ))
                },
            )?)
        })?;
        if state != "kept" {
            return Ok(Kept::Failed(format!(
                "the file the repair kept aside is {state}"
            )));
        }
        let dir = match open_repairable(self, config, set, MarkerRule::Required)? {
            Ok(dir) => dir,
            Err(why) => return Ok(Kept::Failed(why)),
        };
        let area = match open_retention(self, Scope::Source(set), &dir, generation as u64, false)? {
            Ok(Some(area)) => area,
            Ok(None) => {
                return Ok(Kept::Failed(
                    "the kept files folder it was kept in is gone".into(),
                ));
            }
            Err(why) => return Ok(Kept::Failed(why)),
        };
        Ok(Kept::Found {
            dir: area.generation,
            name: kept_name,
            identity,
            file: FileStat {
                size: size as u64,
                mtime_ns: mtime.parse().unwrap_or_default(),
            },
        })
    }

    /// The file cleanup `plan` kept aside from the drift entry `origin`, once it has.
    fn drift_kept(&self, config: &Config, plan: i64, origin: &str) -> Result<Kept> {
        type KeptRow = (i64, i64, String, String, i64, i64, Identity);
        let kept: Option<KeptRow> = self.read(|c| {
            Ok(c.query_row(
                "SELECT k.folder_id, k.generation, k.kept_name, k.state, k.size, k.mtime_ns, k.dev, k.ino
                 FROM kept_files k JOIN builds b ON b.id = k.build_id
                 WHERE b.plan_id = ?1 AND k.name = ?2 AND k.reason = 'drift'
                 ORDER BY k.id DESC LIMIT 1",
                params![plan, origin],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        Identity::from_sql(r.get(6)?, r.get(7)?),
                    ))
                },
            )
            .optional()?)
        })?;
        let Some((folder, generation, kept_name, state, size, mtime, identity)) = kept else {
            let running: bool = self.read(|c| {
                Ok(c.query_row(
                    "SELECT EXISTS (SELECT 1 FROM jobs WHERE plan_id = ?1 AND state IN ('queued', 'running'))
                         OR EXISTS (SELECT 1 FROM builds WHERE plan_id = ?1 AND state = 'running')",
                    [plan],
                    |r| r.get(0),
                )?)
            })?;
            return Ok(match running {
                true => Kept::Waiting,
                false => Kept::Failed("the cleanup ended without keeping it aside".into()),
            });
        };
        match state.as_str() {
            "keeping" => return Ok(Kept::Waiting),
            "kept" => {}
            other => {
                return Ok(Kept::Failed(format!(
                    "the file the cleanup kept aside is {other}"
                )));
            }
        }
        let row = self.read(|c| folder_by_id(c, folder))?;
        let dir = match open_folder(config, &row.root, &row.path) {
            Ok(dir) => dir,
            Err(why) => return Ok(Kept::Failed(why)),
        };
        let area =
            match open_retention(self, Scope::Managed(folder), &dir, generation as u64, false)? {
                Ok(Some(area)) => area,
                Ok(None) => {
                    return Ok(Kept::Failed(
                        "the kept files folder it was kept in is gone".into(),
                    ));
                }
                Err(why) => return Ok(Kept::Failed(why)),
            };
        Ok(Kept::Found {
            dir: area.generation,
            name: kept_name,
            identity,
            file: FileStat {
                size: size as u64,
                mtime_ns: mtime as i128,
            },
        })
    }

    /// Publishes addition `row` from the kept file `name` in `src`, recorded as `identity` and `file`, into the
    /// Additions Set `dest`: its one ROM, read through a descriptor on the kept file while it is still the file
    /// recorded, repacked as TorrentZip under a recorded staged name, checked to hold exactly that ROM, and renamed to
    /// the addition's file name without replacing anything. The kept file is only read.
    fn publish_kept(
        &self,
        row: &AdditionRow,
        src: &FolderDir,
        name: &str,
        identity: Identity,
        file: &FileStat,
        dest: &FolderDir,
    ) -> Result<Settled> {
        let id = row.addition.id;
        let file_name = &row.addition.file_name;
        let Some(recorded) = &row.addition.rom else {
            return self.fail_addition(id, "it records no ROM to be added".into());
        };
        if row.raw_state == "staging"
            && let Some(staged) = row.staged
        {
            let rom = recorded;
            // Renamed into place before a stop: what is there is the addition only while it is the staged file and
            // still holds exactly its ROM.
            for at in [
                file_name.as_str(),
                row.staged_name.as_deref().unwrap_or_default(),
            ] {
                let Some(st) = dest.stat(at).map_err(io)? else {
                    continue;
                };
                if Identity::of(&st) != staged || FolderDir::kind(&st) != FileType::RegularFile {
                    continue;
                }
                let held = dest.open_file(at).map_err(io)?;
                if Identity::of_fd(&held).map_err(io)? != staged
                    || !holds_exactly(&held, rom, row.addition.loose)
                {
                    return self.fail_addition(
                        id,
                        format!("{at} is not the file Title Select wrote; it was left as it is"),
                    );
                }
                if at != file_name.as_str() {
                    return self.rename_staged(row, &held, at, dest);
                }
                dest.sync().map_err(io)?;
                return self.activate(row, &held, staged, dest);
            }
        }
        let source = match src.open_file(name) {
            Ok(source) => source,
            Err(e) => return self.fail_addition(id, format!("its kept file cannot be read: {e}")),
        };
        // The kept file is read only while it is the file kept, and its ctime, size and mtime stay as they were.
        let st = rustix::fs::fstat(&source).map_err(|e| io(e.into()))?;
        let ctime = ctime_ns(&st);
        let unchanged = |source: &std::fs::File| -> Result<bool> {
            let st = rustix::fs::fstat(source).map_err(|e| io(e.into()))?;
            Ok(Identity::of(&st) == identity && file_stat(&st) == *file && ctime_ns(&st) == ctime)
        };
        if !unchanged(&source)? {
            return self.fail_addition(
                id,
                format!(
                    "its kept file {name} is no longer the file that was kept; it stays kept, as it is"
                ),
            );
        }
        let read = probe(&source).map_err(io)?;
        let found = sole_rom(&read, &row.addition.origin);
        let Some((rom, member)) = found.filter(|(rom, _)| same_rom(rom, recorded)) else {
            return self.fail_addition(
                id,
                format!(
                    "its kept file {name} no longer holds exactly the ROM recorded when you approved it; it stays \
                     kept, as it is"
                ),
            );
        };
        if !unchanged(&source)? {
            return self.fail_addition(
                id,
                format!("its kept file {name} changed while it was read; it stays kept, as it is"),
            );
        }
        if dest.stat(file_name).map_err(io)?.is_some() {
            return self.fail_addition(
                id,
                format!(
                    "{}/{file_name} is taken by something else; it was left as it is",
                    dest.path().display()
                ),
            );
        }
        let staged_name = format!(".{file_name}.title-select-new-{}", unique());
        self.write(|tx| {
            tx.execute(
                "UPDATE additions SET state = 'staging', staged_name = ?2, staged_dev = NULL, staged_ino = NULL
                 WHERE id = ?1",
                params![id, staged_name],
            )?;
            Ok(())
        })?;
        let staged = match dest.create_new(&staged_name) {
            Ok(staged) => staged,
            Err(e) => return self.fail_addition(id, format!("its file cannot be written: {e}")),
        };
        let staged_id = Identity::of_fd(&staged).map_err(io)?;
        let (dev, ino) = staged_id.to_sql();
        self.write(|tx| {
            tx.execute(
                "UPDATE additions SET staged_dev = ?2, staged_ino = ?3 WHERE id = ?1",
                params![id, dev, ino],
            )?;
            Ok(())
        })?;
        // Written in the format the storage policy said for its system's source sets when it was approved.
        let read = [(&rom, 0, member.as_deref())];
        let written = match row.addition.loose {
            true => write_loose(&staged, &read, &[&source]),
            false => repack_sources(&staged, &read, &[&source]),
        }
        .and_then(|_| staged.sync_all().map_err(|e| e.to_string()));
        if let Err(why) = written {
            return self.fail_addition(
                id,
                format!("{why}; the partial copy was left at {staged_name}"),
            );
        }
        if !holds_exactly(&staged, &rom, row.addition.loose) || !unchanged(&source)? {
            return self.fail_addition(
                id,
                format!(
                    "its copy does not hold exactly the ROM its kept file held, or the kept file changed while it was \
                     read; the copy was left at {staged_name}"
                ),
            );
        }
        self.fault_point(Fault::AdditionStaged);
        self.rename_staged(row, &staged, &staged_name, dest)
    }

    /// Renames the staged file `held`, checked to hold exactly its addition's ROM, to its addition's file name without replacing
    /// anything, and records the addition active only if that name is then that file.
    fn rename_staged(
        &self,
        row: &AdditionRow,
        held: &std::fs::File,
        staged_name: &str,
        dest: &FolderDir,
    ) -> Result<Settled> {
        let file_name = &row.addition.file_name;
        let staged = Identity::of_fd(held).map_err(io)?;
        if let Err(e) = dest.rename_new(staged_name, dest, file_name) {
            return self.fail_addition(
                row.addition.id,
                format!("{e}; its file was left at {staged_name}"),
            );
        }
        self.fault_point(Fault::AdditionRenamed);
        dest.sync().map_err(io)?;
        self.activate(row, held, staged, dest)
    }

    /// Records addition `row` active once its file name in `dest` is found to be `held`, the file `staged`, unchanged
    /// while it is read back: its stamps and SHA-256 are read through `held`.
    fn activate(
        &self,
        row: &AdditionRow,
        held: &std::fs::File,
        staged: Identity,
        dest: &FolderDir,
    ) -> Result<Settled> {
        let file_name = &row.addition.file_name;
        let st = rustix::fs::fstat(held).map_err(|e| io(e.into()))?;
        let (sha256, _) = sha256_of(held).map_err(io)?;
        let after = rustix::fs::fstat(held).map_err(|e| io(e.into()))?;
        let in_place = dest
            .stat(file_name)
            .map_err(io)?
            .is_some_and(|at| Identity::of(&at) == staged && file_stat(&at) == file_stat(&after));
        if !in_place || Identity::of(&after) != staged {
            return self.fail_addition(
                row.addition.id,
                format!(
                    "{}/{file_name} is not the file Title Select put there; it was left as it is",
                    dest.path().display()
                ),
            );
        }
        if file_stat(&st) != file_stat(&after) || ctime_ns(&st) != ctime_ns(&after) {
            return self.fail_addition(
                row.addition.id,
                format!("{file_name} changed while it was read back; it was left as it is"),
            );
        }
        let published = Published {
            identity: staged,
            file: file_stat(&after),
            ctime_ns: Some(ctime_ns(&after)),
            sha256,
        };
        self.record_active(row.addition.id, &published)?;
        Ok(Settled::Done)
    }
}

/// The kept file of a retired addition.
struct KeptRec {
    id: i64,
    generation: i64,
    kept_name: String,
    identity: Identity,
    file: FileStat,
    sha256: Option<String>,
}

/// Whether the latest kept file of addition `id` has no hash recorded through a descriptor, as one found in place
/// of it does not.
fn kept_untrusted(c: &Connection, id: i64) -> Result<bool> {
    Ok(kept_of(c, id, "kept")?.is_none_or(|k| k.sha256.is_none()))
}

/// The latest kept file of addition `id` in `state`.
fn kept_of(c: &Connection, id: i64, state: &str) -> Result<Option<KeptRec>> {
    Ok(c.query_row(
        "SELECT id, generation, kept_name, dev, ino, size, mtime_ns, sha256 FROM additions_kept
         WHERE addition_id = ?1 AND state = ?2 ORDER BY id DESC LIMIT 1",
        params![id, state],
        |r| {
            Ok(KeptRec {
                id: r.get(0)?,
                generation: r.get(1)?,
                kept_name: r.get(2)?,
                identity: Identity::from_sql(r.get(3)?, r.get(4)?),
                file: FileStat {
                    size: r.get::<_, i64>(5)? as u64,
                    mtime_ns: r.get::<_, String>(6)?.parse().unwrap_or_default(),
                },
                sha256: r.get(7)?,
            })
        },
    )
    .optional()?)
}

/// Where settling an addition leaves it.
enum Settled {
    /// Settled for now: published, failed, retired or restored, or back as it was, saying why.
    Done,
    /// Waiting for whatever adds it to finish, which queues its Additions Job again.
    Waiting,
    /// Left as it is for a reason that may pass, to be tried again when the service next starts.
    Blocked(String),
}

/// Where the kept file an addition is published from stands.
enum Kept {
    /// Not kept yet: whatever keeps it aside has not finished.
    Waiting,
    /// It never will be, or no longer is, for this reason.
    Failed(String),
    Found {
        dir: FolderDir,
        name: String,
        identity: Identity,
        file: FileStat,
    },
}

fn set_record(c: &Connection, set: SetRow) -> Result<AdditionsSet> {
    let additions = additions_where(c, "a.set_id = ?1", [set.id])?
        .into_iter()
        .map(|a| a.addition)
        .collect();
    let kept = crate::repairs::deletion::additions_kept_of(c, set.id)?;
    let mut stmt = c.prepare(
        "SELECT name, dev, ino, found_at FROM additions_set_leftovers WHERE set_id = ?1 ORDER BY id",
    )?;
    let leftovers = stmt
        .query_map([set.id], |r| {
            Ok(AdditionsLeftover {
                name: r.get(0)?,
                identity: identity_at(r, 1)?.map(Into::into),
                found_at: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(AdditionsSet {
        id: set.id,
        system: set.system,
        root: set.root,
        path: set.path,
        created_at: set.created_at,
        additions,
        kept,
        leftovers,
    })
}
