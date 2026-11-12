//! Stored Audits: run against a registered Source Set, kept as rows, pruned to summaries.

use std::collections::{BTreeMap, HashSet};
use std::sync::{Mutex, PoisonError};

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params, params_from_iter};
use sha2::{Digest, Sha256};

use super::dats::revision_by_id;
use super::dats::{DatRevisionRecord, REVISION_COLUMNS, revision_from_row};
use super::jobs::JobRow;
use super::repairs::open_repair;
use super::sets::{Resolved, SourceSetRow, resolve, set_by_id, set_by_name};
use super::{Error, Result, Store, now};
use crate::config::Config;
use title_select_api::{AuditSummary, EntryRow, GameRow, SourceSetSummary};
use title_select_engine::payload::Checksums;
use title_select_engine::{
    ALGORITHMS, AuditEvidence, DatRevision, Entry, Evidence, EvidenceLevel, FileStamp, Finding,
    Game, GameStatus, GameWarning, Header, HeaderRule, NamedHeaderRule, ONE_WORKER_EACH, Options,
    PayloadRead, PayloadReads, Progress, Report, Reuse, RomRef, SCHEMA, Summary, Workers,
};

/// An Audit the store has recorded, with the report generated from its rows.
#[derive(Debug)]
pub struct StoredAudit {
    pub id: i64,
    pub report: Report,
}

/// How an Audit runs: for which Job, with how many workers, and how it gets its checksums.
struct How {
    job: Option<i64>,
    workers: Workers,
    evidence: AuditEvidence,
    resumes: Option<i64>,
}

/// An Audit whose row is inserted as running, before its scan.
pub(super) struct RunningAudit {
    id: i64,
    set_id: i64,
    resolved: Resolved,
    revision: DatRevisionRecord,
    /// The Job running it, if any; it changes state with the Audit.
    job: Option<i64>,
    workers: Workers,
    evidence: AuditEvidence,
    /// For a Job queued again after a restart interrupted it, the interrupted Job, whose reads a full Audit resumes
    /// from.
    resumes: Option<i64>,
}

/// The payload reads recorded for a Source Set, as an Audit of it reuses and records them.
struct StoredReads<'s> {
    store: &'s Store,
    set_id: i64,
    /// The running Audit, whose own stamps are recorded with each read it makes or reuses.
    audit: i64,
    /// The Job recording its reads, if any.
    job: Option<i64>,
    /// Which recorded reads may be reused: any, only those of one Job, or none.
    reuse: Option<Option<i64>>,
    /// The first error reading or recording, which fails the Audit once its scan ends.
    error: Mutex<Option<Error>>,
}

impl<'s> StoredReads<'s> {
    /// A full Audit reuses nothing, except, when it resumes an interrupted Job, what that Job read.
    fn new(store: &'s Store, running: &RunningAudit) -> StoredReads<'s> {
        let reuse = match (running.evidence, running.resumes) {
            (AuditEvidence::Full, None) => None,
            (AuditEvidence::Full, Some(job)) => Some(Some(job)),
            (AuditEvidence::Quick | AuditEvidence::Incremental, _) => Some(None),
        };
        StoredReads {
            store,
            set_id: running.set_id,
            audit: running.id,
            job: running.job,
            reuse,
            error: Mutex::new(None),
        }
    }

    /// Records `stamp` as what this Audit read of `path`, in `tx`.
    fn stamp(
        &self,
        tx: &Transaction,
        path: &str,
        unreadable: bool,
        stamp: &FileStamp,
    ) -> Result<()> {
        tx.execute(
            "DELETE FROM audit_stamps WHERE audit_id = ?1 AND path = ?2",
            params![self.audit, path],
        )?;
        tx.execute(
            "INSERT INTO audit_stamps (audit_id, path, unreadable, dev, ino, size, mtime_ns, ctime_ns)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                self.audit,
                path,
                unreadable,
                stamp.dev as i64,
                stamp.ino as i64,
                stamp.size as i64,
                stamp.mtime_ns as i64,
                stamp.ctime_ns as i64,
            ],
        )?;
        Ok(())
    }

    fn keep(&self, err: Error) {
        self.error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert(err);
    }
}

impl PayloadReads for StoredReads<'_> {
    fn prior(&self, path: &str) -> Option<PayloadRead> {
        let only_job = self.reuse?;
        let found = self.store.read(|c| {
            let row = c
                .query_row(
                    "SELECT dev, ino, size, mtime_ns, ctime_ns, read_at, members, header_rule FROM payload_reads
                     WHERE source_set_id = ?1 AND path = ?2 AND (?3 IS NULL OR job_id = ?3)",
                    params![self.set_id, path, only_job],
                    |r| {
                        Ok((
                            FileStamp {
                                dev: r.get::<_, i64>(0)? as u64,
                                ino: r.get::<_, i64>(1)? as u64,
                                size: r.get::<_, i64>(2)? as u64,
                                mtime_ns: r.get::<_, i64>(3)? as i128,
                                ctime_ns: r.get::<_, i64>(4)? as i128,
                            },
                            r.get::<_, String>(5)?,
                            r.get::<_, String>(6)?,
                            r.get::<_, Option<String>>(7)?,
                        ))
                    },
                )
                .optional()?;
            let Some((stamp, read_at, members, header_rule)) = row else {
                return Ok(None);
            };
            let members: Vec<(Option<String>, Checksums)> =
                serde_json::from_str(&members).map_err(corrupt)?;
            Ok(Some(PayloadRead {
                stamp,
                read_at: timestamp(&read_at)?,
                members,
                header_rule: header_rule
                    .map(|r| HeaderRule::parse(&r).ok_or_else(|| corrupt(&r)))
                    .transpose()?,
            }))
        });
        found.unwrap_or_else(|err| {
            self.keep(err);
            None
        })
    }

    fn record(&self, path: &str, read: &PayloadRead) {
        let recorded = serde_json::to_string(&read.members)
            .map_err(corrupt)
            .and_then(|members| {
                self.store.write(|tx| {
                    tx.execute(
                        "INSERT OR REPLACE INTO payload_reads
                             (source_set_id, path, dev, ino, size, mtime_ns, ctime_ns, read_at, job_id, members,
                              header_rule)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                        params![
                            self.set_id,
                            path,
                            read.stamp.dev as i64,
                            read.stamp.ino as i64,
                            read.stamp.size as i64,
                            read.stamp.mtime_ns as i64,
                            read.stamp.ctime_ns as i64,
                            read.read_at.to_string(),
                            self.job,
                            members,
                            read.header_rule.map(HeaderRule::as_str)
                        ],
                    )?;
                    self.stamp(tx, path, false, &read.stamp)
                })
            });
        if let Err(err) = recorded {
            self.keep(err);
        }
    }

    fn record_unreadable(&self, path: &str, stamp: &FileStamp) {
        if let Err(err) = self.store.write(|tx| self.stamp(tx, path, true, stamp)) {
            self.keep(err);
        }
    }

    fn reused(&self, path: &str, stamp: &FileStamp) {
        if let Err(err) = self.store.write(|tx| self.stamp(tx, path, false, stamp)) {
            self.keep(err);
        }
    }

    fn claimed(&self, path: &str, stamp: &FileStamp) {
        let recorded = self.store.write(|tx| {
            tx.execute(
                "INSERT OR REPLACE INTO audit_claim_stamps (audit_id, path, dev, ino, size, mtime_ns, ctime_ns)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    self.audit,
                    path,
                    stamp.dev as i64,
                    stamp.ino as i64,
                    stamp.size as i64,
                    stamp.mtime_ns as i64,
                    stamp.ctime_ns as i64,
                ],
            )?;
            Ok(())
        });
        if let Err(err) = recorded {
            self.keep(err);
        }
    }
}

const SUMMARY_QUERY: &str = "SELECT a.id, s.name, a.started_at, a.finished_at, r.sha256, r.version,
            a.pruned_at IS NOT NULL, a.matched, a.misnamed, a.conflicting, a.mismatched, a.unmatched, a.unreadable,
            a.complete, a.incomplete, a.missing, a.evidence,
            CASE WHEN a.pruned_at IS NULL THEN
                (SELECT COUNT(*) FROM audit_entries e WHERE e.audit_id = a.id AND e.evidence_level = 'claim')
            END,
            a.header_missing, a.header_rule
     FROM audits a
     JOIN dat_revisions r ON r.id = a.dat_revision_id
     JOIN source_sets s ON s.id = a.source_set_id";

fn summary_from_row(r: &Row) -> rusqlite::Result<AuditSummary> {
    let (findings, games) = counts_from(r, 7)?;
    Ok(AuditSummary {
        id: r.get(0)?,
        source_set: r.get(1)?,
        started_at: r.get(2)?,
        finished_at: r.get(3)?,
        dat_revision_sha256: r.get(4)?,
        dat_revision_version: r.get(5)?,
        pruned: r.get(6)?,
        evidence: evidence_from(r, 16)?,
        claimed: r.get::<_, Option<i64>>(17)?.map(|n| n as usize),
        findings,
        games,
        warnings: warnings_from(r, 18)?,
        header_rule: r
            .get::<_, Option<String>>(18 + GameWarning::ALL.len())?
            .as_deref()
            .map(NamedHeaderRule::new),
    })
}

/// The Game Warning counts of `audits`, from the column `first` on, in [`GameWarning::ALL`] order; those of none
/// are left out.
fn warnings_from(row: &Row, first: usize) -> rusqlite::Result<BTreeMap<GameWarning, usize>> {
    let mut warnings = BTreeMap::new();
    for (i, w) in GameWarning::ALL.iter().enumerate() {
        let n = row.get::<_, i64>(first + i)? as usize;
        if n > 0 {
            warnings.insert(*w, n);
        }
    }
    Ok(warnings)
}

/// The Game Warning count columns of `audits`, each named as the report names its warning.
fn warning_columns(prefix: &str) -> String {
    GameWarning::ALL
        .iter()
        .map(|w| format!("{prefix}{}", w.as_str()))
        .collect::<Vec<_>>()
        .join(", ")
}

pub(super) const ENTRY_COLUMNS: &str = "path, member, finding, roms, named_game, named_rom, evidence_level, algorithms, reason, read_at, reused, header_bytes";

/// The `seq` column's index, when selected after [`ENTRY_COLUMNS`].
const ENTRY_SEQ: usize = 12;

const GAME_COLUMNS: &str =
    "name, status, missing_roms, claimed_roms, warnings, available_elsewhere";

/// The `seq` column's index, when selected after [`GAME_COLUMNS`].
const GAME_SEQ: usize = 6;

pub(super) fn evidence_from(row: &Row, i: usize) -> rusqlite::Result<AuditEvidence> {
    let text: String = row.get(i)?;
    AuditEvidence::parse(&text).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            i,
            rusqlite::types::Type::Text,
            format!("unknown audit evidence {text}").into(),
        )
    })
}

/// The count columns of `audits`, Findings then Game Statuses, named as the report names them.
fn count_columns(prefix: &str) -> String {
    Finding::ALL
        .iter()
        .map(|f| f.as_str())
        .chain(GameStatus::ALL.iter().map(|g| g.as_str()))
        .map(|c| format!("{prefix}{c}"))
        .collect::<Vec<_>>()
        .join(", ")
}

type Counts = (BTreeMap<Finding, usize>, BTreeMap<GameStatus, usize>);

fn counts_from(row: &Row, first: usize) -> rusqlite::Result<Counts> {
    let mut findings = BTreeMap::new();
    for (i, f) in Finding::ALL.iter().enumerate() {
        findings.insert(*f, row.get::<_, i64>(first + i)? as usize);
    }
    let mut games = BTreeMap::new();
    for (i, g) in GameStatus::ALL.iter().enumerate() {
        games.insert(
            *g,
            row.get::<_, i64>(first + Finding::ALL.len() + i)? as usize,
        );
    }
    Ok((findings, games))
}

fn corrupt(what: impl std::fmt::Display) -> Error {
    Error::Io(format!("unexpected stored value: {what}"))
}

fn timestamp(s: &str) -> Result<jiff::Timestamp> {
    s.parse().map_err(|_| corrupt(s))
}

/// The DAT Revision an Audit of `set` uses: the one whose SHA-256 starts with `prefix`; else the latest
/// Audit's; else the newest revision of the bound DAT.
pub(super) fn choose_revision(
    c: &Connection,
    set: &SourceSetRow,
    prefix: Option<&str>,
) -> Result<(i64, DatRevisionRecord)> {
    let base =
        format!("SELECT {REVISION_COLUMNS} FROM dat_revisions r JOIN dats d ON d.id = r.dat_id");
    let dat = &set.record.dat;
    if let Some(prefix) = prefix {
        let prefix = prefix.to_ascii_lowercase();
        if prefix.is_empty() || !prefix.chars().all(|ch| ch.is_ascii_hexdigit()) {
            return Err(Error::Refused(format!(
                "{prefix:?}: a DAT revision is chosen by a prefix of its SHA-256"
            )));
        }
        let mut stmt = c.prepare(&format!(
            "{base} WHERE r.dat_id = ?1 AND substr(r.sha256, 1, length(?2)) = ?2 ORDER BY r.id"
        ))?;
        let mut found = stmt
            .query_map(params![set.dat_id, prefix], revision_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        return match found.len() {
            1 => Ok(found.remove(0)),
            0 => Err(Error::Refused(format!(
                "no DAT revision of {dat} has a SHA-256 starting {prefix}"
            ))),
            _ => Err(Error::Refused(format!(
                "{prefix} matches more than one DAT revision of {dat}"
            ))),
        };
    }
    let latest_audit = c
        .query_row(
            &format!(
                "{base} WHERE r.id = (SELECT dat_revision_id FROM audits
                                      WHERE source_set_id = ?1 AND status = 'finished'
                                      ORDER BY finish_seq DESC LIMIT 1)"
            ),
            [set.id],
            revision_from_row,
        )
        .optional()?;
    if let Some(found) = latest_audit {
        return Ok(found);
    }
    c.query_row(
        &format!("{base} WHERE r.dat_id = ?1 ORDER BY r.header_order DESC LIMIT 1"),
        [set.dat_id],
        revision_from_row,
    )
    .optional()?
    .ok_or_else(|| Error::Refused(format!("no DAT revision of {dat} has been imported")))
}

/// Finishes the running Audit `id`: its counts and times, its Entries and its Game Statuses. Forgets the payload
/// reads of files in its Source Set that it did not find.
fn finish_audit(tx: &Transaction, id: i64, report: &Report) -> Result<()> {
    let watch = crate::observe::StaleWatch::start(tx)?;
    let mut values = vec![
        Value::Text(report.source_set.clone()),
        Value::Text(report.started_at.to_string()),
        Value::Text(report.finished_at.to_string()),
    ];
    for f in Finding::ALL {
        values.push(Value::Integer(report.summary.findings[&f] as i64));
    }
    for g in GameStatus::ALL {
        values.push(Value::Integer(report.summary.games[&g] as i64));
    }
    for w in GameWarning::ALL {
        let n = report.summary.warnings.get(&w).copied().unwrap_or(0);
        values.push(Value::Integer(n as i64));
    }
    values.push(match &report.dat_revision.header_rule {
        Some(rule) => Value::Text(rule.name.clone()),
        None => Value::Null,
    });
    let counts: Vec<String> = Finding::ALL
        .iter()
        .map(|f| f.as_str())
        .chain(GameStatus::ALL.iter().map(|g| g.as_str()))
        .chain(GameWarning::ALL.iter().map(|w| w.as_str()))
        .chain(["header_rule"])
        .map(|c| format!("{c} = ?"))
        .collect();
    values.push(Value::Integer(id));
    let updated = tx.execute(
        &format!(
            "UPDATE audits SET status = 'finished',
                               finish_seq = (SELECT coalesce(max(finish_seq), 0) + 1 FROM audits),
                               source_set_path = ?, started_at = ?, finished_at = ?, {}
             WHERE id = ? AND status = 'running'",
            counts.join(", ")
        ),
        params_from_iter(values),
    )?;
    if updated != 1 {
        return Err(Error::Io(format!("audit {id} is no longer running")));
    }
    crate::stale::note_followed_revisions(tx)?;
    watch.finish(tx)?;
    // The Audit now holds its revision, so a restored blob needs no other hold.
    tx.execute(
        "UPDATE dat_revisions SET restored_at = NULL
         WHERE id = (SELECT dat_revision_id FROM audits WHERE id = ?1)",
        [id],
    )?;

    let found: HashSet<&str> = report.entries.iter().map(|e| e.path.as_str()).collect();
    let set_id: i64 = tx.query_row(
        "SELECT source_set_id FROM audits WHERE id = ?1",
        [id],
        |r| r.get(0),
    )?;
    let recorded: Vec<String> = tx
        .prepare("SELECT path FROM payload_reads WHERE source_set_id = ?1")?
        .query_map([set_id], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    for path in recorded.iter().filter(|p| !found.contains(p.as_str())) {
        tx.execute(
            "DELETE FROM payload_reads WHERE source_set_id = ?1 AND path = ?2",
            params![set_id, path],
        )?;
    }

    let mut entry = tx.prepare(
        "INSERT INTO audit_entries (audit_id, seq, path, member, finding, roms, named_game, named_rom,
                                    evidence_level, algorithms, reason, read_at, reused, header_bytes)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
    )?;
    for (seq, e) in report.entries.iter().enumerate() {
        let roms = serde_json::to_string(&e.roms).map_err(corrupt)?;
        entry.execute(params![
            id,
            seq as i64,
            e.path,
            e.member,
            e.finding.as_str(),
            roms,
            e.named_rom.as_ref().map(|r| &r.game),
            e.named_rom.as_ref().map(|r| &r.name),
            e.evidence.as_ref().map(|ev| ev.level.as_str()),
            e.evidence.as_ref().map(|ev| ev.algorithms.join(",")),
            e.reason,
            e.evidence
                .as_ref()
                .and_then(|ev| ev.read_at)
                .map(|at| at.to_string()),
            e.evidence.as_ref().is_some_and(|ev| ev.reused.is_some()),
            e.evidence
                .as_ref()
                .and_then(|ev| ev.header)
                .map(|h| h.bytes as i64),
        ])?;
    }
    let mut game = tx.prepare(
        "INSERT INTO audit_games (audit_id, seq, name, status, missing_roms, claimed_roms, warnings,
                                  available_elsewhere)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;
    for (seq, g) in report.games.iter().enumerate() {
        let missing = serde_json::to_string(&g.missing_roms).map_err(corrupt)?;
        let claimed = serde_json::to_string(&g.claimed_roms).map_err(corrupt)?;
        let warnings = serde_json::to_string(&g.warnings).map_err(corrupt)?;
        let elsewhere = serde_json::to_string(&g.available_elsewhere).map_err(corrupt)?;
        game.execute(params![
            id,
            seq as i64,
            g.name,
            g.status.as_str(),
            missing,
            claimed,
            warnings,
            elsewhere
        ])?;
    }
    drop((entry, game));
    let content = content_sha256(tx, id)?;
    tx.execute(
        "UPDATE audits SET content_sha256 = ?2 WHERE id = ?1",
        params![id, content],
    )?;
    Ok(())
}

/// What the Audit `id` found, as a SHA-256 over its DAT Revision, its Entries and its Game Statuses in a fixed order:
/// equal for two Audits that found the same. How an Entry was verified (its evidence, and when it was read) is left
/// out: a re-read of an unchanged file finds the same. A Header, Game Warnings and Available Elsewhere ROMs count only
/// where there are any, so an Audit that found none hashes as Audits did before there were any.
pub(super) fn content_sha256(c: &Connection, id: i64) -> Result<String> {
    let mut hasher = Sha256::new();
    let mut field = |value: Option<String>| {
        match value {
            Some(v) => {
                hasher.update((v.len() as u64).to_le_bytes());
                hasher.update(v.as_bytes());
            }
            None => hasher.update(u64::MAX.to_le_bytes()),
        };
    };
    let revision: i64 = c.query_row(
        "SELECT dat_revision_id FROM audits WHERE id = ?1",
        [id],
        |r| r.get(0),
    )?;
    field(Some(revision.to_string()));
    let mut entries = c.prepare(
        "SELECT path, member, finding, roms, named_game, named_rom, reason, header_bytes FROM audit_entries
         WHERE audit_id = ?1 ORDER BY path, member, seq",
    )?;
    let mut rows = entries.query([id])?;
    while let Some(row) = rows.next()? {
        for i in 0..7 {
            field(row.get(i)?);
        }
        if let Some(bytes) = row.get::<_, Option<i64>>(7)? {
            field(Some(format!("header {bytes}")));
        }
    }
    field(None);
    let mut games = c.prepare(
        "SELECT name, status, missing_roms, claimed_roms, warnings, available_elsewhere FROM audit_games
         WHERE audit_id = ?1 ORDER BY name, seq",
    )?;
    let mut rows = games.query([id])?;
    while let Some(row) = rows.next()? {
        for i in 0..4 {
            field(row.get(i)?);
        }
        let warnings: String = row.get(4)?;
        if warnings != "[]" {
            field(Some(format!("warnings {warnings}")));
        }
        let elsewhere: String = row.get(5)?;
        if elsewhere != "[]" {
            field(Some(format!("available elsewhere {elsewhere}")));
        }
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// The Audit `id`'s content SHA-256: as recorded when it finished, or else worked out now.
pub(super) fn recorded_content_sha256(c: &Connection, id: i64) -> Result<String> {
    let recorded: Option<String> = c.query_row(
        "SELECT content_sha256 FROM audits WHERE id = ?1",
        [id],
        |r| r.get(0),
    )?;
    match recorded {
        Some(sha) => Ok(sha),
        None => content_sha256(c, id),
    }
}

/// Deletes the Audits matching `condition`, with their Entries and Game Statuses.
pub(super) fn delete_audits(
    tx: &Transaction,
    condition: &str,
    params: impl rusqlite::Params + Copy,
) -> Result<usize> {
    // Claim stamps go with their audit by `ON DELETE CASCADE`.
    for table in ["audit_entries", "audit_games", "audit_citations"] {
        tx.execute(
            &format!(
                "DELETE FROM {table} WHERE audit_id IN (SELECT id FROM audits WHERE {condition})"
            ),
            params,
        )?;
    }
    Ok(tx.execute(&format!("DELETE FROM audits WHERE {condition}"), params)?)
}

pub(super) fn entry_from_row(row: &Row) -> Result<Entry> {
    let finding: String = row.get(2)?;
    let roms: String = row.get(3)?;
    let named_game: Option<String> = row.get(4)?;
    let named_name: Option<String> = row.get(5)?;
    let level: Option<String> = row.get(6)?;
    let algorithms: Option<String> = row.get(7)?;
    let read_at: Option<String> = row.get(9)?;
    let reused: bool = row.get(10)?;
    let header: Option<i64> = row.get(11)?;
    let evidence = match (level, algorithms) {
        (Some(level), Some(algorithms)) => Some(Evidence {
            level: EvidenceLevel::parse(&level).ok_or_else(|| corrupt(&level))?,
            algorithms: algorithms
                .split(',')
                .map(|a| {
                    ALGORITHMS
                        .into_iter()
                        .find(|k| *k == a)
                        .map(Into::into)
                        .ok_or_else(|| corrupt(a))
                })
                .collect::<Result<_>>()?,
            read_at: read_at.as_deref().map(timestamp).transpose()?,
            reused: reused.then_some(Reuse::UnchangedFileHeuristic),
            header: header.map(|bytes| Header {
                bytes: bytes as u64,
            }),
        }),
        _ => None,
    };
    Ok(Entry {
        path: row.get(0)?,
        member: row.get(1)?,
        finding: Finding::parse(&finding).ok_or_else(|| corrupt(&finding))?,
        roms: serde_json::from_str(&roms).map_err(corrupt)?,
        named_rom: match (named_game, named_name) {
            (Some(game), Some(name)) => Some(RomRef { game, name }),
            _ => None,
        },
        evidence,
        reason: row.get(8)?,
    })
}

fn game_from_row(row: &Row) -> Result<Game> {
    let status: String = row.get(1)?;
    let missing: String = row.get(2)?;
    let claimed: String = row.get(3)?;
    let warnings: String = row.get(4)?;
    let elsewhere: String = row.get(5)?;
    Ok(Game {
        name: row.get(0)?,
        status: GameStatus::parse(&status).ok_or_else(|| corrupt(&status))?,
        missing_roms: serde_json::from_str(&missing).map_err(corrupt)?,
        claimed_roms: serde_json::from_str(&claimed).map_err(corrupt)?,
        warnings: serde_json::from_str(&warnings).map_err(corrupt)?,
        available_elsewhere: serde_json::from_str(&elsewhere).map_err(corrupt)?,
    })
}

/// The report of the finished Audit `audit_id`, generated from its rows on `c`.
fn read_report(c: &Connection, audit_id: i64) -> Result<Report> {
    let head = c
        .query_row(
            &format!(
                "SELECT a.source_set_path, a.started_at, a.finished_at, a.pruned_at IS NOT NULL,
                        r.sha256, d.name, r.version, r.file_name, a.evidence, a.header_rule, {}, {}
                 FROM audits a
                 JOIN dat_revisions r ON r.id = a.dat_revision_id
                 JOIN dats d ON d.id = r.dat_id
                 WHERE a.id = ?1 AND a.status = 'finished'",
                count_columns("a."),
                warning_columns("a.")
            ),
            [audit_id],
            |r| {
                Ok((
                    (
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, bool>(3)?,
                    ),
                    DatRevision {
                        sha256: r.get(4)?,
                        name: r.get(5)?,
                        version: r.get(6)?,
                        file: r.get(7)?,
                        header_rule: r
                            .get::<_, Option<String>>(9)?
                            .as_deref()
                            .map(NamedHeaderRule::new),
                    },
                    evidence_from(r, 8)?,
                    counts_from(r, 10)?,
                    warnings_from(r, 10 + Finding::ALL.len() + GameStatus::ALL.len())?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("no finished audit {audit_id}")))?;
    let (
        (source_set, started_at, finished_at, pruned),
        dat_revision,
        evidence,
        (findings, games),
        warnings,
    ) = head;
    if pruned {
        return Err(Error::Refused(format!(
            "audit {audit_id} was pruned; only its summary is kept"
        )));
    }
    let mut stmt = c.prepare(&format!(
        "SELECT {ENTRY_COLUMNS} FROM audit_entries WHERE audit_id = ?1 ORDER BY seq"
    ))?;
    let mut rows = stmt.query([audit_id])?;
    let mut entries = Vec::new();
    while let Some(row) = rows.next()? {
        entries.push(entry_from_row(row)?);
    }
    let mut stmt = c.prepare(&format!(
        "SELECT {GAME_COLUMNS} FROM audit_games WHERE audit_id = ?1 ORDER BY seq"
    ))?;
    let mut rows = stmt.query([audit_id])?;
    let mut game_rows = Vec::new();
    while let Some(row) = rows.next()? {
        game_rows.push(game_from_row(row)?);
    }
    Ok(Report {
        schema: SCHEMA,
        dat_revision,
        source_set,
        evidence,
        started_at: timestamp(&started_at)?,
        finished_at: timestamp(&finished_at)?,
        summary: Summary {
            findings,
            games,
            warnings,
        },
        entries,
        games: game_rows,
    })
}

impl Store {
    /// Audits the registered Source Set `name` against the DAT Revision chosen by `revision` (a SHA-256 prefix),
    /// or by default the latest Audit's, else the newest revision of its DAT, getting checksums as
    /// `evidence` says. Each file's payload read is recorded as it finishes, for later Audits to reuse. The Audit is
    /// recorded in one transaction when it finishes; older Audits are then pruned and unheld DAT blobs dropped.
    pub fn audit_source_set(
        &self,
        config: &Config,
        name: &str,
        revision: Option<&str>,
        evidence: AuditEvidence,
    ) -> Result<StoredAudit> {
        let running = self.begin_audit(config, name, revision, evidence)?;
        self.complete_audit(config, running, None)
    }

    fn begin_audit(
        &self,
        config: &Config,
        name: &str,
        revision: Option<&str>,
        evidence: AuditEvidence,
    ) -> Result<RunningAudit> {
        let set = self.read(|c| set_by_name(c, name))?;
        let how = How {
            job: None,
            workers: ONE_WORKER_EACH,
            evidence,
            resumes: None,
        };
        self.begin(config, set, how, |tx, set| {
            choose_revision(tx, set, revision)
        })
    }

    /// Starts the Audit of a Running Job: its Source Set against the DAT Revision chosen when it was queued.
    pub(super) fn begin_job_audit(&self, config: &Config, job: &JobRow) -> Result<RunningAudit> {
        let set = self.read(|c| set_by_id(c, job.source_set_id))?;
        let revision_id = job.dat_revision_id;
        let how = How {
            job: Some(job.id),
            workers: job.workers,
            evidence: job.evidence,
            resumes: job.requeued_from,
        };
        self.begin(config, set, how, |tx, _| revision_by_id(tx, revision_id))
    }

    /// Chooses the revision with `choose` and inserts the running row in one transaction, which pins the blob:
    /// retention, which also runs under the writer, treats a running Audit as a holder.
    fn begin(
        &self,
        config: &Config,
        set: SourceSetRow,
        how: How,
        choose: impl FnOnce(&Transaction, &SourceSetRow) -> Result<(i64, DatRevisionRecord)>,
    ) -> Result<RunningAudit> {
        let resolved = resolve(config, &set.record.root, &set.record.path)?;
        let source_set_path = resolved.path.display().to_string();
        let (id, revision) = self.write(|tx| {
            crate::repairs::check_no_repair(tx, set.id, &set.record.name)?;
            let (revision_id, revision) = choose(tx, &set)?;
            check_blob_present(&revision)?;
            tx.execute(
                "INSERT INTO audits (source_set_id, dat_revision_id, status, source_set_path, started_at, job_id,
                                     evidence)
                 VALUES (?1, ?2, 'running', ?3, ?4, ?5, ?6)",
                params![
                    set.id,
                    revision_id,
                    source_set_path,
                    now(),
                    how.job,
                    how.evidence.as_str()
                ],
            )?;
            Ok((tx.last_insert_rowid(), revision))
        })?;
        Ok(RunningAudit {
            id,
            set_id: set.id,
            resolved,
            revision,
            job: how.job,
            workers: how.workers,
            evidence: how.evidence,
            resumes: how.resumes,
        })
    }

    /// Finishes a running Audit, then prunes the Source Set's Audits and drops unheld DAT blobs.
    pub(super) fn complete_audit(
        &self,
        config: &Config,
        running: RunningAudit,
        progress: Option<&(dyn Fn(Progress) + Sync)>,
    ) -> Result<StoredAudit> {
        let set_id = running.set_id;
        let stored = self.finish(running, progress)?;
        self.prune(set_id, config.audits.keep)?;
        self.retain_blobs()?;
        Ok(stored)
    }

    /// Scans a running Audit and finishes it, and its Job if it has one, in one transaction. Its report is read
    /// back in that transaction, so no other Audit's pruning can come between. An Audit that fails is deleted, and
    /// its Job Failed, in one transaction.
    fn finish(
        &self,
        running: RunningAudit,
        progress: Option<&(dyn Fn(Progress) + Sync)>,
    ) -> Result<StoredAudit> {
        let id = running.id;
        let job = running.job;
        let finished = self.scan(&running, progress).and_then(|report| {
            self.write(|tx| {
                finish_audit(tx, id, &report)?;
                if let Some(job) = job {
                    super::jobs::succeed(tx, job, id)?;
                }
                read_report(tx, id)
            })
        });
        match finished {
            Ok(report) => Ok(StoredAudit { id, report }),
            Err(err) => {
                // Best effort: a running row that survives this is deleted when the store next opens, and its
                // Job is Interrupted when the Service next starts.
                let _ = self.write(|tx| {
                    delete_audits(tx, "id = ?1", [id])?;
                    if let Some(job) = job {
                        super::jobs::fail(tx, job, &err.to_string())?;
                    }
                    Ok(())
                });
                Err(err)
            }
        }
    }

    /// Reads the Source Set's files against the revision's blob, confined to the Root.
    fn scan(
        &self,
        running: &RunningAudit,
        progress: Option<&(dyn Fn(Progress) + Sync)>,
    ) -> Result<Report> {
        let revision = &running.revision;
        let blob = self.blob_path(&revision.sha256);
        let reads = StoredReads::new(self, running);
        let report = title_select_engine::audit_with(
            &blob,
            &running.resolved.path,
            &Options {
                file: revision.file.clone(),
                confine: Some(&running.resolved.canonical_root),
                workers: running.workers,
                progress,
                evidence: running.evidence,
                reads: Some(&reads),
            },
        )?;
        if let Some(err) = reads
            .error
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner)
        {
            return Err(err);
        }
        if report.dat_revision.sha256 != revision.sha256 {
            return Err(Error::Io(format!(
                "{}: the blob does not match its SHA-256",
                blob.display()
            )));
        }
        Ok(report)
    }

    /// Deletes the rows of Audits left running by a process that stopped mid-Audit.
    pub(super) fn discard_unfinished_audits(&self) -> Result<usize> {
        self.write(|tx| delete_audits(tx, "status = 'running'", []))
    }

    /// The `schema: 4` report of a stored Audit, generated from its rows.
    pub fn report(&self, audit_id: i64) -> Result<Report> {
        self.read(|conn| {
            // One read transaction, so a concurrent prune cannot leave the counts without their rows. It only
            // reads, so ending it by rollback when it drops is enough.
            let tx = conn.unchecked_transaction()?;
            read_report(&tx, audit_id)
        })
    }

    /// The Audits of the Source Set `name`, the latest to finish first.
    pub fn history(&self, name: &str) -> Result<Vec<AuditSummary>> {
        let set = self.read(|c| set_by_name(c, name))?;
        self.source_set_history_by_id(set.id)
    }

    /// The finished Audits of Source Set `id`, newest first, whether it is registered or Unregistered.
    pub fn source_set_history_by_id(&self, id: i64) -> Result<Vec<AuditSummary>> {
        self.read(|c| {
            crate::sets::set_by_id(c, id)?;
            let mut stmt = c.prepare(&format!("{SUMMARY_QUERY} WHERE a.source_set_id = ?1 AND a.status = 'finished' ORDER BY a.finish_seq DESC"))?;
            let rows = stmt.query_map([id], summary_from_row)?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }

    /// Every registered Source Set, by name, with its latest finished Audit and its open Repair: what the list of
    /// Source Sets shows, in one read.
    pub fn source_set_summaries(&self) -> Result<Vec<SourceSetSummary>> {
        let sets = self.source_sets()?;
        self.read(|c| {
            let mut latest = c.prepare(&format!(
                "{SUMMARY_QUERY} WHERE s.name = ?1 AND s.unregistered_at IS NULL AND a.status = 'finished'
                 ORDER BY a.finish_seq DESC LIMIT 1"
            ))?;
            sets.into_iter()
                .map(|set| {
                    let latest = latest
                        .query_row([&set.name], summary_from_row)
                        .optional()?;
                    let repair = open_repair(c, &set.name, latest.as_ref().map(|a| a.id))?;
                    Ok(SourceSetSummary {
                        set,
                        latest,
                        repair,
                    })
                })
                .collect()
        })
    }

    /// The summary of the finished Audit `audit_id`, pruned or not.
    pub fn audit(&self, audit_id: i64) -> Result<AuditSummary> {
        self.read(|c| {
            c.query_row(
                &format!("{SUMMARY_QUERY} WHERE a.id = ?1 AND a.status = 'finished'"),
                [audit_id],
                summary_from_row,
            )
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("no finished audit {audit_id}")))
        })
    }

    /// Up to `limit` of a full Audit's Entries after the one at `after`, in report order, optionally only those with
    /// `finding`. The second value is the position to pass as `after` for the next page, if there may be one.
    pub fn entries(
        &self,
        audit_id: i64,
        after: Option<u64>,
        limit: usize,
        finding: Option<Finding>,
    ) -> Result<(Vec<EntryRow>, Option<u64>)> {
        self.read(|conn| {
            let tx = conn.unchecked_transaction()?;
            check_full(&tx, audit_id)?;
            let mut stmt = tx.prepare(&format!(
                "SELECT {ENTRY_COLUMNS}, seq FROM audit_entries
                 WHERE audit_id = ?1 AND seq > ?2 AND (?3 IS NULL OR finding = ?3)
                 ORDER BY seq LIMIT ?4"
            ))?;
            let mut rows = stmt.query(params![
                audit_id,
                after.map_or(-1, |a| a as i64),
                finding.map(Finding::as_str),
                limit as i64 + 1
            ])?;
            let mut items = Vec::new();
            while let Some(row) = rows.next()? {
                items.push(EntryRow {
                    seq: row.get::<_, i64>(ENTRY_SEQ)? as u64,
                    entry: entry_from_row(row)?,
                });
            }
            Ok(page(items, limit, |e| e.seq))
        })
    }

    /// Calls `f` with each of a full Audit's Entries in report order, in one read transaction, until it returns
    /// false.
    pub fn for_each_entry(&self, audit_id: i64, mut f: impl FnMut(EntryRow) -> bool) -> Result<()> {
        self.read(|conn| {
            let tx = conn.unchecked_transaction()?;
            check_full(&tx, audit_id)?;
            let mut stmt = tx.prepare(&format!(
                "SELECT {ENTRY_COLUMNS}, seq FROM audit_entries WHERE audit_id = ?1 ORDER BY seq"
            ))?;
            let mut rows = stmt.query([audit_id])?;
            while let Some(row) = rows.next()? {
                let entry = EntryRow {
                    seq: row.get::<_, i64>(ENTRY_SEQ)? as u64,
                    entry: entry_from_row(row)?,
                };
                if !f(entry) {
                    break;
                }
            }
            Ok(())
        })
    }

    /// Up to `limit` of a full Audit's Game Statuses after the one at `after`, like [`Store::entries`].
    pub fn games(
        &self,
        audit_id: i64,
        after: Option<u64>,
        limit: usize,
        status: Option<GameStatus>,
    ) -> Result<(Vec<GameRow>, Option<u64>)> {
        self.read(|conn| {
            let tx = conn.unchecked_transaction()?;
            check_full(&tx, audit_id)?;
            let mut stmt = tx.prepare(&format!(
                "SELECT {GAME_COLUMNS}, seq FROM audit_games
                 WHERE audit_id = ?1 AND seq > ?2 AND (?3 IS NULL OR status = ?3)
                 ORDER BY seq LIMIT ?4"
            ))?;
            let mut rows = stmt.query(params![
                audit_id,
                after.map_or(-1, |a| a as i64),
                status.map(GameStatus::as_str),
                limit as i64 + 1
            ])?;
            let mut items = Vec::new();
            while let Some(row) = rows.next()? {
                items.push(GameRow {
                    seq: row.get::<_, i64>(GAME_SEQ)? as u64,
                    game: game_from_row(row)?,
                });
            }
            Ok(page(items, limit, |g| g.seq))
        })
    }

    /// Prunes the Source Set's full Audits beyond the latest `keep` to finish to their summary rows, except any
    /// cited as provenance. The latest Audit is always kept.
    fn prune(&self, set_id: i64, keep: usize) -> Result<Vec<i64>> {
        self.write(|tx| {
            let mut stmt = tx.prepare(
                "SELECT id, EXISTS (SELECT 1 FROM audit_citations c WHERE c.audit_id = a.id)
                 FROM audits a
                 WHERE source_set_id = ?1 AND status = 'finished' AND pruned_at IS NULL
                 ORDER BY finish_seq DESC",
            )?;
            let full = stmt
                .query_map([set_id], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, bool>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let pruned: Vec<i64> = full
                .into_iter()
                .skip(keep.max(1))
                .filter(|(_, cited)| !cited)
                .map(|(id, _)| id)
                .collect();
            let at = now();
            for id in &pruned {
                tx.execute("DELETE FROM audit_entries WHERE audit_id = ?1", [id])?;
                tx.execute("DELETE FROM audit_games WHERE audit_id = ?1", [id])?;
                tx.execute("DELETE FROM audit_stamps WHERE audit_id = ?1", [id])?;
                tx.execute("DELETE FROM audit_claim_stamps WHERE audit_id = ?1", [id])?;
                tx.execute(
                    "UPDATE audits SET pruned_at = ?1 WHERE id = ?2",
                    params![at, id],
                )?;
            }
            Ok(pruned)
        })
    }
}

/// Keeps the first `limit` items; the cursor is the last kept item's position when more followed.
fn page<T>(mut items: Vec<T>, limit: usize, seq: impl Fn(&T) -> u64) -> (Vec<T>, Option<u64>) {
    let more = items.len() > limit;
    items.truncate(limit);
    let next = if more { items.last().map(seq) } else { None };
    (items, next)
}

/// Refuses an Audit that is not finished or whose Entries were pruned.
fn check_full(c: &Connection, audit_id: i64) -> Result<()> {
    let pruned: Option<bool> = c
        .query_row(
            "SELECT pruned_at IS NOT NULL FROM audits WHERE id = ?1 AND status = 'finished'",
            [audit_id],
            |r| r.get(0),
        )
        .optional()?;
    match pruned {
        None => Err(Error::NotFound(format!("no finished audit {audit_id}"))),
        Some(true) => Err(Error::Refused(format!(
            "audit {audit_id} was pruned; only its summary is kept"
        ))),
        Some(false) => Ok(()),
    }
}

pub(super) fn check_blob_present(revision: &DatRevisionRecord) -> Result<()> {
    if revision.blob_present {
        return Ok(());
    }
    Err(Error::Refused(format!(
        "DAT revision {} of {} is no longer kept; import its file ({}) again to restore it",
        revision.sha256,
        revision.dat(),
        revision.file
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store with keep = 1 and one Source Set `Test` in a Root, bound to an imported DAT.
    fn store_with_a_set() -> (tempfile::TempDir, Store, Config) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("sets");
        std::fs::create_dir_all(root.join("Test")).unwrap();
        let dat = tmp.path().join("Test.dat");
        std::fs::write(
            &dat,
            "<datafile><header><name>Test</name><version>1</version></header></datafile>",
        )
        .unwrap();
        let config = Config::parse(&format!(
            "[roots.sets]\npath = {:?}\n\n[audits]\nkeep = 1\n",
            root.display().to_string()
        ))
        .unwrap();
        let store = Store::open(&tmp.path().join("store")).unwrap();
        store.import_dat(&dat, "Test", None).unwrap();
        store
            .add_source_set(&config, "sets", "Test", "Test/Test")
            .unwrap();

        (tmp, store, config)
    }

    #[test]
    fn the_latest_audit_is_the_last_to_finish_and_is_never_pruned_by_its_own_completion() {
        let (_tmp, store, config) = store_with_a_set();
        let first = store
            .begin_audit(&config, "Test", None, AuditEvidence::Incremental)
            .unwrap();
        let second = store
            .begin_audit(&config, "Test", None, AuditEvidence::Incremental)
            .unwrap();
        let (first_id, second_id) = (first.id, second.id);
        store.complete_audit(&config, second, None).unwrap();
        let completed = store.complete_audit(&config, first, None).unwrap();

        assert_eq!(completed.id, first_id);
        let history = store.history("Test").unwrap();
        assert_eq!(
            history.iter().map(|a| (a.id, a.pruned)).collect::<Vec<_>>(),
            vec![(first_id, false), (second_id, true)],
            "the Audit that finished last is the latest"
        );
        assert!(store.report(first_id).is_ok());
        let set = &store.source_sets().unwrap()[0];
        let last = set.last_verified_against.as_ref().unwrap();
        assert_eq!(last.finished_at, history[0].finished_at);
    }

    #[test]
    fn a_finished_audits_report_survives_another_audit_pruning_it_before_its_own_pruning() {
        let (_tmp, store, config) = store_with_a_set();
        let first = store
            .begin_audit(&config, "Test", None, AuditEvidence::Incremental)
            .unwrap();
        let second = store
            .begin_audit(&config, "Test", None, AuditEvidence::Incremental)
            .unwrap();
        let (first_id, set_id) = (first.id, first.set_id);

        let finished = store.finish(first, None).unwrap();
        store.complete_audit(&config, second, None).unwrap();
        store.prune(set_id, config.audits.keep).unwrap();

        assert_eq!(finished.id, first_id);
        assert_eq!(finished.report.dat_revision.name, "Test");
        assert!(
            store.history("Test").unwrap()[1].pruned,
            "the other Audit finished later and pruned it"
        );
        assert!(matches!(store.report(first_id), Err(Error::Refused(_))));
    }

    #[test]
    fn pruning_an_audit_deletes_the_stamps_it_read() {
        let (tmp, store, config) = store_with_a_set();
        std::fs::write(tmp.path().join("sets/Test/readme.txt"), b"hello").unwrap();
        let first = store
            .audit_source_set(&config, "Test", None, AuditEvidence::Full)
            .unwrap();
        let second = store
            .audit_source_set(&config, "Test", None, AuditEvidence::Full)
            .unwrap();

        let stamps = |id: i64| -> i64 {
            store
                .read(|c| {
                    Ok(c.query_row(
                        "SELECT COUNT(*) FROM audit_stamps WHERE audit_id = ?1",
                        [id],
                        |r| r.get(0),
                    )?)
                })
                .unwrap()
        };
        assert!(store.audit(first.id).unwrap().pruned);
        assert_eq!(stamps(first.id), 0);
        assert_eq!(stamps(second.id), 1);
    }
}
