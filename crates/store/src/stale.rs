//! Stale Source Sets: the stat walk, a low-priority read-only Job that compares each file's stamp with the latest
//! Audit's by the rule an Incremental Audit uses to reuse a read, and records what looks changed.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::jobs::{check_no_active, finish_running, job_by_id};
use super::sets::{resolve, set_by_name};
use super::{Error, Result, Store, now};
use crate::config::Config;
use title_select_api::{FollowedRevision, Job, JobKind, JobState, StatWalk};
use title_select_engine::{FileStamp, StatEntry};

/// Why a restart Interrupted a stat walk, which is never queued again: the next one finds the same.
pub(crate) const WALK_INTERRUPTED: &str = "the service stopped while the check for changes was running; nothing in \
     the source set was changed. Check it again";
/// Why a stat walk stopped early: a Repair or Build wanted the Source Set or the job slot.
const WALK_YIELDED: &str = "stopped to let a repair or build run; nothing in the source set was changed. Check it \
     again once it ends";
/// How often a running stat walk asks whether a Repair or Build waits for it.
const YIELD_POLL: Duration = Duration::from_millis(500);

/// The stamps the latest Audit saw, by path relative to the Source Set.
struct Baseline {
    audit: i64,
    /// Every path the Audit saw, with its stamp where it recorded one.
    stamps: BTreeMap<String, Option<FileStamp>>,
    /// When the Audit started, in nanoseconds since the Unix epoch: a file it saw without a stamp looks changed if
    /// its modification or status change time is later.
    started_ns: i128,
}

/// What a stat walk counted.
#[derive(Debug, Default, PartialEq, Eq)]
struct Counts {
    changed: u64,
    added: u64,
    removed: u64,
}

impl Counts {
    fn of(baseline: &Baseline, walked: &[StatEntry]) -> Counts {
        let mut counts = Counts::default();
        let mut seen = BTreeSet::new();
        for entry in walked {
            let (rel, stamp) = match entry {
                StatEntry::File { rel, stamp } => (rel, Some(stamp.as_ref())),
                StatEntry::Unreadable { rel } => (rel, None),
            };
            seen.insert(rel.as_str());
            match baseline.stamps.get(rel) {
                None => counts.added += 1,
                Some(before) => {
                    let changed = match (before, stamp) {
                        (Some(before), Some(Some(now))) => before != now,
                        (None, Some(Some(now))) => {
                            now.mtime_ns > baseline.started_ns || now.ctime_ns > baseline.started_ns
                        }
                        // A file the audit stamped that can no longer be reached: a dangling symlink, or one that
                        // now leaves the root.
                        (Some(_), None) => true,
                        // Unreadable to the audit too, or a platform with no stamps.
                        (None, None) | (_, Some(None)) => false,
                    };
                    if changed {
                        counts.changed += 1;
                    }
                }
            }
        }
        counts.removed = baseline
            .stamps
            .keys()
            .filter(|path| !seen.contains(path.as_str()))
            .count() as u64;
        counts
    }
}

/// The Source Set's latest finished Audit, if any.
fn latest_audit(c: &Connection, set: i64) -> Result<Option<i64>> {
    Ok(c.query_row(
        "SELECT id FROM audits WHERE source_set_id = ?1 AND status = 'finished' ORDER BY finish_seq DESC LIMIT 1",
        [set],
        |r| r.get(0),
    )
    .optional()?)
}

fn baseline(c: &Connection, audit: i64) -> Result<Baseline> {
    let started: String = c.query_row(
        "SELECT started_at FROM audits WHERE id = ?1",
        [audit],
        |r| r.get(0),
    )?;
    let started_ns = started
        .parse::<jiff::Timestamp>()
        .map_err(|e| Error::Io(format!("audit {audit}: {e}")))?
        .as_nanosecond();
    let mut stamps = BTreeMap::new();
    let mut entries = c.prepare("SELECT DISTINCT path FROM audit_entries WHERE audit_id = ?1")?;
    for path in entries.query_map([audit], |r| r.get::<_, String>(0))? {
        stamps.insert(path?, None);
    }
    // A read's stamp wins over a claim's: audit_stamps last.
    for table in ["audit_claim_stamps", "audit_stamps"] {
        let mut rows = c.prepare(&format!(
            "SELECT path, dev, ino, size, mtime_ns, ctime_ns FROM {table} WHERE audit_id = ?1"
        ))?;
        for row in rows.query_map([audit], |r| {
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
        })? {
            let (path, stamp) = row?;
            stamps.insert(path, Some(stamp));
        }
    }
    Ok(Baseline {
        audit,
        stamps,
        started_ns,
    })
}

/// Whether a Repair or Build wants what a stat walk of Source Set `set` holds: one of the Source Set Queued or
/// Running, one of it that ran since `since`, or any Queued, which waits for a job slot.
fn yield_to(c: &Connection, set: i64, since: &str) -> Result<bool> {
    Ok(c.query_row(
        "SELECT EXISTS (SELECT 1 FROM jobs WHERE kind IN ('build', 'repair') AND (
             state = 'queued'
             OR (source_set_id = ?1 AND (state = 'running' OR finished_at >= ?2))))",
        params![set, since],
        |r| r.get(0),
    )?)
}

/// Records what the walk Job `job` of Source Set `set` found against `audit`.
fn record(tx: &Transaction, set: i64, audit: i64, job: i64, counts: &Counts) -> Result<()> {
    let at = now();
    let latest: Option<(i64, i64, i64, i64, i64)> = tx
        .query_row(
            "SELECT id, audit_id, changed, added, removed FROM stat_walks
             WHERE source_set_id = ?1 ORDER BY id DESC LIMIT 1",
            [set],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let same = |(_, a, c, ad, r): (i64, i64, i64, i64, i64)| {
        a == audit
            && c as u64 == counts.changed
            && ad as u64 == counts.added
            && r as u64 == counts.removed
    };
    if let Some(latest) = latest.filter(|l| same(*l)) {
        tx.execute(
            "UPDATE stat_walks SET checked_at = ?2, job_id = ?3 WHERE id = ?1",
            params![latest.0, at, job],
        )?;
        return Ok(());
    }
    tx.execute(
        "DELETE FROM stat_walks WHERE source_set_id = ?1 AND changed + added + removed = 0",
        [set],
    )?;
    tx.execute(
        "INSERT INTO stat_walks (source_set_id, audit_id, job_id, found_at, checked_at, changed, added, removed)
         VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6, ?7)",
        params![
            set,
            audit,
            job,
            at,
            counts.changed as i64,
            counts.added as i64,
            counts.removed as i64
        ],
    )?;
    Ok(())
}

/// Records, as the history of each Following Source Set, the newest revision of its DAT where it is newer than its
/// latest Audit's: what makes it Stale now. Called as a revision is imported, an Audit finishes or you follow.
pub(crate) fn note_followed_revisions(tx: &Transaction) -> Result<()> {
    tx.execute(
        "INSERT OR IGNORE INTO followed_revisions (source_set_id, dat_revision_id, found_at)
         SELECT s.id, n.id, ?1 FROM source_sets s
         JOIN audits a ON a.id = (SELECT id FROM audits WHERE source_set_id = s.id AND status = 'finished'
                                  ORDER BY finish_seq DESC LIMIT 1)
         JOIN dat_revisions n ON n.id = (SELECT id FROM dat_revisions WHERE dat_id = s.dat_id
                                         ORDER BY header_order DESC LIMIT 1)
         JOIN dat_revisions v ON v.id = a.dat_revision_id
         WHERE s.follow AND n.header_order > v.header_order AND s.unregistered_at IS NULL",
        [now()],
    )?;
    Ok(())
}

impl Store {
    /// The DAT Revisions that made the Source Set `name` Stale because it Follows its DAT, newest first.
    pub fn followed_revisions(&self, name: &str) -> Result<Vec<FollowedRevision>> {
        self.read(|c| {
            let set = set_by_name(c, name)?;
            let mut rows = c.prepare(
                "SELECT r.sha256, r.version, r.imported_at, f.found_at FROM followed_revisions f
                 JOIN dat_revisions r ON r.id = f.dat_revision_id
                 WHERE f.source_set_id = ?1 ORDER BY r.header_order DESC, r.id DESC",
            )?;
            let found = rows
                .query_map([set.id], |r| {
                    Ok(FollowedRevision {
                        source_set: name.to_owned(),
                        sha256: r.get(0)?,
                        version: r.get(1)?,
                        imported_at: r.get(2)?,
                        found_at: r.get(3)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(found)
        })
    }

    /// Queues a stat walk of the Source Set `name`, started by `actor`. Refused before its first Audit, which it
    /// compares with, and while one is Queued or Running. A Repair or Build of it never waits for one.
    pub fn enqueue_stat_walk(&self, name: &str, actor: &str) -> Result<Job> {
        self.write(|tx| {
            let set = set_by_name(tx, name)?;
            if latest_audit(tx, set.id)?.is_none() {
                return Err(Error::Refused(format!(
                    "source set {name} has never been audited; there is nothing to compare its files with"
                )));
            }
            check_no_active(tx, set.id, name, JobKind::StatWalk)?;
            tx.execute(
                "INSERT INTO jobs (state, kind, source_set_id, actor, readers, hashers, created_at)
                 VALUES ('queued', 'stat_walk', ?1, ?2, 1, 1, ?3)",
                params![set.id, actor, now()],
            )?;
            job_by_id(tx, tx.last_insert_rowid())
        })
    }

    /// Runs the Running stat walk Job `id`: stats every file of its Source Set, reads no payload and writes nothing
    /// there. It stops, recording nothing and ending Interrupted, once a Repair or Build wants the Source Set or the
    /// job slot. Returns the Job as it ends.
    pub fn run_stat_walk_job(&self, config: &Config, id: i64) -> Result<Job> {
        let job = self.job(id)?;
        if job.kind != JobKind::StatWalk {
            return Err(Error::Refused(format!("job {id} is not a stat walk")));
        }
        if job.state != JobState::Running {
            return Err(Error::Refused(format!(
                "job {id} is {}, not running",
                job.state.as_str()
            )));
        }
        let since = job.started_at.clone().unwrap_or_else(now);
        let outcome = self.stat_walk(config, &job.source_set, &since);
        self.write(|tx| {
            let (state, error) = match &outcome {
                Ok(Some((set, audit, counts))) => {
                    if yield_to(tx, *set, &since)? {
                        (JobState::Interrupted, Some(WALK_YIELDED.to_owned()))
                    } else {
                        record(tx, *set, *audit, id, counts)?;
                        (JobState::Succeeded, None)
                    }
                }
                Ok(None) => (JobState::Interrupted, Some(WALK_YIELDED.to_owned())),
                Err(err) => (JobState::Failed, Some(err.to_string())),
            };
            match finish_running(tx, id, state, error.as_deref()) {
                Err(Error::Io(_)) | Ok(()) => Ok(()),
                Err(err) => Err(err),
            }
        })?;
        self.job(id)
    }

    /// Walks Source Set `name`, returning its id, the Audit it compared with and what it counted; none if it
    /// yielded.
    fn stat_walk(
        &self,
        config: &Config,
        name: &str,
        since: &str,
    ) -> Result<Option<(i64, i64, Counts)>> {
        let (set, baseline) = self.read(|c| {
            let set = set_by_name(c, name)?;
            let audit = latest_audit(c, set.id)?.ok_or_else(|| {
                Error::Refused(format!("source set {name} has never been audited"))
            })?;
            Ok((set, baseline(c, audit)?))
        })?;
        let resolved = resolve(config, &set.record.root, &set.record.path)?;
        let polled = Cell::new(Instant::now());
        let stop = Cell::new(false);
        let keep_going = || {
            if !stop.get() && polled.get().elapsed() >= YIELD_POLL {
                polled.set(Instant::now());
                // An error asking counts as a reason to stop: the walk is only ever a hint.
                stop.set(self.read(|c| yield_to(c, set.id, since)).unwrap_or(true));
            }
            !stop.get()
        };
        let Some(walked) = title_select_engine::stat_walk(
            &resolved.path,
            Some(&resolved.canonical_root),
            &keep_going,
        )?
        else {
            return Ok(None);
        };
        Ok(Some((
            set.id,
            baseline.audit,
            Counts::of(&baseline, &walked),
        )))
    }

    /// The stat walks of the Source Set `name` that found files changed, added or removed, newest first.
    pub fn stat_walks(&self, name: &str) -> Result<Vec<StatWalk>> {
        self.read(|c| {
            let set = set_by_name(c, name)?;
            let mut rows = c.prepare(
                "SELECT id, audit_id, found_at, checked_at, changed, added, removed FROM stat_walks
                 WHERE source_set_id = ?1 AND changed + added + removed > 0 ORDER BY id DESC",
            )?;
            let walks = rows
                .query_map([set.id], |r| {
                    Ok(StatWalk {
                        id: r.get(0)?,
                        source_set: name.to_owned(),
                        audit: r.get(1)?,
                        found_at: r.get(2)?,
                        checked_at: r.get(3)?,
                        changed: r.get::<_, i64>(4)? as u64,
                        added: r.get::<_, i64>(5)? as u64,
                        removed: r.get::<_, i64>(6)? as u64,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(walks)
        })
    }

    /// Queues a stat walk of every audited Source Set that has none Queued or Running and was last checked, or else
    /// last audited, longer than `every` ago, so a restart doesn't check everything again. A check counts from when
    /// it finished, however long it waited to run. Returns the Jobs queued.
    pub fn enqueue_stat_walks(&self, actor: &str, every: Duration) -> Result<Vec<Job>> {
        let due: Vec<(String, String)> = self.read(|c| {
            let mut rows = c.prepare(
                "SELECT s.name, coalesce(
                     (SELECT max(j.finished_at) FROM jobs j WHERE j.source_set_id = s.id AND j.kind = 'stat_walk'),
                     (SELECT max(w.checked_at) FROM stat_walks w WHERE w.source_set_id = s.id),
                     (SELECT max(a.finished_at) FROM audits a WHERE a.source_set_id = s.id AND a.status = 'finished'))
                 FROM source_sets s
                 WHERE s.unregistered_at IS NULL
                 AND EXISTS (SELECT 1 FROM audits a WHERE a.source_set_id = s.id AND a.status = 'finished')
                 AND NOT EXISTS (SELECT 1 FROM jobs j WHERE j.source_set_id = s.id AND j.kind = 'stat_walk'
                                 AND j.state IN ('queued', 'running'))
                 ORDER BY s.name",
            )?;
            Ok(rows
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<Vec<_>, _>>()?)
        })?;
        let now = jiff::Timestamp::now();
        let every = jiff::SignedDuration::try_from(every).unwrap_or(jiff::SignedDuration::MAX);
        let mut queued = Vec::new();
        for (name, last) in due {
            let last = last
                .parse::<jiff::Timestamp>()
                .map_err(|e| Error::Io(format!("source set {name}: {e}")))?;
            if now.duration_since(last) < every {
                continue;
            }
            match self.enqueue_stat_walk(&name, actor) {
                Ok(job) => queued.push(job),
                Err(Error::JobActive { .. } | Error::Refused(_) | Error::NotFound(_)) => {}
                Err(err) => return Err(err),
            }
        }
        Ok(queued)
    }
}
