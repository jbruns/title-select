//! Repairing a Source Set: a Change Plan proposed from its latest Audit, on Payload Evidence only, and from assessed
//! items of Ingest Folders, carried out once you approve it. A repair writes only to a Source Set its Root lets it
//! write to, found writable when it runs, with this store's marker, which the first approved repair writes.
//!
//! Every change is a step recorded as a durable intent with the approval, before anything changes, and advanced
//! before each change, so that recovery rolls it forward from where it stopped. A Game is written to a staged name
//! recorded first, checked against the DAT, and published by a rename that never replaces anything, only once each
//! file it reads or displaces is kept aside, by one rename that never replaces anything, into the Source Set's kept
//! files: nothing in a Source Set is deleted, except kept files you approve deleting. A Game filled from an Ingest
//! Folder is moved with the one move out of an Ingest Folder there is.

mod deletion;
mod planning;

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io;

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use rustix::fs::{Access, AtFlags, FileType};
use sha2::{Digest, Sha256};
use title_select_api::build::{Approval, ExpectedRom, FileStat, MARKER, kept_name};
use title_select_api::ingest::FolderEntry;
use title_select_api::repair::{
    PlanRepair, PlannedStep, RepairOutcome, RepairPlan, RepairPlanContent, RepairRom, RepairStep,
    SeenFile, SourceSetMarker,
};
use title_select_api::{AuditEvidence, Job, JobKind, JobState, Workers};
use title_select_engine::build::{FromSource, repack_sources, verify};
use title_select_engine::dat::Dat;
use title_select_engine::{FileStamp, Progress};

use crate::builds::sha256_of;
use crate::checks::Seen;
use crate::config::Config;
use crate::folder_dir::{FolderDir, Identity, file_stat};
use crate::folders::{MARKER_LIMIT, MARKER_NEW, store_id};
use crate::ingest::OwedAudits;
use crate::ingest::fill_candidates;
use crate::ingest::moves::{Destination, Move};
use crate::jobs::{finish_running, job_by_id};
use crate::kept::{Scope, open_area_deleting, open_retention, settle_dir_temps, unique};
use crate::plans::{json, to_json};
use crate::sets::{set_by_id, set_by_name};
use crate::{Error, Fault, Result, Store, now};

/// What a Source Set's marker must be for a repair to go on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MarkerRule {
    /// This store's marker for the Source Set.
    Required,
    /// This store's marker, or none yet: the first repair writes it.
    Creating,
}

/// Why a Root does not let a repair write to `path`.
fn not_writable(set: &str, root: &str, path: &str) -> String {
    format!(
        "source set {set} is in root {root}, which does not let Title Select write to it. Declare the root writable, \
         or list \"{path}\" in its writable_paths, then restart the service"
    )
}

enum MarkerFound {
    Absent,
    Unreadable(String),
    Present(SourceSetMarker),
}

fn read_set_marker(dir: &FolderDir) -> MarkerFound {
    match dir.read(MARKER, MARKER_LIMIT) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => MarkerFound::Absent,
        Err(e) => MarkerFound::Unreadable(e.to_string()),
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(marker) => MarkerFound::Present(marker),
            Err(e) => MarkerFound::Unreadable(e.to_string()),
        },
    }
}

/// Checks, through `dir`, that the Source Set's marker is `expected`, or, under [`MarkerRule::Creating`], absent.
fn check_set_marker(
    dir: &FolderDir,
    expected: &SourceSetMarker,
    rule: MarkerRule,
) -> std::result::Result<(), String> {
    let path = dir.path().display();
    match read_set_marker(dir) {
        MarkerFound::Present(marker) if marker == *expected => Ok(()),
        MarkerFound::Absent if rule == MarkerRule::Creating => Ok(()),
        MarkerFound::Present(marker) if marker.store != expected.store => Err(format!(
            "{path}: its .title-select marker is from another store; is this the right folder, or the right mount? \
             Nothing in it was changed"
        )),
        MarkerFound::Present(marker) => Err(format!(
            "{path}: its .title-select marker is for source set {}, not {}; is this the right mount? Nothing in it \
             was changed",
            marker.source_set, expected.source_set
        )),
        MarkerFound::Absent => Err(format!(
            "{path}: its .title-select marker, which its first repair wrote, is gone; is its share mounted? Nothing \
             in it was changed"
        )),
        MarkerFound::Unreadable(e) => Err(format!(
            "{path}: its .title-select marker cannot be read ({e}); nothing in it was changed"
        )),
    }
}

/// Opens Source Set `set` for a repair to write to: only when its Root lets a repair write to it, the folder is
/// found writable now, and its marker is as `rule` says.
pub(crate) fn open_repairable(
    store: &Store,
    config: &Config,
    set: i64,
    rule: MarkerRule,
) -> Result<std::result::Result<FolderDir, String>> {
    let row = store.read(|c| set_by_id(c, set))?;
    let record = &row.record;
    let Some(root) = config.root(&record.root) else {
        return Ok(Err(format!(
            "there is no root {} in the configuration",
            record.root
        )));
    };
    if !root.may_write(&record.path) {
        return Ok(Err(not_writable(&record.name, &record.root, &record.path)));
    }
    let dir = match FolderDir::open(&root.path, &record.path) {
        Ok(dir) => dir,
        Err(e) => return Ok(Err(e.to_string())),
    };
    if let Err(e) = rustix::fs::accessat(&dir, ".", Access::WRITE_OK, AtFlags::EACCESS) {
        return Ok(Err(format!(
            "{}: Title Select cannot write to it ({e}); nothing in it was changed",
            dir.path().display()
        )));
    }
    let expected = SourceSetMarker {
        store: store.read(store_id)?,
        source_set: set,
    };
    Ok(check_set_marker(&dir, &expected, rule).map(|()| dir))
}

/// Opens the folder holding `rel`, a path relative to `dir`, without following a symlink: `None` for `dir` itself.
/// Returns it with the last component.
fn parent_of<'a>(dir: &FolderDir, rel: &'a str) -> io::Result<(Option<FolderDir>, &'a str)> {
    let Some((parents, name)) = rel.rsplit_once('/') else {
        return Ok((None, rel));
    };
    let mut parts = parents.split('/');
    let mut found = dir.sub(parts.next().unwrap_or_default())?;
    for part in parts {
        found = found.sub(part)?;
    }
    Ok((Some(found), name))
}

/// Every entry under `dir` but Title Select's own, by path relative to it, and the regular files among them as seen.
// `st_nlink` is narrower than u64 on some platforms.
#[allow(clippy::unnecessary_cast)]
fn walk(dir: &FolderDir) -> io::Result<(Vec<FolderEntry>, BTreeMap<String, Seen>)> {
    fn visit(
        dir: &FolderDir,
        prefix: &str,
        entries: &mut Vec<FolderEntry>,
        files: &mut BTreeMap<String, Seen>,
    ) -> io::Result<()> {
        for (name, _) in dir.entries()? {
            if title_select_engine::is_own_entry(&name) {
                continue;
            }
            let Some(st) = dir.stat(&name)? else {
                continue;
            };
            let rel = format!("{prefix}{name}");
            let seen = Seen::of(&st);
            entries.push(FolderEntry {
                name: rel.clone(),
                identity: seen.identity.into(),
                file: seen.file.clone(),
                ctime_ns: seen.ctime_ns,
                nlink: st.st_nlink as u64,
            });
            match FolderDir::kind(&st) {
                FileType::RegularFile => {
                    files.insert(rel, seen);
                }
                FileType::Directory => visit(&dir.sub(&name)?, &format!("{rel}/"), entries, files)?,
                _ => {}
            }
        }
        Ok(())
    }
    let (mut entries, mut files) = (Vec::new(), BTreeMap::new());
    visit(dir, "", &mut entries, &mut files)?;
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok((entries, files))
}

fn seen_file(seen: &Seen) -> SeenFile {
    SeenFile {
        identity: seen.identity.into(),
        file: seen.file.clone(),
        ctime_ns: seen.ctime_ns,
    }
}

fn seen_of(seen: &SeenFile) -> Seen {
    Seen {
        identity: seen.identity.into(),
        file: seen.file.clone(),
        ctime_ns: seen.ctime_ns,
    }
}

/// Whether `name` in `dir` is, without following a symlink, the regular file `seen`, unchanged.
fn is_seen(dir: &FolderDir, name: &str, seen: &Seen) -> io::Result<bool> {
    Ok(matches!(dir.stat(name)?, Some(st)
        if FolderDir::kind(&st) == FileType::RegularFile && Seen::of(&st) == *seen))
}

/// Whether `name` in `dir`, opened without following a symlink, is still the file `seen` and is valid TorrentZip
/// holding exactly `roms`, each payload hashed again.
fn holds_exactly(
    dir: &FolderDir,
    name: &str,
    seen: &Seen,
    roms: &[ExpectedRom],
) -> io::Result<std::result::Result<(), String>> {
    let file = dir.open_file(name)?;
    if Seen::of(&rustix::fs::fstat(&file)?) != *seen {
        return Ok(Err("it changed while it was opened".into()));
    }
    Ok(verify(&file, roms))
}

/// Whether `name` in `dir` is, without following a symlink, the regular file `id`.
fn is_file(dir: &FolderDir, name: &str, id: Identity) -> io::Result<bool> {
    Ok(matches!(dir.stat(name)?, Some(st)
        if FolderDir::kind(&st) == FileType::RegularFile && Identity::of(&st) == id))
}

fn undeleted(dir: &FolderDir, staged_name: &str) -> String {
    format!(
        "{}/{staged_name}, a file Title Select staged and won't use, could not be deleted; it was left where it is",
        dir.path().display()
    )
}

fn io_err(e: io::Error) -> Error {
    Error::Io(e.to_string())
}

fn digest(content: &RepairPlanContent) -> String {
    let bytes = serde_json::to_vec(content).expect("a plan always serializes");
    Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

const PLAN_QUERY: &str = "SELECT id, digest, created_at, content, approved_digest, approved_by, approved_at, \
     finished_at, outcomes, recovery_error, audit_job_id, audit_owed, audit_error, \
     (SELECT max(j.id) FROM jobs j WHERE j.repair_plan_id = repair_plans.id) FROM repair_plans";

fn plan_from_row(r: &Row) -> rusqlite::Result<RepairPlan> {
    let outcomes: Option<String> = r.get(8)?;
    Ok(RepairPlan {
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
        audit_job: r.get(10)?,
        audit_owed: r.get(11)?,
        audit_error: r.get(12)?,
        job: r.get(13)?,
    })
}

fn plan_by_id(c: &Connection, id: i64) -> Result<RepairPlan> {
    c.query_row(&format!("{PLAN_QUERY} WHERE id = ?1"), [id], plan_from_row)
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("no repair plan {id}")))
}

/// The generation the Source Set's last approved repair kept files under, and whether its marker was written.
fn source_folder(c: &Connection, set: i64) -> Result<(u64, bool)> {
    Ok(c.query_row(
        "SELECT generation, marker_at IS NOT NULL FROM source_folders WHERE source_set_id = ?1",
        [set],
        |r| Ok((r.get::<_, i64>(0)? as u64, r.get(1)?)),
    )
    .optional()?
    .unwrap_or((0, false)))
}

/// Why nothing else is done to Source Set `set` now, when a repair or a deletion of its kept files is unfinished.
pub(crate) fn repair_active(c: &Connection, set: i64) -> Result<Option<String>> {
    let repair: Option<i64> = c
        .query_row(
            "SELECT id FROM repair_plans
             WHERE source_set_id = ?1 AND approved_at IS NOT NULL AND finished_at IS NULL",
            [set],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = repair {
        return Ok(Some(format!(
            "repair {id} of the source set is not finished; its repair job finishes it, or, if that job failed, the \
             one queued when the service next starts"
        )));
    }
    let deletion: Option<i64> = c
        .query_row(
            "SELECT id FROM source_kept_deletions
             WHERE source_set_id = ?1 AND approved_at IS NOT NULL AND finished_at IS NULL",
            [set],
            |r| r.get(0),
        )
        .optional()?;
    Ok(deletion.map(|id| {
        format!(
            "deletion {id} of kept files of the source set is not finished; the service finishes it when it next \
             starts"
        )
    }))
}

/// Refuses to start reading Source Set `set_name` (`set`) while a repair reserves it: a Repair Job Queued or
/// Running, or a repair or deletion of its kept files not finished. An Audit or a Build reading the Source Set calls
/// this in the transaction that starts or queues it.
pub(crate) fn check_no_repair(c: &Connection, set: i64, set_name: &str) -> Result<()> {
    let job: Option<i64> = c
        .query_row(
            "SELECT id FROM jobs WHERE source_set_id = ?1 AND kind = 'repair' AND state IN ('queued', 'running')",
            [set],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(job) = job {
        return Err(Error::JobActive {
            source_set: set_name.to_owned(),
            job,
            kind: JobKind::Repair,
        });
    }
    match repair_active(c, set)? {
        Some(why) => Err(Error::Conflict(format!("source set {set_name}: {why}"))),
        None => Ok(()),
    }
}

/// Why a repair of Source Set `set` cannot be approved now: an Audit, Build or repair of it Queued or Running, an
/// Audit of it running outside a Job, or a repair or deletion of its kept files not finished.
fn reserved(c: &Connection, set: i64) -> Result<Option<String>> {
    if let Some(why) = repair_active(c, set)? {
        return Ok(Some(why));
    }
    let job: Option<(i64, String)> = c
        .query_row(
            "SELECT id, kind FROM jobs WHERE source_set_id = ?1 AND kind IN ('audit', 'build', 'repair')
             AND state IN ('queued', 'running') ORDER BY id LIMIT 1",
            [set],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((job, kind)) = job {
        return Ok(Some(format!(
            "{kind} job {job}, which reads the source set, is queued or running; approve the repair once it ends"
        )));
    }
    let owed: Option<i64> = c
        .query_row(
            "SELECT id FROM repair_plans WHERE source_set_id = ?1 AND audit_owed = 1",
            [set],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = owed {
        return Ok(Some(format!(
            "the incremental audit repair {id} owes is not queued yet; it is once the job it waits for ends"
        )));
    }
    let audit: Option<i64> = c
        .query_row(
            "SELECT id FROM audits WHERE source_set_id = ?1 AND status = 'running'",
            [set],
            |r| r.get(0),
        )
        .optional()?;
    Ok(audit.map(|id| {
        format!("audit {id} of the source set is running; approve the repair once it ends")
    }))
}

/// Queues a Repair Job for approved repair `plan`, as `last`'s next attempt when it is queued again. Returns its id.
pub(crate) fn queue_repair_job(tx: &Transaction, plan: i64, last: Option<i64>) -> Result<i64> {
    tx.execute(
        "INSERT INTO jobs (state, kind, source_set_id, dat_revision_id, actor, attempt, requeued_from, readers,
                           hashers, created_at, repair_plan_id, cancel_requested_at)
         SELECT 'queued', 'repair', p.source_set_id, a.dat_revision_id, p.approved_by,
                coalesce((SELECT attempt + 1 FROM jobs WHERE id = ?2), 1), ?2, max(p.readers, 1), max(p.hashers, 1),
                ?3, p.id, (SELECT cancel_requested_at FROM jobs WHERE id = ?2)
         FROM repair_plans p JOIN audits a ON a.id = p.audit_id WHERE p.id = ?1",
        params![plan, last, now()],
    )?;
    Ok(tx.last_insert_rowid())
}

/// One step of an approved repair, as recorded.
struct StepRow {
    id: i64,
    seq: u32,
    item: String,
    step: RepairStep,
    state: String,
    staged_name: Option<String>,
    staged: Option<Identity>,
    move_id: Option<i64>,
    error: Option<String>,
    note: Option<String>,
    aside_name: Option<String>,
    aside_why: Option<String>,
    discard_slot: Option<String>,
}

const STEP_QUERY: &str = "SELECT id, seq, item, detail, state, staged_name, staged_dev, staged_ino, move_id, \
     error, note, aside_name, aside_why, discard_slot FROM repair_steps";

fn step_from_row(r: &Row) -> rusqlite::Result<StepRow> {
    let staged = match (r.get::<_, Option<i64>>(6)?, r.get::<_, Option<i64>>(7)?) {
        (Some(dev), Some(ino)) => Some(Identity::from_sql(dev, ino)),
        _ => None,
    };
    Ok(StepRow {
        id: r.get(0)?,
        seq: r.get::<_, i64>(1)? as u32,
        item: r.get(2)?,
        step: json(r, 3)?,
        state: r.get(4)?,
        staged_name: r.get(5)?,
        staged,
        move_id: r.get(8)?,
        error: r.get(9)?,
        note: r.get(10)?,
        aside_name: r.get(11)?,
        aside_why: r.get(12)?,
        discard_slot: r.get(13)?,
    })
}

fn steps_of(c: &Connection, plan: i64) -> Result<Vec<StepRow>> {
    let mut stmt = c.prepare(&format!("{STEP_QUERY} WHERE plan_id = ?1 ORDER BY seq"))?;
    Ok(stmt
        .query_map([plan], step_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

fn is_terminal(state: &str) -> bool {
    matches!(state, "done" | "failed" | "skipped")
}

/// How running a step ended: `Err` leaves it unfinished, for recovery.
type Ran = Result<std::result::Result<(), String>>;

/// A Source Set being repaired: the plan, and its folder opened.
struct Repairing<'a> {
    plan: &'a RepairPlan,
    dir: FolderDir,
}

impl Repairing<'_> {
    fn scope(&self) -> Scope {
        Scope::Source(self.plan.content.set_id)
    }
}

impl Store {
    /// Proposes a repair of Source Set `name` from its latest Audit and the assessed items of Ingest Folders, with
    /// the choices `request` makes in place of the defaults. Refused unless the Source Set's Root lets a repair write
    /// to it. Changes nothing in the Source Set.
    pub fn plan_repair(
        &self,
        config: &Config,
        name: &str,
        request: &PlanRepair,
    ) -> Result<RepairPlan> {
        let set = self.read(|c| set_by_name(c, name))?;
        let record = &set.record;
        let root = config.root(&record.root).ok_or_else(|| {
            Error::Refused(format!(
                "there is no root {} in the configuration",
                record.root
            ))
        })?;
        if !root.may_write(&record.path) {
            return Err(Error::Refused(not_writable(
                &record.name,
                &record.root,
                &record.path,
            )));
        }
        let audit = self
            .history(name)?
            .into_iter()
            .next()
            .ok_or_else(|| Error::Refused(format!("audit {name} before you repair it")))?;
        let report = self.report(audit.id)?;
        let dat = Dat::read(&self.blob_path(&report.dat_revision.sha256))
            .map_err(|e| Error::Io(e.to_string()))?;
        let dir = FolderDir::open(&root.path, &record.path).map_err(io_err)?;
        let folder = dir.identity().map_err(io_err)?;
        let (fingerprint, files) = walk(&dir).map_err(io_err)?;
        let stamps = self.read(|c| audit_stamps(c, audit.id, false))?;
        let unreadable = self.read(|c| audit_stamps(c, audit.id, true))?;
        let candidates =
            self.read(|c| fill_candidates(c, &record.name, &report.dat_revision.sha256))?;
        let torrentzip = |path: &str| -> bool {
            let Ok((parent, name)) = parent_of(&dir, path) else {
                return false;
            };
            parent
                .as_ref()
                .unwrap_or(&dir)
                .open_file(name)
                .is_ok_and(|file| {
                    title_select_engine::torrentzip::central(&mut io::BufReader::new(file)).is_ok()
                })
        };
        let planned = planning::plan(&planning::Inputs {
            report: &report,
            dat: &dat,
            files: &files,
            stamps: &stamps,
            unreadable: &unreadable,
            torrentzip: &torrentzip,
            candidates: &candidates,
            choices: &request.choices,
        })?;
        let (generation, marked) = self.read(|c| source_folder(c, set.id))?;
        let content = RepairPlanContent {
            source_set: record.name.clone(),
            set_id: set.id,
            root: record.root.clone(),
            path: record.path.clone(),
            audit: audit.id,
            dat_revision: report.dat_revision.sha256.clone(),
            generation: generation + 1,
            creates_marker: !marked,
            folder: folder.into(),
            items: planned.items,
            steps: planned.steps,
            fingerprint,
            notes: planned.notes,
        };
        let digest = digest(&content);
        self.write(|tx| {
            tx.execute(
                "INSERT INTO repair_plans (source_set_id, audit_id, content, digest, created_at, readers, hashers)
                 VALUES (?1, ?2, ?3, ?4, ?5, 0, 0)",
                params![set.id, audit.id, to_json(&content), digest, now()],
            )?;
            plan_by_id(tx, tx.last_insert_rowid())
        })
    }

    pub fn repair_plan(&self, id: i64) -> Result<RepairPlan> {
        self.read(|c| plan_by_id(c, id))
    }

    /// The repair plans of Source Set `name`, newest first.
    pub fn repair_plans(&self, name: &str) -> Result<Vec<RepairPlan>> {
        self.read(|c| {
            let set = set_by_name(c, name)?;
            let mut stmt = c.prepare(&format!(
                "{PLAN_QUERY} WHERE source_set_id = ?1 ORDER BY id DESC"
            ))?;
            Ok(stmt
                .query_map([set.id], plan_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    /// Approves repair plan `id`, reviewed as `digest`, as `actor`, and carries it out. Refused unless its Audit is
    /// still the Source Set's latest, no other repair was approved since it was made, the Root still lets a repair
    /// write to the Source Set and it is writable, its marker is this store's (or, for its first repair, absent),
    /// every entry in it is as the plan saw it, and no Job of the Source Set is Queued or Running. Once it ends, an
    /// incremental Audit of the Source Set is queued, with `workers`.
    pub fn approve_repair(
        &self,
        config: &Config,
        id: i64,
        digest: &str,
        actor: &str,
        workers: Workers,
    ) -> Result<RepairPlan> {
        let plan = self.repair_plan(id)?;
        if plan.approval.is_some() {
            return Err(Error::Conflict(format!(
                "repair plan {id} was already approved"
            )));
        }
        if plan.digest != digest {
            return Err(Error::Conflict(format!(
                "repair plan {id} is not the plan you reviewed (its digest is {}); review it again",
                plan.digest
            )));
        }
        let content = &plan.content;
        let set = self.read(|c| set_by_id(c, content.set_id))?;
        if set.record.root != content.root || set.record.path != content.path {
            return Err(Error::Conflict(format!(
                "source set {} moved since the plan was made; plan it again",
                content.source_set
            )));
        }
        let latest = self.history(&set.record.name)?.into_iter().next();
        if latest.as_ref().map(|a| a.id) != Some(content.audit) {
            return Err(Error::Conflict(format!(
                "source set {} was audited again since the plan was made; plan the repair again from the latest \
                 audit",
                content.source_set
            )));
        }
        let (generation, _) = self.read(|c| source_folder(c, content.set_id))?;
        if generation + 1 != content.generation {
            return Err(Error::Conflict(format!(
                "another repair of source set {} was approved since this one was made; plan it again",
                content.source_set
            )));
        }
        let rule = match content.creates_marker {
            true => MarkerRule::Creating,
            false => MarkerRule::Required,
        };
        let dir = open_repairable(self, config, content.set_id, rule)?.map_err(Error::Refused)?;
        let (fingerprint, _) = walk(&dir).map_err(io_err)?;
        if Identity::from(content.folder) != dir.identity().map_err(io_err)?
            || fingerprint != content.fingerprint
        {
            return Err(Error::Conflict(format!(
                "source set {} changed since the plan was made; nothing was changed. Audit it, plan again and \
                 review the new plan",
                content.source_set
            )));
        }
        if let Some(why) = self.read(|c| reserved(c, content.set_id))? {
            return Err(Error::Conflict(why));
        }
        for step in &content.steps {
            if let RepairStep::Fill { item, .. } = &step.step
                && let Some(why) = self.read(|c| crate::ingest::item_unmovable(c, *item, false))?
            {
                return Err(Error::Conflict(format!(
                    "{why}; nothing was changed. Plan the repair again and review the new plan"
                )));
            }
        }
        if let Err(why) = settle_dir_temps(self, Scope::Source(content.set_id), &dir)? {
            return Err(Error::Refused(why));
        }
        self.fault_point(crate::Fault::RepairApproving);
        self.write(|tx| {
            // The reservation: checked again in the transaction that queues the Repair Job, which Audits and Builds
            // of the Source Set check in theirs.
            if let Some(why) = reserved(tx, content.set_id)? {
                return Err(Error::Conflict(why));
            }
            let latest: Option<i64> = tx
                .query_row(
                    "SELECT id FROM audits WHERE source_set_id = ?1 AND status = 'finished'
                     ORDER BY finish_seq DESC LIMIT 1",
                    [content.set_id],
                    |r| r.get(0),
                )
                .optional()?;
            if latest != Some(content.audit) {
                return Err(Error::Conflict(format!(
                    "source set {} was audited again since the plan was made; plan the repair again from the \
                     latest audit",
                    content.source_set
                )));
            }
            let advanced = tx.execute(
                "INSERT INTO source_folders (source_set_id, generation) VALUES (?1, ?2)
                 ON CONFLICT (source_set_id) DO UPDATE SET generation = excluded.generation
                 WHERE generation = ?3",
                params![
                    content.set_id,
                    content.generation as i64,
                    generation as i64
                ],
            )?;
            let approved = tx.execute(
                "UPDATE repair_plans SET approved_digest = ?2, approved_by = ?3, approved_at = ?4, readers = ?5,
                                         hashers = ?6
                 WHERE id = ?1 AND approved_at IS NULL",
                params![
                    id,
                    digest,
                    actor,
                    now(),
                    workers.readers as i64,
                    workers.hashers as i64
                ],
            )?;
            if advanced != 1 || approved != 1 {
                return Err(Error::Conflict(format!(
                    "repair plan {id} was approved, or another repair applied, meanwhile; review it again"
                )));
            }
            for (seq, PlannedStep { item, step }) in content.steps.iter().enumerate() {
                tx.execute(
                    "INSERT INTO repair_steps (plan_id, seq, item, detail, state) VALUES (?1, ?2, ?3, ?4, 'pending')",
                    params![id, seq as i64, item, to_json(step)],
                )?;
            }
            queue_repair_job(tx, id, None)?;
            Ok(())
        })?;
        self.repair_plan(id)
    }

    /// Run when the service starts, after [`Store::recover_jobs`]: queues a Repair Job for each approved repair that
    /// is not finished and has none Queued or Running, as when the last one failed. Returns the Jobs queued.
    pub fn recover_repairs(&self) -> Result<Vec<Job>> {
        self.write(|tx| {
            let mut stmt = tx.prepare(
                "SELECT p.id, (SELECT max(j.id) FROM jobs j WHERE j.repair_plan_id = p.id) FROM repair_plans p
                 WHERE p.approved_at IS NOT NULL AND p.finished_at IS NULL
                   AND NOT EXISTS (SELECT 1 FROM jobs j WHERE j.repair_plan_id = p.id
                                   AND j.state IN ('queued', 'running'))
                 ORDER BY p.id",
            )?;
            let unfinished = stmt
                .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            unfinished
                .into_iter()
                .map(|(plan, last)| {
                    let job = queue_repair_job(tx, plan, last)?;
                    job_by_id(tx, job)
                })
                .collect()
        })
    }

    /// Runs the Running Repair Job `id`: carries out each step of its repair not yet ended, calling `progress` as
    /// steps end, and stopping at the next safe point once you cancel it. Once every step has ended, the repair's
    /// outcome, the incremental Audit it owes and the Job's end are recorded in one transaction; that Audit is
    /// queued by [`Store::fulfil_repair_audits`], which the service runs after every Job ends. A repair it cannot go on with is left unfinished, with why, and the Job Failed; nothing else is done
    /// to its Source Set until a later Job, queued when the service next starts, finishes it. Returns the Job as it
    /// ends.
    pub fn run_repair_job(
        &self,
        config: &Config,
        id: i64,
        progress: Option<&(dyn Fn(Progress) + Sync)>,
    ) -> Result<Job> {
        let job = self.job(id)?;
        let Some(plan) = job.repair else {
            return Err(Error::Refused(format!("job {id} is not a repair")));
        };
        if job.state != JobState::Running {
            return Err(Error::Refused(format!(
                "job {id} is {}, not running",
                job.state.as_str()
            )));
        }
        let ran = self.run_repair(config, plan, id, progress);
        let why = match &ran {
            Ok(Ok(())) => None,
            Ok(Err(why)) => Some(why.clone()),
            Err(err) => Some(err.to_string()),
        };
        if let Some(why) = why {
            self.write(|tx| {
                tx.execute(
                    "UPDATE repair_plans SET recovery_error = ?2 WHERE id = ?1 AND finished_at IS NULL",
                    params![plan, why],
                )?;
                match finish_running(tx, id, JobState::Failed, Some(&why)) {
                    Err(Error::Io(_)) | Ok(()) => Ok(()),
                    Err(err) => Err(err),
                }
            })?;
        }
        drop(ran);
        self.job(id)
    }

    /// Asks the Queued or Running Repair Job `id` to stop at its next safe point: between two steps. Each step that
    /// changed something, and every step of the same item, still ends as planned; every other is skipped.
    pub fn cancel_job(&self, id: i64) -> Result<Job> {
        self.write(|tx| {
            let job = job_by_id(tx, id)?;
            if job.kind != JobKind::Repair {
                return Err(Error::Refused(format!(
                    "job {id} is not a repair; only a repair job can be cancelled"
                )));
            }
            if job.state.is_final() {
                return Err(Error::Conflict(format!("job {id} has already ended")));
            }
            tx.execute(
                "UPDATE jobs SET cancel_requested_at = ?2, version = version + 1
                 WHERE id = ?1 AND cancel_requested_at IS NULL",
                params![id, now()],
            )?;
            job_by_id(tx, id)
        })
    }

    /// Queues each incremental Audit a finished repair owes, once no other Job of its Source Set stands in the way.
    pub fn fulfil_repair_audits(&self) -> Result<OwedAudits> {
        let owed: Vec<i64> = self.read(|c| {
            let mut stmt =
                c.prepare("SELECT id FROM repair_plans WHERE audit_owed = 1 ORDER BY id")?;
            Ok(stmt
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })?;
        let mut done = OwedAudits::default();
        for id in owed {
            if let Some(why) = self.queue_repair_audit(id)? {
                done.failed.push(why);
            }
            if let Some(job) = self.repair_plan(id)?.audit_job {
                done.queued.push(self.job(job)?);
            }
        }
        Ok(done)
    }

    /// Queues the incremental Audit repair `id` owes; one that must wait for another Job stays owed. Returns why it
    /// could not be queued, if it could not.
    fn queue_repair_audit(&self, id: i64) -> Result<Option<String>> {
        let (set, actor, readers, hashers): (String, String, i64, i64) = self.read(|c| {
            Ok(c.query_row(
                "SELECT s.name, p.approved_by, p.readers, p.hashers
                 FROM repair_plans p JOIN source_sets s ON s.id = p.source_set_id WHERE p.id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?)
        })?;
        let workers = Workers {
            readers: (readers as usize).max(1),
            hashers: (hashers as usize).max(1),
        };
        let (job, owed, error) =
            match self.enqueue_audit(&set, None, &actor, workers, AuditEvidence::Incremental) {
                Ok(job) => (Some(job.id), false, None),
                Err(Error::JobActive { .. }) => (None, true, None),
                Err(err) => (None, false, Some(err.to_string())),
            };
        self.write(|tx| {
            tx.execute(
                "UPDATE repair_plans SET audit_job_id = ?2, audit_owed = ?3, audit_error = ?4 WHERE id = ?1",
                params![id, job, owed, error],
            )?;
            Ok(())
        })?;
        Ok(error.map(|why| format!("an incremental audit of {set} after repair {id}: {why}")))
    }

    /// Carries out each step of approved repair `id` not yet ended, as Running Job `job`, then, once every step has
    /// ended, records what it did. `Err` says why it was left unfinished.
    fn run_repair(
        &self,
        config: &Config,
        id: i64,
        job: i64,
        progress: Option<&(dyn Fn(Progress) + Sync)>,
    ) -> Ran {
        let plan = self.repair_plan(id)?;
        let content = &plan.content;
        let (_, marked) = self.read(|c| source_folder(c, content.set_id))?;
        let rule = match marked {
            true => MarkerRule::Required,
            false => MarkerRule::Creating,
        };
        // Until a step starts, nothing the plan names was changed, so a repair that cannot start ends rather than
        // being tried again; once one has, it is left for recovery to finish.
        let unstarted = self
            .read(|c| steps_of(c, id))?
            .iter()
            .all(|s| s.state == "pending");
        let stop = |why: String| -> Ran {
            if unstarted {
                self.abandon_repair(&plan, job, &why)?;
            }
            Ok(Err(why))
        };
        let dir = match open_repairable(self, config, content.set_id, rule)? {
            Ok(dir) => dir,
            Err(why) => return stop(why),
        };
        if Identity::from(content.folder) != dir.identity().map_err(io_err)? {
            return stop(format!(
                "{} is no longer the folder the plan was made for; nothing in it was changed",
                dir.path().display()
            ));
        }
        // Cancelled before its first repair wrote the marker, nothing in the Source Set was changed, nor is.
        let untouched = !marked && self.cancel_requested(job)?;
        if !marked
            && !untouched
            && let Err(why) = self.bootstrap_marker(content.set_id, &dir)?
        {
            return stop(why);
        }
        if !untouched && let Err(why) = settle_dir_temps(self, Scope::Source(content.set_id), &dir)?
        {
            return Ok(Err(why));
        }
        let repairing = Repairing { plan: &plan, dir };
        let total = self.read(|c| steps_of(c, id))?.len();
        let mut cancelled = self.read(|c| {
            Ok(c.query_row(
                "SELECT cancelled FROM repair_plans WHERE id = ?1",
                [id],
                |r| r.get::<_, bool>(0),
            )?)
        })?;
        if cancelled && let Err(why) = self.discard_skipped(&repairing)? {
            return Ok(Err(why));
        }
        for seq in 0..total {
            if !cancelled && (untouched || self.cancel_requested(job)?) {
                cancelled = true;
                if let Err(why) = self.cancel_uncommitted(&repairing)? {
                    return Ok(Err(why));
                }
            }
            let step = self.other_step(id, seq as u32)?;
            if !is_terminal(&step.state)
                && let Err(why) = self.run_step(config, &repairing, &step)?
            {
                return Ok(Err(why));
            }
            if let Some(progress) = progress {
                progress(Progress {
                    files_done: seq + 1,
                    files_total: total,
                });
            }
        }
        self.finish_repair(&plan, job, cancelled)?;
        Ok(Ok(()))
    }

    /// Ends approved repair `plan`, none of whose steps started, as its Job `job` fails with `why`: every step fails
    /// with it, the plan finishes, owing no Audit since nothing was changed, and its Source Set is released.
    fn abandon_repair(&self, plan: &RepairPlan, job: i64, why: &str) -> Result<()> {
        let at = now();
        self.write(|tx| {
            tx.execute(
                "UPDATE repair_steps SET state = 'failed', error = ?2, finished_at = ?3
                 WHERE plan_id = ?1 AND state = 'pending'",
                params![plan.id, why, at],
            )?;
            Ok(())
        })?;
        let outcomes = self.outcomes(plan)?;
        self.write(|tx| {
            tx.execute(
                "UPDATE repair_plans SET finished_at = ?2, outcomes = ?3, recovery_error = NULL WHERE id = ?1",
                params![plan.id, at, to_json(&outcomes)],
            )?;
            finish_running(tx, job, JobState::Failed, Some(why))
        })
    }

    /// Writes Source Set `set`'s first marker through `dir`, durably: that its temporary file is about to be created
    /// is committed first, and its identity as soon as it is; it is written and synced, renamed to the marker only
    /// while it is still that file and by a rename that never replaces anything, and the folder synced before the
    /// marker is recorded. A temporary file a stop left is written again and published, but only while it is the
    /// file recorded, or, when its identity was not yet committed, the empty file this service's user just made.
    /// Anything else at either name, a marker that appeared meanwhile included, stops the repair with nothing changed.
    fn bootstrap_marker(
        &self,
        set: i64,
        dir: &FolderDir,
    ) -> Result<std::result::Result<(), String>> {
        let path = dir.path().display().to_string();
        let marker = SourceSetMarker {
            store: self.read(store_id)?,
            source_set: set,
        };
        let mut json = serde_json::to_vec_pretty(&marker).expect("a marker always serializes");
        json.push(b'\n');
        let (pending, recorded) = self.read(|c| {
            Ok(c.query_row(
                "SELECT marker_temp_pending, marker_temp_dev, marker_temp_ino FROM source_folders
                 WHERE source_set_id = ?1",
                [set],
                |r| {
                    let id = match (r.get::<_, Option<i64>>(1)?, r.get::<_, Option<i64>>(2)?) {
                        (Some(dev), Some(ino)) => Some(Identity::from_sql(dev, ino)),
                        _ => None,
                    };
                    Ok((r.get::<_, bool>(0)?, id))
                },
            )?)
        })?;
        let stat = |name: &str| dir.stat(name).map_err(io_err);
        let is_file = |st: &rustix::fs::Stat, id: Identity| {
            FolderDir::kind(st) == FileType::RegularFile && Identity::of(st) == id
        };
        let forget = |store: &Store| {
            store.write(|tx| {
                tx.execute(
                    "UPDATE source_folders SET marker_temp_pending = 0, marker_temp_dev = NULL,
                         marker_temp_ino = NULL WHERE source_set_id = ?1",
                    [set],
                )?;
                Ok(())
            })
        };
        if let Some(st) = stat(MARKER)? {
            // Renamed into place before a stop, or put there by something else.
            if let Some(id) = recorded
                && is_file(&st, id)
                && dir.read(MARKER, MARKER_LIMIT).map_err(io_err)? == json
            {
                dir.sync().map_err(io_err)?;
                return self.record_marker(set).map(Ok);
            }
            // The temporary file recorded is gone, so what is there may be what the rename published in its place.
            if recorded.is_some() && stat(MARKER_NEW)?.is_none() {
                return self.unpublish_marker(set, dir);
            }
            forget(self)?;
            return Ok(Err(format!(
                "{path}: a .title-select marker appeared in it before its first repair wrote one; is this the right \
                 folder? Nothing in it was changed"
            )));
        }
        let left = stat(MARKER_NEW)?;
        let id = match (left, recorded) {
            (Some(st), Some(id)) if is_file(&st, id) => id,
            (Some(st), None)
                if pending
                    && FolderDir::kind(&st) == FileType::RegularFile
                    && st.st_size == 0
                    && st.st_nlink == 1
                    && st.st_uid == rustix::process::geteuid().as_raw() =>
            {
                self.record_marker_temp(set, Identity::of(&st))?;
                Identity::of(&st)
            }
            (Some(_), _) => {
                forget(self)?;
                return Ok(Err(format!(
                    "{path}: {MARKER_NEW} is not the file Title Select was writing its marker to; move it out of the \
                     folder, then plan the repair again. Nothing in it was changed"
                )));
            }
            (None, _) => {
                self.write(|tx| {
                    tx.execute(
                        "UPDATE source_folders SET marker_temp_pending = 1, marker_temp_dev = NULL,
                             marker_temp_ino = NULL WHERE source_set_id = ?1",
                        [set],
                    )?;
                    Ok(())
                })?;
                let file = match dir.create_new(MARKER_NEW) {
                    Ok(file) => file,
                    Err(e) => {
                        forget(self)?;
                        return Ok(Err(format!("{path}: {e}; nothing in it was changed")));
                    }
                };
                let id = Identity::of_fd(&file).map_err(io_err)?;
                self.record_marker_temp(set, id)?;
                id
            }
        };
        let mut file = dir.open_write(MARKER_NEW).map_err(io_err)?;
        if Identity::of_fd(&file).map_err(io_err)? != id {
            forget(self)?;
            return Ok(Err(format!(
                "{path}: {MARKER_NEW} was replaced while the marker was written; nothing in it was changed"
            )));
        }
        {
            use std::io::Write;
            file.set_len(0)
                .and_then(|()| file.write_all(&json))
                .and_then(|()| file.sync_all())
                .map_err(io_err)?;
        }
        drop(file);
        self.fault_point(Fault::RepairMarkerWritten);
        if !stat(MARKER_NEW)?.is_some_and(|st| is_file(&st, id)) {
            forget(self)?;
            return Ok(Err(format!(
                "{path}: {MARKER_NEW} was replaced while the marker was written; nothing in it was changed"
            )));
        }
        if let Err(e) = dir.rename_new(MARKER_NEW, dir, MARKER) {
            return Ok(Err(match e.kind() {
                io::ErrorKind::AlreadyExists => format!(
                    "{path}: a .title-select marker appeared in it while its first repair wrote one; is this the \
                     right folder? Nothing in it was changed"
                ),
                _ => format!("{path}: {e}; nothing in it was changed"),
            }));
        }
        self.fault_point(Fault::RepairMarkerPublished);
        dir.sync().map_err(io_err)?;
        if !stat(MARKER)?.is_some_and(|st| is_file(&st, id)) {
            return self.unpublish_marker(set, dir);
        }
        self.record_marker(set).map(Ok)
    }

    /// Puts back, as the temporary name, what the first marker's rename published in place of the file recorded, by
    /// a rename that never replaces anything, then forgets the temporary file; never leaves it as the marker.
    fn unpublish_marker(
        &self,
        set: i64,
        dir: &FolderDir,
    ) -> Result<std::result::Result<(), String>> {
        let path = dir.path().display().to_string();
        if let Err(e) = dir.rename_new(MARKER, dir, MARKER_NEW) {
            return Ok(Err(format!(
                "{path}: its .title-select marker was replaced as it was written, and what replaced it could not be \
                 moved back to {MARKER_NEW} ({e}); move it out of the folder, then plan the repair again"
            )));
        }
        dir.sync().map_err(io_err)?;
        self.write(|tx| {
            tx.execute(
                "UPDATE source_folders SET marker_temp_pending = 0, marker_temp_dev = NULL,
                     marker_temp_ino = NULL WHERE source_set_id = ?1",
                [set],
            )?;
            Ok(())
        })?;
        Ok(Err(format!(
            "{path}: its .title-select marker was replaced as it was written, so what replaced it was moved back to \
             {MARKER_NEW}; move it out of the folder, then plan the repair again. Nothing else in it was changed"
        )))
    }

    fn record_marker_temp(&self, set: i64, id: Identity) -> Result<()> {
        let (dev, ino) = id.to_sql();
        self.write(|tx| {
            tx.execute(
                "UPDATE source_folders SET marker_temp_pending = 1, marker_temp_dev = ?2, marker_temp_ino = ?3
                 WHERE source_set_id = ?1",
                params![set, dev, ino],
            )?;
            Ok(())
        })
    }

    fn record_marker(&self, set: i64) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE source_folders SET marker_at = ?2, marker_temp_pending = 0, marker_temp_dev = NULL,
                     marker_temp_ino = NULL WHERE source_set_id = ?1",
                params![set, now()],
            )?;
            Ok(())
        })
    }

    fn cancel_requested(&self, job: i64) -> Result<bool> {
        self.read(|c| {
            Ok(c.query_row(
                "SELECT cancel_requested_at IS NOT NULL FROM jobs WHERE id = ?1",
                [job],
                |r| r.get(0),
            )?)
        })
    }

    /// Once you cancel a repair: skips every step of each item nothing was yet changed for, deleting the files its
    /// Builds staged. An item is committed, and runs to its end, once one of its steps other than a Build has
    /// started, or a committed or started step keeping a file aside needs one of its Builds.
    fn cancel_uncommitted(&self, r: &Repairing) -> Ran {
        let steps = self.read(|c| steps_of(c, r.plan.id))?;
        // A step skipped changed nothing, so it commits nothing.
        let started = |s: &StepRow| !matches!(s.state.as_str(), "pending" | "skipped");
        let mut committed: std::collections::HashSet<&str> = steps
            .iter()
            .filter(|s| !matches!(s.step, RepairStep::Build { .. }) && started(s))
            .map(|s| s.item.as_str())
            .collect();
        loop {
            let before = committed.len();
            for step in &steps {
                if let RepairStep::Keep { needs, affects, .. } = &step.step
                    && (started(step)
                        || committed.contains(step.item.as_str())
                        || affects.iter().any(|a| committed.contains(a.as_str())))
                {
                    committed.insert(step.item.as_str());
                    committed.extend(affects.iter().map(String::as_str));
                    for need in needs {
                        if let Some(build) = steps.iter().find(|b| b.seq == *need) {
                            committed.insert(build.item.as_str());
                        }
                    }
                }
            }
            if committed.len() == before {
                break;
            }
        }
        let skipped: Vec<i64> = steps
            .iter()
            .filter(|s| {
                !committed.contains(s.item.as_str())
                    && !matches!(s.state.as_str(), "failed" | "skipped")
            })
            .map(|s| s.id)
            .collect();
        self.write(|tx| {
            tx.execute(
                "UPDATE repair_plans SET cancelled = 1 WHERE id = ?1",
                [r.plan.id],
            )?;
            for id in &skipped {
                tx.execute(
                    "UPDATE repair_steps SET state = 'skipped', error = ?2, finished_at = ?3 WHERE id = ?1",
                    params![
                        id,
                        "you cancelled the repair before it changed anything for this",
                        now()
                    ],
                )?;
            }
            Ok(())
        })?;
        self.fault_point(Fault::RepairCancelled);
        self.discard_skipped(r)
    }

    /// Deletes each file a skipped Build staged, if it is still there. Returns why one could not be, leaving the
    /// repair unfinished so a later Job tries again.
    fn discard_skipped(&self, r: &Repairing) -> Ran {
        for step in self.read(|c| steps_of(c, r.plan.id))? {
            if let (RepairStep::Build { .. }, "skipped", Some(name), Some(id)) = (
                &step.step,
                step.state.as_str(),
                &step.staged_name,
                step.staged,
            ) && let Err(why) = self.discard_staged(r, &step, name, id)?
            {
                return Ok(Err(why));
            }
        }
        Ok(Ok(()))
    }

    /// Records what repair `plan` did, the incremental Audit it owes, and the end of its Job `job`, in one
    /// transaction.
    fn finish_repair(&self, plan: &RepairPlan, job: i64, cancelled: bool) -> Result<()> {
        let outcomes = self.outcomes(plan)?;
        self.write(|tx| {
            tx.execute(
                "UPDATE repair_plans SET finished_at = ?2, outcomes = ?3, recovery_error = NULL, audit_owed = 1
                 WHERE id = ?1",
                params![plan.id, now(), to_json(&outcomes)],
            )?;
            match cancelled {
                true => finish_running(
                    tx,
                    job,
                    JobState::Interrupted,
                    Some(
                        "you cancelled the repair; what it had started was finished, and the rest was left as it \
                         was",
                    ),
                ),
                false => finish_running(tx, job, JobState::Succeeded, None),
            }
        })?;
        self.fault_point(Fault::RepairFinished);
        Ok(())
    }

    /// What each item of repair `plan` came to, from its steps as recorded.
    fn outcomes(&self, plan: &RepairPlan) -> Result<Vec<RepairOutcome>> {
        let steps = self.read(|c| steps_of(c, plan.id))?;
        let mut outcomes: Vec<RepairOutcome> = Vec::new();
        for step in &steps {
            let outcome = match outcomes.iter_mut().find(|o| o.item == step.item) {
                Some(outcome) => outcome,
                None => {
                    outcomes.push(RepairOutcome {
                        item: step.item.clone(),
                        done: true,
                        detail: Vec::new(),
                        error: None,
                    });
                    outcomes.last_mut().expect("just pushed")
                }
            };
            let describe = step.step.describe();
            match step.state.as_str() {
                "done" => outcome.detail.push(describe),
                _ => {
                    outcome.done = false;
                    let why = step.error.clone().unwrap_or_else(|| "not done".into());
                    outcome.detail.push(format!("did not {describe}: {why}"));
                    outcome.error.get_or_insert(why);
                }
            }
            outcome.detail.extend(step.note.clone());
        }
        Ok(outcomes)
    }

    fn step_failed(&self, step: &StepRow, state: &str, why: String) -> Ran {
        self.fail_step(step, state, why)?;
        Ok(Ok(()))
    }

    fn fail_step(&self, step: &StepRow, state: &str, why: String) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE repair_steps SET state = ?2, error = ?3, finished_at = ?4 WHERE id = ?1",
                params![step.id, state, why, now()],
            )?;
            Ok(())
        })
    }

    fn step_state(&self, step: &StepRow, state: &str) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE repair_steps SET state = ?2 WHERE id = ?1",
                params![step.id, state],
            )?;
            Ok(())
        })
    }

    fn step_done(&self, step: &StepRow, note: Option<String>) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE repair_steps SET state = 'done', note = ?2, finished_at = ?3 WHERE id = ?1",
                params![step.id, note, now()],
            )?;
            Ok(())
        })
    }

    /// The state of step `seq` of the plan, and the step itself.
    fn other_step(&self, plan: i64, seq: u32) -> Result<StepRow> {
        self.read(|c| {
            c.query_row(
                &format!("{STEP_QUERY} WHERE plan_id = ?1 AND seq = ?2"),
                params![plan, seq as i64],
                step_from_row,
            )
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("no step {seq} of repair plan {plan}")))
        })
    }

    fn run_step(&self, config: &Config, r: &Repairing, step: &StepRow) -> Ran {
        match &step.step {
            RepairStep::Build {
                target,
                roms,
                sources,
            } => self.run_build(r, step, target, roms, sources),
            RepairStep::Keep {
                path,
                kept_name,
                reason,
                seen,
                needs,
                ..
            } => {
                for need in needs {
                    let build = self.other_step(r.plan.id, *need)?;
                    if build.state != "done" {
                        return self.step_failed(
                            step,
                            "skipped",
                            format!(
                                "{path} was left as it is, since its game's new file was not written: {}",
                                build.error.unwrap_or_default()
                            ),
                        );
                    }
                }
                self.run_keep(r, step, path, kept_name, reason, &seen_of(seen))
            }
            RepairStep::Rename {
                from,
                to,
                seen,
                roms,
            } => self.run_rename(r, step, from, to, &seen_of(seen), roms),
            RepairStep::Publish { build, target } => self.run_publish(r, step, *build, target),
            RepairStep::Fill { .. } => self.run_fill(config, r, step),
        }
    }

    /// Writes a Build step's file under a staged name recorded first, from its sources while each is still the file
    /// the plan saw, and checks it against the DAT.
    fn run_build(
        &self,
        r: &Repairing,
        step: &StepRow,
        target: &str,
        roms: &[RepairRom],
        sources: &BTreeMap<String, SeenFile>,
    ) -> Ran {
        let dir = &r.dir;
        // A staged file a stop left being deleted is deleted before anything is staged again.
        if let (Some(staged_name), Some(id), Some(_)) =
            (&step.staged_name, step.staged, &step.discard_slot)
            && let Err(why) = self.discard_staged(r, step, staged_name, id)?
        {
            return Ok(Err(why));
        }
        if let (Some(staged_name), "staging") = (&step.staged_name, step.state.as_str()) {
            match step.staged {
                Some(id) if is_file(dir, staged_name, id).map_err(io_err)? => {
                    let file = dir.open_file(staged_name).map_err(io_err)?;
                    let expected: Vec<_> = roms.iter().map(|r| r.rom.clone()).collect();
                    if verify(&file, &expected).is_ok() && file.sync_all().is_ok() {
                        dir.sync().map_err(io_err)?;
                        return self.build_staged(step, &file);
                    }
                    drop(file);
                    if let Err(why) = self.discard_staged(r, step, staged_name, id)? {
                        return Ok(Err(why));
                    }
                }
                Some(_) => {}
                None if dir.stat(staged_name).map_err(io_err)?.is_some() => {
                    return self.step_failed(
                        step,
                        "failed",
                        format!(
                            "{}/{staged_name} is not the file Title Select was writing; it was left as it is",
                            dir.path().display()
                        ),
                    );
                }
                None => {}
            }
        }
        let staged_name = format!(".{target}.title-select-new-{}", unique());
        self.write(|tx| {
            tx.execute(
                "UPDATE repair_steps SET state = 'staging', staged_name = ?2, staged_dev = NULL, staged_ino = NULL
                 WHERE id = ?1",
                params![step.id, staged_name],
            )?;
            Ok(())
        })?;
        let file = match dir.create_new(&staged_name) {
            Ok(file) => file,
            Err(e) => return self.step_failed(step, "failed", e.to_string()),
        };
        let id = Identity::of_fd(&file).map_err(io_err)?;
        let (dev, ino) = id.to_sql();
        self.write(|tx| {
            tx.execute(
                "UPDATE repair_steps SET staged_dev = ?2, staged_ino = ?3 WHERE id = ?1",
                params![step.id, dev, ino],
            )?;
            Ok(())
        })?;
        let written = (|| -> std::result::Result<(), String> {
            let mut opened: Vec<File> = Vec::new();
            let mut index: HashMap<&str, usize> = HashMap::new();
            for (path, seen) in sources {
                let (parent, name) = parent_of(dir, path).map_err(|e| e.to_string())?;
                let file = parent
                    .as_ref()
                    .unwrap_or(dir)
                    .open_file(name)
                    .map_err(|e| e.to_string())?;
                let st = rustix::fs::fstat(&file).map_err(|e| e.to_string())?;
                if Seen::of(&st) != seen_of(seen) {
                    return Err(format!(
                        "{path} changed since the plan was made; it was left as it is"
                    ));
                }
                index.insert(path, opened.len());
                opened.push(file);
            }
            let from: Vec<FromSource> = roms
                .iter()
                .map(|rom| (&rom.rom, index[rom.from.as_str()], rom.member.as_deref()))
                .collect();
            let refs: Vec<&File> = opened.iter().collect();
            repack_sources(&file, &from, &refs)?;
            let expected: Vec<_> = roms.iter().map(|r| r.rom.clone()).collect();
            verify(&file, &expected)
                .map_err(|why| format!("the new {target} did not check: {why}"))?;
            for ((path, seen), source) in sources.iter().zip(&opened) {
                let st = rustix::fs::fstat(source).map_err(|e| e.to_string())?;
                if Seen::of(&st) != seen_of(seen) {
                    return Err(format!(
                        "{path} changed while it was read; it was left as it is"
                    ));
                }
            }
            file.sync_all().map_err(|e| e.to_string())?;
            Ok(())
        })();
        if let Err(why) = written {
            drop(file);
            // The step stays unfinished until what it staged is deleted, so nothing is left behind untracked.
            if let Err(left) = self.discard_staged(r, step, &staged_name, id)? {
                return Ok(Err(format!("{why}; {left}")));
            }
            return self.step_failed(step, "failed", why);
        }
        dir.sync().map_err(io_err)?;
        self.build_staged(step, &file)
    }

    fn build_staged(&self, step: &StepRow, file: &File) -> Ran {
        let (sha, _) = sha256_of(file).map_err(io_err)?;
        self.write(|tx| {
            tx.execute(
                "UPDATE repair_steps SET state = 'done', sha256 = ?2, finished_at = ?3 WHERE id = ?1",
                params![step.id, sha, now()],
            )?;
            Ok(())
        })?;
        self.fault_point(Fault::RepairStaged(step.seq));
        Ok(Ok(()))
    }

    /// Deletes a staged file Title Select wrote and will not publish: moved into the private `.deleting` folder
    /// under a slot recorded on `step` first, and deleted there only while it is still the file `id`. Anything else
    /// found in the slot was swapped in for it and is put back under `staged_name`, never deleted. Resumes from
    /// wherever a stop left the file; the intent is cleared only once the slot is empty and the file is gone from
    /// `staged_name`. Returns why it could not be, leaving the intent for a later try.
    fn discard_staged(
        &self,
        r: &Repairing,
        step: &StepRow,
        staged_name: &str,
        id: Identity,
    ) -> Ran {
        let dir = &r.dir;
        let left = || Ok(Err(undeleted(dir, staged_name)));
        let recorded: Option<String> = self.read(|c| {
            Ok(c.query_row(
                "SELECT discard_slot FROM repair_steps WHERE id = ?1",
                [step.id],
                |r| r.get(0),
            )?)
        })?;
        let here = is_file(dir, staged_name, id).map_err(io_err)?;
        if recorded.is_none() && !here {
            return Ok(Ok(()));
        }
        let slot = match recorded {
            Some(slot) => slot,
            None => {
                let slot = format!("repair-staged-{}", unique());
                self.write(|tx| {
                    tx.execute(
                        "UPDATE repair_steps SET discard_slot = ?2 WHERE id = ?1",
                        params![step.id, slot],
                    )?;
                    Ok(())
                })?;
                slot
            }
        };
        let Ok(Some(deleting)) = open_area_deleting(self, r.scope(), dir, true)? else {
            return left();
        };
        if here {
            if dir.rename_new(staged_name, &deleting, &slot).is_err() {
                return left();
            }
            self.fault_point(Fault::RepairDiscarding(step.seq));
        }
        // Whether this call or one before a stop moved it, both folders are synced before it is deleted.
        dir.sync().map_err(io_err)?;
        deleting.sync().map_err(io_err)?;
        match deleting.stat(&slot).map_err(io_err)? {
            Some(st)
                if FolderDir::kind(&st) == FileType::RegularFile && Identity::of(&st) == id =>
            {
                deleting.unlink(&slot).map_err(io_err)?;
                deleting.sync().map_err(io_err)?;
            }
            Some(_) => {
                if deleting.rename_new(&slot, dir, staged_name).is_err() {
                    return Ok(Err(format!(
                        "what was moved to be deleted from {}/{staged_name} is not the file Title Select staged, \
                         and could not be put back; it was left at .title-select-retained/.deleting/{slot}",
                        dir.path().display()
                    )));
                }
                deleting.sync().map_err(io_err)?;
                dir.sync().map_err(io_err)?;
            }
            None => {}
        }
        if is_file(dir, staged_name, id).map_err(io_err)?
            || deleting.stat(&slot).map_err(io_err)?.is_some()
        {
            return left();
        }
        self.write(|tx| {
            tx.execute(
                "UPDATE repair_steps SET discard_slot = NULL WHERE id = ?1",
                [step.id],
            )?;
            Ok(())
        })?;
        Ok(Ok(()))
    }

    /// Keeps `path` aside as `kept`, by one rename that never replaces anything, with the intent recorded first.
    fn run_keep(
        &self,
        r: &Repairing,
        step: &StepRow,
        path: &str,
        kept: &str,
        reason: &str,
        seen: &Seen,
    ) -> Ran {
        let done = |tx: &Transaction, kept_id: i64| -> Result<()> {
            tx.execute(
                "UPDATE repair_steps SET state = 'done', kept_id = ?2, finished_at = ?3 WHERE id = ?1",
                params![step.id, kept_id, now()],
            )?;
            Ok(())
        };
        match self.keep_file(r, step, path, kept, reason, seen, &done)? {
            Ok(Some(_)) => {
                self.fault_point(Fault::RepairStepDone(step.seq));
                Ok(Ok(()))
            }
            Ok(None) => Ok(Ok(())),
            Err(why) => Ok(Err(why)),
        }
    }

    /// Moves `path` into the kept files of this repair's generation as `kept`, while it is the file `seen`, then
    /// records it kept and runs `finish` in the same transaction, so that the step ends with the record. A record a
    /// stop left, of this file under this name, is taken as this one. `None` when the step failed instead, as
    /// recorded; `Err` leaves it for recovery.
    #[allow(clippy::too_many_arguments)]
    fn keep_file(
        &self,
        r: &Repairing,
        step: &StepRow,
        path: &str,
        kept: &str,
        reason: &str,
        seen: &Seen,
        finish: &dyn Fn(&Transaction, i64) -> Result<()>,
    ) -> Result<std::result::Result<Option<i64>, String>> {
        let content = &r.plan.content;
        let (parent, name) = match parent_of(&r.dir, path) {
            Ok(found) => found,
            Err(e) => {
                self.fail_step(step, "failed", format!("{e}; it was left as it is"))?;
                return Ok(Ok(None));
            }
        };
        let parent = parent.as_ref().unwrap_or(&r.dir);
        let retention = match open_retention(self, r.scope(), &r.dir, content.generation, true)? {
            Ok(Some(retention)) => retention,
            Ok(None) => return Ok(Err("the kept files folder is missing".into())),
            Err(why) => return Ok(Err(why)),
        };
        let generation = &retention.generation;
        let in_place = is_seen(parent, name, seen).map_err(io_err)?;
        if step.state == "pending" || (in_place && step.state == "moving") {
            if !in_place {
                self.fail_step(
                    step,
                    "failed",
                    format!("{path} changed since the plan was made; it was left as it is"),
                )?;
                return Ok(Ok(None));
            }
            self.step_state(step, "moving")?;
            if let Err(e) = parent.rename_new(name, generation, kept) {
                self.fail_step(
                    step,
                    "failed",
                    format!("{path} could not be kept aside: {e}; it was left as it is"),
                )?;
                return Ok(Ok(None));
            }
            self.fault_point(Fault::RepairMoved(step.seq));
        }
        // Whether this call or one before a stop moved it, both folders are synced before it is recorded kept.
        parent.sync().map_err(io_err)?;
        generation.sync().map_err(io_err)?;
        let st = match generation.stat(kept).map_err(io_err)? {
            Some(st)
                if FolderDir::kind(&st) == FileType::RegularFile
                    && Identity::of(&st) == seen.identity =>
            {
                st
            }
            Some(st) => {
                return self.keep_substitute(r, step, path, (parent, name), generation, kept, &st);
            }
            None => {
                self.fail_step(
                    step,
                    "failed",
                    format!("{path} is neither where it was nor kept as {kept}"),
                )?;
                return Ok(Ok(None));
            }
        };
        let file = file_stat(&st);
        let (dev, ino) = seen.identity.to_sql();
        let kept_id = self.write(|tx| {
            let recorded: Option<(i64, i64, i64, String)> = tx
                .query_row(
                    "SELECT id, dev, ino, name FROM source_kept
                     WHERE source_set_id = ?1 AND generation = ?2 AND kept_name = ?3",
                    params![content.set_id, content.generation as i64, kept],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?;
            if let Some((id, rdev, rino, name)) = recorded {
                if (rdev, rino) != (dev, ino) || name != path {
                    return Ok(Err(format!(
                        "{kept} in the kept files is recorded as another file than {path}; it was left as it is"
                    )));
                }
                finish(tx, id)?;
                return Ok(Ok(id));
            }
            tx.execute(
                "INSERT INTO source_kept (source_set_id, plan_id, generation, name, kept_name, reason, size,
                                          mtime_ns, dev, ino, kept_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    content.set_id,
                    r.plan.id,
                    content.generation as i64,
                    path,
                    kept,
                    reason,
                    file.size as i64,
                    file.mtime_ns.to_string(),
                    dev,
                    ino,
                    now()
                ],
            )?;
            let id = tx.last_insert_rowid();
            finish(tx, id)?;
            Ok(Ok(id))
        })?;
        Ok(kept_id.map(Some))
    }

    /// Settles `st`, found kept as `kept` where the rename of `path` should have put the file planned: another file
    /// was moved in its place. A regular file is recorded kept under its own identity, in the same transaction that
    /// fails the step; anything else is put back as `path` without replacing anything, or, when that name was taken,
    /// left where it is, as the step fails saying so. A regular file is never left in the kept files unrecorded.
    #[allow(clippy::too_many_arguments)]
    fn keep_substitute(
        &self,
        r: &Repairing,
        step: &StepRow,
        path: &str,
        (parent, name): (&FolderDir, &str),
        generation: &FolderDir,
        kept: &str,
        st: &rustix::fs::Stat,
    ) -> Result<std::result::Result<Option<i64>, String>> {
        let content = &r.plan.content;
        if FolderDir::kind(st) != FileType::RegularFile {
            // Not a file, so never recorded kept: put back if its name is free, else left visible where it is.
            let why = match generation.rename_new(kept, parent, name) {
                Ok(()) => {
                    parent.sync().map_err(io_err)?;
                    generation.sync().map_err(io_err)?;
                    format!(
                        "{path} was replaced by something else as it was kept aside; that was put back as {path}, \
                         which was left as it is"
                    )
                }
                Err(e) => format!(
                    "{path} was replaced by something else as it was kept aside, which could not be put back ({e}); \
                     it was left at {kept} in the kept files"
                ),
            };
            self.fail_step(step, "failed", why)?;
            return Ok(Ok(None));
        }
        let found = Identity::of(st);
        let (dev, ino) = found.to_sql();
        let file = file_stat(st);
        let why = format!(
            "{path} was replaced by another file as it was kept aside; that file was kept as {kept} instead, and {path} \
             left as it is"
        );
        let settled = self.write(|tx| {
            let recorded: Option<(i64, i64)> = tx
                .query_row(
                    "SELECT dev, ino FROM source_kept
                     WHERE source_set_id = ?1 AND generation = ?2 AND kept_name = ?3",
                    params![content.set_id, content.generation as i64, kept],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            match recorded {
                Some(ids) if ids != (dev, ino) => {
                    return Ok(Err(format!(
                        "{kept} in the kept files is recorded as another file than the one found there"
                    )));
                }
                Some(_) => {}
                None => {
                    tx.execute(
                        "INSERT INTO source_kept (source_set_id, plan_id, generation, name, kept_name, reason, size,
                                                  mtime_ns, dev, ino, kept_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                        params![
                            content.set_id,
                            r.plan.id,
                            content.generation as i64,
                            path,
                            kept,
                            format!("found in place of {path} as it was kept aside"),
                            file.size as i64,
                            file.mtime_ns.to_string(),
                            dev,
                            ino,
                            now()
                        ],
                    )?;
                }
            }
            tx.execute(
                "UPDATE repair_steps SET state = 'failed', error = ?2, finished_at = ?3 WHERE id = ?1",
                params![step.id, why, now()],
            )?;
            Ok(Ok(()))
        })?;
        Ok(settled.map(|()| None))
    }

    /// Renames `from` to `to`, by one rename that never replaces anything, while `from` is the file `seen` and reads
    /// as exactly `roms`.
    fn run_rename(
        &self,
        r: &Repairing,
        step: &StepRow,
        from: &str,
        to: &str,
        seen: &Seen,
        roms: &[ExpectedRom],
    ) -> Ran {
        let (parent, name) = match parent_of(&r.dir, from) {
            Ok(found) => found,
            Err(e) => {
                return self.step_failed(step, "failed", format!("{e}; it was left as it is"));
            }
        };
        let parent = parent.as_ref().unwrap_or(&r.dir);
        let in_place = is_seen(parent, name, seen).map_err(io_err)?;
        if step.state == "pending" || (in_place && step.state == "moving") {
            if !in_place {
                return self.step_failed(
                    step,
                    "failed",
                    format!("{from} changed since the plan was made; it was left as it is"),
                );
            }
            if let Err(why) = holds_exactly(parent, name, seen, roms).map_err(io_err)? {
                return self.step_failed(
                    step,
                    "failed",
                    format!(
                        "{from} no longer reads as the audit found it: {why}; it was left as it is"
                    ),
                );
            }
            self.step_state(step, "moving")?;
            if let Err(e) = parent.rename_new(name, &r.dir, to) {
                return self.step_failed(
                    step,
                    "failed",
                    format!("{from} could not be renamed: {e}; it was left as it is"),
                );
            }
            self.fault_point(Fault::RepairMoved(step.seq));
        }
        parent.sync().map_err(io_err)?;
        r.dir.sync().map_err(io_err)?;
        if !is_file(&r.dir, to, seen.identity).map_err(io_err)? {
            if r.dir.stat(to).map_err(io_err)?.is_none() {
                return self.step_failed(
                    step,
                    "failed",
                    format!("{from} is neither where it was nor at {to}"),
                );
            }
            // Something else was renamed in place of the file planned: it is put back, never left under the name.
            let why = match r.dir.rename_new(to, parent, name) {
                Ok(()) => {
                    parent.sync().map_err(io_err)?;
                    r.dir.sync().map_err(io_err)?;
                    format!(
                        "{from} was replaced by another file as it was renamed; that file was put back as {from}"
                    )
                }
                Err(e) => format!(
                    "{from} was replaced by another file as it was renamed, and that file could not be put back \
                     ({e}); it was left as {to}, for the audit this repair queues to read"
                ),
            };
            return self.step_failed(step, "failed", why);
        }
        self.step_done(step, None)?;
        self.fault_point(Fault::RepairStepDone(step.seq));
        Ok(Ok(()))
    }

    /// Publishes the staged file of Build step `build` as `target`, by one rename that never replaces anything. A
    /// staged file that cannot be published is kept aside rather than left.
    fn run_publish(&self, r: &Repairing, step: &StepRow, build: u32, target: &str) -> Ran {
        let built = self.other_step(r.plan.id, build)?;
        let (Some(staged_name), Some(staged), "done") =
            (&built.staged_name, built.staged, built.state.as_str())
        else {
            return self.step_failed(
                step,
                "skipped",
                format!(
                    "the new {target} was not written: {}",
                    built.error.unwrap_or_default()
                ),
            );
        };
        if let Some(kept) = &step.aside_name {
            let why = step.aside_why.clone().unwrap_or_default();
            return self.keep_unpublished(r, step, staged_name, staged, kept, why);
        }
        let dir = &r.dir;
        let at_staged = is_file(dir, staged_name, staged).map_err(io_err)?;
        if step.state == "pending" || (at_staged && step.state == "moving") {
            if !at_staged {
                return self.step_failed(
                    step,
                    "failed",
                    format!("the new {target} is no longer at {staged_name}"),
                );
            }
            self.step_state(step, "moving")?;
            if let Err(e) = dir.rename_new(staged_name, dir, target) {
                let why = format!("the new {target} could not be put in place: {e}");
                let kept = match self.record_aside(r, step, staged_name, staged, target, &why)? {
                    Ok(kept) => kept,
                    Err(left) => return self.step_failed(step, "failed", left),
                };
                return self.keep_unpublished(r, step, staged_name, staged, &kept, why);
            }
            self.fault_point(Fault::RepairMoved(step.seq));
        }
        dir.sync().map_err(io_err)?;
        if !is_file(dir, target, staged).map_err(io_err)? {
            return self.step_failed(
                step,
                "failed",
                format!("{target} is not the new file Title Select wrote"),
            );
        }
        self.step_done(step, None)?;
        self.fault_point(Fault::RepairStepDone(step.seq));
        Ok(Ok(()))
    }

    /// Records the intent to keep the staged file `staged_name`, `staged`, that could not be published as `target`,
    /// aside under the first name free in this repair's generation, and why. Returns that name, or why the file was
    /// left where it is.
    fn record_aside(
        &self,
        r: &Repairing,
        step: &StepRow,
        staged_name: &str,
        staged: Identity,
        target: &str,
        why: &str,
    ) -> Result<std::result::Result<String, String>> {
        let generation =
            match open_retention(self, r.scope(), &r.dir, r.plan.content.generation, true)? {
                Ok(Some(retention)) => retention.generation,
                _ => return Ok(Err(format!("{why}; it was left at {staged_name}"))),
            };
        let kept = (1..)
            .map(|n| kept_name(target, n))
            .find(|name| matches!(generation.stat(name), Ok(None)))
            .expect("a free name");
        let (dev, ino) = staged.to_sql();
        self.write(|tx| {
            tx.execute(
                "UPDATE repair_steps SET aside_name = ?2, aside_why = ?3, staged_name = ?4, staged_dev = ?5,
                                         staged_ino = ?6
                 WHERE id = ?1",
                params![step.id, kept, why, staged_name, dev, ino],
            )?;
            Ok(())
        })?;
        Ok(Ok(kept))
    }

    /// Keeps the staged file `staged_name`, `staged`, that could not be published aside as `kept`, its intent
    /// recorded, and fails the step with `why`; from wherever a stop left it.
    fn keep_unpublished(
        &self,
        r: &Repairing,
        step: &StepRow,
        staged_name: &str,
        staged: Identity,
        kept: &str,
        why: String,
    ) -> Ran {
        let generation =
            match open_retention(self, r.scope(), &r.dir, r.plan.content.generation, true)? {
                Ok(Some(retention)) => retention.generation,
                _ => {
                    return self.step_failed(
                        step,
                        "failed",
                        format!("{why}; it was left at {staged_name}"),
                    );
                }
            };
        let found = |d: &FolderDir, name: &str| -> Result<Option<Seen>> {
            Ok(d.stat(name)
                .map_err(io_err)?
                .filter(|st| Identity::of(st) == staged)
                .map(|st| Seen::of(&st)))
        };
        let Some(seen) = found(&r.dir, staged_name)?.or(found(&generation, kept)?) else {
            return self.step_failed(
                step,
                "failed",
                format!("{why}; it is neither at {staged_name} nor kept as {kept}"),
            );
        };
        let keeping = StepRow {
            state: "moving".into(),
            ..self.other_step(r.plan.id, step.seq)?
        };
        let target = match &step.step {
            RepairStep::Publish { target, .. } => target.as_str(),
            _ => staged_name,
        };
        let reason = format!("a repair's new {target}, which could not be put in place");
        let failed = |tx: &Transaction, _: i64| -> Result<()> {
            tx.execute(
                "UPDATE repair_steps SET state = 'failed', error = ?2, finished_at = ?3 WHERE id = ?1",
                params![step.id, format!("{why}; it was kept aside as {kept}"), now()],
            )?;
            Ok(())
        };
        match self.keep_file(r, &keeping, staged_name, kept, &reason, &seen, &failed)? {
            Ok(Some(_)) => Ok(Ok(())),
            Ok(None) => {
                let left = self
                    .other_step(r.plan.id, step.seq)?
                    .error
                    .unwrap_or_default();
                self.step_failed(step, "failed", format!("{why}; {left}"))
            }
            Err(left) => Ok(Err(left)),
        }
    }

    /// Fills a Game from an Ingest Folder item with the move out of an Ingest Folder, its intent recorded with the
    /// step. The item's original is deleted only when the move names the one ROM it is, exactly.
    fn run_fill(&self, config: &Config, r: &Repairing, step: &StepRow) -> Ran {
        let RepairStep::Fill {
            target,
            ingest_root,
            item,
            name,
            seen,
            sha256,
            roms,
            exact,
        } = &step.step
        else {
            unreachable!("a fill step");
        };
        let move_id = match (step.state.as_str(), step.move_id) {
            ("moving", Some(move_id)) => move_id,
            _ => {
                let steps = self.read(|c| steps_of(c, r.plan.id))?;
                if let Some(left) = steps.iter().find(|s| {
                    s.item == step.item
                        && matches!(s.step, RepairStep::Keep { .. })
                        && matches!(s.state.as_str(), "failed" | "skipped")
                }) {
                    return self.step_failed(
                        step,
                        "skipped",
                        format!(
                            "{name} in ingest folder {ingest_root} was left as it is, since {}",
                            left.error.clone().unwrap_or_default()
                        ),
                    );
                }
                let Some((folder, generation)) = self.write(|tx| {
                    let folder: Option<i64> = tx
                        .query_row(
                            "SELECT id FROM ingest_folders WHERE root = ?1",
                            [ingest_root],
                            |r| r.get(0),
                        )
                        .optional()?;
                    let Some(folder) = folder else {
                        return Ok(None);
                    };
                    tx.execute(
                        "UPDATE ingest_folders SET generation = generation + 1 WHERE id = ?1",
                        [folder],
                    )?;
                    let generation: i64 = tx.query_row(
                        "SELECT generation FROM ingest_folders WHERE id = ?1",
                        [folder],
                        |r| r.get(0),
                    )?;
                    Ok(Some((folder, generation as u64)))
                })?
                else {
                    return self.step_failed(
                        step,
                        "failed",
                        format!("ingest folder {ingest_root} is no longer registered"),
                    );
                };
                let content = &r.plan.content;
                let mv = Move {
                    folder,
                    plan: None,
                    item: Some(*item),
                    name,
                    seen: seen_of(seen),
                    sha256: sha256.clone(),
                    dest: Destination::Path {
                        root: content.root.clone(),
                        rel: content.path.clone(),
                        name: target.clone(),
                        generation,
                    },
                    repack: Some(
                        roms.iter()
                            .map(|rom| (rom.rom.clone(), rom.member.clone()))
                            .collect(),
                    ),
                    exact: exact.clone(),
                };
                let recorded = self.record_move(config, mv, &mut |tx, move_id| {
                    tx.execute(
                        "UPDATE repair_steps SET state = 'moving', move_id = ?2 WHERE id = ?1",
                        params![step.id, move_id],
                    )?;
                    Ok(())
                })?;
                match recorded {
                    Ok(move_id) => move_id,
                    Err(why) => return self.step_failed(step, "failed", why),
                }
            }
        };
        match self.drive_move(config, move_id)? {
            Ok(moved) => {
                self.step_done(step, moved.note)?;
                Ok(Ok(()))
            }
            Err(why) => {
                let state: String = self.read(|c| {
                    Ok(c.query_row(
                        "SELECT state FROM ingest_moves WHERE id = ?1",
                        [move_id],
                        |r| r.get(0),
                    )?)
                })?;
                match state.as_str() {
                    "failed" => self.step_failed(step, "failed", why),
                    _ => Ok(Err(why)),
                }
            }
        }
    }
}

/// The stamp each file had when Audit `audit` read its payloads, by path: those it read in full, or, when
/// `unreadable`, those with a payload it could not read.
fn audit_stamps(
    c: &Connection,
    audit: i64,
    unreadable: bool,
) -> Result<HashMap<String, FileStamp>> {
    let mut stmt = c.prepare(
        "SELECT path, dev, ino, size, mtime_ns, ctime_ns FROM audit_stamps WHERE audit_id = ?1 AND unreadable = ?2",
    )?;
    Ok(stmt
        .query_map(params![audit, unreadable], |r| {
            Ok((
                r.get::<_, String>(0)?,
                FileStamp {
                    dev: r.get::<_, i64>(1)? as u64,
                    ino: r.get::<_, i64>(2)? as u64,
                    size: r.get::<_, i64>(3)? as u64,
                    mtime_ns: r.get::<_, i64>(4)? as i128,
                    ctime_ns: r.get::<_, i64>(5)? as i128,
                },
            ))
        })?
        .collect::<rusqlite::Result<HashMap<_, _>>>()?)
}

/// Whether the file seen as `seen` is still as `stamp` recorded it when its payloads were read.
fn as_read(seen: &Seen, stamp: &FileStamp) -> bool {
    let id: title_select_api::build::FileIdentity = seen.identity.into();
    id.dev == stamp.dev
        && id.ino == stamp.ino
        && seen.file
            == FileStat {
                size: stamp.size,
                mtime_ns: stamp.mtime_ns,
            }
        && seen.ctime_ns == stamp.ctime_ns
}
