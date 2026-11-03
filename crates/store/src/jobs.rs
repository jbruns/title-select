//! Durable Jobs: Queued, Running, Succeeded, Failed or Interrupted, each state change one transaction (ADR 0003).

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};

use super::audits::{check_blob_present, choose_revision, delete_audits};
use super::sets::set_by_name;
use super::{Error, Result, Store, now};
use crate::config::Config;
use title_select_api::{Job, JobKind, JobState, ProfileVersionRef, Workers};
use title_select_engine::{AuditEvidence, Progress};

/// Why a restart Interrupted a Job.
const INTERRUPTED: &str = "the service stopped while the job was running";
/// Why a restart Interrupted a Build Job, which is never queued again: its plan may no longer hold.
const BUILD_INTERRUPTED: &str = "the service stopped while the build was running; the files it applied stay, \
     and a new plan builds the rest";
/// Why a restart Interrupted a Check Job, which is never queued again: nothing depends on it.
const CHECK_INTERRUPTED: &str = "the service stopped while the check was running; nothing in the folder was changed. Check it again";
/// Why a restart Interrupted an Ingest Scan Job, which is never queued again: the next scan finds the same.
const SCAN_INTERRUPTED: &str = "the service stopped while the scan was running; nothing in the folder was changed. Scan it again";
/// Why a restart Failed a Job that was already queued again once.
const INTERRUPTED_AGAIN: &str =
    "the service stopped while the job was running, after it had already been queued again once";

/// What starting the Service did to a Job it found Running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interruption {
    /// The Job as it now is: Interrupted, or Failed when it was already the Job queued again.
    pub job: Job,
    /// The fresh Job queued in its place, if any.
    pub requeued: Option<Job>,
}

/// A Job's row, with the ids the store needs to run it.
pub(super) struct JobRow {
    pub id: i64,
    pub source_set_id: i64,
    pub dat_revision_id: i64,
    pub workers: Workers,
    pub evidence: AuditEvidence,
    pub requeued_from: Option<i64>,
}

pub(super) const JOB_QUERY: &str =
    "SELECT j.id, j.state, coalesce(s.name, ''), coalesce(r.sha256, ''), j.actor, j.attempt, j.requeued_from,
            j.audit_id, j.error, j.readers, j.hashers, j.created_at, j.started_at, j.finished_at, j.version,
            j.kind, p.name, v.version, j.selection_id, j.library_folder_id, j.plan_id, i.root, j.evidence,
            j.repair_plan_id, j.cancel_requested_at, ad.provider || '/' || ad.name
     FROM jobs j
     LEFT JOIN source_sets s ON s.id = j.source_set_id
     LEFT JOIN dat_revisions r ON r.id = j.dat_revision_id
     LEFT JOIN ingest_folders i ON i.id = j.ingest_folder_id
     LEFT JOIN profile_versions v ON v.id = j.profile_version_id
     LEFT JOIN profiles p ON p.id = v.profile_id
     LEFT JOIN additions_sets a ON a.id = j.additions_set_id
     LEFT JOIN dats ad ON ad.id = a.dat_id";

pub(super) fn job_from_row(row: &Row) -> rusqlite::Result<Job> {
    let state: String = row.get(1)?;
    let state = JobState::parse(&state).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            1,
            rusqlite::types::Type::Text,
            format!("unknown job state {state}").into(),
        )
    })?;
    let kind: String = row.get(15)?;
    let kind = JobKind::parse(&kind).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            15,
            rusqlite::types::Type::Text,
            format!("unknown job kind {kind}").into(),
        )
    })?;
    let profile = match row.get::<_, Option<String>>(16)? {
        Some(name) => Some(ProfileVersionRef {
            name,
            version: row.get(17)?,
        }),
        None => None,
    };
    Ok(Job {
        id: row.get(0)?,
        kind,
        profile,
        selection: row.get(18)?,
        library_folder: row.get(19)?,
        plan: row.get(20)?,
        ingest_folder: row.get(21)?,
        evidence: match row.get::<_, Option<String>>(22)? {
            Some(text) => Some(AuditEvidence::parse(&text).ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    22,
                    rusqlite::types::Type::Text,
                    format!("unknown audit evidence {text}").into(),
                )
            })?),
            None => None,
        },
        repair: row.get(23)?,
        cancel_requested_at: row.get(24)?,
        additions_set: row.get(25)?,
        state,
        source_set: row.get(2)?,
        dat_revision: row.get(3)?,
        actor: row.get(4)?,
        attempt: row.get(5)?,
        requeued_from: row.get(6)?,
        audit: row.get(7)?,
        error: row.get(8)?,
        workers: Workers {
            readers: row.get::<_, i64>(9)? as usize,
            hashers: row.get::<_, i64>(10)? as usize,
        },
        created_at: row.get(11)?,
        started_at: row.get(12)?,
        finished_at: row.get(13)?,
        version: row.get::<_, i64>(14)? as u64,
    })
}

pub(crate) fn job_by_id(c: &Connection, id: i64) -> Result<Job> {
    c.query_row(&format!("{JOB_QUERY} WHERE j.id = ?1"), [id], job_from_row)
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("no job {id}")))
}

/// Moves the Running Job `id` to `to`, failing if it is not Running.
pub(crate) fn finish_running(
    tx: &Transaction,
    id: i64,
    to: JobState,
    error: Option<&str>,
) -> Result<()> {
    let updated = tx.execute(
        "UPDATE jobs SET state = ?2, error = ?3, finished_at = ?4, version = version + 1
         WHERE id = ?1 AND state = 'running'",
        params![id, to.as_str(), error, now()],
    )?;
    if updated != 1 {
        return Err(Error::Io(format!("job {id} is no longer running")));
    }
    release_holds(tx, id)
}

/// What holds a Build Job's Selection and Audit from pruning, from its approval until it ends.
pub(super) fn build_hold(job: i64) -> String {
    format!("build-job:{job}")
}

/// Releases the holds of Job `id`, which has ended. A Build that succeeded has made its folder cite its inputs.
fn release_holds(tx: &Transaction, id: i64) -> Result<()> {
    let hold = build_hold(id);
    tx.execute(
        "DELETE FROM selection_citations WHERE cited_by = ?1",
        [&hold],
    )?;
    tx.execute("DELETE FROM audit_citations WHERE cited_by = ?1", [&hold])?;
    Ok(())
}

/// Marks the Running Job `id` Succeeded, having recorded `audit_id`.
pub(super) fn succeed(tx: &Transaction, id: i64, audit_id: i64) -> Result<()> {
    finish_running(tx, id, JobState::Succeeded, None)?;
    tx.execute(
        "UPDATE jobs SET audit_id = ?2 WHERE id = ?1",
        params![id, audit_id],
    )?;
    Ok(())
}

/// Marks the Running Job `id` Failed with `error`, and discards its running Audit, which releases its DAT Revision.
pub(super) fn fail(tx: &Transaction, id: i64, error: &str) -> Result<()> {
    finish_running(tx, id, JobState::Failed, Some(error))?;
    delete_audits(tx, "job_id = ?1 AND status = 'running'", [id])?;
    Ok(())
}

/// Refuses a Job of `kind` while the Source Set has one Queued or Running.
pub(crate) fn check_no_active(
    c: &Connection,
    set_id: i64,
    set_name: &str,
    kind: JobKind,
) -> Result<()> {
    let active: Option<i64> = c
        .query_row(
            "SELECT id FROM jobs WHERE source_set_id = ?1 AND kind = ?2 AND state IN ('queued', 'running')",
            params![set_id, kind.as_str()],
            |r| r.get(0),
        )
        .optional()?;
    match active {
        Some(job) => Err(Error::JobActive {
            source_set: set_name.to_owned(),
            job,
            kind,
        }),
        None => Ok(()),
    }
}

impl Store {
    /// Queues an Audit of the Source Set `name`, started by `actor`, pinned now to the DAT Revision `revision` (a
    /// SHA-256 prefix) or by default the latest Audit's, else the newest imported revision of its DAT, getting
    /// checksums as `evidence` says. Refused, naming the Job, while the Source Set already has an Audit Job Queued or
    /// Running.
    pub fn enqueue_audit(
        &self,
        name: &str,
        revision: Option<&str>,
        actor: &str,
        workers: Workers,
        evidence: AuditEvidence,
    ) -> Result<Job> {
        if workers.readers == 0 || workers.hashers == 0 {
            return Err(Error::Refused(
                "a job needs at least one payload reader and one hash worker".into(),
            ));
        }
        self.write(|tx| {
            let set = set_by_name(tx, name)?;
            check_no_active(tx, set.id, &set.record.name, JobKind::Audit)?;
            crate::repairs::check_no_repair(tx, set.id, &set.record.name)?;
            let (revision_id, chosen) = choose_revision(tx, &set, revision)?;
            check_blob_present(&chosen)?;
            tx.execute(
                "INSERT INTO jobs (state, source_set_id, dat_revision_id, actor, readers, hashers, created_at, evidence)
                 VALUES ('queued', ?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    set.id,
                    revision_id,
                    actor,
                    workers.readers as i64,
                    workers.hashers as i64,
                    now(),
                    evidence.as_str()
                ],
            )?;
            job_by_id(tx, tx.last_insert_rowid())
        })
    }

    /// Moves the oldest Queued Job to Running and returns it, or returns `None` when none is Queued.
    pub fn claim_job(&self) -> Result<Option<Job>> {
        self.write(|tx| {
            let id: Option<i64> = tx
                .query_row(
                    "SELECT id FROM jobs WHERE state = 'queued' ORDER BY id LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(id) = id else { return Ok(None) };
            tx.execute(
                "UPDATE jobs SET state = 'running', started_at = ?2, version = version + 1 WHERE id = ?1",
                params![id, now()],
            )?;
            job_by_id(tx, id).map(Some)
        })
    }

    /// Runs the Running Job `id`'s Audit to completion, calling `progress` as files are read. The Audit and the
    /// Job's Succeeded state are recorded in one transaction; an Audit that fails is discarded and the Job Failed in
    /// one transaction. Returns the Job as it ends. An error means the Job's outcome could not be recorded, or that
    /// pruning or retention failed after the Job Succeeded.
    pub fn run_audit_job(
        &self,
        config: &Config,
        id: i64,
        progress: Option<&(dyn Fn(Progress) + Sync)>,
    ) -> Result<Job> {
        let row = self.read(|c| {
            c.query_row(
                "SELECT id, source_set_id, dat_revision_id, readers, hashers, state, kind, evidence, requeued_from
                 FROM jobs WHERE id = ?1",
                [id],
                |r| {
                    Ok((
                        JobRow {
                            id: r.get(0)?,
                            source_set_id: r.get(1)?,
                            dat_revision_id: r.get(2)?,
                            workers: Workers {
                                readers: r.get::<_, i64>(3)? as usize,
                                hashers: r.get::<_, i64>(4)? as usize,
                            },
                            evidence: r
                                .get::<_, Option<String>>(7)?
                                .as_deref()
                                .and_then(AuditEvidence::parse)
                                .unwrap_or(AuditEvidence::Full),
                            requeued_from: r.get(8)?,
                        },
                        r.get::<_, String>(5)?,
                        r.get::<_, String>(6)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("no job {id}")))
        })?;
        let (row, state, kind) = row;
        if kind != JobKind::Audit.as_str() {
            return Err(Error::Refused(format!("job {id} is not an audit")));
        }
        if state != JobState::Running.as_str() {
            return Err(Error::Refused(format!("job {id} is {state}, not running")));
        }
        let outcome = self
            .begin_job_audit(config, &row)
            .and_then(|running| self.complete_audit(config, running, progress));
        if let Err(err) = &outcome {
            // A failure before the Audit's row existed leaves the Job Running; one after has Failed it already.
            let message = err.to_string();
            let _ = self.write(|tx| match fail(tx, id, &message) {
                Err(Error::Io(_)) => Ok(()),
                other => other,
            });
        }
        let job = self.job(id)?;
        match outcome {
            Ok(_) => Ok(job),
            Err(err) if job.state == JobState::Succeeded => Err(err),
            Err(_) if job.state.is_final() => Ok(job),
            Err(err) => Err(err),
        }
    }

    /// Marks the Running Job `id` Failed with `error` and discards its running Audit, if any, in one transaction.
    /// Does nothing to a Job that is not Running.
    pub fn fail_job(&self, id: i64, error: &str) -> Result<Job> {
        self.write(|tx| match fail(tx, id, error) {
            Err(Error::Io(_)) | Ok(()) => job_by_id(tx, id),
            Err(err) => Err(err),
        })
    }

    /// Run when the Service starts: every Job still Running was cut off by a stop. Its Audit was already discarded
    /// when the store opened. A first attempt becomes Interrupted and a fresh Job is queued for it with `workers`; a
    /// Job that was itself queued again becomes Failed. A Build Job becomes Interrupted and is not queued again, since
    /// its plan may no longer hold; [`Store::recover_builds`] then cleans up its folder. A Check Job becomes
    /// Interrupted too. Each Job's change is one transaction.
    pub fn recover_jobs(&self, workers: Workers) -> Result<Vec<Interruption>> {
        let running: Vec<(i64, u32, String)> = self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT id, attempt, kind FROM jobs WHERE state = 'running' ORDER BY id",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })?;
        running
            .into_iter()
            .map(|(id, attempt, kind)| {
                self.write(|tx| {
                    if kind == JobKind::Build.as_str() {
                        finish_running(tx, id, JobState::Interrupted, Some(BUILD_INTERRUPTED))?;
                        tx.execute(
                            "UPDATE builds SET state = 'interrupted', finished_at = ?2
                             WHERE job_id = ?1 AND state = 'running'",
                            params![id, now()],
                        )?;
                        return Ok(Interruption {
                            job: job_by_id(tx, id)?,
                            requeued: None,
                        });
                    }
                    if kind == JobKind::Check.as_str() || kind == JobKind::IngestScan.as_str() {
                        let why = if kind == JobKind::Check.as_str() {
                            CHECK_INTERRUPTED
                        } else {
                            SCAN_INTERRUPTED
                        };
                        finish_running(tx, id, JobState::Interrupted, Some(why))?;
                        return Ok(Interruption {
                            job: job_by_id(tx, id)?,
                            requeued: None,
                        });
                    }
                    if kind == JobKind::Repair.as_str() {
                        // Every step is a durable intent, so the repair always runs on from where it stopped.
                        finish_running(tx, id, JobState::Interrupted, Some(INTERRUPTED))?;
                        let plan: i64 = tx.query_row(
                            "SELECT repair_plan_id FROM jobs WHERE id = ?1",
                            [id],
                            |r| r.get(0),
                        )?;
                        let requeued = crate::repairs::queue_repair_job(tx, plan, Some(id))?;
                        return Ok(Interruption {
                            job: job_by_id(tx, id)?,
                            requeued: Some(job_by_id(tx, requeued)?),
                        });
                    }
                    if kind == JobKind::Additions.as_str() {
                        // Every addition records its step before it is taken; another Job settles what this left.
                        finish_running(tx, id, JobState::Interrupted, Some(INTERRUPTED))?;
                        let requeued = crate::additions::queue_additions_jobs(tx)?;
                        return Ok(Interruption {
                            job: job_by_id(tx, id)?,
                            requeued: match requeued.first() {
                                Some(job) => Some(job_by_id(tx, *job)?),
                                None => None,
                            },
                        });
                    }
                    if attempt > 1 {
                        finish_running(tx, id, JobState::Failed, Some(INTERRUPTED_AGAIN))?;
                        return Ok(Interruption {
                            job: job_by_id(tx, id)?,
                            requeued: None,
                        });
                    }
                    finish_running(tx, id, JobState::Interrupted, Some(INTERRUPTED))?;
                    tx.execute(
                        "INSERT INTO jobs (state, source_set_id, dat_revision_id, actor, attempt, requeued_from,
                                           readers, hashers, created_at, kind, profile_version_id, evidence)
                         SELECT 'queued', source_set_id, dat_revision_id, actor, attempt + 1, id, ?2, ?3, ?4,
                                kind, profile_version_id, evidence
                         FROM jobs WHERE id = ?1",
                        params![id, workers.readers as i64, workers.hashers as i64, now()],
                    )?;
                    let requeued = job_by_id(tx, tx.last_insert_rowid())?;
                    Ok(Interruption {
                        job: job_by_id(tx, id)?,
                        requeued: Some(requeued),
                    })
                })
            })
            .collect()
    }

    pub fn job(&self, id: i64) -> Result<Job> {
        self.read(|c| job_by_id(c, id))
    }

    /// Every Queued or Running Job and the `recent` latest others, newest first, read in one query.
    pub fn job_snapshot(&self, recent: usize) -> Result<Vec<Job>> {
        self.read(|c| {
            let mut stmt = c.prepare(&format!(
                "{JOB_QUERY} WHERE j.state IN ('queued', 'running')
                    OR j.id IN (SELECT id FROM jobs WHERE state NOT IN ('queued', 'running') ORDER BY id DESC LIMIT ?1)
                 ORDER BY j.id DESC"
            ))?;
            let rows = stmt.query_map([recent as i64], job_from_row)?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }

    /// The latest `limit` Jobs, newest first.
    pub fn jobs(&self, limit: usize) -> Result<Vec<Job>> {
        self.read(|c| {
            let mut stmt = c.prepare(&format!("{JOB_QUERY} ORDER BY j.id DESC LIMIT ?1"))?;
            let rows = stmt.query_map([limit as i64], job_from_row)?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }
}
